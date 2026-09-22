//! Candidate pipeline normalization and Session metadata patch construction.

use std::collections::BTreeMap;

use crate::antigravity::annotation::AnnotationTitleResult;
use crate::antigravity::project::{
    WorkspaceEvaluation, evaluate_blob_workspace, evaluate_summary_workspace,
};
use crate::antigravity::protobuf::{
    MAX_TITLE_BYTES, ProtoError, RawAntigravityUsageCandidate, RawModelField, parse_step_metadata,
};
use crate::domain::{
    AgentRole, MetadataQualityStatus, Patch, ProjectKind, ResolvedThreadPatch, SessionIdentity,
};
use crate::source::SourceId;

/// Quarantine reason codes for Antigravity candidates.
pub const GEN_METADATA_MALFORMED: &str = "GEN_METADATA_MALFORMED";
pub const USAGE_RESPONSE_ID_MISSING: &str = "USAGE_RESPONSE_ID_MISSING";
pub const USAGE_RESPONSE_ID_INVALID: &str = "USAGE_RESPONSE_ID_INVALID";
pub const USAGE_RESPONSE_ID_CONFLICT: &str = "USAGE_RESPONSE_ID_CONFLICT";
pub const USAGE_MODEL_MISSING: &str = "USAGE_MODEL_MISSING";
pub const USAGE_MODEL_INVALID: &str = "USAGE_MODEL_INVALID";
pub const USAGE_STEP_NOT_FOUND: &str = "USAGE_STEP_NOT_FOUND";
pub const USAGE_STEP_NOT_UNIQUE: &str = "USAGE_STEP_NOT_UNIQUE";
pub const USAGE_STEP_KIND_MISMATCH: &str = "USAGE_STEP_KIND_MISMATCH";
pub const USAGE_STEP_METADATA_INVALID: &str = "USAGE_STEP_METADATA_INVALID";
pub const USAGE_TIMESTAMP_INVALID: &str = "USAGE_TIMESTAMP_INVALID";
pub const USAGE_TOKEN_INVALID: &str = "USAGE_TOKEN_INVALID";
pub const USAGE_EVENT_MUTATION_CONFLICT: &str = "USAGE_EVENT_MUTATION_CONFLICT";

use crate::source::adapter::CanonicalUsageEventWrite;
use crate::usage::{NormalizedTokenUsage, event::EventKind};

/// Validated, normalized Usage event record ready for canonical compare.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AntigravityUsageRecord {
    pub gen_idx: i64,
    pub response_id: String,
    pub model: String,
    pub occurred_at_ms: i64,
    pub uncached_input_tokens: i64,
    pub cached_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub payload_digest: String,
}

impl AntigravityUsageRecord {
    pub fn to_canonical_event(
        &self,
        conversation_id: &str,
    ) -> Result<CanonicalUsageEventWrite, String> {
        let identity = SessionIdentity::namespaced(SourceId::ANTIGRAVITY, conversation_id)
            .map_err(|e| e.to_string())?;
        let event_id = format!("{conversation_id}:{}", self.response_id);
        let input_tokens = self
            .uncached_input_tokens
            .checked_add(self.cached_tokens)
            .ok_or_else(|| "input tokens overflow".to_string())?;
        let total_tokens = input_tokens
            .checked_add(self.output_tokens)
            .ok_or_else(|| "total tokens overflow".to_string())?;
        let usage = NormalizedTokenUsage::new(
            input_tokens,
            self.cached_tokens,
            None,
            self.output_tokens,
            self.reasoning_tokens,
            total_tokens,
        )
        .map_err(|e| e.to_string())?;

        Ok(CanonicalUsageEventWrite {
            event_id,
            kind: EventKind::Normal,
            occurred_at_ms: self.occurred_at_ms,
            thread_id: identity.thread_id.clone(),
            root_session_id: identity.thread_id,
            turn_key: None,
            model: self.model.clone(),
            reasoning_effort: None,
            estimated_cost_nanos_usd: None,
            usage,
            created_at_ms: self.occurred_at_ms,
        })
    }
}

