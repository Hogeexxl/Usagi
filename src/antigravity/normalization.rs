//! Candidate pipeline normalization and Session metadata patch construction.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::antigravity::project::{WorkspaceEvaluation, evaluate_summary_workspace};
use crate::antigravity::protobuf::{
    MAX_TITLE_BYTES, ProtoError, RawAntigravityUsageCandidate, parse_step_metadata,
};
use crate::cost::{
    BundledPricingRepository, CostEstimateOutcome, CostEstimator, UsageCostGranularity,
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
    pub reasoning_effort: Option<String>,
    pub root_session_id: String,
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
        let outcome = crate::cost::estimate_for_source(
            &BundledPricingRepository::new(),
            &CostEstimator::new(),
            &SourceId::ANTIGRAVITY,
            &self.model,
            self.occurred_at_ms,
            UsageCostGranularity::RequestScoped,
            &usage,
        )
        .map_err(|_| "usage cost estimation failed".to_string())?;
        let estimated_cost_nanos_usd = match outcome {
            CostEstimateOutcome::Known(cost) => Some(cost.total_nanos_usd),
            CostEstimateOutcome::Unknown(_) => None,
        };

        Ok(CanonicalUsageEventWrite {
            event_id,
            kind: EventKind::Normal,
            occurred_at_ms: self.occurred_at_ms,
            thread_id: identity.thread_id.clone(),
            root_session_id: self.root_session_id.clone(),
            turn_key: None,
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            estimated_cost_nanos_usd,
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
    executor_models: &HashMap<Vec<u8>, String>,
    root_session_id: &str,
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
        let Some(selected_model) = c
            .execution_id
            .as_ref()
            .and_then(|id| executor_models.get(id))
        else {
            outcome
                .quarantine_records
                .push(AntigravityQuarantineRecord {
                    conversation_id: conversation_id.to_string(),
                    payload_digest: c.payload_digest,
                    gen_idx: Some(c.gen_idx),
                    response_id: Some(resp_id),
                    reason_code: USAGE_MODEL_MISSING,
                });
            continue;
        };
        let (model, reasoning_effort) = split_selected_model(selected_model);

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
            reasoning_effort,
            root_session_id: root_session_id.to_owned(),
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

fn split_selected_model(selected: &str) -> (String, Option<String>) {
    if let Some((model, effort)) = selected.rsplit_once('-')
        && matches!(effort, "low" | "medium" | "high" | "tiered")
    {
        (model.to_owned(), Some(effort.to_owned()))
    } else {
        (selected.to_owned(), None)
    }
}

/// Metadata read from `conversation_summaries.db` for a conversation.
#[derive(Clone, Debug)]
pub struct DiscoveredSummaryRow {
    pub title: Option<String>,
    pub workspace_uris: Option<String>,
    pub last_modified_time_ms: Option<i64>,
    pub(crate) title_invalid: bool,
    pub(crate) workspace_invalid: bool,
    pub parent_conversation_id: Option<String>,
    pub nesting_depth: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AntigravityHierarchy {
    pub parent_thread_id: Option<String>,
    pub root_session_id: String,
    pub agent_role: AgentRole,
}

/// Resolve a session's hierarchy from authoritative summary rows.
pub fn resolve_hierarchy(
    conversation_id: &str,
    summary: &DiscoveredSummaryRow,
    summaries: &HashMap<String, DiscoveredSummaryRow>,
) -> Result<AntigravityHierarchy, String> {
    if summary.nesting_depth < 0 {
        return Err(format!("negative nesting_depth for {conversation_id}"));
    }

    let parent_thread_id = summary
        .parent_conversation_id
        .as_deref()
        .map(antigravity_thread_id)
        .transpose()?;
    let mut current_id = conversation_id.to_owned();
    let mut current_summary = summary;
    let mut visited = BTreeSet::new();

    let root_id = loop {
        if !visited.insert(current_id.clone()) {
            return Err(format!("parent conversation cycle includes {current_id}"));
        }

        match current_summary.nesting_depth {
            0 => {
                if let Some(parent_id) = current_summary.parent_conversation_id.as_deref() {
                    if visited.contains(parent_id) {
                        return Err(format!("parent conversation cycle includes {parent_id}"));
                    }
                    return Err(format!(
                        "root conversation {current_id} has parent {parent_id}"
                    ));
                }
                break current_id;
            }
            depth => {
                let parent_id = current_summary
                    .parent_conversation_id
                    .as_deref()
                    .ok_or_else(|| format!("nested conversation {current_id} has no parent"))?;
                if visited.contains(parent_id) {
                    return Err(format!("parent conversation cycle includes {parent_id}"));
                }
                let parent_summary = summaries.get(parent_id).ok_or_else(|| {
                    format!("missing parent summary {parent_id} for {current_id}")
                })?;
                if parent_summary.nesting_depth != depth - 1 {
                    return Err(format!(
                        "nesting_depth mismatch between {current_id} and parent {parent_id}"
                    ));
                }
                current_id = parent_id.to_owned();
                current_summary = parent_summary;
            }
        }
    };

    Ok(AntigravityHierarchy {
        parent_thread_id,
        root_session_id: antigravity_thread_id(&root_id)?,
        agent_role: if summary.parent_conversation_id.is_some() {
            AgentRole::Subagent
        } else {
            AgentRole::Main
        },
    })
}

fn antigravity_thread_id(conversation_id: &str) -> Result<String, String> {
    SessionIdentity::namespaced(SourceId::ANTIGRAVITY, conversation_id)
        .map(|identity| identity.thread_id)
        .map_err(|error| format!("invalid conversation ID {conversation_id}: {error}"))
}

/// Construct `ResolvedThreadPatch` according to Sections 4.5 and 4.6 truth tables.
pub fn build_metadata_patch(
    conversation_id: &str,
    summary: &DiscoveredSummaryRow,
    hierarchy: &AntigravityHierarchy,
    valid_usage_max_occurred_at_ms: Option<i64>,
) -> (ResolvedThreadPatch, MetadataQualityStatus) {
    let identity = SessionIdentity::namespaced(SourceId::ANTIGRAVITY, conversation_id).unwrap();

    let mut is_partial = false;
    // 1. Resolve Title
    let (title_patch, title_quality_partial) = resolve_patch_title(summary);
    if title_quality_partial {
        is_partial = true;
    }
    // 2. Resolve Workspace
    let workspace = if summary.workspace_invalid {
        WorkspaceEvaluation::Unknown
    } else {
        evaluate_summary_workspace(summary.workspace_uris.as_deref())
    };
    let (project_name, project_path, project_kind, ws_quality_partial) = match workspace {
        WorkspaceEvaluation::Projectless => (
            Patch::Clear,
            Patch::Clear,
            Patch::Set(ProjectKind::Projectless),
            false,
        ),
        WorkspaceEvaluation::Project {
            project_name,
            project_path,
        } => (
            Patch::Set(project_name),
            Patch::Set(project_path),
            Patch::Set(ProjectKind::Project),
            false,
        ),
        WorkspaceEvaluation::Unknown => (
            Patch::Clear,
            Patch::Clear,
            Patch::Set(ProjectKind::Unknown),
            true,
        ),
        WorkspaceEvaluation::Keep => (Patch::Keep, Patch::Keep, Patch::Keep, true),
    };

    if ws_quality_partial {
        is_partial = true;
    }

    // 3. Resolve updated_at_ms and resolved_at_ms
    let (updated_at_patch, resolved_at_ms) = match summary.last_modified_time_ms {
        Some(time_ms) if time_ms >= 0 => (Patch::Set(time_ms), time_ms),
        _ => {
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
        parent_thread_id: match &hierarchy.parent_thread_id {
            Some(parent) => Patch::Set(parent.clone()),
            None => Patch::Clear,
        },
        root_session_id: Patch::Set(hierarchy.root_session_id.clone()),
        agent_role: Patch::Set(hierarchy.agent_role),
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
        full_resolution: true,
    };

    (patch, quality)
}

fn resolve_patch_title(summary: &DiscoveredSummaryRow) -> (Patch<String>, bool) {
    let normalized_summary_title = if summary.title_invalid {
        Some(Err(()))
    } else {
        match &summary.title {
            Some(t) => {
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else if trimmed.len() > MAX_TITLE_BYTES || trimmed.chars().any(char::is_control) {
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
        Some(Err(())) => (Patch::Clear, true),
        None => (Patch::Clear, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(parent: Option<&str>, depth: i64) -> DiscoveredSummaryRow {
        DiscoveredSummaryRow {
            title: Some("Title".into()),
            workspace_uris: Some("[]".into()),
            last_modified_time_ms: Some(123),
            title_invalid: false,
            workspace_invalid: false,
            parent_conversation_id: parent.map(str::to_owned),
            nesting_depth: depth,
        }
    }

    fn candidate(
        gen_idx: i64,
        execution_id: Vec<u8>,
        response_id: &str,
    ) -> RawAntigravityUsageCandidate {
        RawAntigravityUsageCandidate {
            gen_idx,
            payload_digest: format!("digest-{gen_idx}"),
            response_id: Some(response_id.to_owned()),
            execution_id: Some(execution_id),
            uncached_input_tokens: Some(7),
            cached_tokens: Some(3),
            output_tokens: Some(11),
            reasoning_tokens: Some(4),
        }
    }

    #[test]
    fn selected_model_slug_splits_only_known_effort_suffixes() {
        for (slug, expected_model, expected_effort) in [
            ("gemini-3.8-flash-high", "gemini-3.8-flash", Some("high")),
            (
                "gemini-3.8-flash-medium",
                "gemini-3.8-flash",
                Some("medium"),
            ),
            ("gemini-3.8-flash-low", "gemini-3.8-flash", Some("low")),
            (
                "gemini-3.8-flash-tiered",
                "gemini-3.8-flash",
                Some("tiered"),
            ),
            ("gemini-3.8-flash", "gemini-3.8-flash", None),
        ] {
            assert_eq!(
                split_selected_model(slug),
                (
                    expected_model.to_owned(),
                    expected_effort.map(str::to_owned)
                )
            );
        }
    }

    #[test]
    fn duplicate_response_ids_are_quarantined_before_model_lookup() {
        let candidates = vec![
            candidate(0, vec![1], "resp-dup"),
            candidate(1, vec![2], "resp-dup"),
        ];
        let outcome = normalize_candidates(
            "conversation",
            candidates,
            Vec::new(),
            &BTreeMap::new(),
            &HashMap::new(),
            "antigravity:root",
        );

        assert!(outcome.valid_records.is_empty());
        assert_eq!(outcome.quarantine_records.len(), 2);
        assert!(
            outcome
                .quarantine_records
                .iter()
                .all(|record| record.reason_code == USAGE_RESPONSE_ID_CONFLICT)
        );
    }

    #[test]
    fn executor_model_is_matched_by_execution_id_and_usage_is_preserved() {
        let mut steps = BTreeMap::new();
        steps.insert(
            "response".into(),
            vec![StepIndexEntry {
                step_idx: 1,
                metadata_bytes: Some(vec![0x0a, 0x02, 0x08, 0x01, 0x18, 0x02]),
            }],
        );
        let mut executor_models = HashMap::new();
        executor_models.insert(vec![1], "gemini-3.8-flash-low".into());
        executor_models.insert(vec![2], "gemini-3.8-pro-high".into());

        let outcome = normalize_candidates(
            "conversation",
            vec![candidate(0, vec![2], "response")],
            Vec::new(),
            &steps,
            &executor_models,
            "antigravity:root",
        );

        assert!(outcome.quarantine_records.is_empty());
        let record = &outcome.valid_records[0];
        assert_eq!(record.model, "gemini-3.8-pro");
        assert_eq!(record.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(record.root_session_id, "antigravity:root");
        assert_eq!(record.uncached_input_tokens, 7);
        assert_eq!(record.cached_tokens, 3);
        assert_eq!(record.output_tokens, 11);
        assert_eq!(record.reasoning_tokens, 4);

        let event = record.to_canonical_event("conversation").unwrap();
        assert_eq!(event.model, "gemini-3.8-pro");
        assert_eq!(event.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(event.usage.input_tokens, 10);
        assert_eq!(event.usage.cached_tokens, 3);
        assert_eq!(event.usage.output_tokens, 11);
        assert_eq!(event.usage.reasoning_tokens, 4);
        assert_eq!(event.usage.total_tokens, 21);
        assert_eq!(event.estimated_cost_nanos_usd, None);

        let flash_record = AntigravityUsageRecord {
            model: "gemini-3.8-flash".into(),
            ..record.clone()
        };
        let flash_event = flash_record.to_canonical_event("conversation").unwrap();
        assert_eq!(flash_event.estimated_cost_nanos_usd, Some(46_725));

        let unverified_record = AntigravityUsageRecord {
            model: "gpt-5.6-sol".into(),
            ..record.clone()
        };
        let unverified_event = unverified_record
            .to_canonical_event("conversation")
            .unwrap();
        assert_eq!(unverified_event.estimated_cost_nanos_usd, None);
    }

    #[test]
    fn hierarchy_resolves_nested_sessions_to_the_topmost_root() {
        let root_id = "root-id";
        let parent_id = "parent-id";
        let child_id = "child-id";
        let root = summary(None, 0);
        let parent = summary(Some(root_id), 1);
        let child = summary(Some(parent_id), 2);
        let summaries = HashMap::from([
            (root_id.into(), root.clone()),
            (parent_id.into(), parent),
            (child_id.into(), child.clone()),
        ]);

        let hierarchy = resolve_hierarchy(child_id, &child, &summaries).unwrap();
        assert_eq!(
            hierarchy.parent_thread_id.as_deref(),
            Some("antigravity:parent-id")
        );
        assert_eq!(hierarchy.root_session_id, "antigravity:root-id");
        assert_eq!(hierarchy.agent_role, AgentRole::Subagent);

        let root_hierarchy = resolve_hierarchy(root_id, &root, &summaries).unwrap();
        assert_eq!(root_hierarchy.parent_thread_id, None);
        assert_eq!(root_hierarchy.root_session_id, "antigravity:root-id");
        assert_eq!(root_hierarchy.agent_role, AgentRole::Main);

        let (patch, _) = build_metadata_patch(child_id, &child, &hierarchy, Some(123));
        assert_eq!(
            patch.parent_thread_id,
            Patch::Set("antigravity:parent-id".into())
        );
        assert_eq!(
            patch.root_session_id,
            Patch::Set("antigravity:root-id".into())
        );
        assert_eq!(patch.agent_role, Patch::Set(AgentRole::Subagent));
    }

    #[test]
    fn hierarchy_rejects_missing_ancestors_depth_mismatch_and_cycles() {
        let child = summary(Some("missing-root"), 1);
        assert!(
            resolve_hierarchy("child", &child, &HashMap::new())
                .unwrap_err()
                .contains("missing parent summary")
        );

        let wrong_depth_parent = summary(None, 0);
        let child = summary(Some("parent"), 2);
        let summaries = HashMap::from([("parent".into(), wrong_depth_parent)]);
        assert!(
            resolve_hierarchy("child", &child, &summaries)
                .unwrap_err()
                .contains("nesting_depth mismatch")
        );

        let a = summary(Some("b"), 2);
        let b = summary(Some("a"), 1);
        let summaries = HashMap::from([("a".into(), a.clone()), ("b".into(), b)]);
        assert!(
            resolve_hierarchy("a", &a, &summaries)
                .unwrap_err()
                .contains("cycle")
        );

        let invalid_root = summary(Some("parent"), 0);
        assert!(
            resolve_hierarchy("invalid-root", &invalid_root, &HashMap::new())
                .unwrap_err()
                .contains("root conversation")
        );
    }
}
