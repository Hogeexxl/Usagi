//! Privacy-safe parser for Codex rollout usage records.

use chrono::DateTime;
use serde_json::{Map, Value};

use crate::codex::normalization::{CodexRawTokenUsage, CodexRolloutAdapter};
use crate::usage::normalized::NormalizedTokenUsage;

pub struct CompleteUsageLine {
    start_offset: u64,
    end_offset: u64,
    bytes: Vec<u8>,
}

impl CompleteUsageLine {
    pub fn new(start_offset: u64, bytes_with_newline: Vec<u8>) -> Option<Self> {
        if !bytes_with_newline.ends_with(b"\n") {
            return None;
        }
        let length = u64::try_from(bytes_with_newline.len()).ok()?;
        Some(Self {
            start_offset,
            end_offset: start_offset.checked_add(length)?,
            bytes: bytes_with_newline,
        })
    }

    pub const fn start_offset(&self) -> u64 {
        self.start_offset
    }

    pub const fn end_offset(&self) -> u64 {
        self.end_offset
    }

    pub(crate) fn json_bytes(&self) -> &[u8] {
        let without_lf = &self.bytes[..self.bytes.len() - 1];
        without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenValueError {
    MissingRequiredField,
    InvalidRequiredField,
    InvalidVector,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NormalizedTokenValue {
    Valid(NormalizedTokenUsage),
    Invalid(TokenValueError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OptionalTokenValue {
    Missing,
    Valid(NormalizedTokenUsage),
    Invalid(TokenValueError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenCountInfo {
    pub current_total: NormalizedTokenValue,
    pub last_usage: OptionalTokenValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenCountRecord {
    pub occurred_at_ms: Option<i64>,
    pub info: Option<TokenCountInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleKind {
    Started,
    Completed,
    Aborted,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleRecord {
    pub kind: LifecycleKind,
    pub turn_id: Option<String>,
    pub occurred_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnContextRecord {
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub occurred_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "usage", rename_all = "snake_case")]
pub enum UsageValue {
    Missing,
    Invalid,
    Valid(NormalizedTokenUsage),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResponseUsageEvidence {
    pub response_id: String,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub usage: UsageValue,
    pub thread_token_usage: UsageValue,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionEvidence {
    pub compaction_response_id: Option<String>,
    pub latest_token_usage_record: Option<ResponseUsageEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexOperation {
    Response,
    Compaction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceKind {
    Explicit,
    Legacy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseUsageRecord {
    pub occurred_at_ms: Option<i64>,
    pub evidence: ResponseUsageEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactedRecord {
    pub occurred_at_ms: Option<i64>,
    pub evidence: CompactionEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageRawRecord {
    ResponseUsage(ResponseUsageRecord),
    Compacted(CompactedRecord),
    TokenCount(TokenCountRecord),
    TurnContext(TurnContextRecord),
    Lifecycle(LifecycleRecord),
    Ignored,
    Unknown,
    Malformed,
    OversizedComplete { start_offset: u64, end_offset: u64 },
}

/// Parser for one complete Codex JSONL line. It does not use a model
/// capability matrix; missing cache-write is represented as `None`.
pub struct CodexRolloutParser;

impl CodexRolloutParser {
    pub fn parse_line(&self, line: &CompleteUsageLine) -> UsageRawRecord {
        let Ok(value) = serde_json::from_slice::<Value>(line.json_bytes()) else {
            return UsageRawRecord::Malformed;
        };
        let Some(object) = value.as_object() else {
            return UsageRawRecord::Malformed;
        };
        let outer_timestamp = object.get("timestamp").and_then(parse_timestamp_ms);
        match object.get("type").and_then(Value::as_str) {
            Some("turn_context") => self.parse_turn_context(object, outer_timestamp),
            Some("event_msg") => self.parse_event_msg(object, outer_timestamp),
            Some("token_usage_record") => {
                let Some(payload) = object.get("payload").and_then(Value::as_object) else {
                    return UsageRawRecord::Malformed;
                };
                let Some(evidence) = response_evidence(payload) else {
                    return UsageRawRecord::Malformed;
                };
                UsageRawRecord::ResponseUsage(ResponseUsageRecord {
                    occurred_at_ms: payload
                        .get("timestamp")
                        .and_then(parse_timestamp_ms)
                        .or(outer_timestamp),
                    evidence,
                })
            }
            Some("compacted") => {
                let Some(payload) = object.get("payload").and_then(Value::as_object) else {
                    return UsageRawRecord::Malformed;
                };
                let latest_token_usage_record = match payload.get("latest_token_usage_record") {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(latest)) => {
                        if !valid_optional_thread_id(latest) {
                            return UsageRawRecord::Malformed;
                        }
                        response_evidence(latest)
                    }
                    Some(_) => return UsageRawRecord::Malformed,
                };
                UsageRawRecord::Compacted(CompactedRecord {
                    occurred_at_ms: payload
                        .get("timestamp")
                        .and_then(parse_timestamp_ms)
                        .or(outer_timestamp),
                    evidence: CompactionEvidence {
                        compaction_response_id: safe_string(payload.get("compaction_response_id")),
                        latest_token_usage_record,
                    },
                })
            }
            Some(_) => UsageRawRecord::Unknown,
            None => UsageRawRecord::Malformed,
        }
    }

    pub const fn oversized_complete(start_offset: u64, end_offset: u64) -> UsageRawRecord {
        UsageRawRecord::OversizedComplete {
            start_offset,
            end_offset,
        }
    }

    fn parse_turn_context(
        &self,
        object: &Map<String, Value>,
        outer_timestamp: Option<i64>,
    ) -> UsageRawRecord {
        let payload = object.get("payload").and_then(Value::as_object);
        let reasoning_effort = payload.and_then(|value| {
            // `effort` is the canonical rollout field.  Compatibility
            // fallback is allowed only when that field is absent (not when
            // it is present but empty/invalid).
            match value.get("effort") {
                Some(effort) => normalize_reasoning_effort(effort),
                None => value
                    .get("reasoning_effort")
                    .and_then(normalize_reasoning_effort),
            }
        });
        UsageRawRecord::TurnContext(TurnContextRecord {
            turn_id: payload.and_then(|value| safe_string(value.get("turn_id"))),
            model: payload.and_then(|value| safe_string(value.get("model"))),
            reasoning_effort,
            occurred_at_ms: payload
                .and_then(|value| value.get("timestamp"))
                .and_then(parse_timestamp_ms)
                .or(outer_timestamp),
        })
    }

    fn parse_event_msg(
        &self,
        object: &Map<String, Value>,
        outer_timestamp: Option<i64>,
    ) -> UsageRawRecord {
        let Some(payload) = object.get("payload").and_then(Value::as_object) else {
            return UsageRawRecord::Malformed;
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("token_count") => {
                let occurred_at_ms = payload
                    .get("timestamp")
                    .and_then(parse_timestamp_ms)
                    .or(outer_timestamp);
                let info = match payload.get("info") {
                    None | Some(Value::Null) => None,
                    Some(Value::Object(info)) => Some(TokenCountInfo {
                        current_total: normalize_required_snapshot(info.get("total_token_usage")),
                        last_usage: normalize_optional_snapshot(info.get("last_token_usage")),
                    }),
                    Some(_) => {
                        return UsageRawRecord::TokenCount(TokenCountRecord {
                            occurred_at_ms,
                            info: Some(TokenCountInfo {
                                current_total: NormalizedTokenValue::Invalid(
                                    TokenValueError::InvalidRequiredField,
                                ),
                                last_usage: OptionalTokenValue::Missing,
                            }),
                        });
                    }
                };
                UsageRawRecord::TokenCount(TokenCountRecord {
                    occurred_at_ms,
                    info,
                })
            }
            Some("task_started" | "turn_started") => {
                lifecycle(payload, LifecycleKind::Started, outer_timestamp)
            }
            Some("task_complete" | "turn_complete") => lifecycle(
                payload,
                if payload.get("error").is_some_and(|value| !value.is_null()) {
                    LifecycleKind::Failed
                } else {
                    LifecycleKind::Completed
                },
                outer_timestamp,
            ),
            Some("turn_aborted") => lifecycle(payload, LifecycleKind::Aborted, outer_timestamp),
            Some("rate_limits") => UsageRawRecord::Ignored,
            Some(_) => UsageRawRecord::Unknown,
            None => UsageRawRecord::Malformed,
        }
    }
}

fn response_evidence(payload: &Map<String, Value>) -> Option<ResponseUsageEvidence> {
    if !valid_optional_thread_id(payload) {
        return None;
    }
    Some(ResponseUsageEvidence {
        response_id: safe_string(payload.get("response_id"))?,
        thread_id: safe_string(payload.get("thread_id")),
        session_id: safe_string(payload.get("session_id")),
        turn_id: safe_string(payload.get("turn_id")),
        usage: evidence_usage(payload.get("usage")),
        thread_token_usage: evidence_usage(payload.get("thread_token_usage")),
    })
}

fn valid_optional_thread_id(payload: &Map<String, Value>) -> bool {
    match payload.get("thread_id") {
        None | Some(Value::Null) => true,
        value => safe_string(value).is_some(),
    }
}

fn evidence_usage(value: Option<&Value>) -> UsageValue {
    match value {
        None => UsageValue::Missing,
        Some(value) => match normalize_snapshot(value) {
            Ok(usage) => UsageValue::Valid(usage),
            Err(_) => UsageValue::Invalid,
        },
    }
}

/// Normalize the rollout reasoning-effort value without imposing a model
/// capability allowlist.  Unknown-but-safe values remain usable dimensions;
/// empty and control-containing values are treated as unavailable.
fn normalize_reasoning_effort(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if raw.chars().any(char::is_control) {
        return None;
    }
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_lowercase())
}

fn lifecycle(
    payload: &Map<String, Value>,
    kind: LifecycleKind,
    outer_timestamp: Option<i64>,
) -> UsageRawRecord {
    UsageRawRecord::Lifecycle(LifecycleRecord {
        kind,
        turn_id: safe_string(payload.get("turn_id")),
        occurred_at_ms: payload
            .get("timestamp")
            .and_then(parse_timestamp_ms)
            .or(outer_timestamp),
    })
}

fn normalize_required_snapshot(value: Option<&Value>) -> NormalizedTokenValue {
    let Some(value) = value else {
        return NormalizedTokenValue::Invalid(TokenValueError::MissingRequiredField);
    };
    match normalize_snapshot(value) {
        Ok(value) => NormalizedTokenValue::Valid(value),
        Err(error) => NormalizedTokenValue::Invalid(error),
    }
}

fn normalize_optional_snapshot(value: Option<&Value>) -> OptionalTokenValue {
    let Some(value) = value else {
        return OptionalTokenValue::Missing;
    };
    match normalize_snapshot(value) {
        Ok(value) => OptionalTokenValue::Valid(value),
        Err(error) => OptionalTokenValue::Invalid(error),
    }
}

fn normalize_snapshot(value: &Value) -> Result<NormalizedTokenUsage, TokenValueError> {
    let object = value
        .as_object()
        .ok_or(TokenValueError::InvalidRequiredField)?;
    let required = |field| {
        object
            .get(field)
            .ok_or(TokenValueError::MissingRequiredField)?
            .as_i64()
            .ok_or(TokenValueError::InvalidRequiredField)
    };
    let cache_write_tokens = object
        .get("cache_write_input_tokens")
        .map(|value| value.as_i64().ok_or(TokenValueError::InvalidRequiredField))
        .transpose()?;
    CodexRolloutAdapter::normalize(CodexRawTokenUsage {
        input_tokens: required("input_tokens")?,
        cached_input_tokens: required("cached_input_tokens")?,
        cache_write_input_tokens: cache_write_tokens,
        output_tokens: required("output_tokens")?,
        reasoning_output_tokens: required("reasoning_output_tokens")?,
        total_tokens: required("total_tokens")?,
    })
    .map_err(|_| TokenValueError::InvalidVector)
}

fn safe_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
        .map(ToOwned::to_owned)
}

fn parse_timestamp_ms(value: &Value) -> Option<i64> {
    value.as_i64().filter(|value| *value >= 0).or_else(|| {
        value
            .as_str()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
            .filter(|value| *value >= 0)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn line(json: &str) -> CompleteUsageLine {
        CompleteUsageLine::new(0, format!("{json}\n").into_bytes()).unwrap()
    }

    fn snapshot_json(write: Option<&str>) -> String {
        format!(
            "{{\"input_tokens\":10,\"cached_input_tokens\":2,{}\"output_tokens\":4,\"reasoning_output_tokens\":1,\"total_tokens\":14}}",
            write
                .map(|value| format!("\"cache_write_input_tokens\":{value},"))
                .unwrap_or_default()
        )
    }

    fn token_line(total: &str, last: Option<&str>) -> CompleteUsageLine {
        let last = last
            .map(|value| format!(",\"last_token_usage\":{value}"))
            .unwrap_or_default();
        line(&format!(
            "{{\"timestamp\":\"1970-01-01T00:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{total}{last}}}}}}}"
        ))
    }

    fn valid_snapshot() -> Value {
        json!({
            "input_tokens": 10,
            "cached_input_tokens": 2,
            "cache_write_input_tokens": 3,
            "output_tokens": 4,
            "reasoning_output_tokens": 1,
            "total_tokens": 14,
        })
    }

    #[test]
    fn compaction_parse_schema() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex/compaction");
        let mut responses = Vec::new();
        let mut embedded = Vec::new();
        let mut id_only = false;
        let mut usage_null = false;
        let mut context_estimate = false;
        for name in [
            "schema_top_and_embedded.jsonl",
            "schema_id_only.jsonl",
            "schema_usage_null.jsonl",
            "schema_top_only.jsonl",
            "schema_embedded_only.jsonl",
            "schema_zero_context_estimate.jsonl",
        ] {
            let contents = std::fs::read_to_string(directory.join(name))
                .expect("compaction fixtures must be ready before the Phase 1 gate");
            let mut top_count = 0;
            let mut embedded_count = 0;
            for raw in contents.lines() {
                let value: Value = serde_json::from_str(raw).unwrap();
                match CodexRolloutParser.parse_line(&line(raw)) {
                    UsageRawRecord::ResponseUsage(record) => {
                        if matches!(record.evidence.usage, UsageValue::Valid(_)) {
                            assert!(record.occurred_at_ms.is_some());
                            top_count += 1;
                            responses.push(record.evidence);
                        }
                    }
                    UsageRawRecord::Compacted(record) => {
                        if record.evidence.compaction_response_id.is_some()
                            && record.evidence.latest_token_usage_record.is_none()
                        {
                            id_only = true;
                        }
                        if value["payload"]["latest_token_usage_record"].is_object()
                            && value["payload"]["latest_token_usage_record"]["usage"] == Value::Null
                        {
                            assert!(matches!(
                                record.evidence.latest_token_usage_record.as_ref(),
                                Some(ResponseUsageEvidence {
                                    usage: UsageValue::Invalid,
                                    ..
                                })
                            ));
                            usage_null = true;
                        }
                        if let Some(evidence) = record.evidence.latest_token_usage_record
                            && matches!(evidence.usage, UsageValue::Valid(_))
                        {
                            assert_eq!(
                                record.evidence.compaction_response_id.as_deref(),
                                Some(evidence.response_id.as_str())
                            );
                            embedded.push(evidence);
                            embedded_count += 1;
                        }
                    }
                    UsageRawRecord::TokenCount(record)
                        if name == "schema_zero_context_estimate.jsonl" =>
                    {
                        assert!(matches!(
                            record.info.unwrap().last_usage,
                            OptionalTokenValue::Invalid(_)
                        ));
                        context_estimate = true;
                    }
                    _ => {}
                }
            }
            match name {
                "schema_top_only.jsonl" => assert_eq!((top_count, embedded_count), (1, 0)),
                "schema_embedded_only.jsonl" => assert_eq!((top_count, embedded_count), (0, 1)),
                _ => {}
            }
        }
        assert!(!responses.is_empty(), "actual top-level evidence missing");
        assert!(!embedded.is_empty(), "actual embedded evidence missing");
        assert!(id_only, "actual id-only fixture missing");
        assert!(
            usage_null,
            "explicitly constructed usage-null fixture missing"
        );
        assert!(
            context_estimate,
            "actual zero-component context estimate fixture missing"
        );
        assert!(
            embedded.iter().any(|evidence| responses.contains(evidence)),
            "top-level and embedded evidence must preserve the same six-dimensional usage and IDs"
        );
        assert!(responses.iter().any(|evidence| evidence.usage
            == UsageValue::Valid(
                NormalizedTokenUsage::new(311996, 311040, Some(0), 4993, 0, 316989).unwrap()
            )));
    }

    #[test]
    fn compaction_parse_required_values_and_context_estimate() {
        let mut payload = json!({
            "response_id": "fixture-response",
            "thread_id": "fixture-child",
            "session_id": "fixture-root",
            "turn_id": "fixture-turn",
            "usage": valid_snapshot(),
            "thread_token_usage": valid_snapshot(),
            "unknown_body": "BODY_SENTINEL"
        });
        let parse = |payload: &Value| {
            CodexRolloutParser.parse_line(&line(
                &json!({"type":"token_usage_record", "timestamp":1000, "payload":payload})
                    .to_string(),
            ))
        };
        let UsageRawRecord::ResponseUsage(record) = parse(&payload) else {
            panic!("expected response evidence");
        };
        assert_eq!(record.occurred_at_ms, Some(1000));
        assert_eq!(record.evidence.thread_id.as_deref(), Some("fixture-child"));
        assert_eq!(record.evidence.session_id.as_deref(), Some("fixture-root"));
        assert!(!format!("{record:?}").contains("BODY_SENTINEL"));
        payload["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
        let UsageRawRecord::ResponseUsage(record) = parse(&payload) else {
            panic!("expected response evidence");
        };
        assert!(matches!(record.evidence.usage,
            UsageValue::Valid(usage) if usage.cache_write_tokens.is_none()));
        payload["usage"] = json!({"input_tokens":0,"cached_input_tokens":0,
            "output_tokens":0,"reasoning_output_tokens":0,"total_tokens":100});
        let UsageRawRecord::ResponseUsage(record) = parse(&payload) else {
            panic!("expected response evidence");
        };
        assert_eq!(record.evidence.usage, UsageValue::Invalid);
        payload.as_object_mut().unwrap().remove("usage");
        let UsageRawRecord::ResponseUsage(record) = parse(&payload) else {
            panic!("expected response evidence");
        };
        assert_eq!(record.evidence.usage, UsageValue::Missing);
        payload.as_object_mut().unwrap().remove("response_id");
        assert_eq!(parse(&payload), UsageRawRecord::Malformed);
        let UsageRawRecord::Compacted(marker) = CodexRolloutParser.parse_line(&line(
            &json!({"type":"compacted","payload":{
                "compaction_response_id":"marker-id","latest_token_usage_record":payload
            }})
            .to_string(),
        )) else {
            panic!("expected marker without a usable response identity");
        };
        assert_eq!(
            marker.evidence.compaction_response_id.as_deref(),
            Some("marker-id")
        );
        assert!(marker.evidence.latest_token_usage_record.is_none());
    }

    #[test]
    fn compaction_parse_optional_thread_id() {
        for thread_id in [None, Some(Value::Null)] {
            let mut payload = json!({"response_id":"fixture-response","usage":valid_snapshot()});
            if let Some(value) = thread_id {
                payload["thread_id"] = value;
            }
            let UsageRawRecord::ResponseUsage(record) = CodexRolloutParser.parse_line(&line(
                &json!({"type":"token_usage_record","payload":payload}).to_string(),
            )) else {
                panic!("missing or null thread ID is optional");
            };
            assert!(record.evidence.thread_id.is_none());
        }
        for thread_id in [json!(42), json!(""), json!("  "), json!("bad\nthread")] {
            let payload = json!({"response_id":"fixture-response","thread_id":thread_id,"usage":valid_snapshot()});
            for (kind, payload) in [
                ("token_usage_record", payload.clone()),
                ("compacted", json!({"latest_token_usage_record":payload})),
            ] {
                assert_eq!(
                    CodexRolloutParser
                        .parse_line(&line(&json!({"type":kind,"payload":payload}).to_string())),
                    UsageRawRecord::Malformed
                );
            }
        }
    }

    fn parse_snapshot(value: Value) -> NormalizedTokenValue {
        let parser = CodexRolloutParser;
        let UsageRawRecord::TokenCount(record) =
            parser.parse_line(&token_line(&value.to_string(), None))
        else {
            panic!("expected token count");
        };
        record.info.unwrap().current_total
    }

    #[test]
    fn t_dc_011_to_017_parser_boundary_and_two_state_mapping() {
        let parser = CodexRolloutParser;
        let UsageRawRecord::TokenCount(record) = parser.parse_line(&token_line(
            &snapshot_json(Some("3")),
            Some(&snapshot_json(Some("3"))),
        )) else {
            panic!("expected token count");
        };
        let info = record.info.unwrap();
        assert!(
            matches!(info.current_total, NormalizedTokenValue::Valid(value) if value.cache_write_tokens == Some(3))
        );
        assert!(
            matches!(info.last_usage, OptionalTokenValue::Valid(value) if value.cached_tokens == 2)
        );
        let UsageRawRecord::TokenCount(record) = parser.parse_line(&token_line(
            &snapshot_json(None),
            Some(&snapshot_json(None)),
        )) else {
            panic!("expected token count");
        };
        let info = record.info.unwrap();
        assert!(
            matches!(info.last_usage, OptionalTokenValue::Valid(value) if value.cache_write_tokens.is_none())
        );
        let UsageRawRecord::TokenCount(record) = parser.parse_line(&token_line("null", None))
        else {
            panic!("expected token count");
        };
        assert!(matches!(
            record.info.unwrap().current_total,
            NormalizedTokenValue::Invalid(_)
        ));
        assert!(matches!(
            parser.parse_line(&line("not-json")),
            UsageRawRecord::Malformed
        ));
        assert!(matches!(
            parser.parse_line(&line(
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}"
            )),
            UsageRawRecord::Lifecycle(LifecycleRecord {
                kind: LifecycleKind::Started,
                ..
            })
        ));
    }

    #[test]
    fn t_dc_014_required_raw_invalid_matrix() {
        for field in [
            "input_tokens",
            "cached_input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "total_tokens",
        ] {
            let mut value = valid_snapshot();
            value.as_object_mut().unwrap().remove(field);
            assert!(
                matches!(parse_snapshot(value), NormalizedTokenValue::Invalid(_)),
                "missing {field}"
            );

            for replacement in [
                Value::String("10".to_owned()),
                json!(1.5),
                json!(-1),
                json!(9_223_372_036_854_775_808_u64),
            ] {
                let mut value = valid_snapshot();
                value
                    .as_object_mut()
                    .unwrap()
                    .insert(field.to_owned(), replacement);
                assert!(
                    matches!(parse_snapshot(value), NormalizedTokenValue::Invalid(_)),
                    "invalid {field}"
                );
            }
        }

        let mut invalid = valid_snapshot();
        invalid
            .as_object_mut()
            .unwrap()
            .insert("input_tokens".to_owned(), Value::String("10".to_owned()));
        let parser = CodexRolloutParser;
        let UsageRawRecord::TokenCount(record) = parser.parse_line(&token_line(
            &invalid.to_string(),
            Some(&invalid.to_string()),
        )) else {
            panic!("expected token count");
        };
        let info = record.info.unwrap();
        assert!(matches!(
            info.current_total,
            NormalizedTokenValue::Invalid(_)
        ));
        assert!(matches!(info.last_usage, OptionalTokenValue::Invalid(_)));
        let UsageRawRecord::TokenCount(record) =
            parser.parse_line(&token_line(&valid_snapshot().to_string(), None))
        else {
            panic!("expected token count");
        };
        assert!(matches!(
            record.info.unwrap().last_usage,
            OptionalTokenValue::Missing
        ));
    }

    #[test]
    fn parser_lifecycle_aliases_unknown_records_and_boundaries_are_safe() {
        let parser = CodexRolloutParser;
        for (wire, expected) in [
            ("task_started", LifecycleKind::Started),
            ("turn_started", LifecycleKind::Started),
            ("task_complete", LifecycleKind::Completed),
            ("turn_complete", LifecycleKind::Completed),
            ("turn_aborted", LifecycleKind::Aborted),
        ] {
            let record = line(&format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{wire}\"}}}}"
            ));
            assert!(matches!(
                parser.parse_line(&record),
                UsageRawRecord::Lifecycle(LifecycleRecord {
                    kind,
                    turn_id: None,
                    occurred_at_ms: None,
                }) if kind == expected
            ));
        }
        let failed = line(
            r#"{"type":"event_msg","payload":{"type":"task_complete","error":{"code":"FAILED"}}}"#,
        );
        assert!(matches!(
            parser.parse_line(&failed),
            UsageRawRecord::Lifecycle(LifecycleRecord {
                kind: LifecycleKind::Failed,
                ..
            })
        ));
        assert!(matches!(
            parser.parse_line(&line(
                r#"{"type":"event_msg","payload":{"type":"rate_limits"}}"#,
            )),
            UsageRawRecord::Ignored
        ));
        assert!(matches!(
            parser.parse_line(&line(
                r#"{"type":"event_msg","payload":{"type":"future","body":"BODY_SENTINEL"}}"#,
            )),
            UsageRawRecord::Unknown
        ));
        assert!(matches!(
            parser.parse_line(&line("not-json")),
            UsageRawRecord::Malformed
        ));
        assert!(CompleteUsageLine::new(0, b"half-line".to_vec()).is_none());
        assert_eq!(
            CodexRolloutParser::oversized_complete(10, 20),
            UsageRawRecord::OversizedComplete {
                start_offset: 10,
                end_offset: 20,
            }
        );

        let large_ignored = "x".repeat(4 * 1024 * 1024);
        let large = line(&format!(
            "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":null,\"future\":\"{large_ignored}\"}}}}"
        ));
        assert!(large.end_offset() > 4 * 1024 * 1024);
        assert!(matches!(
            parser.parse_line(&large),
            UsageRawRecord::TokenCount(TokenCountRecord { info: None, .. })
        ));
    }

    #[test]
    fn t_mu03_c01_reasoning_effort_priority_normalization_and_safety() {
        let parser = CodexRolloutParser;
        let parse_effort = |payload: Value| {
            let line =
                line(&serde_json::json!({"type": "turn_context", "payload": payload}).to_string());
            let UsageRawRecord::TurnContext(record) = parser.parse_line(&line) else {
                panic!("expected turn context");
            };
            record.reasoning_effort
        };

        assert_eq!(
            parse_effort(
                serde_json::json!({"model": "gpt", "effort": "  HIGH  ", "reasoning_effort": "low"})
            ),
            Some("high".to_owned())
        );
        assert_eq!(
            parse_effort(serde_json::json!({"model": "gpt", "reasoning_effort": " Medium "})),
            Some("medium".to_owned())
        );
        // A present-but-empty canonical field does not fall back to the
        // compatibility field.
        assert_eq!(
            parse_effort(
                serde_json::json!({"model": "gpt", "effort": "", "reasoning_effort": "high"})
            ),
            None
        );
        assert_eq!(
            parse_effort(serde_json::json!({"model": "gpt", "effort": "high\n"})),
            None
        );
    }

    #[test]
    fn t_dc_015_raw_canonical_invariant_matrix() {
        for (field, replacement) in [
            ("cached_input_tokens", json!(11)),
            ("reasoning_output_tokens", json!(5)),
            ("total_tokens", json!(15)),
            ("cache_write_input_tokens", json!(9)),
        ] {
            let mut value = valid_snapshot();
            value
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), replacement);
            assert!(
                matches!(parse_snapshot(value), NormalizedTokenValue::Invalid(_)),
                "invalid {field}"
            );
        }
    }
}