/// Source-private quarantine diagnosis record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AntigravityQuarantineRecord {
    pub conversation_id: String,
    pub payload_digest: String,
    pub gen_idx: Option<i64>,
    pub response_id: Option<String>,
    pub reason_code: &'static str,
}

/// An entry in the step index constructed from type-15 steps.
#[derive(Clone, Debug)]
pub struct StepIndexEntry {
    pub step_idx: i64,
    pub metadata_bytes: Option<Vec<u8>>,
}

/// Outcome of running the deterministic Usage candidate pipeline on a conversation.
#[derive(Clone, Debug, Default)]
pub struct NormalizedUsagePipelineOutcome {
    pub valid_records: Vec<AntigravityUsageRecord>,
    pub quarantine_records: Vec<AntigravityQuarantineRecord>,
}

/// Run the candidate normalization pipeline according to Section 4.10.3 state machine.
pub fn normalize_candidates(
    conversation_id: &str,
    candidates: Vec<RawAntigravityUsageCandidate>,
    initial_quarantines: Vec<AntigravityQuarantineRecord>,
    step_index: &BTreeMap<String, Vec<StepIndexEntry>>,
) -> NormalizedUsagePipelineOutcome {
    let mut outcome = NormalizedUsagePipelineOutcome {
        valid_records: Vec::new(),
        quarantine_records: initial_quarantines,
    };

    // 1. Separate candidates with missing response_id
    let mut with_id: Vec<RawAntigravityUsageCandidate> = Vec::new();
    for c in candidates {
        match c.response_id {
            None => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: None,
                        reason_code: USAGE_RESPONSE_ID_MISSING,
                    });
            }
            Some(_) => {
                with_id.push(c);
            }
        }
    }

    // 2. Group by normalized response_id
    let mut groups: BTreeMap<String, Vec<RawAntigravityUsageCandidate>> = BTreeMap::new();
    for c in with_id {
        let resp_id = c.response_id.clone().unwrap();
        groups.entry(resp_id).or_default().push(c);
    }

    // 3. Process groups
    for (resp_id, group) in groups {
        if group.len() > 1 {
            // [INV-JOIN-01] Group size > 1 -> USAGE_RESPONSE_ID_CONFLICT for all
            for c in group {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id.clone()),
                        reason_code: USAGE_RESPONSE_ID_CONFLICT,
                    });
            }
            continue;
        }

        let c = group.into_iter().next().unwrap();

        // 4. Resolve model
        let model = match resolve_model(&c.model_display_name, &c.response_model) {
            Ok(m) => m,
            Err(reason) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: reason,
                    });
                continue;
            }
        };

        // 5. Step matching
        let matching_steps = step_index.get(&resp_id);
        let step_entry = match matching_steps {
            None => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_STEP_NOT_FOUND,
                    });
                continue;
            }
            Some(steps) if steps.len() > 1 => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_STEP_NOT_UNIQUE,
                    });
                continue;
            }
            Some(steps) => &steps[0],
        };

        // 6. Decode matched step metadata
        let metadata_bytes = match &step_entry.metadata_bytes {
            Some(b) => b,
            None => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_STEP_METADATA_INVALID,
                    });
                continue;
            }
        };

        let decoded_meta = match parse_step_metadata(metadata_bytes) {
            Ok(dm) => dm,
            Err(ProtoError::InvalidTimestamp) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TIMESTAMP_INVALID,
                    });
                continue;
            }
            Err(_) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_STEP_METADATA_INVALID,
                    });
                continue;
            }
        };

        // 7. Check source == 2
        if decoded_meta.source != 2 {
            outcome
                .quarantine_records
                .push(AntigravityQuarantineRecord {
                    conversation_id: conversation_id.to_string(),
                    payload_digest: c.payload_digest,
                    gen_idx: Some(c.gen_idx),
                    response_id: Some(resp_id),
                    reason_code: USAGE_STEP_KIND_MISMATCH,
                });
            continue;
        }

        // 8. Token conversion and validation [INV-TOKEN-02] / [INV-TOKEN-03]
        let uncached_raw = c.uncached_input_tokens.unwrap_or(0);
        let cached_raw = c.cached_tokens.unwrap_or(0);
        let output_raw = c.output_tokens.unwrap_or(0);
        let reasoning_raw = c.reasoning_tokens.unwrap_or(0);

        let uncached_input = match i64::try_from(uncached_raw) {
            Ok(v) => v,
            Err(_) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TOKEN_INVALID,
                    });
                continue;
            }
        };

        let cached_tokens = match i64::try_from(cached_raw) {
            Ok(v) => v,
            Err(_) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TOKEN_INVALID,
                    });
                continue;
            }
        };

        let output_tokens = match i64::try_from(output_raw) {
            Ok(v) => v,
            Err(_) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TOKEN_INVALID,
                    });
                continue;
            }
        };

        let reasoning_tokens = match i64::try_from(reasoning_raw) {
            Ok(v) => v,
            Err(_) => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TOKEN_INVALID,
                    });
                continue;
            }
        };

        let input_tokens = match uncached_input.checked_add(cached_tokens) {
            Some(v) => v,
            None => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TOKEN_INVALID,
                    });
                continue;
            }
        };

        let total_tokens = match input_tokens.checked_add(output_tokens) {
            Some(v) => v,
            None => {
                outcome
                    .quarantine_records
                    .push(AntigravityQuarantineRecord {
                        conversation_id: conversation_id.to_string(),
                        payload_digest: c.payload_digest,
                        gen_idx: Some(c.gen_idx),
                        response_id: Some(resp_id),
                        reason_code: USAGE_TOKEN_INVALID,
                    });
                continue;
            }
        };

        // Token invariant check:
        // cached_tokens <= input_tokens
        // reasoning_tokens <= output_tokens
        // total_tokens == input_tokens + output_tokens
        if cached_tokens > input_tokens
            || reasoning_tokens > output_tokens
            || total_tokens != input_tokens + output_tokens
            || input_tokens < 0
            || output_tokens < 0
            || cached_tokens < 0
            || reasoning_tokens < 0
        {
            outcome
                .quarantine_records
                .push(AntigravityQuarantineRecord {
                    conversation_id: conversation_id.to_string(),
                    payload_digest: c.payload_digest,
                    gen_idx: Some(c.gen_idx),
                    response_id: Some(resp_id),
                    reason_code: USAGE_TOKEN_INVALID,
                });
            continue;
        }

        outcome.valid_records.push(AntigravityUsageRecord {
            gen_idx: c.gen_idx,
            response_id: resp_id,
            model,
            occurred_at_ms: decoded_meta.occurred_at_ms,
            uncached_input_tokens: uncached_input,
            cached_tokens,
            output_tokens,
            reasoning_tokens,
            payload_digest: c.payload_digest,
        });
    }

    outcome
}

