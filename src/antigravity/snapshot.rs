//! In-memory snapshot taken from external Antigravity databases before transaction.
//!
//! Per `[INV-WAL-03]`, `[INV-EXT-04]`, `[INV-TXN-02]`, all external SQLite reads,
//! schema probes, protobuf decodings, normalizations, and annotation reads happen
//! here, prior to opening any Usagi write transaction.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::antigravity::annotation::read_annotation_title;
use crate::antigravity::config::AntigravityConfig;
use crate::antigravity::discovery::discover_inventory;
use crate::antigravity::normalization::{
    AntigravityQuarantineRecord, AntigravityUsageRecord, build_metadata_patch, normalize_candidates,
};
use crate::antigravity::reader::{open_external_db, read_conversation_snapshot};
use crate::domain::{ResolvedThreadPatch, SessionIdentity};
use crate::source::SourceId;
use crate::source::adapter::{CanonicalUsageEventWrite, SourceAdapterError};

/// In-memory snapshot data for a single discovered conversation.
#[derive(Clone, Debug)]
pub struct ConversationSnapshot {
    pub conversation_id: String,
    pub identity: SessionIdentity,
    pub metadata_patch: Option<ResolvedThreadPatch>,
    pub valid_records: Vec<(AntigravityUsageRecord, CanonicalUsageEventWrite)>,
    pub source_quarantines: Vec<AntigravityQuarantineRecord>,
    pub observed_gen_max_idx: i64,
    pub observed_step_max_idx: i64,
}

/// Full source snapshot for an Antigravity scan run.
#[derive(Clone, Debug)]
pub struct AntigravitySourceSnapshot {
    pub conversations: Vec<ConversationSnapshot>,
}

/// Take an in-memory snapshot of all discovered Antigravity conversations.
///
/// Returns source-level errors before opening any Usagi write transaction.
pub fn take_source_snapshot(
    config: &AntigravityConfig,
    cancellation: &AtomicBool,
) -> Result<AntigravitySourceSnapshot, SourceAdapterError> {
    if cancellation.load(Ordering::Acquire) {
        return Err(SourceAdapterError::with_code(
            "OPERATION_CANCELLED",
            "Antigravity scan was cancelled before discovery",
        ));
    }

    let inventory = discover_inventory(config, cancellation)
        .map_err(|e| SourceAdapterError::with_code(e.code(), e.to_string()))?;

    let mut conversations = Vec::with_capacity(inventory.conversations.len());

    for discovered in inventory.conversations {
        if cancellation.load(Ordering::Acquire) {
            return Err(SourceAdapterError::with_code(
                "OPERATION_CANCELLED",
                "Antigravity scan was cancelled during conversation processing",
            ));
        }

        let conn = open_external_db(&discovered.db_path)
            .map_err(|e| SourceAdapterError::with_code(e.code(), e.to_string()))?;

        let data = read_conversation_snapshot(&conn, &discovered.conversation_id, cancellation)
            .map_err(|e| SourceAdapterError::with_code(e.code(), e.to_string()))?;

        let norm = normalize_candidates(
            &discovered.conversation_id,
            data.candidates,
            data.initial_quarantines,
            &data.step_index,
        );

        let max_time = norm.valid_records.iter().map(|r| r.occurred_at_ms).max();

        let annotation_res = read_annotation_title(config, &discovered.conversation_id);
        if let crate::antigravity::annotation::AnnotationTitleResult::SecurityEscape(ref err) =
            annotation_res
        {
            return Err(SourceAdapterError::with_code(
                crate::antigravity::ANTIGRAVITY_CONFIG_INVALID,
                err.clone(),
            ));
        }

        let (patch, _quality) = build_metadata_patch(
            &discovered.conversation_id,
            discovered.summary_row.as_ref(),
            annotation_res,
            data.trajectory_workspace.as_deref(),
            max_time,
        );

        let mut valid_records = Vec::with_capacity(norm.valid_records.len());
        for record in norm.valid_records {
            let event = record
                .to_canonical_event(&discovered.conversation_id)
                .map_err(|e| SourceAdapterError::with_code("USAGE_CANONICAL_CONVERT_FAILED", e))?;
            valid_records.push((record, event));
        }

        let identity =
            SessionIdentity::namespaced(SourceId::ANTIGRAVITY, &discovered.conversation_id)
                .map_err(|e| {
                    SourceAdapterError::with_code("ANTIGRAVITY_IDENTITY_INVALID", e.to_string())
                })?;

        conversations.push(ConversationSnapshot {
            conversation_id: discovered.conversation_id,
            identity,
            metadata_patch: Some(patch),
            valid_records,
            source_quarantines: norm.quarantine_records,
            observed_gen_max_idx: data.observed_gen_max_idx,
            observed_step_max_idx: data.observed_step_max_idx,
        });

        if cancellation.load(Ordering::Acquire) {
            return Err(SourceAdapterError::with_code(
                "OPERATION_CANCELLED",
                "Antigravity scan was cancelled after conversation processing",
            ));
        }
    }

    Ok(AntigravitySourceSnapshot { conversations })
}