fn resolve_model(
    primary: &RawModelField,
    fallback: &RawModelField,
) -> Result<String, &'static str> {
    match primary {
        RawModelField::Valid(m) => Ok(m.clone()),
        RawModelField::Invalid => Err(USAGE_MODEL_INVALID),
        RawModelField::Missing | RawModelField::Empty => match fallback {
            RawModelField::Valid(m) => Ok(m.clone()),
            RawModelField::Invalid => Err(USAGE_MODEL_INVALID),
            RawModelField::Missing | RawModelField::Empty => Err(USAGE_MODEL_MISSING),
        },
    }
}

/// Normalized title resolution from summary or annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedTitle {
    Set(String),
    Clear,
    Keep,
}

/// Metadata read from `conversation_summaries.db` for a conversation.
#[derive(Clone, Debug)]
pub struct DiscoveredSummaryRow {
    pub title: Option<String>,
    pub workspace_uris: Option<String>,
    pub last_modified_time_ms: Option<i64>,
    pub(crate) title_invalid: bool,
    pub(crate) workspace_invalid: bool,
}

/// Construct `ResolvedThreadPatch` according to Sections 4.5 and 4.6 truth tables.
pub fn build_metadata_patch(
    conversation_id: &str,
    summary: Option<&DiscoveredSummaryRow>,
    annotation_title_result: AnnotationTitleResult,
    blob_workspace: Option<&str>,
    valid_usage_max_occurred_at_ms: Option<i64>,
) -> (ResolvedThreadPatch, MetadataQualityStatus) {
    let identity = SessionIdentity::namespaced(SourceId::ANTIGRAVITY, conversation_id).unwrap();

    let mut is_partial = false;
    let mut has_clear = false;

    // 1. Resolve Title
    let (title_patch, title_quality_partial) =
        resolve_patch_title(summary, &annotation_title_result);
    if title_quality_partial {
        is_partial = true;
    }
    if matches!(title_patch, Patch::Clear) {
        has_clear = true;
    }

    // 2. Resolve Workspace
    let (project_name, project_path, project_kind, ws_quality_partial) = match summary {
        Some(s) => {
            let workspace = if s.workspace_invalid {
                WorkspaceEvaluation::Unknown
            } else {
                evaluate_summary_workspace(s.workspace_uris.as_deref())
            };
            match workspace {
                WorkspaceEvaluation::Projectless => {
                    has_clear = true;
                    (
                        Patch::Clear,
                        Patch::Clear,
                        Patch::Set(ProjectKind::Projectless),
                        false,
                    )
                }
                WorkspaceEvaluation::Project {
                    project_name,
                    project_path,
                } => (
                    Patch::Set(project_name),
                    Patch::Set(project_path),
                    Patch::Set(ProjectKind::Project),
                    false,
                ),
                WorkspaceEvaluation::Unknown => {
                    has_clear = true;
                    (
                        Patch::Clear,
                        Patch::Clear,
                        Patch::Set(ProjectKind::Unknown),
                        true,
                    )
                }
                WorkspaceEvaluation::Keep => (Patch::Keep, Patch::Keep, Patch::Keep, true),
            }
        }
        None => {
            // DB fallback
            is_partial = true; // summary missing is always Partial
            match evaluate_blob_workspace(blob_workspace) {
                WorkspaceEvaluation::Projectless => {
                    has_clear = true;
                    (
                        Patch::Clear,
                        Patch::Clear,
                        Patch::Set(ProjectKind::Projectless),
                        false,
                    )
                }
                WorkspaceEvaluation::Project {
                    project_name,
                    project_path,
                } => (
                    Patch::Set(project_name),
                    Patch::Set(project_path),
                    Patch::Set(ProjectKind::Project),
                    false,
                ),
                WorkspaceEvaluation::Unknown | WorkspaceEvaluation::Keep => {
                    (Patch::Keep, Patch::Keep, Patch::Keep, true)
                }
            }
        }
    };

    if ws_quality_partial {
        is_partial = true;
    }

    // 3. Resolve updated_at_ms and resolved_at_ms
    let (updated_at_patch, resolved_at_ms) = match summary {
        Some(s) => match s.last_modified_time_ms {
            Some(time_ms) if time_ms >= 0 => (Patch::Set(time_ms), time_ms),
            _ => {
                is_partial = true; // last_modified_time invalid -> Partial
                let fallback_time = valid_usage_max_occurred_at_ms.unwrap_or(0);
                (Patch::Keep, fallback_time)
            }
        },
        None => {
            is_partial = true;
            let fallback_time = valid_usage_max_occurred_at_ms.unwrap_or(0);
            (Patch::Keep, fallback_time)
        }
    };

    let quality = if is_partial {
        MetadataQualityStatus::Partial
    } else {
        MetadataQualityStatus::Complete
    };

    let patch = ResolvedThreadPatch {
        thread_id: identity.thread_id.clone(),
        source: identity.source,
        native_session_id: identity.native_session_id,
        parent_thread_id: Patch::Keep,
        root_session_id: Patch::Set(identity.thread_id),
        agent_role: Patch::Set(AgentRole::Main),
        title: title_patch,
        project_name,
        project_path,
        project_kind,
        metadata_model: Patch::Keep,
        created_at_ms: Patch::Keep,
        updated_at_ms: updated_at_patch,
        archived: Patch::Keep,
        metadata_quality_status: quality,
        resolved_at_ms,
        full_resolution: has_clear,
    };

    (patch, quality)
}

fn resolve_patch_title(
    summary: Option<&DiscoveredSummaryRow>,
    annotation_res: &AnnotationTitleResult,
) -> (Patch<String>, bool) {
    match summary {
        Some(s) => {
            let normalized_summary_title = if s.title_invalid {
                Some(Err(()))
            } else {
                match &s.title {
                    Some(t) => {
                        let trimmed = t.trim();
                        if trimmed.is_empty() {
                            None
                        } else if trimmed.len() > MAX_TITLE_BYTES
                            || trimmed.chars().any(char::is_control)
                        {
                            Some(Err(())) // Invalid
                        } else {
                            Some(Ok(trimmed.to_string())) // Valid
                        }
                    }
                    None => None, // SQL NULL is a missing title, not corruption
                }
            };

            match normalized_summary_title {
                Some(Ok(title)) => (Patch::Set(title), false),
                Some(Err(())) => {
                    // Summary title invalid -> quality is Partial, try annotation fallback
                    match annotation_res {
                        AnnotationTitleResult::Valid(annot_title) => {
                            (Patch::Set(annot_title.clone()), true)
                        }
                        _ => (Patch::Clear, true),
                    }
                }
                None => {
                    // Summary title missing or trim empty
                    match annotation_res {
                        AnnotationTitleResult::Valid(annot_title) => {
                            (Patch::Set(annot_title.clone()), false)
                        }
                        AnnotationTitleResult::Empty | AnnotationTitleResult::NotFound => {
                            (Patch::Clear, false)
                        }
                        AnnotationTitleResult::Malformed => (Patch::Clear, true),
                        AnnotationTitleResult::SecurityEscape(_) => (Patch::Clear, true),
                    }
                }
            }
        }
        None => {
            // Summary row missing
            match annotation_res {
                AnnotationTitleResult::Valid(annot_title) => {
                    (Patch::Set(annot_title.clone()), true)
                }
                _ => (Patch::Keep, true),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_td_p1_model_fallback_01_truth_table() {
        // 1. Primary Valid -> Valid (no fallback)
        assert_eq!(
            resolve_model(
                &RawModelField::Valid("flash".into()),
                &RawModelField::Valid("pro".into())
            ),
            Ok("flash".into())
        );

        // 2. Primary Missing, fallback Valid -> Valid
        assert_eq!(
            resolve_model(&RawModelField::Missing, &RawModelField::Valid("pro".into())),
            Ok("pro".into())
        );

        // 3. Primary Empty, fallback Valid -> Valid
        assert_eq!(
            resolve_model(&RawModelField::Empty, &RawModelField::Valid("pro".into())),
            Ok("pro".into())
        );

        // 4. Primary Invalid, fallback Valid -> USAGE_MODEL_INVALID (never masked!)
        assert_eq!(
            resolve_model(&RawModelField::Invalid, &RawModelField::Valid("pro".into())),
            Err(USAGE_MODEL_INVALID)
        );

        // 5. Primary Missing, fallback Invalid -> USAGE_MODEL_INVALID
        assert_eq!(
            resolve_model(&RawModelField::Missing, &RawModelField::Invalid),
            Err(USAGE_MODEL_INVALID)
        );

        // 6. Both Missing -> USAGE_MODEL_MISSING
        assert_eq!(
            resolve_model(&RawModelField::Missing, &RawModelField::Missing),
            Err(USAGE_MODEL_MISSING)
        );
    }

    #[test]
    fn test_td_p3_gen_dup_01_duplicate_response_id_precedence() {
        let cid = "effa6389-921a-497e-87e0-5a2962526c07";
        let c1 = RawAntigravityUsageCandidate {
            gen_idx: 0,
            payload_digest: "digest1".into(),
            response_id: Some("resp-dup".into()),
            model_display_name: RawModelField::Invalid, // Model is invalid!
            response_model: RawModelField::Missing,
            uncached_input_tokens: Some(100),
            cached_tokens: Some(0),
            output_tokens: Some(50),
            reasoning_tokens: Some(0),
        };
        let c2 = RawAntigravityUsageCandidate {
            gen_idx: 1,
            payload_digest: "digest2".into(),
            response_id: Some("resp-dup".into()),
            model_display_name: RawModelField::Valid("gemini-3.8-flash".into()),
            response_model: RawModelField::Missing,
            uncached_input_tokens: Some(200),
            cached_tokens: Some(0),
            output_tokens: Some(60),
            reasoning_tokens: Some(0),
        };

        let step_index = BTreeMap::new();
        let outcome = normalize_candidates(cid, vec![c1, c2], Vec::new(), &step_index);
        assert_eq!(outcome.valid_records.len(), 0);
        assert_eq!(outcome.quarantine_records.len(), 2);
        // Both MUST receive USAGE_RESPONSE_ID_CONFLICT, NOT USAGE_MODEL_INVALID!
        for q in &outcome.quarantine_records {
            assert_eq!(q.reason_code, USAGE_RESPONSE_ID_CONFLICT);
        }
    }

    #[test]
    fn test_td_p3_meta_time_01_summary_malformed_time() {
        let cid = "effa6389-921a-497e-87e0-5a2962526c07";
        let summary = DiscoveredSummaryRow {
            title: Some("Valid Title".into()),
            workspace_uris: Some("[]".into()),
            last_modified_time_ms: None, // Malformed time!
            title_invalid: false,
            workspace_invalid: false,
        };

        let (patch, quality) = build_metadata_patch(
            cid,
            Some(&summary),
            AnnotationTitleResult::NotFound,
            None,
            Some(123456789), // valid Usage max
        );

        assert_eq!(patch.title, Patch::Set("Valid Title".into()));
        assert_eq!(patch.project_kind, Patch::Set(ProjectKind::Projectless));
        assert_eq!(patch.updated_at_ms, Patch::Keep); // updated_at_ms is Keep!
        assert_eq!(patch.resolved_at_ms, 123456789); // fallback to usage max!
        assert_eq!(quality, MetadataQualityStatus::Partial);
    }

    #[test]
    fn test_td_p1_num_01_invalid_timestamp_has_timestamp_reason() {
        let candidate = RawAntigravityUsageCandidate {
            gen_idx: 0,
            payload_digest: "digest".into(),
            response_id: Some("response".into()),
            model_display_name: RawModelField::Valid("model".into()),
            response_model: RawModelField::Missing,
            uncached_input_tokens: Some(1),
            cached_tokens: Some(0),
            output_tokens: Some(1),
            reasoning_tokens: Some(0),
        };
        let mut timestamp = Vec::new();
        timestamp.extend([0x0a, 0x02, 0x08, 0x01]); // timestamp has no nanos issue
        timestamp.extend([0x18, 0x02]); // source = 2
        // Replace timestamp payload with an invalid negative/overflow value.
        timestamp.splice(
            0..4,
            [
                0x0a, 0x0b, 0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
            ],
        );
        let mut steps = BTreeMap::new();
        steps.insert(
            "response".into(),
            vec![StepIndexEntry {
                step_idx: 1,
                metadata_bytes: Some(timestamp),
            }],
        );
        let outcome = normalize_candidates("conversation", vec![candidate], Vec::new(), &steps);
        assert_eq!(outcome.valid_records.len(), 0);
        assert_eq!(outcome.quarantine_records.len(), 1);
        assert_eq!(
            outcome.quarantine_records[0].reason_code,
            USAGE_TIMESTAMP_INVALID
        );
    }
}
