//! Atomic persistence seam for usage ingestion batches.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};

use crate::codex::domain::CheckpointProcessingStatus;
use crate::codex::usage::{CodexOperation, EvidenceKind};
use crate::domain::SourceUsageEpochState;
use crate::source::{CanonicalUsageEventWrite, SourceStorageError, UsageWriteTarget};
use crate::usage::event::EventKind;
use crate::usage::normalized::NormalizedTokenUsage;

use super::{CodexStorage, CodexStorageError, CodexWriteTxn};
use crate::codex::normalization::canonical_algorithm_for;
use crate::storage::{Result as StorageResult, StorageError};

pub(crate) const MAX_USAGE_BATCH_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_USAGE_BATCH_LINES: u64 = 4096;
pub(crate) const MAX_USAGE_BATCH_WRITE_UNITS: u64 = 2048;
const MAX_LEGAL_LINE_BYTES: u64 = 8 * 1024 * 1024;

type SnapshotColumns = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<Vec<u8>>,
);
type ExistingAnomaly = (
    Option<i64>,
    String,
    i64,
    i64,
    Option<i64>,
    String,
    String,
    String,
);

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsageEventHoldReference {
    source_file_id: i64,
    file_generation: i64,
    event_id: String,
    hold_reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageTailStatus {
    Unverified,
    None,
    HalfLine,
}

impl UsageTailStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::None => "none",
            Self::HalfLine => "half_line",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "unverified" => Ok(Self::Unverified),
            "none" => Ok(Self::None),
            "half_line" => Ok(Self::HalfLine),
            _ => Err(StorageError::invalid_state("invalid usage raw-tail status")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageChainState {
    Continuous,
    Interrupted(UsageGapReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageContinuationState {
    ReplayedAncestor,
    OwningLive,
}

impl UsageContinuationState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReplayedAncestor => "replayed_ancestor",
            Self::OwningLive => "owning_live",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "replayed_ancestor" => Ok(Self::ReplayedAncestor),
            "owning_live" => Ok(Self::OwningLive),
            _ => Err(StorageError::invalid_state(
                "invalid usage continuation state",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageGapReason {
    Malformed,
    Oversized,
    TotalInvalid,
    OwnershipGap,
    ParserGap,
}

impl UsageGapReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Oversized => "oversized",
            Self::TotalInvalid => "total_invalid",
            Self::OwnershipGap => "ownership_gap",
            Self::ParserGap => "parser_gap",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "malformed" => Ok(Self::Malformed),
            "oversized" => Ok(Self::Oversized),
            "total_invalid" => Ok(Self::TotalInvalid),
            "ownership_gap" => Ok(Self::OwnershipGap),
            "parser_gap" => Ok(Self::ParserGap),
            _ => Err(StorageError::invalid_state(
                "invalid usage chain block reason",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSnapshot {
    pub vector: NormalizedTokenUsage,
    pub fingerprint: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSourceStateWrite {
    pub file_generation: i64,
    pub device_id: i64,
    pub inode: i64,
    pub usage_parser_version: i64,
    pub canonical_algorithm_version: i64,
    pub resolved_through_offset: i64,
    pub observed_raw_size: i64,
    pub raw_tail_status: UsageTailStatus,
    pub raw_tail_start_offset: Option<i64>,
    pub owning_thread_id: String,
    pub root_session_id: String,
    pub continuation_state: UsageContinuationState,
    pub previous_total: Option<UsageSnapshot>,
    pub previous_total_offset: Option<i64>,
    pub chain_state: UsageChainState,
    pub active_turn_key: Option<String>,
    pub active_model: Option<String>,
    pub active_model_offset: Option<i64>,
    pub active_reasoning_effort: Option<String>,
    pub active_reasoning_effort_offset: Option<i64>,
    pub reconciliation_state_json: String,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageEventWrite {
    pub event_id: String,
    pub kind: EventKind,
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub turn_key: Option<String>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub estimated_cost_nanos_usd: Option<i64>,
    pub usage: NormalizedTokenUsage,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageOccurrenceWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: i64,
    pub source_end_offset: i64,
    pub event_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillUsageEventWrite {
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub model: Option<String>,
    pub skill_name: String,
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: i64,
    pub source_end_offset: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageTurnStatus {
    Open,
    Completed,
    Aborted,
    Failed,
}

impl UsageTurnStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UsageTurnModelState {
    None,
    Single(String),
    Mixed,
}

impl UsageTurnModelState {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single(_) => "single",
            Self::Mixed => "mixed",
        }
    }

    fn single_model(&self) -> Option<&str> {
        match self {
            Self::Single(model) => Some(model),
            Self::None | Self::Mixed => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UsageTurnReasoningEffortState {
    None,
    Single(String),
    Mixed,
}

impl UsageTurnReasoningEffortState {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single(_) => "single",
            Self::Mixed => "mixed",
        }
    }

    fn single_effort(&self) -> Option<&str> {
        match self {
            Self::Single(effort) => Some(effort),
            Self::None | Self::Mixed => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UsageCompensationBlocks {
    pub start_missing: bool,
    pub time_missing: bool,
    pub reset: bool,
    pub ownership_gap: bool,
    pub parser_gap: bool,
    pub required_invalid: bool,
    pub model_unresolved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageTurnWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub thread_id: String,
    pub turn_key: String,
    pub raw_turn_id: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub start_offset: i64,
    pub end_offset: Option<i64>,
    pub status: UsageTurnStatus,
    pub start_total: Option<UsageSnapshot>,
    pub last_total: Option<UsageSnapshot>,
    pub accounted: UsageSnapshot,
    pub accounted_candidate_count: i64,
    pub model_state: UsageTurnModelState,
    pub reasoning_effort_state: UsageTurnReasoningEffortState,
    pub unresolved_reasoning_effort_seen: bool,
    pub unresolved_model_seen: bool,
    pub blocks: UsageCompensationBlocks,
    pub quality_status: &'static str,
    pub state_through_offset: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageAnomalyKind {
    UsageTimeMissing,
    RequiredTotalInvalid,
    LastUsageInvalid,
    TotalChainReset,
    CacheWriteChainDecrease,
    TurnAccountedExceedsTotal,
    TurnCacheWriteDeltaNegative,
    TurnIdMismatch,
    TurnReplaced,
    ArithmeticOverflow,
    ReconciliationPatchTooLarge,
    ResponseUsageConflict,
    ResponseOwnershipMismatch,
    CompactionIdentityMismatch,
    LegacyCoverageAmbiguous,
    ThreadUsageMismatch,
}

impl UsageAnomalyKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UsageTimeMissing => "USAGE_TIME_MISSING",
            Self::RequiredTotalInvalid => "REQUIRED_TOTAL_INVALID",
            Self::LastUsageInvalid => "LAST_USAGE_INVALID",
            Self::TotalChainReset => "TOTAL_CHAIN_RESET",
            Self::CacheWriteChainDecrease => "CACHE_WRITE_CHAIN_DECREASE",
            Self::TurnAccountedExceedsTotal => "TURN_ACCOUNTED_EXCEEDS_TOTAL",
            Self::TurnCacheWriteDeltaNegative => "TURN_CACHE_WRITE_DELTA_NEGATIVE",
            Self::TurnIdMismatch => "TURN_ID_MISMATCH",
            Self::TurnReplaced => "TURN_REPLACED",
            Self::ArithmeticOverflow => "TOKEN_ARITHMETIC_OVERFLOW",
            Self::ReconciliationPatchTooLarge => "RECONCILIATION_PATCH_TOO_LARGE",
            Self::ResponseUsageConflict => "RESPONSE_USAGE_CONFLICT",
            Self::ResponseOwnershipMismatch => "RESPONSE_OWNERSHIP_MISMATCH",
            Self::CompactionIdentityMismatch => "COMPACTION_IDENTITY_MISMATCH",
            Self::LegacyCoverageAmbiguous => "LEGACY_COVERAGE_AMBIGUOUS",
            Self::ThreadUsageMismatch => "THREAD_USAGE_MISMATCH",
        }
    }

    fn parse(value: &str) -> StorageResult<Self> {
        match value {
            "USAGE_TIME_MISSING" => Ok(Self::UsageTimeMissing),
            "REQUIRED_TOTAL_INVALID" => Ok(Self::RequiredTotalInvalid),
            "LAST_USAGE_INVALID" => Ok(Self::LastUsageInvalid),
            "TOTAL_CHAIN_RESET" => Ok(Self::TotalChainReset),
            "CACHE_WRITE_CHAIN_DECREASE" => Ok(Self::CacheWriteChainDecrease),
            "TURN_ACCOUNTED_EXCEEDS_TOTAL" => Ok(Self::TurnAccountedExceedsTotal),
            "TURN_CACHE_WRITE_DELTA_NEGATIVE" => Ok(Self::TurnCacheWriteDeltaNegative),
            "TURN_ID_MISMATCH" => Ok(Self::TurnIdMismatch),
            "TURN_REPLACED" => Ok(Self::TurnReplaced),
            "TOKEN_ARITHMETIC_OVERFLOW" => Ok(Self::ArithmeticOverflow),
            "RECONCILIATION_PATCH_TOO_LARGE" => Ok(Self::ReconciliationPatchTooLarge),
            "RESPONSE_USAGE_CONFLICT" => Ok(Self::ResponseUsageConflict),
            "RESPONSE_OWNERSHIP_MISMATCH" => Ok(Self::ResponseOwnershipMismatch),
            "COMPACTION_IDENTITY_MISMATCH" => Ok(Self::CompactionIdentityMismatch),
            "LEGACY_COVERAGE_AMBIGUOUS" => Ok(Self::LegacyCoverageAmbiguous),
            "THREAD_USAGE_MISMATCH" => Ok(Self::ThreadUsageMismatch),
            _ => Err(StorageError::invalid_state("invalid usage anomaly type")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageAnomalyWrite {
    pub anomaly_id: String,
    pub detected_at_ms: i64,
    pub occurred_at_ms: Option<i64>,
    pub kind: UsageAnomalyKind,
    pub severity_error: bool,
    pub source_start_offset: Option<i64>,
    pub turn_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageQuarantineDiagnostic {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub owning_thread_id: String,
    pub anomaly: UsageAnomalyWrite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageEventFactWrite {
    pub event_id: String,
    pub owning_thread_id: String,
    pub response_id: Option<String>,
    pub evidence_kind: EvidenceKind,
    pub operation: CodexOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageCompactionMarkerWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: i64,
    pub source_end_offset: i64,
    pub owning_thread_id: String,
    pub root_session_id: String,
    pub occurred_at_ms: Option<i64>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub response_id: Option<String>,
    pub resolved_event_id: Option<String>,
    pub unknown_reason: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageReconciliationWindowWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: i64,
    pub source_end_offset: i64,
    pub owning_thread_id: String,
    pub turn_key: Option<String>,
    pub state_json: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageEventHoldReason {
    Replay,
    Carry,
}

impl UsageEventHoldReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Carry => "carry",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageEventHoldWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub event_id: String,
    pub hold_reason: UsageEventHoldReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UsagePrivateRowKey {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageTurnRewriteWrite {
    pub expected: UsageTurnWrite,
    pub replacement: UsageTurnWrite,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconciliationPatchWrite {
    pub delete_event_ids: Vec<String>,
    pub delete_markers: Vec<UsagePrivateRowKey>,
    pub delete_windows: Vec<UsagePrivateRowKey>,
    pub delete_holds: Vec<(i64, i64, String)>,
    pub events: Vec<UsageEventWrite>,
    pub occurrences: Vec<UsageOccurrenceWrite>,
    pub facts: Vec<UsageEventFactWrite>,
    pub marker_upserts: Vec<UsageCompactionMarkerWrite>,
    pub window_upserts: Vec<UsageReconciliationWindowWrite>,
    pub hold_upserts: Vec<UsageEventHoldWrite>,
    pub turn_upserts: Vec<UsageTurnWrite>,
    pub turn_rewrites: Vec<UsageTurnRewriteWrite>,
    pub anomalies: Vec<UsageAnomalyWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageCheckpointExpectation {
    pub parser_version: i64,
    pub committed_offset: i64,
    pub guard_hash: Option<Vec<u8>>,
    pub processing_status: CheckpointProcessingStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSourceCommit {
    pub source_file_id: i64,
    pub expected_file_generation: i64,
    pub expected_previous_thread_id: Option<String>,
    pub expected_checkpoint: UsageCheckpointExpectation,
    pub expected_checkpoint_missing: bool,
    pub expected_state: Option<UsageSourceStateWrite>,
    pub local_replay: bool,
    pub batch_start_offset: i64,
    pub fixed_observed_raw_size: i64,
    pub last_complete_offset: i64,
    pub source_bytes_consumed: i64,
    pub complete_line_count: i64,
    pub canonical_event_count: i64,
    pub occurrence_count: i64,
    pub evidence_write_count: i64,
    pub write_unit_count: i64,
    pub replayed_prefix_bytes: i64,
    pub replayed_prefix_lines: i64,
    pub fixed_view_exhausted: bool,
    pub tail_status: UsageTailStatus,
    pub tail_start_offset: Option<i64>,
    pub skill_events: Vec<SkillUsageEventWrite>,
    pub patch: ReconciliationPatchWrite,
    pub reconciliation_request: crate::codex::ingestion::usage_processor::ReconciliationRequest,
    pub reconciliation_expected_fingerprint: Vec<u8>,
    pub updated_state: UsageSourceStateWrite,
    pub next_guard_hash: Option<Vec<u8>>,
    pub committed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageCommitBatch {
    pub ledger_epoch: i64,
    pub usage_parser_version: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub sources: Vec<UsageSourceCommit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UsageCommitOutcome {
    pub sources_committed: usize,
    pub events_inserted: usize,
    pub events_deduplicated: usize,
    pub data_revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsagePlanAction {
    ReadFrom,
    BuildFrom,
    LocalReplay,
    ResumeOwningLive,
    VerifyRawTail,
    CompleteOnly,
    BeginCarry,
    ResumeCarry,
    Skip,
    BlockedRelationship,
    RebuildRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageBuildCompletion {
    Pending,
    Rebuilt,
    Carried,
    Blocked,
    Quarantined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageCarryPhase {
    None,
    Occurrences,
    Facts,
    Markers,
    Windows,
    Turns,
    Anomalies,
    Finalize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CarryStepOutcome {
    Progress,
    FinalizedMissing,
    FinalizedPresent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageBuildPlanState {
    pub build_epoch: i64,
    pub target_parser_version: i64,
    pub expected_file_generation: i64,
    pub expected_device_id: i64,
    pub expected_inode: i64,
    pub expected_owning_thread_id: Option<String>,
    pub expected_root_session_id: Option<String>,
    pub active_committed_offset: i64,
    pub active_guard_hash: Option<Vec<u8>>,
    pub active_state_fingerprint: Option<Vec<u8>>,
    pub required_through_offset: i64,
    pub observed_raw_size: i64,
    pub raw_tail_status: UsageTailStatus,
    pub raw_tail_start_offset: Option<i64>,
    pub completion_status: UsageBuildCompletion,
    pub carry_phase: UsageCarryPhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSourceScanPlan {
    pub source_file_id: i64,
    pub action: UsagePlanAction,
    pub start_offset: i64,
    pub observed_size: i64,
    pub owning_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub checkpoint: Option<UsageCheckpointExpectation>,
    pub state: Option<UsageSourceStateWrite>,
    pub open_turn: Option<crate::codex::ingestion::usage_processor::TurnState>,
    pub build: Option<UsageBuildPlanState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageScanState {
    pub epoch: SourceUsageEpochState,
    pub plans: Vec<UsageSourceScanPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageReconciliationBasicProof {
    pub device_id: i64,
    pub inode: i64,
    pub observed_raw_size: i64,
    pub expected_checkpoint: Option<UsageCheckpointExpectation>,
    pub expected_state: Option<UsageSourceStateWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWindowMetadata {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
    pub source_end_offset: u64,
    pub owning_thread_id: String,
    pub turn_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageReconciliationContext {
    pub context: crate::codex::ingestion::usage_processor::ReconciliationContext,
    pub window_metadata:
        BTreeMap<crate::codex::ingestion::usage_processor::WindowKey, UsageWindowMetadata>,
    pub window_proposals: BTreeMap<
        crate::codex::ingestion::usage_processor::WindowKey,
        Vec<UsageWindowProposalBinding>,
    >,
    pub response_occurrences: BTreeMap<
        crate::codex::ingestion::usage_processor::ResponseKey,
        Vec<crate::codex::ingestion::usage_processor::Occurrence>,
    >,
    pub closure_response_keys: BTreeSet<crate::codex::ingestion::usage_processor::ResponseKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWindowProposalBinding {
    pub proposal: crate::codex::ingestion::usage_processor::CanonicalUsageProposal,
    pub fact: crate::codex::ingestion::usage_processor::UsageEventFact,
    pub occurrences: Vec<crate::codex::ingestion::usage_processor::Occurrence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkListRow {
    pub source_file_id: i64,
    pub owning_thread_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkList {
    pub epoch: SourceUsageEpochState,
    pub threads: Vec<UsageWorkThread>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkThread {
    pub thread_id: String,
    pub source_file_ids: Vec<i64>,
}

// CodexStorage business wrappers are defined below.

#[derive(Debug)]
pub(crate) struct CodexUsageCommitBridgeResult {
    pub outcome: UsageCommitOutcome,
    pub visible_changed: bool,
}

pub(super) fn load_usage_work_list(
    storage: &CodexStorage<'_>,
    present_source_ids: &[i64],
    parser_version: i64,
) -> Result<UsageWorkList, CodexStorageError> {
    validate_usage_source_ids(present_source_ids).map_err(CodexStorageError::from)?;
    storage.with_read(|connection| {
        let transaction = connection
            .unchecked_transaction()
            .map_err(CodexStorageError::from)?;
        let epoch = read_epoch(&transaction).map_err(CodexStorageError::from)?;
        let canonical = canonical_algorithm_for(epoch.working_parser_version());
        let mut rows = Vec::new();
        if !(epoch.active_epoch == 0 && epoch.build_epoch.is_none())
            && parser_version == epoch.working_parser_version()
            && let Some(canonical) = canonical
        {
            let mut query_ids = present_source_ids.to_vec();
            if let Some(build_epoch) = epoch.build_epoch {
                let mut statement = transaction
                    .prepare(
                        "SELECT source_file_id FROM codex_usage_build_sources
                         WHERE build_epoch=?1 ORDER BY source_file_id",
                    )
                    .map_err(CodexStorageError::from)?;
                for row in statement
                    .query_map([build_epoch], |row| row.get::<_, i64>(0))
                    .map_err(CodexStorageError::from)?
                {
                    query_ids.push(row.map_err(CodexStorageError::from)?);
                }
            }
            query_ids.sort_unstable();
            query_ids.dedup();
            const MAX_WORKLIST_BIND_IDS: usize = 900;
            for chunk in query_ids.chunks(MAX_WORKLIST_BIND_IDS) {
                if epoch.build_epoch.is_some() {
                    load_usage_build_work_list_chunk(
                        &transaction,
                        epoch.clone(),
                        parser_version,
                        canonical,
                        chunk,
                        &mut rows,
                    )
                    .map_err(CodexStorageError::from)?;
                } else {
                    load_usage_stable_work_list_chunk(
                        &transaction,
                        epoch.clone(),
                        parser_version,
                        canonical,
                        chunk,
                        &mut rows,
                    )
                    .map_err(CodexStorageError::from)?;
                }
            }
        }
        rows.sort_by(|left, right| {
            left.owning_thread_id
                .cmp(&right.owning_thread_id)
                .then_with(|| left.source_file_id.cmp(&right.source_file_id))
        });
        rows.dedup_by_key(|row| row.source_file_id);
        transaction.commit().map_err(CodexStorageError::from)?;
        let mut threads = Vec::<UsageWorkThread>::new();
        for row in rows {
            match threads.last_mut() {
                Some(thread) if thread.thread_id == row.owning_thread_id => {
                    thread.source_file_ids.push(row.source_file_id);
                }
                _ => threads.push(UsageWorkThread {
                    thread_id: row.owning_thread_id,
                    source_file_ids: vec![row.source_file_id],
                }),
            }
        }
        Ok(UsageWorkList { epoch, threads })
    })
}

pub(super) fn load_usage_scan_state_exact(
    storage: &CodexStorage<'_>,
    source_file_ids: &[i64],
    parser_version: i64,
    expected_epoch: SourceUsageEpochState,
) -> Result<UsageScanState, CodexStorageError> {
    load_usage_scan_state_inner(
        storage,
        source_file_ids,
        parser_version,
        Some(expected_epoch),
    )
}

pub(super) fn load_usage_scan_state(
    storage: &CodexStorage<'_>,
    source_file_ids: &[i64],
    parser_version: i64,
) -> Result<UsageScanState, CodexStorageError> {
    load_usage_scan_state_inner(storage, source_file_ids, parser_version, None)
}

fn load_usage_scan_state_inner(
    storage: &CodexStorage<'_>,
    source_file_ids: &[i64],
    parser_version: i64,
    expected_epoch: Option<SourceUsageEpochState>,
) -> Result<UsageScanState, CodexStorageError> {
    validate_usage_source_ids(source_file_ids).map_err(CodexStorageError::from)?;
    storage.with_read(|connection| {
        let transaction = connection
            .unchecked_transaction()
            .map_err(CodexStorageError::from)?;
        let epoch = read_epoch(&transaction).map_err(CodexStorageError::from)?;
        if expected_epoch.as_ref().is_some_and(|value| value != &epoch) {
            return Err(CodexStorageError::Storage(StorageError::invalid_state(
                "usage epoch changed while loading exact plans",
            )));
        }
        let mut plans = Vec::with_capacity(source_file_ids.len());
        for source_file_id in source_file_ids {
            plans.push(
                load_source_plan(&transaction, *source_file_id, parser_version, epoch.clone())
                    .map_err(CodexStorageError::from)?,
            );
        }
        plans.sort_by_key(|plan| plan.source_file_id);
        transaction.commit().map_err(CodexStorageError::from)?;
        Ok(UsageScanState { epoch, plans })
    })
}

pub(super) fn load_usage_reconciliation_context(
    storage: &CodexStorage<'_>,
    ledger_epoch: i64,
    context: crate::codex::ingestion::usage_processor::UsageContext,
    request: crate::codex::ingestion::usage_processor::ReconciliationRequest,
    basic_proof: UsageReconciliationBasicProof,
) -> Result<UsageReconciliationContext, CodexStorageError> {
    storage.with_read(|connection| {
        let transaction = connection
            .unchecked_transaction()
            .map_err(CodexStorageError::from)?;
        let epoch = read_epoch(&transaction).map_err(CodexStorageError::from)?;
        if ledger_epoch != epoch.working_epoch() {
            return Err(CodexStorageError::Storage(StorageError::invalid_state(
                "usage reconciliation epoch changed",
            )));
        }
        if !reconciliation_basic_proof_matches(&transaction, ledger_epoch, &context, &basic_proof)
            .map_err(CodexStorageError::from)?
        {
            return Err(CodexStorageError::ReconciliationPlanStale);
        }
        let result =
            read_reconciliation_context(&transaction, ledger_epoch, context, request, &basic_proof)
                .map_err(CodexStorageError::from)?;
        transaction.commit().map_err(CodexStorageError::from)?;
        Ok(result)
    })
}

fn read_reconciliation_context(
    connection: &Connection,
    epoch: i64,
    context: crate::codex::ingestion::usage_processor::UsageContext,
    request: crate::codex::ingestion::usage_processor::ReconciliationRequest,
    basic_proof: &UsageReconciliationBasicProof,
) -> StorageResult<UsageReconciliationContext> {
    use crate::codex::ingestion::usage_processor::{
        AffectedTurn, ReconciliationContext, ReconciliationRequest, ResponseKey, WindowKey,
    };

    if epoch <= 0
        || context.source_file_id <= 0
        || context.file_generation <= 0
        || context.owning_thread_id.is_empty()
        || context.root_session_id.is_empty()
        || request.response_keys.len() + request.owning_turn_keys.len() > 8192
        || request
            != ReconciliationRequest::new(
                request.response_keys.clone(),
                request.owning_turn_keys.clone(),
            )
        || request.response_keys.iter().any(|key| {
            key.owning_thread_id != context.owning_thread_id || key.response_id.is_empty()
        })
        || request
            .owning_turn_keys
            .iter()
            .any(|(thread, _)| thread != &context.owning_thread_id)
    {
        return Err(StorageError::invalid_state(
            "invalid usage reconciliation request",
        ));
    }
    validate_reconciliation_basic_proof(connection, epoch, &context, basic_proof)?;
    validate_reconciliation_scope(connection, &context)?;

    let mut reconciliation = ReconciliationContext {
        request: request.clone(),
        ..ReconciliationContext::default()
    };
    let mut result = UsageReconciliationContext {
        context: ReconciliationContext::default(),
        window_metadata: BTreeMap::new(),
        window_proposals: BTreeMap::new(),
        response_occurrences: BTreeMap::new(),
        closure_response_keys: request.response_keys.iter().cloned().collect(),
    };
    let mut affected = BTreeMap::new();
    for (thread_id, turn_key) in &request.owning_turn_keys {
        for window in load_turn_windows(connection, epoch, thread_id, turn_key.as_deref())? {
            if window.owning_thread_id != *thread_id || window.turn_key != *turn_key {
                return Err(StorageError::usage_conflict(
                    "reconciliation window is bound to a different Turn",
                ));
            }
            let key = WindowKey {
                source_file_id: window.source_file_id,
                file_generation: window.file_generation,
                start_offset: window.source_start_offset,
            };
            validate_window_physical_source(connection, &window)?;
            let metadata = UsageWindowMetadata {
                source_file_id: window.source_file_id,
                file_generation: window.file_generation,
                source_start_offset: window.source_start_offset,
                source_end_offset: window.source_end_offset,
                owning_thread_id: window.owning_thread_id.clone(),
                turn_key: window.turn_key.clone(),
            };
            if reconciliation
                .windows
                .insert(key, window.state.clone())
                .is_some()
            {
                return Err(StorageError::invalid_state(
                    "duplicate persisted reconciliation window key",
                ));
            }
            result.window_metadata.insert(key, metadata);
            for response_id in window
                .state
                .explicit_response_ids
                .iter()
                .chain(window.state.legacy_covered_response_ids.iter())
            {
                result.closure_response_keys.insert(ResponseKey {
                    owning_thread_id: window.owning_thread_id.clone(),
                    response_id: response_id.clone(),
                });
            }
            let mut proposals = Vec::with_capacity(window.state.proposal_event_ids.len());
            for event_id in &window.state.proposal_event_ids {
                let (proposal, fact) = load_event_binding(connection, epoch, event_id)?
                    .ok_or_else(|| {
                        StorageError::usage_conflict(
                            "reconciliation window proposal is missing its canonical fact",
                        )
                    })?;
                if fact.owning_thread_id != window.owning_thread_id {
                    return Err(StorageError::usage_conflict(
                        "reconciliation window proposal belongs to another Thread",
                    ));
                }
                validate_proposal_thread_binding(connection, &proposal, &window.owning_thread_id)?;
                let occurrences = load_event_occurrences(connection, epoch, event_id)?;
                if occurrences.is_empty() {
                    return Err(StorageError::usage_conflict(
                        "reconciliation window proposal has no physical occurrence",
                    ));
                }
                validate_occurrence_sources(connection, &occurrences, &fact.owning_thread_id)?;
                proposals.push(UsageWindowProposalBinding {
                    proposal,
                    fact,
                    occurrences,
                });
            }
            result.window_proposals.insert(key, proposals);
        }
        for snapshot in load_turn_snapshots(connection, epoch, thread_id, turn_key.as_deref())? {
            validate_turn_physical_source(connection, &snapshot)?;
            let (compensation_events, compensation_occurrences) =
                load_turn_compensation(connection, epoch, &snapshot)?;
            let key = snapshot.key.clone();
            if affected
                .insert(
                    key,
                    AffectedTurn {
                        snapshot,
                        compensation_events,
                        compensation_occurrences,
                    },
                )
                .is_some()
            {
                return Err(StorageError::usage_conflict(
                    "duplicate persisted Turn dependency key",
                ));
            }
        }
    }
    reconciliation.affected_turns = affected;
    for (thread_id, turn_key) in &request.owning_turn_keys {
        let Some(turn_key) = turn_key.as_deref() else {
            continue;
        };
        let mut statement = connection.prepare(
            "SELECT DISTINCT f.response_id
             FROM codex_usage_event_facts f
             JOIN usage_events e
               ON e.source='codex' AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
             JOIN codex_usage_event_occurrences o
               ON o.source='codex' AND o.ledger_epoch=f.ledger_epoch AND o.event_id=f.event_id
             WHERE f.source='codex' AND f.ledger_epoch=?1
               AND f.owning_thread_id=?2 AND f.response_id IS NOT NULL
               AND f.evidence_kind='explicit' AND e.turn_key=?3
               AND o.source_file_id=?4 AND o.file_generation=?5
             ORDER BY f.response_id",
        )?;
        for row in statement.query_map(
            params![
                epoch,
                thread_id,
                turn_key,
                context.source_file_id,
                context.file_generation
            ],
            |row| row.get::<_, String>(0),
        )? {
            result.closure_response_keys.insert(ResponseKey {
                owning_thread_id: thread_id.clone(),
                response_id: row?,
            });
        }
    }
    for key in &result.closure_response_keys {
        let markers = load_response_markers(connection, epoch, key)?;
        for marker in &markers {
            validate_marker_physical_source(connection, marker)?;
        }
        if let Some(binding) = load_response_binding(connection, epoch, key)? {
            validate_proposal_thread_binding(connection, &binding.proposal, &key.owning_thread_id)?;
            let occurrences =
                load_event_occurrences(connection, epoch, &binding.proposal.event_id)?;
            validate_occurrence_sources(connection, &occurrences, &binding.fact.owning_thread_id)?;
            let holds = load_event_holds(connection, epoch, &binding.proposal.event_id)?;
            validate_event_hold_sources(connection, &holds, &key.owning_thread_id)?;
            let has_resolved_marker = markers.iter().any(|marker| {
                marker.resolved_event_id.as_deref() == Some(binding.proposal.event_id.as_str())
            });
            if occurrences.is_empty() && !has_resolved_marker && holds.is_empty() {
                return Err(StorageError::usage_conflict(
                    "response binding has no occurrence, resolved marker, or hold",
                ));
            }
            reconciliation.bindings.insert(key.clone(), binding);
            result.response_occurrences.insert(key.clone(), occurrences);
        }
        reconciliation.markers.extend(markers);
    }
    reconciliation.markers.sort_by_key(|marker| {
        (
            marker.source_file_id,
            marker.file_generation,
            marker.source_start_offset,
        )
    });
    reconciliation.markers.dedup_by(|left, right| {
        left.source_file_id == right.source_file_id
            && left.file_generation == right.file_generation
            && left.source_start_offset == right.source_start_offset
    });
    result.context = reconciliation;
    result.context.expected_fingerprint =
        compute_reconciliation_context_fingerprint(connection, epoch, &context, &result)?;
    Ok(result)
}

fn validate_reconciliation_basic_proof(
    connection: &Connection,
    epoch: i64,
    context: &crate::codex::ingestion::usage_processor::UsageContext,
    proof: &UsageReconciliationBasicProof,
) -> StorageResult<()> {
    if reconciliation_basic_proof_matches(connection, epoch, context, proof)? {
        Ok(())
    } else {
        Err(StorageError::usage_conflict(
            "usage reconciliation basic proof changed",
        ))
    }
}

fn reconciliation_basic_proof_matches(
    connection: &Connection,
    epoch: i64,
    context: &crate::codex::ingestion::usage_processor::UsageContext,
    proof: &UsageReconciliationBasicProof,
) -> StorageResult<bool> {
    let source: Option<(Option<String>, i64, i64, i64, i64, String)> = connection
        .query_row(
            "SELECT thread_id,file_generation,device_id,inode,observed_size,file_status
             FROM codex_source_files WHERE source_file_id=?1",
            [context.source_file_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((thread_id, generation, device_id, inode, observed_size, status)) = source else {
        return Ok(false);
    };
    if thread_id.as_deref() != Some(context.owning_thread_id.as_str())
        || generation != context.file_generation
        || device_id != proof.device_id
        || inode != proof.inode
        || observed_size != proof.observed_raw_size
        || status != "present"
        || proof.observed_raw_size < 0
    {
        return Ok(false);
    }
    if read_usage_checkpoint(connection, context.source_file_id)? != proof.expected_checkpoint {
        return Ok(false);
    }
    if read_usage_source_state(connection, epoch, context.source_file_id)? != proof.expected_state {
        return Ok(false);
    }
    if proof.expected_state.as_ref().is_some_and(|state| {
        state.file_generation != context.file_generation
            || state.device_id != proof.device_id
            || state.inode != proof.inode
            || state.owning_thread_id != context.owning_thread_id
            || state.root_session_id != context.root_session_id
    }) {
        return Ok(false);
    }
    Ok(true)
}

fn validate_reconciliation_scope(
    connection: &Connection,
    context: &crate::codex::ingestion::usage_processor::UsageContext,
) -> StorageResult<()> {
    let scope: Option<(
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    )> = connection
        .query_row(
            "SELECT sf.file_generation,sf.thread_id,t.root_session_id,
                        mf.file_generation,mf.owning_thread_id
                 FROM codex_source_files sf
                 LEFT JOIN threads t ON t.thread_id=sf.thread_id
                 LEFT JOIN codex_rollout_metadata_facts mf ON mf.source_file_id=sf.source_file_id
                 WHERE sf.source_file_id=?1",
            [context.source_file_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((generation, thread, root, metadata_generation, metadata_thread)) = scope else {
        return Err(StorageError::usage_conflict(
            "usage reconciliation source metadata is missing",
        ));
    };
    if generation != context.file_generation
        || thread.as_deref() != Some(context.owning_thread_id.as_str())
        || root.as_deref() != Some(context.root_session_id.as_str())
        || metadata_generation.is_some_and(|value| value != generation)
        || metadata_thread
            .as_deref()
            .is_some_and(|value| value != context.owning_thread_id)
    {
        return Err(StorageError::usage_conflict(
            "usage reconciliation source binding changed",
        ));
    }
    Ok(())
}

fn load_response_binding(
    connection: &Connection,
    epoch: i64,
    key: &crate::codex::ingestion::usage_processor::ResponseKey,
) -> StorageResult<Option<crate::codex::ingestion::usage_processor::ResponseBinding>> {
    use crate::codex::ingestion::usage_processor::{
        CanonicalUsageProposal, ResponseBinding, UsageEventFact,
    };

    let mut statement = connection.prepare(
        "SELECT e.event_id,e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,
                e.turn_key,e.model,e.reasoning_effort,e.input_tokens,e.cached_tokens,
                e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens,
                f.owning_thread_id,f.response_id,f.evidence_kind,f.operation
         FROM codex_usage_event_facts f JOIN usage_events e
           ON e.source=f.source AND e.source_epoch=f.ledger_epoch AND e.event_id=f.event_id
         WHERE f.source='codex' AND f.ledger_epoch=?1 AND f.owning_thread_id=?2
           AND f.response_id=?3 ORDER BY e.event_id",
    )?;
    let mut rows = statement.query(params![epoch, key.owning_thread_id, key.response_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let usage = NormalizedTokenUsage::new(
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    )
    .map_err(super::to_domain_sql_error)?;
    let event_id: String = row.get(0)?;
    let proposal = CanonicalUsageProposal {
        event_id: event_id.clone(),
        kind: parse_event_kind(&row.get::<_, String>(1)?)?,
        occurred_at_ms: row.get(2)?,
        thread_id: row.get(3)?,
        root_session_id: row.get(4)?,
        turn_key: row.get(5)?,
        model: row.get(6)?,
        reasoning_effort: row.get(7)?,
        usage,
    };
    let fact = UsageEventFact {
        event_id,
        owning_thread_id: row.get(14)?,
        response_id: row.get(15)?,
        evidence_kind: parse_evidence_kind(&row.get::<_, String>(16)?)?,
        operation: parse_codex_operation(&row.get::<_, String>(17)?)?,
    };
    if fact.response_id.as_deref() != Some(key.response_id.as_str())
        || fact.owning_thread_id != key.owning_thread_id
        || rows.next()?.is_some()
    {
        return Err(StorageError::invalid_state(
            "response identity has multiple canonical bindings",
        ));
    }
    Ok(Some(ResponseBinding { proposal, fact }))
}

fn load_event_binding(
    connection: &Connection,
    epoch: i64,
    event_id: &str,
) -> StorageResult<
    Option<(
        crate::codex::ingestion::usage_processor::CanonicalUsageProposal,
        crate::codex::ingestion::usage_processor::UsageEventFact,
    )>,
> {
    use crate::codex::ingestion::usage_processor::{CanonicalUsageProposal, UsageEventFact};

    let mut statement = connection.prepare(
        "SELECT e.event_id,e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,
                e.turn_key,e.model,e.reasoning_effort,e.input_tokens,e.cached_tokens,
                e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens,
                f.owning_thread_id,f.response_id,f.evidence_kind,f.operation
         FROM usage_events e JOIN codex_usage_event_facts f
           ON f.source=e.source AND f.ledger_epoch=e.source_epoch AND f.event_id=e.event_id
         WHERE e.source='codex' AND e.source_epoch=?1 AND e.event_id=?2
         ORDER BY f.owning_thread_id,f.response_id",
    )?;
    let mut rows = statement.query(params![epoch, event_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let usage = NormalizedTokenUsage::new(
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    )
    .map_err(super::to_domain_sql_error)?;
    let id: String = row.get(0)?;
    let proposal = CanonicalUsageProposal {
        event_id: id.clone(),
        kind: parse_event_kind(&row.get::<_, String>(1)?)?,
        occurred_at_ms: row.get(2)?,
        thread_id: row.get(3)?,
        root_session_id: row.get(4)?,
        turn_key: row.get(5)?,
        model: row.get(6)?,
        reasoning_effort: row.get(7)?,
        usage,
    };
    let fact = UsageEventFact {
        event_id: id,
        owning_thread_id: row.get(14)?,
        response_id: row.get(15)?,
        evidence_kind: parse_evidence_kind(&row.get::<_, String>(16)?)?,
        operation: parse_codex_operation(&row.get::<_, String>(17)?)?,
    };
    if fact.event_id != event_id
        || fact.owning_thread_id != proposal.thread_id
        || rows.next()?.is_some()
    {
        return Err(StorageError::usage_conflict(
            "canonical event has an ambiguous reconciliation fact",
        ));
    }
    Ok(Some((proposal, fact)))
}

fn load_event_occurrences(
    connection: &Connection,
    epoch: i64,
    event_id: &str,
) -> StorageResult<Vec<crate::codex::ingestion::usage_processor::Occurrence>> {
    use crate::codex::ingestion::usage_processor::Occurrence;

    let mut statement = connection.prepare(
        "SELECT source_file_id,file_generation,source_start_offset,source_end_offset,event_id
         FROM codex_usage_event_occurrences INDEXED BY codex_usage_event_occurrences_event_idx
         WHERE source='codex' AND ledger_epoch=?1 AND event_id=?2
         ORDER BY source_file_id,file_generation,source_start_offset",
    )?;
    statement
        .query_map(params![epoch, event_id], |row| {
            Ok(Occurrence {
                source_file_id: row.get(0)?,
                file_generation: row.get(1)?,
                source_start_offset: u64::try_from(row.get::<_, i64>(2)?).map_err(|_| {
                    rusqlite::Error::InvalidParameterName("invalid occurrence offset".to_owned())
                })?,
                source_end_offset: u64::try_from(row.get::<_, i64>(3)?).map_err(|_| {
                    rusqlite::Error::InvalidParameterName("invalid occurrence offset".to_owned())
                })?,
                event_id: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::from)
}

fn load_event_holds(
    connection: &Connection,
    epoch: i64,
    event_id: &str,
) -> StorageResult<Vec<UsageEventHoldReference>> {
    let mut statement = connection.prepare(
        "SELECT source_file_id,file_generation,event_id,hold_reason
         FROM codex_usage_event_holds
         WHERE source='codex' AND ledger_epoch=?1 AND event_id=?2
         ORDER BY source_file_id,file_generation,event_id",
    )?;
    statement
        .query_map(params![epoch, event_id], |row| {
            Ok(UsageEventHoldReference {
                source_file_id: row.get(0)?,
                file_generation: row.get(1)?,
                event_id: row.get(2)?,
                hold_reason: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::from)
}

fn validate_event_hold_sources(
    connection: &Connection,
    holds: &[UsageEventHoldReference],
    owning_thread_id: &str,
) -> StorageResult<()> {
    for hold in holds {
        if hold.source_file_id <= 0
            || hold.file_generation <= 0
            || hold.event_id.is_empty()
            || !matches!(hold.hold_reason.as_str(), "replay" | "carry")
        {
            return Err(StorageError::invalid_state(
                "invalid Codex usage event hold",
            ));
        }
        let metadata: Option<(Option<String>, i64, Option<i64>, Option<String>)> = connection
            .query_row(
                "SELECT sf.thread_id,sf.file_generation,mf.file_generation,mf.owning_thread_id
                 FROM codex_source_files sf
                 LEFT JOIN codex_rollout_metadata_facts mf
                   ON mf.source_file_id=sf.source_file_id
                 WHERE sf.source_file_id=?1",
                [hold.source_file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((source_thread, source_generation, metadata_generation, metadata_thread)) =
            metadata
        else {
            return Err(StorageError::usage_conflict(
                "response hold source metadata is missing",
            ));
        };
        if source_thread.as_deref() != Some(owning_thread_id)
            || source_generation != hold.file_generation
            || metadata_generation.is_some_and(|generation| generation != hold.file_generation)
            || metadata_thread
                .as_deref()
                .is_some_and(|thread| thread != owning_thread_id)
        {
            return Err(StorageError::usage_conflict(
                "response hold source binding changed",
            ));
        }
    }
    Ok(())
}

fn validate_occurrence_sources(
    connection: &Connection,
    occurrences: &[crate::codex::ingestion::usage_processor::Occurrence],
    owning_thread_id: &str,
) -> StorageResult<()> {
    for occurrence in occurrences {
        if occurrence.source_file_id <= 0
            || occurrence.file_generation <= 0
            || occurrence.source_end_offset <= occurrence.source_start_offset
        {
            return Err(StorageError::usage_conflict(
                "reconciliation dependency has an invalid physical occurrence",
            ));
        }
        validate_physical_source_binding(
            connection,
            occurrence.source_file_id,
            occurrence.file_generation,
            owning_thread_id,
            i64::try_from(occurrence.source_end_offset).map_err(|_| {
                StorageError::usage_conflict("reconciliation occurrence offset exceeds storage")
            })?,
        )?;
    }
    Ok(())
}

fn validate_physical_source_binding(
    connection: &Connection,
    source_file_id: i64,
    file_generation: i64,
    owning_thread_id: &str,
    required_through_offset: i64,
) -> StorageResult<()> {
    let physical: Option<(
        Option<String>,
        i64,
        i64,
        String,
        Option<i64>,
        Option<String>,
    )> = connection
        .query_row(
            "SELECT sf.thread_id,sf.file_generation,sf.observed_size,sf.file_status,
                    mf.file_generation,mf.owning_thread_id
             FROM codex_source_files sf
             LEFT JOIN codex_rollout_metadata_facts mf ON mf.source_file_id=sf.source_file_id
             WHERE sf.source_file_id=?1",
            [source_file_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((thread, generation, observed, status, metadata_generation, metadata_thread)) =
        physical
    else {
        return Err(StorageError::usage_conflict(
            "reconciliation dependency source metadata is missing",
        ));
    };
    if thread.as_deref() != Some(owning_thread_id)
        || generation != file_generation
        || observed < required_through_offset
        || status != "present"
        || metadata_generation.is_some_and(|value| value != file_generation)
        || metadata_thread
            .as_deref()
            .is_some_and(|value| value != owning_thread_id)
    {
        return Err(StorageError::usage_conflict(
            "reconciliation dependency physical source binding changed",
        ));
    }
    Ok(())
}

fn validate_proposal_thread_binding(
    connection: &Connection,
    proposal: &crate::codex::ingestion::usage_processor::CanonicalUsageProposal,
    owning_thread_id: &str,
) -> StorageResult<()> {
    if proposal.thread_id != owning_thread_id {
        return Err(StorageError::usage_conflict(
            "reconciliation canonical event Thread binding changed",
        ));
    }
    let thread: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT source,root_session_id FROM threads WHERE thread_id=?1",
            [owning_thread_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if thread
        .as_ref()
        .map(|(source, root)| (source.as_str(), root.as_deref()))
        != Some(("codex", Some(proposal.root_session_id.as_str())))
    {
        return Err(StorageError::usage_conflict(
            "reconciliation canonical event root binding changed",
        ));
    }
    Ok(())
}

fn validate_window_physical_source(
    connection: &Connection,
    window: &crate::codex::ingestion::usage_processor::LegacyWindowWrite,
) -> StorageResult<()> {
    let start = i64::try_from(window.source_start_offset)
        .map_err(|_| StorageError::usage_conflict("window start offset exceeds storage"))?;
    let end = i64::try_from(window.source_end_offset)
        .map_err(|_| StorageError::usage_conflict("window end offset exceeds storage"))?;
    if start < 0 || end <= start {
        return Err(StorageError::usage_conflict(
            "reconciliation window has an invalid physical range",
        ));
    }
    validate_physical_source_binding(
        connection,
        window.source_file_id,
        window.file_generation,
        &window.owning_thread_id,
        end,
    )
}

fn validate_turn_physical_source(
    connection: &Connection,
    turn: &crate::codex::ingestion::usage_processor::PersistedTurnSnapshot,
) -> StorageResult<()> {
    let required = turn
        .end_offset
        .unwrap_or(turn.state_through_offset)
        .max(turn.state.start_offset);
    let required = i64::try_from(required)
        .map_err(|_| StorageError::usage_conflict("Turn offset exceeds storage"))?;
    validate_physical_source_binding(
        connection,
        turn.key.source_file_id,
        turn.key.file_generation,
        &turn.owning_thread_id,
        required,
    )
}

fn validate_marker_physical_source(
    connection: &Connection,
    marker: &crate::codex::ingestion::usage_processor::CompactionMarkerWrite,
) -> StorageResult<()> {
    let start = i64::try_from(marker.source_start_offset)
        .map_err(|_| StorageError::usage_conflict("marker offset exceeds storage"))?;
    let end = i64::try_from(marker.source_end_offset)
        .map_err(|_| StorageError::usage_conflict("marker offset exceeds storage"))?;
    if start < 0 || end <= start {
        return Err(StorageError::usage_conflict(
            "compaction marker has an invalid physical range",
        ));
    }
    validate_physical_source_binding(
        connection,
        marker.source_file_id,
        marker.file_generation,
        &marker.owning_thread_id,
        end,
    )
}

fn load_response_markers(
    connection: &Connection,
    epoch: i64,
    key: &crate::codex::ingestion::usage_processor::ResponseKey,
) -> StorageResult<Vec<crate::codex::ingestion::usage_processor::CompactionMarkerWrite>> {
    use crate::codex::ingestion::usage_processor::CompactionMarkerWrite;

    let mut statement = connection.prepare(
        "SELECT source_file_id,file_generation,source_start_offset,source_end_offset,
                owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,
                resolved_event_id,unknown_reason
         FROM codex_compaction_markers
         WHERE source='codex' AND ledger_epoch=?1 AND owning_thread_id=?2 AND response_id=?3
         ORDER BY source_file_id,file_generation,source_start_offset",
    )?;
    statement
        .query_map(
            params![epoch, key.owning_thread_id, key.response_id],
            |row| {
                let unknown: Option<String> = row.get(11)?;
                Ok(CompactionMarkerWrite {
                    source_file_id: row.get(0)?,
                    file_generation: row.get(1)?,
                    source_start_offset: u64::try_from(row.get::<_, i64>(2)?).map_err(|_| {
                        rusqlite::Error::InvalidParameterName("invalid marker offset".to_owned())
                    })?,
                    source_end_offset: u64::try_from(row.get::<_, i64>(3)?).map_err(|_| {
                        rusqlite::Error::InvalidParameterName("invalid marker offset".to_owned())
                    })?,
                    owning_thread_id: row.get(4)?,
                    root_session_id: row.get(5)?,
                    occurred_at_ms: row.get(6)?,
                    model: row.get(7)?,
                    reasoning_effort: row.get(8)?,
                    response_id: row.get(9)?,
                    resolved_event_id: row.get(10)?,
                    unknown_reason: unknown
                        .as_deref()
                        .map(parse_marker_unknown_reason)
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::InvalidParameterName(error.to_string())
                        })?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::from)
}

fn load_turn_windows(
    connection: &Connection,
    epoch: i64,
    thread_id: &str,
    turn_key: Option<&str>,
) -> StorageResult<Vec<crate::codex::ingestion::usage_processor::LegacyWindowWrite>> {
    use crate::codex::ingestion::usage_processor::LegacyWindowWrite;

    let mut statement = connection.prepare(
        "SELECT source_file_id,file_generation,source_start_offset,source_end_offset,
                owning_thread_id,turn_key,state_json
         FROM codex_usage_reconciliation_windows
         WHERE source='codex' AND ledger_epoch=?1 AND owning_thread_id=?2 AND turn_key IS ?3
         ORDER BY source_file_id,file_generation,source_start_offset",
    )?;
    let mut windows = Vec::new();
    for row in statement.query_map(params![epoch, thread_id, turn_key], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
        ))
    })? {
        let (source_file_id, file_generation, start, end, owning, turn, state_json) = row?;
        let canonical = canonical_window_state(&state_json)?;
        let state =
            crate::codex::ingestion::usage_processor::LegacyReconciliationWindow::from_json(
                &canonical,
            )
            .map_err(|_| StorageError::invalid_state("invalid persisted reconciliation window"))?;
        windows.push(LegacyWindowWrite {
            source_file_id,
            file_generation,
            source_start_offset: u64::try_from(start)
                .map_err(|_| StorageError::invalid_state("invalid window start offset"))?,
            source_end_offset: u64::try_from(end)
                .map_err(|_| StorageError::invalid_state("invalid window end offset"))?,
            owning_thread_id: owning,
            turn_key: turn,
            state,
        });
    }
    Ok(windows)
}

fn load_turn_snapshots(
    connection: &Connection,
    epoch: i64,
    thread_id: &str,
    turn_key: Option<&str>,
) -> StorageResult<Vec<crate::codex::ingestion::usage_processor::PersistedTurnSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT source_file_id,file_generation,turn_key,thread_id,raw_turn_id,started_at_ms,
                ended_at_ms,start_offset,end_offset,status,
                start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
                start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,
                last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
                last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,
                accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
                accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
                accounted_candidate_count,model_state,single_model,unresolved_model_seen,
                reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
                compensation_allowed,block_start_missing,block_time_missing,block_reset,
                block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,
                quality_status,state_through_offset
         FROM codex_turns
         WHERE ledger_epoch=?1 AND thread_id=?2 AND turn_key IS ?3
         ORDER BY source_file_id,file_generation,turn_key",
    )?;
    let mut snapshots = Vec::new();
    for row in statement.query_map(
        params![epoch, thread_id, turn_key],
        read_persisted_turn_snapshot,
    )? {
        snapshots.push(row?);
    }
    Ok(snapshots)
}

fn load_turn_compensation(
    connection: &Connection,
    epoch: i64,
    turn: &crate::codex::ingestion::usage_processor::PersistedTurnSnapshot,
) -> StorageResult<(
    Vec<crate::codex::ingestion::usage_processor::CanonicalUsageProposal>,
    Vec<crate::codex::ingestion::usage_processor::Occurrence>,
)> {
    use crate::codex::ingestion::usage_processor::CanonicalUsageProposal;

    let mut statement = connection.prepare(
        "SELECT e.event_id,e.event_kind,e.occurred_at_ms,e.thread_id,e.root_session_id,
                e.turn_key,e.model,e.reasoning_effort,e.input_tokens,e.cached_tokens,
                e.cache_write_tokens,e.output_tokens,e.reasoning_tokens,e.total_tokens
         FROM usage_events e
         WHERE e.source='codex' AND e.source_epoch=?1 AND e.thread_id=?2
           AND e.turn_key=?3 AND e.event_kind='turn_compensation'
         ORDER BY e.event_id",
    )?;
    let mut event_map = BTreeMap::new();
    let mut occurrences = Vec::new();
    for row in statement.query_map(
        params![epoch, turn.owning_thread_id, turn.key.turn_key],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
            ))
        },
    )? {
        let (
            event_id,
            kind,
            occurred,
            thread,
            root,
            event_turn,
            model,
            effort,
            input,
            cached,
            cache_write,
            output,
            reasoning,
            total,
        ) = row?;
        if thread != turn.owning_thread_id
            || event_turn.as_deref() != Some(turn.key.turn_key.as_str())
        {
            return Err(StorageError::usage_conflict(
                "Turn compensation event identity changed",
            ));
        }
        let usage = NormalizedTokenUsage::new(input, cached, cache_write, output, reasoning, total)
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        let proposal = CanonicalUsageProposal {
            event_id: event_id.clone(),
            kind: parse_event_kind(&kind)?,
            occurred_at_ms: occurred,
            thread_id: thread,
            root_session_id: root,
            turn_key: event_turn,
            model,
            reasoning_effort: effort,
            usage,
        };
        validate_proposal_thread_binding(connection, &proposal, &turn.owning_thread_id)?;
        event_map.insert(event_id.clone(), proposal);
        let event_occurrences = load_event_occurrences(connection, epoch, &event_id)?;
        if event_occurrences.is_empty() {
            return Err(StorageError::usage_conflict(
                "Turn compensation event has no physical occurrence",
            ));
        }
        validate_occurrence_sources(connection, &event_occurrences, &turn.owning_thread_id)?;
        occurrences.extend(event_occurrences);
    }
    let events = event_map.into_values().collect();
    Ok((events, occurrences))
}

fn read_persisted_turn_snapshot(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::codex::ingestion::usage_processor::PersistedTurnSnapshot> {
    use crate::codex::ingestion::usage_processor::{
        CompensationBlocks, PersistedTurnKey, PersistedTurnSnapshot, PersistedTurnStatus,
        TurnModelState, TurnReasoningEffortState, TurnState,
    };

    let vector = |start: usize| -> rusqlite::Result<Option<NormalizedTokenUsage>> {
        let Some(input) = row.get::<_, Option<i64>>(start)? else {
            for index in 1..6 {
                if row.get::<_, Option<i64>>(start + index)?.is_some() {
                    return Err(rusqlite::Error::InvalidParameterName(
                        "partial persisted Turn usage vector".to_owned(),
                    ));
                }
            }
            if row.get::<_, Option<Vec<u8>>>(start + 6)?.is_some() {
                return Err(rusqlite::Error::InvalidParameterName(
                    "Turn usage fingerprint without vector".to_owned(),
                ));
            }
            return Ok(None);
        };
        let value = NormalizedTokenUsage::new(
            input,
            row.get(start + 1)?,
            row.get(start + 2)?,
            row.get(start + 3)?,
            row.get(start + 4)?,
            row.get(start + 5)?,
        )
        .map_err(super::to_domain_sql_error)?;
        let fingerprint: Option<Vec<u8>> = row.get(start + 6)?;
        if fingerprint.as_deref()
            != Some(crate::codex::normalization::usage_fingerprint(&value).as_slice())
        {
            return Err(rusqlite::Error::InvalidParameterName(
                "persisted Turn usage fingerprint mismatch".to_owned(),
            ));
        }
        Ok(Some(value))
    };
    let start_total = vector(10)?;
    let last_total = vector(17)?;
    let accounted = vector(24)?.ok_or_else(|| {
        rusqlite::Error::InvalidParameterName("Turn accounted usage is missing".to_owned())
    })?;
    let status_text: String = row.get(9)?;
    let status = match status_text.as_str() {
        "open" => PersistedTurnStatus::Open,
        "completed" => PersistedTurnStatus::Completed,
        "aborted" => PersistedTurnStatus::Aborted,
        "failed" => PersistedTurnStatus::Failed,
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid Turn status".to_owned(),
            ));
        }
    };
    let model_state_text: String = row.get(32)?;
    let model_state_value: Option<String> = row.get(33)?;
    let model_state = match (model_state_text.as_str(), model_state_value) {
        ("none", None) => TurnModelState::None,
        ("single", Some(model)) => TurnModelState::Single(model),
        ("mixed", None) => TurnModelState::Mixed,
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid Turn model state".to_owned(),
            ));
        }
    };
    let effort_state_text: String = row.get(35)?;
    let effort_state_value: Option<String> = row.get(36)?;
    let reasoning_effort_state = match (effort_state_text.as_str(), effort_state_value) {
        ("none", None) => TurnReasoningEffortState::None,
        ("single", Some(effort)) => TurnReasoningEffortState::Single(effort),
        ("mixed", None) => TurnReasoningEffortState::Mixed,
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid Turn reasoning state".to_owned(),
            ));
        }
    };
    let blocks = CompensationBlocks {
        start_missing: row.get::<_, i64>(39)? != 0,
        time_missing: row.get::<_, i64>(40)? != 0,
        reset: row.get::<_, i64>(41)? != 0,
        ownership_gap: row.get::<_, i64>(42)? != 0,
        parser_gap: row.get::<_, i64>(43)? != 0,
        required_invalid: row.get::<_, i64>(44)? != 0,
        model_unresolved: row.get::<_, i64>(45)? != 0,
    };
    if (row.get::<_, i64>(38)? != 0) != blocks.allowed() {
        return Err(rusqlite::Error::InvalidParameterName(
            "persisted Turn compensation state mismatch".to_owned(),
        ));
    }
    let quality_status: String = row.get(46)?;
    if !matches!(quality_status.as_str(), "complete" | "partial" | "conflict") {
        return Err(rusqlite::Error::InvalidParameterName(
            "invalid Turn quality state".to_owned(),
        ));
    }
    let source_file_id: i64 = row.get(0)?;
    let file_generation: i64 = row.get(1)?;
    let turn_key: String = row.get(2)?;
    let thread_id: String = row.get(3)?;
    let state = TurnState {
        turn_key: turn_key.clone(),
        raw_turn_id: row.get(4)?,
        started_at_ms: row.get(5)?,
        start_offset: u64::try_from(row.get::<_, i64>(7)?).map_err(|_| {
            rusqlite::Error::InvalidParameterName("invalid Turn start offset".to_owned())
        })?,
        start_total,
        last_total,
        accounted,
        accounted_candidate_count: u64::try_from(row.get::<_, i64>(31)?).map_err(|_| {
            rusqlite::Error::InvalidParameterName("invalid Turn accounted count".to_owned())
        })?,
        model_state,
        unresolved_model_seen: row.get::<_, i64>(34)? != 0,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen: row.get::<_, i64>(37)? != 0,
        blocks,
    };
    Ok(PersistedTurnSnapshot {
        key: PersistedTurnKey {
            source_file_id,
            file_generation,
            turn_key,
        },
        owning_thread_id: thread_id,
        state,
        status,
        ended_at_ms: row.get(6)?,
        end_offset: row
            .get::<_, Option<i64>>(8)?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| {
                rusqlite::Error::InvalidParameterName("invalid Turn end offset".to_owned())
            })?,
        quality_status,
        state_through_offset: u64::try_from(row.get::<_, i64>(47)?).map_err(|_| {
            rusqlite::Error::InvalidParameterName("invalid Turn state offset".to_owned())
        })?,
    })
}

fn parse_event_kind(value: &str) -> StorageResult<EventKind> {
    match value {
        "normal" => Ok(EventKind::Normal),
        "recovered" => Ok(EventKind::Recovered),
        "turn_compensation" => Ok(EventKind::TurnCompensation),
        _ => Err(StorageError::invalid_state(
            "invalid canonical usage event kind",
        )),
    }
}

fn parse_evidence_kind(value: &str) -> StorageResult<EvidenceKind> {
    match value {
        "explicit" => Ok(EvidenceKind::Explicit),
        "legacy" => Ok(EvidenceKind::Legacy),
        _ => Err(StorageError::invalid_state("invalid usage evidence kind")),
    }
}

fn parse_codex_operation(value: &str) -> StorageResult<CodexOperation> {
    match value {
        "response" => Ok(CodexOperation::Response),
        "compaction" => Ok(CodexOperation::Compaction),
        _ => Err(StorageError::invalid_state("invalid Codex usage operation")),
    }
}

fn parse_marker_unknown_reason(
    value: &str,
) -> StorageResult<crate::codex::ingestion::usage_processor::MarkerUnknownReason> {
    use crate::codex::ingestion::usage_processor::MarkerUnknownReason;

    match value {
        "usage_missing" => Ok(MarkerUnknownReason::UsageMissing),
        "identity_missing" => Ok(MarkerUnknownReason::IdentityMissing),
        "usage_invalid" => Ok(MarkerUnknownReason::UsageInvalid),
        "time_missing" => Ok(MarkerUnknownReason::TimeMissing),
        "model_unresolved" => Ok(MarkerUnknownReason::ModelUnresolved),
        _ => Err(StorageError::invalid_state(
            "invalid Compaction marker reason",
        )),
    }
}

fn compute_reconciliation_context_fingerprint(
    connection: &Connection,
    epoch: i64,
    context: &crate::codex::ingestion::usage_processor::UsageContext,
    frozen: &UsageReconciliationContext,
) -> StorageResult<Vec<u8>> {
    use crate::codex::ingestion::usage_processor::{
        PersistedTurnStatus, TurnModelState, TurnReasoningEffortState,
    };

    let reconciliation = &frozen.context;
    let request = &reconciliation.request;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"codex-reconciliation-context-v1\0");
    hash_i64(&mut hasher, epoch);
    hash_i64(&mut hasher, context.source_file_id);
    hash_i64(&mut hasher, context.file_generation);
    hash_text(&mut hasher, &context.owning_thread_id);
    hash_text(&mut hasher, &context.root_session_id);
    hash_len(&mut hasher, request.response_keys.len());
    for key in &request.response_keys {
        hash_text(&mut hasher, &key.owning_thread_id);
        hash_text(&mut hasher, &key.response_id);
        if let Some(binding) = reconciliation.bindings.get(key) {
            hasher.update(&[1]);
            hash_proposal(&mut hasher, &binding.proposal);
            hash_fact(&mut hasher, &binding.fact);
        } else {
            hasher.update(&[0]);
        }
    }
    hash_len(&mut hasher, frozen.closure_response_keys.len());
    for key in &frozen.closure_response_keys {
        hash_text(&mut hasher, &key.owning_thread_id);
        hash_text(&mut hasher, &key.response_id);
        if let Some(binding) = reconciliation.bindings.get(key) {
            hasher.update(&[1]);
            hash_proposal(&mut hasher, &binding.proposal);
            hash_fact(&mut hasher, &binding.fact);
        } else {
            hasher.update(&[0]);
        }
        if let Some(occurrences) = frozen.response_occurrences.get(key) {
            hasher.update(&[1]);
            hash_len(&mut hasher, occurrences.len());
            for occurrence in occurrences {
                hash_occurrence(&mut hasher, occurrence);
            }
        } else {
            hasher.update(&[0]);
        }
    }
    let mut markers = reconciliation.markers.iter().collect::<Vec<_>>();
    markers.sort_by_key(|marker| {
        (
            marker.source_file_id,
            marker.file_generation,
            marker.source_start_offset,
        )
    });
    hash_len(&mut hasher, markers.len());
    for marker in markers {
        hash_i64(&mut hasher, marker.source_file_id);
        hash_i64(&mut hasher, marker.file_generation);
        hash_u64(&mut hasher, marker.source_start_offset);
        hash_u64(&mut hasher, marker.source_end_offset);
        hash_text(&mut hasher, &marker.owning_thread_id);
        hash_text(&mut hasher, &marker.root_session_id);
        hash_opt_i64(&mut hasher, marker.occurred_at_ms);
        hash_opt_text(&mut hasher, marker.model.as_deref());
        hash_opt_text(&mut hasher, marker.reasoning_effort.as_deref());
        hash_opt_text(&mut hasher, marker.response_id.as_deref());
        hash_opt_text(&mut hasher, marker.resolved_event_id.as_deref());
        hash_opt_text(
            &mut hasher,
            marker.unknown_reason.map(marker_unknown_reason_str),
        );
    }
    hash_len(&mut hasher, reconciliation.windows.len());
    for (key, window) in &reconciliation.windows {
        hash_i64(&mut hasher, key.source_file_id);
        hash_i64(&mut hasher, key.file_generation);
        hash_u64(&mut hasher, key.start_offset);
        let json = window
            .to_json()
            .map_err(|_| StorageError::invalid_state("invalid context window"))?;
        hash_text(&mut hasher, &json);
        if let Some(metadata) = frozen.window_metadata.get(key) {
            hasher.update(&[1]);
            hash_i64(&mut hasher, metadata.source_file_id);
            hash_i64(&mut hasher, metadata.file_generation);
            hash_u64(&mut hasher, metadata.source_start_offset);
            hash_u64(&mut hasher, metadata.source_end_offset);
            hash_text(&mut hasher, &metadata.owning_thread_id);
            hash_opt_text(&mut hasher, metadata.turn_key.as_deref());
        } else {
            return Err(StorageError::usage_conflict(
                "reconciliation context omitted physical window metadata",
            ));
        }
        let proposals = frozen.window_proposals.get(key).ok_or_else(|| {
            StorageError::usage_conflict("reconciliation context omitted window proposal closure")
        })?;
        hash_len(&mut hasher, proposals.len());
        for binding in proposals {
            hash_proposal(&mut hasher, &binding.proposal);
            hash_fact(&mut hasher, &binding.fact);
            hash_len(&mut hasher, binding.occurrences.len());
            for occurrence in &binding.occurrences {
                hash_occurrence(&mut hasher, occurrence);
            }
        }
    }
    hash_len(&mut hasher, request.owning_turn_keys.len());
    for (thread_id, turn_key) in &request.owning_turn_keys {
        hash_text(&mut hasher, thread_id);
        hash_opt_text(&mut hasher, turn_key.as_deref());
        let turns = reconciliation
            .affected_turns
            .iter()
            .filter(|(_, affected)| {
                affected.snapshot.owning_thread_id == *thread_id
                    && affected.snapshot.key.turn_key.as_str() == turn_key.as_deref().unwrap_or("")
            })
            .collect::<Vec<_>>();
        hash_len(&mut hasher, turns.len());
        for (key, affected) in turns {
            hash_i64(&mut hasher, key.source_file_id);
            hash_i64(&mut hasher, key.file_generation);
            hash_text(&mut hasher, &key.turn_key);
            let snapshot = &affected.snapshot;
            hash_text(&mut hasher, &snapshot.owning_thread_id);
            hash_text(&mut hasher, &snapshot.state.turn_key);
            hash_opt_text(&mut hasher, snapshot.state.raw_turn_id.as_deref());
            hash_opt_i64(&mut hasher, snapshot.state.started_at_ms);
            hash_u64(&mut hasher, snapshot.state.start_offset);
            hash_optional_usage(&mut hasher, snapshot.state.start_total.as_ref());
            hash_optional_usage(&mut hasher, snapshot.state.last_total.as_ref());
            hash_usage(&mut hasher, &snapshot.state.accounted);
            hash_u64(&mut hasher, snapshot.state.accounted_candidate_count);
            match &snapshot.state.model_state {
                TurnModelState::None => {
                    hasher.update(&[0]);
                }
                TurnModelState::Single(model) => {
                    hasher.update(&[1]);
                    hash_text(&mut hasher, model);
                }
                TurnModelState::Mixed => {
                    hasher.update(&[2]);
                }
            }
            hasher.update(&[u8::from(snapshot.state.unresolved_model_seen)]);
            match &snapshot.state.reasoning_effort_state {
                TurnReasoningEffortState::None => {
                    hasher.update(&[0]);
                }
                TurnReasoningEffortState::Single(effort) => {
                    hasher.update(&[1]);
                    hash_text(&mut hasher, effort);
                }
                TurnReasoningEffortState::Mixed => {
                    hasher.update(&[2]);
                }
            }
            hasher.update(&[u8::from(snapshot.state.unresolved_reasoning_effort_seen)]);
            let blocks = snapshot.state.blocks;
            for flag in [
                blocks.start_missing,
                blocks.time_missing,
                blocks.reset,
                blocks.ownership_gap,
                blocks.parser_gap,
                blocks.required_invalid,
                blocks.model_unresolved,
            ] {
                hasher.update(&[u8::from(flag)]);
            }
            hasher.update(&[match snapshot.status {
                PersistedTurnStatus::Open => 0,
                PersistedTurnStatus::Completed => 1,
                PersistedTurnStatus::Aborted => 2,
                PersistedTurnStatus::Failed => 3,
            }]);
            hash_opt_i64(&mut hasher, snapshot.ended_at_ms);
            hash_opt_u64(&mut hasher, snapshot.end_offset);
            hash_text(&mut hasher, &snapshot.quality_status);
            hash_u64(&mut hasher, snapshot.state_through_offset);
            hash_len(&mut hasher, affected.compensation_events.len());
            for event in &affected.compensation_events {
                hash_proposal(&mut hasher, event);
            }
            hash_len(&mut hasher, affected.compensation_occurrences.len());
            for occurrence in &affected.compensation_occurrences {
                hash_i64(&mut hasher, occurrence.source_file_id);
                hash_i64(&mut hasher, occurrence.file_generation);
                hash_u64(&mut hasher, occurrence.source_start_offset);
                hash_u64(&mut hasher, occurrence.source_end_offset);
                hash_text(&mut hasher, &occurrence.event_id);
            }
        }
    }
    append_context_metadata_fingerprint(connection, epoch, context, frozen, &mut hasher)?;
    Ok(hasher.finalize().as_bytes().to_vec())
}

fn append_context_metadata_fingerprint(
    connection: &Connection,
    epoch: i64,
    context: &crate::codex::ingestion::usage_processor::UsageContext,
    frozen: &UsageReconciliationContext,
    hasher: &mut blake3::Hasher,
) -> StorageResult<()> {
    let reconciliation = &frozen.context;
    let mut source_ids = BTreeSet::from([context.source_file_id]);
    let mut thread_ids = BTreeSet::from([
        context.owning_thread_id.clone(),
        context.root_session_id.clone(),
    ]);
    for key in &reconciliation.request.response_keys {
        thread_ids.insert(key.owning_thread_id.clone());
    }
    for key in &frozen.closure_response_keys {
        thread_ids.insert(key.owning_thread_id.clone());
    }
    for marker in &reconciliation.markers {
        source_ids.insert(marker.source_file_id);
        thread_ids.insert(marker.owning_thread_id.clone());
        thread_ids.insert(marker.root_session_id.clone());
    }
    for key in reconciliation.windows.keys() {
        source_ids.insert(key.source_file_id);
    }
    for affected in reconciliation.affected_turns.values() {
        source_ids.insert(affected.snapshot.key.source_file_id);
        thread_ids.insert(affected.snapshot.owning_thread_id.clone());
        for occurrence in &affected.compensation_occurrences {
            source_ids.insert(occurrence.source_file_id);
        }
        for event in &affected.compensation_events {
            thread_ids.insert(event.thread_id.clone());
            thread_ids.insert(event.root_session_id.clone());
        }
    }
    for binding in reconciliation.bindings.values() {
        thread_ids.insert(binding.proposal.thread_id.clone());
        thread_ids.insert(binding.proposal.root_session_id.clone());
    }
    for proposals in frozen.window_proposals.values() {
        for binding in proposals {
            thread_ids.insert(binding.proposal.thread_id.clone());
            thread_ids.insert(binding.proposal.root_session_id.clone());
            for occurrence in &binding.occurrences {
                source_ids.insert(occurrence.source_file_id);
            }
        }
    }
    for occurrences in frozen.response_occurrences.values() {
        for occurrence in occurrences {
            source_ids.insert(occurrence.source_file_id);
        }
    }
    for metadata in frozen.window_metadata.values() {
        source_ids.insert(metadata.source_file_id);
        thread_ids.insert(metadata.owning_thread_id.clone());
    }
    let mut referenced_event_ids = reconciliation
        .bindings
        .values()
        .map(|binding| binding.proposal.event_id.clone())
        .collect::<BTreeSet<_>>();
    for proposals in frozen.window_proposals.values() {
        referenced_event_ids.extend(
            proposals
                .iter()
                .map(|binding| binding.proposal.event_id.clone()),
        );
    }
    for affected in reconciliation.affected_turns.values() {
        referenced_event_ids.extend(
            affected
                .compensation_events
                .iter()
                .map(|event| event.event_id.clone()),
        );
    }
    hasher.update(b"reconciliation-event-holds-v1\0");
    hash_len(hasher, referenced_event_ids.len());
    for event_id in referenced_event_ids {
        hash_text(hasher, &event_id);
        let holds = load_event_holds(connection, epoch, &event_id)?;
        hash_len(hasher, holds.len());
        for hold in holds {
            hash_i64(hasher, hold.source_file_id);
            hash_i64(hasher, hold.file_generation);
            hash_text(hasher, &hold.event_id);
            hash_text(hasher, &hold.hold_reason);
            source_ids.insert(hold.source_file_id);
        }
    }
    for source_id in source_ids {
        hash_i64(hasher, source_id);
        append_query_rows(
            connection,
            hasher,
            b"codex_source_files",
            "SELECT source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status
             FROM codex_source_files WHERE source_file_id=?1",
            params![source_id],
        )?;
        append_query_rows(
            connection,
            hasher,
            b"codex_rollout_metadata_facts",
            "SELECT source_file_id,file_generation,metadata_parser_version,resolved_through_offset,
                    owning_thread_id,continuation_state,cwd,cwd_provenance,cwd_record_offset,
                    latest_context_model,latest_context_at_ms,parent_thread_id_hint,
                    parent_hint_provenance,parent_hint_record_offset,agent_role_hint,
                    agent_role_provenance,agent_role_record_offset,replay_start_offset,
                    owning_records_start_offset,ownership_confidence,fact_quality_status
             FROM codex_rollout_metadata_facts WHERE source_file_id=?1",
            params![source_id],
        )?;
    }
    for thread_id in thread_ids {
        hash_text(hasher, &thread_id);
        append_query_rows(
            connection,
            hasher,
            b"threads",
            "SELECT thread_id,source,native_session_id,parent_thread_id,root_session_id,
                    agent_role,metadata_model,metadata_quality_status
             FROM threads WHERE thread_id=?1",
            params![thread_id],
        )?;
    }
    Ok(())
}

fn append_query_rows<P: rusqlite::Params>(
    connection: &Connection,
    hasher: &mut blake3::Hasher,
    table_tag: &[u8],
    sql: &str,
    params: P,
) -> StorageResult<()> {
    let mut statement = connection.prepare(sql)?;
    hasher.update(&(table_tag.len() as u64).to_be_bytes());
    hasher.update(table_tag);
    let column_count = statement.column_count() as u64;
    let mut rows = statement.query(params)?;
    while let Some(row) = rows.next()? {
        hasher.update(&[0xff]);
        hasher.update(&column_count.to_be_bytes());
        for index in 0..column_count as usize {
            match row.get_ref(index)? {
                rusqlite::types::ValueRef::Null => {
                    hasher.update(&[0]);
                }
                rusqlite::types::ValueRef::Integer(value) => {
                    hasher.update(&[1]);
                    hasher.update(&value.to_be_bytes());
                }
                rusqlite::types::ValueRef::Real(value) => {
                    hasher.update(&[2]);
                    hasher.update(&value.to_bits().to_be_bytes());
                }
                rusqlite::types::ValueRef::Text(value) => {
                    hasher.update(&[3]);
                    hasher.update(&(value.len() as u64).to_be_bytes());
                    hasher.update(value);
                }
                rusqlite::types::ValueRef::Blob(value) => {
                    hasher.update(&[4]);
                    hasher.update(&(value.len() as u64).to_be_bytes());
                    hasher.update(value);
                }
            };
        }
    }
    hasher.update(&[0xfe]);
    Ok(())
}

pub(super) fn append_usage_source_private_proof(
    connection: &Connection,
    epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    hasher: &mut blake3::Hasher,
) -> StorageResult<()> {
    hasher.update(b"usage-source-private-evidence-v1\0");
    hash_i64(hasher, epoch);
    hash_i64(hasher, source_file_id);
    hash_i64(hasher, file_generation);
    append_query_rows(
        connection,
        hasher,
        b"codex_usage_event_occurrences",
        "SELECT source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                source_end_offset,event_id
         FROM codex_usage_event_occurrences
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
         ORDER BY source_file_id,file_generation,source_start_offset",
        params![epoch, source_file_id, file_generation],
    )?;
    append_query_rows(
        connection,
        hasher,
        b"codex_usage_event_facts",
        "SELECT f.source,f.ledger_epoch,f.event_id,f.owning_thread_id,f.response_id,
                f.evidence_kind,f.operation
         FROM codex_usage_event_facts f
         WHERE f.source='codex' AND f.ledger_epoch=?1 AND (
             EXISTS(SELECT 1 FROM codex_usage_event_occurrences o
                    WHERE o.source=f.source AND o.ledger_epoch=f.ledger_epoch
                      AND o.source_file_id=?2 AND o.file_generation=?3 AND o.event_id=f.event_id)
             OR EXISTS(SELECT 1 FROM codex_compaction_markers m
                       WHERE m.source=f.source AND m.ledger_epoch=f.ledger_epoch
                         AND m.source_file_id=?2 AND m.file_generation=?3
                         AND m.resolved_event_id=f.event_id)
             OR EXISTS(SELECT 1 FROM codex_turns t JOIN usage_events e
                       ON e.source='codex' AND e.source_epoch=t.ledger_epoch
                         AND e.thread_id=t.thread_id AND e.turn_key=t.turn_key
                       WHERE t.ledger_epoch=f.ledger_epoch AND t.source_file_id=?2
                         AND t.file_generation=?3 AND e.event_kind='turn_compensation'
                         AND e.event_id=f.event_id))
         ORDER BY f.event_id",
        params![epoch, source_file_id, file_generation],
    )?;
    let mut windows = connection.prepare(
        "SELECT state_json FROM codex_usage_reconciliation_windows
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
         ORDER BY source_file_id,file_generation,source_start_offset",
    )?;
    for row in windows.query_map(params![epoch, source_file_id, file_generation], |row| {
        row.get::<_, String>(0)
    })? {
        canonical_window_state(&row?)?;
    }
    drop(windows);
    append_query_rows(
        connection,
        hasher,
        b"codex_compaction_markers",
        "SELECT source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                reasoning_effort,response_id,resolved_event_id,unknown_reason
         FROM codex_compaction_markers
         WHERE source='codex' AND ledger_epoch=?1 AND (
             (source_file_id=?2 AND file_generation=?3)
             OR resolved_event_id IN (SELECT event_id FROM codex_usage_event_occurrences
                 WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3))
         ORDER BY source_file_id,file_generation,source_start_offset",
        params![epoch,source_file_id,file_generation],
    )?;
    append_query_rows(
        connection,
        hasher,
        b"codex_usage_reconciliation_windows",
        "SELECT source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                source_end_offset,owning_thread_id,turn_key,state_json
         FROM codex_usage_reconciliation_windows
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
         ORDER BY source_file_id,file_generation,source_start_offset",
        params![epoch, source_file_id, file_generation],
    )?;
    append_query_rows(
        connection,
        hasher,
        b"codex_usage_event_holds",
        "SELECT source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason
         FROM codex_usage_event_holds
         WHERE source='codex' AND ledger_epoch=?1 AND (
             (source_file_id=?2 AND file_generation=?3)
             OR event_id IN (SELECT event_id FROM codex_usage_event_occurrences
                 WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3))
         ORDER BY source_file_id,file_generation,event_id",
        params![epoch,source_file_id,file_generation],
    )?;
    append_query_rows(
        connection,
        hasher,
        b"codex_turns",
        "SELECT ledger_epoch,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,
                started_at_ms,ended_at_ms,start_offset,end_offset,status,
                start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
                start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,
                start_total_fingerprint,last_total_input_tokens,last_total_cached_tokens,
                last_total_cache_write_tokens,last_total_output_tokens,last_total_reasoning_tokens,
                last_total_total_tokens,last_total_fingerprint,accounted_input_tokens,
                accounted_cached_tokens,accounted_cache_write_tokens,accounted_output_tokens,
                accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
                accounted_candidate_count,model_state,single_model,unresolved_model_seen,
                reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
                compensation_allowed,block_start_missing,block_time_missing,block_reset,
                block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,
                quality_status,state_through_offset
         FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
         ORDER BY source_file_id,file_generation,turn_key",
        params![epoch, source_file_id, file_generation],
    )?;
    append_query_rows(
        connection,
        hasher,
        b"turn_compensation_events",
        "SELECT e.source,e.source_epoch,e.event_id,e.event_kind,e.occurred_at_ms,e.thread_id,
                e.root_session_id,e.turn_key,e.model,e.reasoning_effort,e.estimated_cost_nanos_usd,
                e.input_tokens,e.cached_tokens,e.cache_write_tokens,e.output_tokens,
                e.reasoning_tokens,e.total_tokens,e.quality_status
         FROM usage_events e
         WHERE e.source='codex' AND e.source_epoch=?1 AND e.event_kind='turn_compensation'
           AND EXISTS(SELECT 1 FROM codex_turns t
                      WHERE t.ledger_epoch=e.source_epoch AND t.source_file_id=?2
                        AND t.file_generation=?3 AND t.thread_id=e.thread_id AND t.turn_key=e.turn_key)
         ORDER BY e.event_id",
        params![epoch,source_file_id,file_generation],
    )?;
    append_query_rows(
        connection,
        hasher,
        b"turn_compensation_occurrences",
        "SELECT o.source,o.ledger_epoch,o.source_file_id,o.file_generation,o.source_start_offset,
                o.source_end_offset,o.event_id
         FROM codex_usage_event_occurrences o JOIN usage_events e
           ON e.source=o.source AND e.source_epoch=o.ledger_epoch AND e.event_id=o.event_id
         WHERE o.source='codex' AND o.ledger_epoch=?1 AND e.event_kind='turn_compensation'
           AND EXISTS(SELECT 1 FROM codex_turns t
                      WHERE t.ledger_epoch=e.source_epoch AND t.source_file_id=?2
                        AND t.file_generation=?3 AND t.thread_id=e.thread_id AND t.turn_key=e.turn_key)
         ORDER BY o.source_file_id,o.file_generation,o.source_start_offset,o.event_id",
        params![epoch,source_file_id,file_generation],
    )?;
    Ok(())
}

fn hash_len(hasher: &mut blake3::Hasher, len: usize) {
    hasher.update(&(len as u64).to_be_bytes());
}

fn hash_text(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(&[1]);
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

fn hash_opt_text(hasher: &mut blake3::Hasher, value: Option<&str>) {
    if let Some(value) = value {
        hasher.update(&[1]);
        hash_text(hasher, value);
    } else {
        hasher.update(&[0]);
    }
}

fn hash_i64(hasher: &mut blake3::Hasher, value: i64) {
    hasher.update(&[1]);
    hasher.update(&value.to_be_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&[1]);
    hasher.update(&value.to_be_bytes());
}

fn hash_opt_i64(hasher: &mut blake3::Hasher, value: Option<i64>) {
    if let Some(value) = value {
        hasher.update(&[1]);
        hasher.update(&value.to_be_bytes());
    } else {
        hasher.update(&[0]);
    }
}

fn hash_opt_u64(hasher: &mut blake3::Hasher, value: Option<u64>) {
    if let Some(value) = value {
        hasher.update(&[1]);
        hasher.update(&value.to_be_bytes());
    } else {
        hasher.update(&[0]);
    }
}

fn hash_usage(hasher: &mut blake3::Hasher, usage: &NormalizedTokenUsage) {
    hash_i64(hasher, usage.input_tokens);
    hash_i64(hasher, usage.cached_tokens);
    hash_opt_i64(hasher, usage.cache_write_tokens);
    hash_i64(hasher, usage.output_tokens);
    hash_i64(hasher, usage.reasoning_tokens);
    hash_i64(hasher, usage.total_tokens);
}

fn hash_optional_usage(hasher: &mut blake3::Hasher, usage: Option<&NormalizedTokenUsage>) {
    if let Some(usage) = usage {
        hasher.update(&[1]);
        hash_usage(hasher, usage);
    } else {
        hasher.update(&[0]);
    }
}

fn hash_proposal(
    hasher: &mut blake3::Hasher,
    proposal: &crate::codex::ingestion::usage_processor::CanonicalUsageProposal,
) {
    hash_text(hasher, &proposal.event_id);
    hasher.update(&[match proposal.kind {
        EventKind::Normal => 0,
        EventKind::Recovered => 1,
        EventKind::TurnCompensation => 2,
    }]);
    hash_i64(hasher, proposal.occurred_at_ms);
    hash_text(hasher, &proposal.thread_id);
    hash_text(hasher, &proposal.root_session_id);
    hash_opt_text(hasher, proposal.turn_key.as_deref());
    hash_text(hasher, &proposal.model);
    hash_opt_text(hasher, proposal.reasoning_effort.as_deref());
    hash_usage(hasher, &proposal.usage);
}

fn hash_occurrence(
    hasher: &mut blake3::Hasher,
    occurrence: &crate::codex::ingestion::usage_processor::Occurrence,
) {
    hash_i64(hasher, occurrence.source_file_id);
    hash_i64(hasher, occurrence.file_generation);
    hash_u64(hasher, occurrence.source_start_offset);
    hash_u64(hasher, occurrence.source_end_offset);
    hash_text(hasher, &occurrence.event_id);
}

fn hash_fact(
    hasher: &mut blake3::Hasher,
    fact: &crate::codex::ingestion::usage_processor::UsageEventFact,
) {
    hash_text(hasher, &fact.event_id);
    hash_text(hasher, &fact.owning_thread_id);
    hash_opt_text(hasher, fact.response_id.as_deref());
    hasher.update(&[match fact.evidence_kind {
        EvidenceKind::Explicit => 0,
        EvidenceKind::Legacy => 1,
    }]);
    hasher.update(&[match fact.operation {
        CodexOperation::Response => 0,
        CodexOperation::Compaction => 1,
    }]);
}

fn marker_unknown_reason_str(
    reason: crate::codex::ingestion::usage_processor::MarkerUnknownReason,
) -> &'static str {
    use crate::codex::ingestion::usage_processor::MarkerUnknownReason;

    match reason {
        MarkerUnknownReason::UsageMissing => "usage_missing",
        MarkerUnknownReason::IdentityMissing => "identity_missing",
        MarkerUnknownReason::UsageInvalid => "usage_invalid",
        MarkerUnknownReason::TimeMissing => "time_missing",
        MarkerUnknownReason::ModelUnresolved => "model_unresolved",
    }
}

pub(super) fn commit_group(
    storage: &CodexStorage<'_>,
    batch: UsageCommitBatch,
) -> Result<UsageCommitOutcome, CodexStorageError> {
    validate_batch(&batch).map_err(CodexStorageError::from)?;
    let mut tx = storage.begin_write_txn()?;
    let bridge = apply_usage_batch(&mut tx, &batch)?;
    if bridge.visible_changed {
        tx.bump_data_revision()?;
    }
    let data_revision = tx.with_private_state(|connection| {
        connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(CodexStorageError::from)
    })?;
    tx.commit()?;
    Ok(UsageCommitOutcome {
        data_revision,
        ..bridge.outcome
    })
}

pub(super) fn begin_carry(
    storage: &CodexStorage<'_>,
    source_file_id: i64,
    now_ms: i64,
) -> Result<(), CodexStorageError> {
    let mut tx = storage.begin_write_txn()?;
    let build_epoch = tx.usage_epoch_state()?.build_epoch.ok_or_else(|| {
        CodexStorageError::Storage(StorageError::invalid_state("usage carry requires a build"))
    })?;
    tx.with_private_state(|connection| {
        begin_usage_carry(connection, source_file_id, now_ms).map_err(CodexStorageError::from)
    })?;
    crate::codex::storage::rebuild::delete_orphan_build_events(&mut tx, build_epoch)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn resume_carry(
    storage: &CodexStorage<'_>,
    source_file_id: i64,
    now_ms: i64,
) -> Result<CarryStepOutcome, CodexStorageError> {
    let mut tx = storage.begin_write_txn()?;
    let outcome = resume_usage_carry(&mut tx, source_file_id, now_ms)?;
    tx.commit()?;
    Ok(outcome)
}

pub(super) fn complete_only(
    storage: &CodexStorage<'_>,
    source_file_id: i64,
    now_ms: i64,
) -> Result<(), CodexStorageError> {
    let mut tx = storage.begin_write_txn()?;
    tx.with_private_state(|connection| {
        complete_usage_build_source(connection, source_file_id, now_ms)
            .map_err(CodexStorageError::from)
    })?;
    tx.commit()?;
    Ok(())
}

pub(super) fn cleanup_inactive(
    storage: &CodexStorage<'_>,
    max_rows: usize,
) -> Result<usize, CodexStorageError> {
    if max_rows == 0 {
        return Ok(0);
    }
    let mut tx = storage.begin_write_txn()?;
    // Keep the historical first-non-zero phase order.  Canonical event rows
    // are the only phase that crosses the source transaction seam.
    let deleted = tx.with_private_state(|connection| {
        cleanup_private_phases(connection, max_rows, 0, 6).map_err(CodexStorageError::from)
    })?;
    if deleted > 0 {
        tx.commit()?;
        return Ok(deleted);
    }
    let epoch = tx.usage_epoch_state()?;
    let excluded_build = epoch.build_epoch.unwrap_or(-1);
    let inactive_epochs = tx.with_private_state(|connection| {
        let mut statement = connection.prepare(
            "SELECT DISTINCT source_epoch FROM usage_events
             WHERE source='codex' AND source_epoch<>?1 AND source_epoch<>?2
             ORDER BY source_epoch",
        )?;
        statement
            .query_map(params![epoch.active_epoch, excluded_build], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    })?;
    for inactive_epoch in inactive_epochs {
        let remaining = max_rows;
        let ids = tx.with_private_state(|connection| {
            let mut statement = connection.prepare(
                "SELECT e.event_id FROM usage_events e
                 WHERE e.source='codex' AND e.source_epoch=?1
                   AND NOT EXISTS (
                     SELECT 1 FROM codex_usage_event_occurrences o
                     WHERE o.source='codex' AND o.ledger_epoch=e.source_epoch
                       AND o.event_id=e.event_id)
                   AND NOT EXISTS (
                     SELECT 1 FROM codex_compaction_markers m
                     WHERE m.source='codex' AND m.ledger_epoch=e.source_epoch
                       AND m.resolved_event_id=e.event_id)
                   AND NOT EXISTS (
                     SELECT 1 FROM codex_usage_event_holds h
                     WHERE h.source='codex' AND h.ledger_epoch=e.source_epoch
                       AND h.event_id=e.event_id)
                 ORDER BY e.rowid LIMIT ?2",
            )?;
            statement
                .query_map(
                    params![inactive_epoch, i64::try_from(remaining).unwrap_or(i64::MAX)],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StorageError::from)
        })?;
        if ids.is_empty() {
            continue;
        }
        tx.with_private_state(|connection| {
            strip_orphan_window_references(connection, inactive_epoch, &ids)
                .map_err(CodexStorageError::from)
        })?;
        let count = tx.delete_inactive_usage_events_no_revision(inactive_epoch, &ids)?;
        if count > 0 {
            tx.commit()?;
            return Ok(count);
        }
    }
    let deleted = tx.with_private_state(|connection| {
        cleanup_private_phases(connection, max_rows, 6, 10).map_err(CodexStorageError::from)
    })?;
    tx.commit()?;
    Ok(deleted)
}

fn apply_usage_batch(
    tx: &mut CodexWriteTxn<'_>,
    batch: &UsageCommitBatch,
) -> Result<CodexUsageCommitBridgeResult, CodexStorageError> {
    validate_batch(batch).map_err(CodexStorageError::from)?;
    let epoch = tx.with_private_state(|transaction| {
        read_epoch(transaction).map_err(CodexStorageError::from)
    })?;
    if batch.ledger_epoch != epoch.working_epoch()
        || batch.usage_parser_version != epoch.working_parser_version()
        || canonical_algorithm_for(batch.usage_parser_version).is_none()
    {
        return Err(CodexStorageError::Storage(StorageError::invalid_state(
            "usage working epoch or parser changed",
        )));
    }
    tx.with_private_state(|transaction| {
        validate_group_relationship(transaction, &batch.thread_id, &batch.root_session_id)
            .map_err(CodexStorageError::from)
    })?;
    let canonical_before = tx.with_private_state(|transaction| {
        capture_affected_canonical_visibility(transaction, batch).map_err(CodexStorageError::from)
    })?;
    let compaction_owners = std::iter::once(batch.thread_id.clone())
        .chain(batch.sources.iter().filter_map(|source| {
            source
                .expected_state
                .as_ref()
                .map(|state| state.owning_thread_id.clone())
        }))
        .collect::<BTreeSet<_>>();
    let compaction_before = if batch.ledger_epoch == epoch.active_epoch {
        Some(tx.with_private_state(|transaction| {
            let mut signatures = BTreeMap::new();
            for owner in &compaction_owners {
                signatures.insert(
                    owner.clone(),
                    crate::codex::analytics::compaction_visibility_signature_for_owner(
                        transaction,
                        batch.ledger_epoch,
                        epoch.active_parser_version,
                        owner,
                    )
                    .map_err(CodexStorageError::from)?,
                );
            }
            Ok::<_, CodexStorageError>(signatures)
        })?)
    } else {
        None
    };
    let skills_before = tx.with_private_state(|transaction| {
        capture_skill_visibility(transaction, batch).map_err(CodexStorageError::from)
    })?;
    let has_local_replay = batch.sources.iter().any(|source| source.local_replay);

    let mut inserted = 0usize;
    let mut deduplicated = 0usize;
    for source in &batch.sources {
        tx.with_private_state(|transaction| {
            validate_source_preconditions(transaction, batch, source)
                .map_err(CodexStorageError::from)?;
            validate_reconciliation_context(transaction, batch, source)
                .map_err(CodexStorageError::from)?;
            if source.local_replay {
                prepare_local_replay(transaction, batch, source)
                    .map_err(CodexStorageError::from)?;
            }
            Ok::<_, CodexStorageError>(())
        })?;
        let target = if batch.ledger_epoch == epoch.active_epoch {
            UsageWriteTarget::Active
        } else {
            UsageWriteTarget::Build
        };
        let (source_inserted, source_deduplicated) =
            apply_reconciliation_patch(tx, batch, source, target)?;
        inserted += source_inserted;
        deduplicated += source_deduplicated;
        tx.with_private_state(|transaction| {
            for skill in &source.skill_events {
                write_or_compare_skill_event(transaction, batch.ledger_epoch, source, skill)
                    .map_err(CodexStorageError::from)?;
            }
            write_source_state(transaction, batch, source).map_err(CodexStorageError::from)?;
            write_usage_checkpoint(transaction, batch, source).map_err(CodexStorageError::from)?;
            update_build_progress(transaction, epoch.clone(), batch, source)
                .map_err(CodexStorageError::from)?;
            verify_source_postconditions(transaction, batch, source)
                .map_err(CodexStorageError::from)?;
            if source.local_replay {
                transaction.execute(
                    "DELETE FROM codex_usage_event_holds
                     WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
                       AND file_generation=?3 AND hold_reason='replay'",
                    params![
                        batch.ledger_epoch,
                        source.source_file_id,
                        source.expected_file_generation
                    ],
                )?;
            }
            Ok::<_, CodexStorageError>(())
        })?;
    }
    if has_local_replay {
        let orphan_ids = tx.with_private_state(|transaction| {
            local_replay_orphan_ids(transaction, batch.ledger_epoch)
                .map_err(CodexStorageError::from)
        })?;
        let target = if batch.ledger_epoch == epoch.active_epoch {
            UsageWriteTarget::Active
        } else {
            UsageWriteTarget::Build
        };
        tx.delete_usage_events_no_revision(target, &orphan_ids)?;
    }
    let token_visibility_changed = tx.with_private_state(|transaction| {
        affected_canonical_visibility_changed(transaction, batch.ledger_epoch, &canonical_before)
            .map_err(CodexStorageError::from)
    })?;
    let skill_visibility_changed = tx.with_private_state(|transaction| {
        affected_skill_visibility_changed(transaction, batch.ledger_epoch, &skills_before)
            .map_err(CodexStorageError::from)
    })?;
    let compaction_visibility_changed = match compaction_before {
        Some(before) => {
            let after = tx.with_private_state(|transaction| {
                let mut signatures = BTreeMap::new();
                for owner in &compaction_owners {
                    signatures.insert(
                        owner.clone(),
                        crate::codex::analytics::compaction_visibility_signature_for_owner(
                            transaction,
                            batch.ledger_epoch,
                            epoch.active_parser_version,
                            owner,
                        )
                        .map_err(CodexStorageError::from)?,
                    );
                }
                Ok::<_, CodexStorageError>(signatures)
            })?;
            before != after
        }
        None => false,
    };
    let canonical_changed =
        token_visibility_changed || skill_visibility_changed || compaction_visibility_changed;

    let active_epoch = epoch.active_epoch;
    let current_revision: i64 = tx.with_private_state(|transaction| {
        transaction
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(CodexStorageError::from)
    })?;
    let data_revision = current_revision;
    Ok(CodexUsageCommitBridgeResult {
        outcome: UsageCommitOutcome {
            sources_committed: batch.sources.len(),
            events_inserted: inserted,
            events_deduplicated: deduplicated,
            data_revision,
        },
        visible_changed: canonical_changed && batch.ledger_epoch == active_epoch,
    })
}

fn apply_reconciliation_patch(
    tx: &mut CodexWriteTxn<'_>,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
    target: UsageWriteTarget,
) -> Result<(usize, usize), CodexStorageError> {
    let patch = &source.patch;
    let mut inserted = 0;
    let mut deduplicated = 0;
    let replaced_ids = patch
        .delete_event_ids
        .iter()
        .filter(|event_id| {
            patch
                .events
                .iter()
                .any(|event| &event.event_id == *event_id)
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    tx.with_private_state(|connection| {
        stage_bound_markers_for_patch(connection, batch.ledger_epoch, patch)
            .map_err(CodexStorageError::from)?;
        for hold in &patch.hold_upserts {
            let canonical_exists: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM usage_events
                 WHERE source='codex' AND source_epoch=?1 AND event_id=?2)",
                params![batch.ledger_epoch, hold.event_id],
                |row| row.get(0),
            )?;
            if canonical_exists != 0 {
                upsert_usage_hold(connection, batch.ledger_epoch, hold)?;
            }
        }
        Ok::<_, CodexStorageError>(())
    })?;

    let held_ids = tx.with_private_state(|connection| {
        let mut statement = connection.prepare(
            "SELECT event_id FROM codex_usage_event_holds
             WHERE source='codex' AND ledger_epoch=?1",
        )?;
        statement
            .query_map([batch.ledger_epoch], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()
            .map_err(StorageError::from)
            .map_err(CodexStorageError::from)
    })?;
    let replaceable_occurrence_event_ids = patch
        .delete_event_ids
        .iter()
        .filter(|event_id| !held_ids.contains(*event_id))
        .cloned()
        .collect::<BTreeSet<_>>();
    let replaced = replaced_ids
        .into_iter()
        .filter(|event_id| !held_ids.contains(event_id))
        .collect::<Vec<_>>();
    if !replaced.is_empty() {
        tx.with_private_state(|connection| {
            strip_orphan_window_references(connection, batch.ledger_epoch, &replaced)
                .map_err(CodexStorageError::from)
        })?;
        tx.delete_usage_events_no_revision(target, &replaced)?;
    }

    for event in &patch.events {
        match write_canonical_event(tx, target, source, event)? {
            crate::source::CanonicalWriteOutcome::Inserted => inserted += 1,
            crate::source::CanonicalWriteOutcome::Duplicate => deduplicated += 1,
        }
    }

    tx.with_private_state(|connection| {
        for occurrence in &patch.occurrences {
            write_or_compare_occurrence(
                connection,
                batch.ledger_epoch,
                source,
                occurrence,
                &replaceable_occurrence_event_ids,
            )
            .map_err(CodexStorageError::from)?;
        }
        for fact in &patch.facts {
            upsert_usage_fact(connection, batch.ledger_epoch, fact)
                .map_err(CodexStorageError::from)?;
        }
        for marker in &patch.marker_upserts {
            upsert_compaction_marker(connection, batch.ledger_epoch, marker)
                .map_err(CodexStorageError::from)?;
        }
        for window in &patch.window_upserts {
            upsert_reconciliation_window(connection, batch.ledger_epoch, window)
                .map_err(CodexStorageError::from)?;
        }
        for turn in &patch.turn_upserts {
            write_turn(
                connection,
                batch.ledger_epoch,
                turn.source_file_id,
                turn.file_generation,
                &turn.thread_id,
                turn,
            )
            .map_err(CodexStorageError::from)?;
        }
        for rewrite in &patch.turn_rewrites {
            write_turn_rewrite(connection, batch.ledger_epoch, rewrite)
                .map_err(CodexStorageError::from)?;
        }
        for anomaly in &patch.anomalies {
            write_anomaly(
                connection,
                batch.ledger_epoch,
                &batch.thread_id,
                source.source_file_id,
                source.expected_file_generation,
                anomaly,
            )
            .map_err(CodexStorageError::from)?;
        }
        Ok::<_, CodexStorageError>(())
    })?;

    let deletions = patch
        .delete_event_ids
        .iter()
        .filter(|event_id| {
            !patch
                .events
                .iter()
                .any(|event| &event.event_id == *event_id)
        })
        .cloned()
        .filter(|event_id| !held_ids.contains(event_id))
        .collect::<Vec<_>>();
    if !deletions.is_empty() {
        tx.with_private_state(|connection| {
            for event_id in &deletions {
                let references: i64 = connection.query_row(
                    "SELECT count(*) FROM codex_usage_event_occurrences
                     WHERE source='codex' AND ledger_epoch=?1 AND event_id=?2",
                    params![batch.ledger_epoch, event_id],
                    |row| row.get(0),
                )?;
                if references != 0 {
                    return Err(StorageError::usage_conflict(
                        "deleted usage event still has occurrences",
                    ));
                }
            }
            Ok::<_, StorageError>(())
        })?;
        tx.with_private_state(|connection| {
            strip_orphan_window_references(connection, batch.ledger_epoch, &deletions)
                .map_err(CodexStorageError::from)
        })?;
        tx.delete_usage_events_no_revision(target, &deletions)?;
    }

    Ok((inserted, deduplicated))
}

fn write_canonical_event(
    tx: &mut CodexWriteTxn<'_>,
    target: UsageWriteTarget,
    source: &UsageSourceCommit,
    event: &UsageEventWrite,
) -> Result<crate::source::CanonicalWriteOutcome, CodexStorageError> {
    Ok(tx.write_usage_no_revision(
        target,
        CanonicalUsageEventWrite {
            event_id: event.event_id.clone(),
            kind: event.kind,
            occurred_at_ms: event.occurred_at_ms,
            thread_id: event.thread_id.clone(),
            root_session_id: event.root_session_id.clone(),
            turn_key: event.turn_key.clone(),
            model: event.model.clone(),
            reasoning_effort: event.reasoning_effort.clone(),
            estimated_cost_nanos_usd: event.estimated_cost_nanos_usd,
            usage: event.usage.clone(),
            created_at_ms: source.committed_at_ms,
        },
    )?)
}

fn stage_bound_markers_for_patch(
    transaction: &Connection,
    epoch: i64,
    patch: &ReconciliationPatchWrite,
) -> StorageResult<()> {
    let mut affected_ids = patch
        .delete_event_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for fact in &patch.facts {
        affected_ids.insert(fact.event_id.clone());
    }
    let mut keys_to_stage = BTreeSet::new();
    for event_id in affected_ids {
        let mut statement = transaction.prepare(
            "SELECT source_file_id,file_generation,source_start_offset
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND resolved_event_id=?2
             ORDER BY source_file_id,file_generation,source_start_offset",
        )?;
        for row in statement.query_map(params![epoch, event_id], |row| {
            Ok(UsagePrivateRowKey {
                source_file_id: row.get(0)?,
                file_generation: row.get(1)?,
                source_start_offset: row.get(2)?,
            })
        })? {
            keys_to_stage.insert(row?);
        }
    }
    for key in &patch.delete_markers {
        keys_to_stage.insert(*key);
    }
    for marker in &patch.marker_upserts {
        keys_to_stage.insert(UsagePrivateRowKey {
            source_file_id: marker.source_file_id,
            file_generation: marker.file_generation,
            source_start_offset: marker.source_start_offset,
        });
    }

    for key in keys_to_stage {
        let exists: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND source_start_offset=?4 AND resolved_event_id IS NOT NULL)",
            params![epoch,key.source_file_id,key.file_generation,key.source_start_offset],
            |row| row.get(0),
        )?;
        if exists == 0 {
            continue;
        }
        let explicit = patch.delete_markers.contains(&key)
            || patch.marker_upserts.iter().any(|marker| {
                marker.source_file_id == key.source_file_id
                    && marker.file_generation == key.file_generation
                    && marker.source_start_offset == key.source_start_offset
            });
        if !explicit {
            return Err(StorageError::usage_conflict(
                "bound Compaction marker is absent from reconciliation patch",
            ));
        }
        transaction.execute(
            "UPDATE codex_compaction_markers
             SET resolved_event_id=NULL,unknown_reason='usage_missing'
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND source_start_offset=?4",
            params![
                epoch,
                key.source_file_id,
                key.file_generation,
                key.source_start_offset
            ],
        )?;
    }
    for key in &patch.delete_markers {
        transaction.execute(
            "DELETE FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND source_start_offset=?4",
            params![
                epoch,
                key.source_file_id,
                key.file_generation,
                key.source_start_offset
            ],
        )?;
    }
    for key in &patch.delete_windows {
        transaction.execute(
            "DELETE FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND source_start_offset=?4",
            params![
                epoch,
                key.source_file_id,
                key.file_generation,
                key.source_start_offset
            ],
        )?;
    }
    for (source_file_id, generation, event_id) in &patch.delete_holds {
        transaction.execute(
            "DELETE FROM codex_usage_event_holds
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND event_id=?4",
            params![epoch, source_file_id, generation, event_id],
        )?;
    }
    Ok(())
}

fn upsert_usage_fact(
    transaction: &Connection,
    epoch: i64,
    fact: &UsageEventFactWrite,
) -> StorageResult<()> {
    let evidence_kind = match fact.evidence_kind {
        EvidenceKind::Explicit => "explicit",
        EvidenceKind::Legacy => "legacy",
    };
    let operation = match fact.operation {
        CodexOperation::Response => "response",
        CodexOperation::Compaction => "compaction",
    };
    let existing: Option<(String, Option<String>, String, String)> = transaction
        .query_row(
            "SELECT owning_thread_id,response_id,evidence_kind,operation
             FROM codex_usage_event_facts
             WHERE source='codex' AND ledger_epoch=?1 AND event_id=?2",
            params![epoch, fact.event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((owner, response, evidence, existing_operation)) = existing {
        if owner != fact.owning_thread_id
            || response != fact.response_id
            || evidence != evidence_kind
        {
            return Err(StorageError::usage_conflict(
                "usage fact identity changed for an existing event",
            ));
        }
        if existing_operation == "response" && operation == "compaction" {
            transaction.execute(
                "UPDATE codex_usage_event_facts SET operation='compaction'
                 WHERE source='codex' AND ledger_epoch=?1 AND event_id=?2
                   AND operation='response'",
                params![epoch, fact.event_id],
            )?;
        }
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO codex_usage_event_facts(
            source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation
         ) VALUES ('codex',?1,?2,?3,?4,?5,?6)",
        params![
            epoch,
            fact.event_id,
            fact.owning_thread_id,
            fact.response_id,
            evidence_kind,
            operation
        ],
    )?;
    Ok(())
}

fn upsert_compaction_marker(
    transaction: &Connection,
    epoch: i64,
    marker: &UsageCompactionMarkerWrite,
) -> StorageResult<()> {
    if (marker.resolved_event_id.is_some()) != marker.unknown_reason.is_none() {
        return Err(StorageError::invalid_state(
            "Compaction marker resolution state is inconsistent",
        ));
    }
    transaction.execute(
        "INSERT INTO codex_compaction_markers(
            source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
            owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,response_id,
            resolved_event_id,unknown_reason
         ) VALUES ('codex',?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
         ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,source_start_offset)
         DO UPDATE SET source_end_offset=excluded.source_end_offset,
            owning_thread_id=excluded.owning_thread_id,root_session_id=excluded.root_session_id,
            occurred_at_ms=excluded.occurred_at_ms,model=excluded.model,
            reasoning_effort=excluded.reasoning_effort,response_id=excluded.response_id,
            resolved_event_id=excluded.resolved_event_id,unknown_reason=excluded.unknown_reason",
        params![epoch,marker.source_file_id,marker.file_generation,marker.source_start_offset,
            marker.source_end_offset,marker.owning_thread_id,marker.root_session_id,
            marker.occurred_at_ms,marker.model,marker.reasoning_effort,marker.response_id,
            marker.resolved_event_id,marker.unknown_reason],
    )?;
    Ok(())
}

fn upsert_reconciliation_window(
    transaction: &Connection,
    epoch: i64,
    window: &UsageReconciliationWindowWrite,
) -> StorageResult<()> {
    let state_json = canonical_window_state(&window.state_json)?;
    transaction.execute(
        "INSERT INTO codex_usage_reconciliation_windows(
            source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
            owning_thread_id,turn_key,state_json
         ) VALUES ('codex',?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,source_start_offset)
         DO UPDATE SET source_end_offset=excluded.source_end_offset,
            owning_thread_id=excluded.owning_thread_id,turn_key=excluded.turn_key,
            state_json=excluded.state_json",
        params![epoch,window.source_file_id,window.file_generation,window.source_start_offset,
            window.source_end_offset,window.owning_thread_id,window.turn_key,state_json],
    )?;
    Ok(())
}

fn canonical_window_state(json: &str) -> StorageResult<String> {
    use crate::codex::ingestion::usage_processor::{CarryError, LegacyReconciliationWindow};

    let window = LegacyReconciliationWindow::from_json(json).map_err(|error| match error {
        CarryError::UnsupportedVersion => {
            StorageError::usage_conflict("reconciliation window version requires parser rebuild")
        }
        CarryError::Invalid => StorageError::invalid_state("invalid reconciliation window state"),
    })?;
    let canonical = window.to_json().map_err(|error| match error {
        CarryError::UnsupportedVersion => {
            StorageError::usage_conflict("reconciliation window version requires parser rebuild")
        }
        CarryError::Invalid => StorageError::invalid_state("invalid reconciliation window state"),
    })?;
    if canonical != json {
        return Err(StorageError::invalid_state(
            "reconciliation window state is not canonical",
        ));
    }
    Ok(canonical)
}

pub(crate) fn strip_orphan_window_references(
    transaction: &Connection,
    epoch: i64,
    orphan_event_ids: &[String],
) -> StorageResult<()> {
    if orphan_event_ids.is_empty() {
        return Ok(());
    }
    let orphan_event_ids = orphan_event_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut statement = transaction.prepare(
        "SELECT source_file_id,file_generation,source_start_offset,state_json
         FROM codex_usage_reconciliation_windows
         WHERE source='codex' AND ledger_epoch=?1
         ORDER BY source_file_id,file_generation,source_start_offset",
    )?;
    let windows = statement
        .query_map([epoch], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (source_file_id, generation, start_offset, state_json) in windows {
        canonical_window_state(&state_json)?;
        let mut window =
            crate::codex::ingestion::usage_processor::LegacyReconciliationWindow::from_json(
                &state_json,
            )
            .map_err(|_| StorageError::invalid_state("invalid reconciliation window state"))?;
        let old_len = window.proposal_event_ids.len();
        window
            .proposal_event_ids
            .retain(|event_id| !orphan_event_ids.contains(event_id.as_str()));
        if window.proposal_event_ids.len() == old_len {
            continue;
        }
        let state_json = window
            .to_json()
            .map_err(|_| StorageError::invalid_state("invalid reconciliation window state"))?;
        transaction.execute(
            "UPDATE codex_usage_reconciliation_windows SET state_json=?1
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?4 AND source_start_offset=?5",
            params![state_json, epoch, source_file_id, generation, start_offset],
        )?;
    }
    Ok(())
}

pub(crate) fn cleanup_private_source_generation(
    transaction: &Connection,
    epoch: i64,
    source_file_id: i64,
    file_generation: i64,
) -> StorageResult<()> {
    transaction.execute(
        "DELETE FROM codex_compaction_markers
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_reconciliation_windows
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_event_holds
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_event_occurrences
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_skill_usage_events
         WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_turns
         WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_ingest_anomalies
         WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    // Keep the active epoch's last committed identity and tail snapshot as the
    // stopped generation's classification boundary until its replacement is
    // scanned and writes a new source state.
    transaction.execute(
        "DELETE FROM codex_usage_session_quarantine_sources
         WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3",
        params![epoch, source_file_id, file_generation],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_session_quarantine
         WHERE ledger_epoch=?1 AND NOT EXISTS (
             SELECT 1 FROM codex_usage_session_quarantine_sources s
             WHERE s.ledger_epoch=codex_usage_session_quarantine.ledger_epoch
               AND s.root_session_id=codex_usage_session_quarantine.root_session_id)",
        [epoch],
    )?;
    Ok(())
}

fn upsert_usage_hold(
    transaction: &Connection,
    epoch: i64,
    hold: &UsageEventHoldWrite,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO codex_usage_event_holds(
            source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason
         ) VALUES ('codex',?1,?2,?3,?4,?5)
         ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,event_id)
         DO UPDATE SET hold_reason=excluded.hold_reason",
        params![
            epoch,
            hold.source_file_id,
            hold.file_generation,
            hold.event_id,
            hold.hold_reason.as_str()
        ],
    )?;
    Ok(())
}

pub(crate) fn begin_usage_carry(
    transaction: &Connection,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    if now_ms < 0 {
        return Err(StorageError::invalid_state("negative carry time"));
    }
    let epoch = read_epoch(transaction)?;
    let build_epoch = epoch
        .build_epoch
        .ok_or_else(|| StorageError::invalid_state("usage carry requires a build"))?;
    let parser = epoch.working_parser_version();
    let plan = load_source_plan(transaction, source_file_id, parser, epoch.clone())?;
    if plan.action != UsagePlanAction::BeginCarry {
        return Err(StorageError::invalid_state(
            "usage source is not eligible for BeginCarry",
        ));
    }
    let build = plan
        .build
        .as_ref()
        .ok_or_else(|| StorageError::invalid_state("usage carry manifest is missing"))?;
    verify_carry_canonical_events(transaction, build_epoch)?;

    let replay_holds: i64 = transaction.query_row(
        "SELECT count(*) FROM codex_usage_event_holds
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3 AND hold_reason='replay'",
        params![
            epoch.active_epoch,
            source_file_id,
            build.expected_file_generation
        ],
        |row| row.get(0),
    )?;
    if replay_holds != 0 {
        return Err(StorageError::usage_conflict(
            "usage source has an unfinished local replay",
        ));
    }

    crate::codex::storage::rebuild::cleanup_build_source(transaction, build_epoch, source_file_id)
        .map_err(rebuild_storage_error)?;

    let changed = transaction.execute(
        "UPDATE codex_source_checkpoints SET parser_version=?1,committed_offset=0,guard_hash=NULL,
                processing_status='rebuild_required',last_error_code=NULL
         WHERE source_file_id=?2 AND consumer_kind='usage'",
        params![parser, source_file_id],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry checkpoint CAS failed",
        ));
    }
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_from_epoch=?1,carry_phase='occurrences',
                carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,
                carry_after_fact_event_id=NULL,carry_after_marker_start_offset=NULL,
                carry_after_window_start_offset=NULL,
                completion_status='pending',completion_error_code=NULL,
                completed_generation=NULL,completed_through_offset=NULL,updated_at_ms=?2
         WHERE build_epoch=?3 AND source_file_id=?4
           AND carry_phase='none' AND completion_status IN ('pending','blocked')
           AND required_through_offset=?5 AND active_committed_offset=?5",
        params![
            epoch.active_epoch,
            now_ms,
            build_epoch,
            source_file_id,
            build.active_committed_offset
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry manifest CAS failed",
        ));
    }
    let working_state_count: i64 = transaction.query_row(
        "SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    if working_state_count != 0 {
        return Err(StorageError::invalid_state(
            "carry-in-progress retained working source state",
        ));
    }
    Ok(())
}

pub(crate) fn resume_usage_carry(
    tx: &mut CodexWriteTxn<'_>,
    source_file_id: i64,
    now_ms: i64,
) -> Result<CarryStepOutcome, CodexStorageError> {
    let (epoch, plan) = tx.with_private_state(|transaction| {
        let epoch = read_epoch(transaction).map_err(CodexStorageError::from)?;
        let plan = load_source_plan(
            transaction,
            source_file_id,
            epoch.working_parser_version(),
            epoch.clone(),
        )
        .map_err(CodexStorageError::from)?;
        Ok::<_, CodexStorageError>((epoch, plan))
    })?;
    if now_ms < 0 {
        return Err(CodexStorageError::Storage(StorageError::invalid_state(
            "negative carry time",
        )));
    }
    let build_epoch = epoch.build_epoch.ok_or_else(|| {
        CodexStorageError::Storage(StorageError::invalid_state("usage carry requires a build"))
    })?;
    if plan.action != UsagePlanAction::ResumeCarry {
        return Err(CodexStorageError::Storage(StorageError::invalid_state(
            "usage source is not in ResumeCarry",
        )));
    }
    let build = plan.build.as_ref().ok_or_else(|| {
        CodexStorageError::Storage(StorageError::invalid_state(
            "usage carry manifest is missing",
        ))
    })?;
    tx.with_private_state(|transaction| {
        verify_carry_db_proof(transaction, epoch.clone(), source_file_id, build)
            .map_err(CodexStorageError::from)
    })?;

    let outcome = match build.carry_phase {
        UsageCarryPhase::Occurrences => {
            let event_ids = tx.with_private_state(|transaction| {
                carry_occurrence_event_ids(
                    transaction,
                    epoch.active_epoch,
                    source_file_id,
                    build.expected_file_generation,
                )
                .map_err(CodexStorageError::from)
            })?;
            for event_id in event_ids {
                tx.copy_usage_event_no_revision(
                    UsageWriteTarget::Active,
                    UsageWriteTarget::Build,
                    &event_id,
                )?;
            }
            tx.with_private_state(|transaction| {
                carry_occurrence_page(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )
                .map_err(CodexStorageError::from)
            })?;
            Ok(CarryStepOutcome::Progress)
        }
        UsageCarryPhase::Facts => {
            let (after, event_ids, has_more) = tx.with_private_state(|transaction| {
                carry_fact_page_event_ids(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                )
                .map_err(CodexStorageError::from)
            })?;
            for event_id in &event_ids {
                tx.copy_usage_event_no_revision(
                    UsageWriteTarget::Active,
                    UsageWriteTarget::Build,
                    event_id,
                )?;
            }
            tx.with_private_state(|transaction| {
                carry_fact_page(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    &event_ids,
                    has_more,
                    after,
                    now_ms,
                )
                .map_err(CodexStorageError::from)
            })?;
            Ok(CarryStepOutcome::Progress)
        }
        UsageCarryPhase::Markers => {
            tx.with_private_state(|transaction| {
                carry_marker_page(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )
                .map_err(CodexStorageError::from)
            })?;
            Ok(CarryStepOutcome::Progress)
        }
        UsageCarryPhase::Windows => {
            tx.with_private_state(|transaction| {
                carry_window_page(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )
                .map_err(CodexStorageError::from)
            })?;
            Ok(CarryStepOutcome::Progress)
        }
        UsageCarryPhase::Turns => {
            tx.with_private_state(|transaction| {
                carry_turn_page(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )
                .map_err(CodexStorageError::from)
            })?;
            Ok(CarryStepOutcome::Progress)
        }
        UsageCarryPhase::Anomalies => {
            tx.with_private_state(|transaction| {
                carry_anomaly_page(
                    transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )
                .map_err(CodexStorageError::from)
            })?;
            Ok(CarryStepOutcome::Progress)
        }
        UsageCarryPhase::Finalize => tx.with_private_state(|transaction| {
            finalize_carry(transaction, epoch, source_file_id, build, now_ms)
                .map_err(CodexStorageError::from)
        }),
        UsageCarryPhase::None => Err(CodexStorageError::Storage(StorageError::invalid_state(
            "usage carry cursor is not initialized",
        ))),
    }?;
    if matches!(
        outcome,
        CarryStepOutcome::FinalizedMissing | CarryStepOutcome::FinalizedPresent
    ) {
        crate::codex::storage::rebuild::delete_orphan_build_events(tx, build_epoch)?;
    }
    Ok(outcome)
}

pub(crate) fn complete_usage_build_source(
    transaction: &Connection,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    if now_ms < 0 {
        return Err(StorageError::invalid_state("negative completion time"));
    }
    let epoch = read_epoch(transaction)?;
    let build_epoch = epoch
        .build_epoch
        .ok_or_else(|| StorageError::invalid_state("CompleteOnly requires a build"))?;
    let plan = load_source_plan(
        transaction,
        source_file_id,
        epoch.working_parser_version(),
        epoch,
    )?;
    if plan.action != UsagePlanAction::CompleteOnly {
        return Err(StorageError::invalid_state(
            "usage source is not eligible for CompleteOnly",
        ));
    }
    let build = plan
        .build
        .ok_or_else(|| StorageError::invalid_state("usage build manifest is missing"))?;
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET completion_status='rebuilt',completion_error_code=NULL,
                completed_generation=required_generation,completed_through_offset=required_through_offset,
                carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                carry_after_fact_event_id=NULL,carry_after_marker_start_offset=NULL,
                carry_after_window_start_offset=NULL,carry_after_turn_key=NULL,
                carry_after_anomaly_id=NULL,updated_at_ms=?1
         WHERE build_epoch=?2 AND source_file_id=?3 AND carry_phase='none'
           AND completion_status IN ('pending','blocked')
           AND required_generation=?4 AND required_through_offset=?5",
        params![
            now_ms,
            build_epoch,
            source_file_id,
            build.expected_file_generation,
            build.required_through_offset
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "CompleteOnly manifest CAS failed",
        ));
    }
    crate::codex::storage::rebuild::verify_completion_row_for_storage(
        transaction,
        build_epoch,
        source_file_id,
    )
    .map_err(|error| StorageError::invalid_state(error.to_string()))
}

pub(crate) fn cleanup_private_phases(
    transaction: &Connection,
    max_rows: usize,
    start_phase: usize,
    end_phase: usize,
) -> StorageResult<usize> {
    if max_rows == 0 {
        return Ok(0);
    }
    let limit = i64::try_from(max_rows)
        .map_err(|_| StorageError::invalid_state("cleanup row limit is too large"))?;
    let (active, build): (i64, Option<i64>) = transaction.query_row(
        "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let excluded_build = build.unwrap_or(-1);
    let statements = [
        "DELETE FROM codex_usage_session_quarantine_sources WHERE rowid IN (
            SELECT rowid FROM codex_usage_session_quarantine_sources
            WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_usage_session_quarantine WHERE rowid IN (
            SELECT q.rowid FROM codex_usage_session_quarantine q
            WHERE q.ledger_epoch<>?1 AND q.ledger_epoch<>?2
              AND NOT EXISTS (
                SELECT 1 FROM codex_usage_session_quarantine_sources qs
                WHERE qs.ledger_epoch=q.ledger_epoch AND qs.root_session_id=q.root_session_id)
            ORDER BY q.ledger_epoch,q.rowid LIMIT ?3)",
        "DELETE FROM codex_compaction_markers WHERE rowid IN (
            SELECT rowid FROM codex_compaction_markers
            WHERE source='codex' AND ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_usage_reconciliation_windows WHERE rowid IN (
            SELECT rowid FROM codex_usage_reconciliation_windows
            WHERE source='codex' AND ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_usage_event_holds WHERE rowid IN (
            SELECT rowid FROM codex_usage_event_holds
            WHERE source='codex' AND ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_usage_event_occurrences WHERE rowid IN (
            SELECT rowid FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_skill_usage_events WHERE rowid IN (
            SELECT rowid FROM codex_skill_usage_events WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_turns WHERE rowid IN (
            SELECT rowid FROM codex_turns WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_ingest_anomalies WHERE rowid IN (
            SELECT rowid FROM codex_ingest_anomalies WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
        "DELETE FROM codex_usage_source_states WHERE rowid IN (
            SELECT rowid FROM codex_usage_source_states WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
            ORDER BY ledger_epoch,rowid LIMIT ?3)",
    ];
    let mut deleted = 0usize;
    for (phase, sql) in statements.into_iter().enumerate() {
        if phase < start_phase {
            continue;
        }
        if phase >= end_phase {
            break;
        }
        if deleted >= max_rows {
            break;
        }
        let remaining = i64::try_from(max_rows - deleted)
            .map_err(|_| StorageError::invalid_state("cleanup row limit is too large"))?;
        let count =
            transaction.execute(sql, params![active, excluded_build, remaining.min(limit)])?;
        deleted += count;
        if count > 0 {
            break;
        }
    }
    Ok(deleted)
}

fn read_epoch(transaction: &Connection) -> StorageResult<SourceUsageEpochState> {
    let values: (i64, Option<i64>, i64, Option<i64>) = transaction.query_row(
        "SELECT active_epoch, build_epoch, active_parser_version,
                build_parser_version FROM source_usage_epochs WHERE source='codex'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    SourceUsageEpochState::new(
        crate::source::SourceId::CODEX,
        values.0,
        values.1,
        values.2,
        values.3,
    )
    .map_err(|error| StorageError::invalid_state(error.to_string()))
}

fn validate_usage_source_ids(source_file_ids: &[i64]) -> StorageResult<()> {
    let mut seen = HashSet::with_capacity(source_file_ids.len());
    for &source_file_id in source_file_ids {
        if source_file_id <= 0 {
            return Err(StorageError::invalid_state(
                "usage source id must be positive",
            ));
        }
        if !seen.insert(source_file_id) {
            return Err(StorageError::invalid_state("duplicate usage source id"));
        }
    }
    Ok(())
}

fn usage_id_values_cte(source_file_ids: &[i64]) -> String {
    let values = source_file_ids
        .iter()
        .enumerate()
        .map(|(index, _)| format!("(?{})", index + 1))
        .collect::<Vec<_>>()
        .join(",");
    format!("WITH input(source_file_id) AS (VALUES {values})")
}

fn load_usage_stable_work_list_chunk(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    parser_version: i64,
    canonical_algorithm: i64,
    source_file_ids: &[i64],
    rows: &mut Vec<UsageWorkListRow>,
) -> StorageResult<()> {
    if source_file_ids.is_empty() {
        return Ok(());
    }
    let base = usage_id_values_cte(source_file_ids);
    let epoch_bind = source_file_ids.len() + 1;
    let parser_bind = epoch_bind + 1;
    let canonical_bind = parser_bind + 1;
    let sql = format!(
        "{base}
         SELECT sf.source_file_id,sf.thread_id
         FROM input i
         JOIN codex_source_files sf ON sf.source_file_id=i.source_file_id
         LEFT JOIN codex_source_checkpoints cp
           ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'
         LEFT JOIN codex_usage_source_states st
           ON st.ledger_epoch=?{epoch_bind} AND st.source_file_id=sf.source_file_id
         LEFT JOIN threads th ON th.thread_id=sf.thread_id
         WHERE sf.file_status='present'
           AND sf.thread_id IS NOT NULL
           AND th.root_session_id IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM codex_usage_session_quarantine_sources qs
               WHERE qs.ledger_epoch=?{epoch_bind}
                 AND qs.source_file_id=sf.source_file_id
                 AND qs.file_generation=sf.file_generation
                 AND qs.device_id=sf.device_id AND qs.inode=sf.inode
                 AND qs.observed_size=sf.observed_size
           )
           AND (cp.source_file_id IS NULL OR NOT (
               cp.processing_status='ready'
               AND cp.parser_version=?{parser_bind}
               AND st.file_generation=sf.file_generation
               AND st.device_id=sf.device_id
               AND st.inode=sf.inode
               AND st.usage_parser_version=?{parser_bind}
               AND st.canonical_algorithm_version=?{canonical_bind}
               AND st.resolved_through_offset=cp.committed_offset
               AND st.observed_raw_size=sf.observed_size
               AND st.owning_thread_id=sf.thread_id
               AND st.root_session_id=th.root_session_id
               AND ((cp.committed_offset=0 AND cp.guard_hash IS NULL)
                    OR (cp.committed_offset>0 AND length(cp.guard_hash)=32))
               AND (
                 (st.active_turn_key IS NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM codex_turns t
                    WHERE t.ledger_epoch=?{epoch_bind}
                      AND t.source_file_id=sf.source_file_id
                      AND t.status='open'))
                 OR
                 (st.active_turn_key IS NOT NULL
                  AND EXISTS (
                    SELECT 1 FROM codex_turns t
                    WHERE t.ledger_epoch=?{epoch_bind}
                      AND t.source_file_id=sf.source_file_id
                      AND t.status='open'
                      AND t.turn_key=st.active_turn_key
                      AND t.state_through_offset<=st.resolved_through_offset
                      AND t.thread_id=sf.thread_id
                      AND t.file_generation=sf.file_generation)
                  AND NOT EXISTS (
                    SELECT 1 FROM codex_turns t
                    WHERE t.ledger_epoch=?{epoch_bind}
                      AND t.source_file_id=sf.source_file_id
                      AND t.status='open'
                      AND t.turn_key<>st.active_turn_key))
               )
               AND (
                 (st.raw_tail_status='none'
                  AND st.raw_tail_start_offset IS NULL
                  AND cp.committed_offset=sf.observed_size)
                 OR
                 (st.raw_tail_status='half_line'
                  AND st.raw_tail_start_offset=cp.committed_offset
                  AND cp.committed_offset<sf.observed_size)
               )
             ))
         ORDER BY sf.thread_id,sf.source_file_id"
    );
    let mut values = source_file_ids.to_vec();
    values.extend([epoch.working_epoch(), parser_version, canonical_algorithm]);
    let mut statement = transaction.prepare(&sql)?;
    for row in statement.query_map(params_from_iter(values), |row| {
        Ok(UsageWorkListRow {
            source_file_id: row.get(0)?,
            owning_thread_id: row.get(1)?,
        })
    })? {
        rows.push(row?);
    }
    Ok(())
}

fn load_usage_build_work_list_chunk(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    parser_version: i64,
    canonical_algorithm: i64,
    source_file_ids: &[i64],
    rows: &mut Vec<UsageWorkListRow>,
) -> StorageResult<()> {
    if source_file_ids.is_empty() {
        return Ok(());
    }
    let base = usage_id_values_cte(source_file_ids);
    let build_bind = source_file_ids.len() + 1;
    let parser_bind = build_bind + 1;
    let canonical_bind = parser_bind + 1;
    let sql = format!(
        "{base}
         SELECT sf.source_file_id,sf.thread_id
         FROM input i
         JOIN codex_source_files sf ON sf.source_file_id=i.source_file_id
         LEFT JOIN codex_usage_build_sources b
           ON b.build_epoch=?{build_bind} AND b.source_file_id=sf.source_file_id
         LEFT JOIN codex_source_checkpoints cp
           ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'
         LEFT JOIN codex_usage_source_states st
           ON st.ledger_epoch=?{build_bind} AND st.source_file_id=sf.source_file_id
         LEFT JOIN threads th ON th.thread_id=sf.thread_id
         WHERE sf.thread_id IS NOT NULL
           AND th.root_session_id IS NOT NULL
           AND (
             (b.source_file_id IS NULL AND sf.file_status='present')
             OR b.completion_status IN ('pending','blocked')
             OR b.carry_phase<>'none'
             OR (
               b.completion_status IN ('rebuilt','carried')
               AND NOT (
                 sf.file_generation=b.expected_file_generation
                 AND sf.device_id=b.expected_device_id
                 AND sf.inode=b.expected_inode
                 AND cp.parser_version=b.target_parser_version
                 AND cp.processing_status='ready'
                 AND cp.committed_offset=st.resolved_through_offset
                 AND st.file_generation=b.expected_file_generation
                 AND st.device_id=b.expected_device_id
                 AND st.inode=b.expected_inode
                 AND b.target_parser_version=?{parser_bind}
                 AND st.usage_parser_version=b.target_parser_version
                 AND st.canonical_algorithm_version=?{canonical_bind}
                 AND st.observed_raw_size=b.observed_raw_size
                 AND st.owning_thread_id IS b.expected_owning_thread_id
                 AND st.root_session_id IS b.expected_root_session_id
                 AND st.continuation_state IN ('replayed_ancestor','owning_live')
                 AND st.raw_tail_status=b.raw_tail_status
                 AND st.raw_tail_start_offset IS b.raw_tail_start_offset
                 AND (b.completion_status<>'carried' OR sf.file_status='missing')
                 AND b.completed_generation=b.required_generation
                 AND b.completed_through_offset>=b.required_through_offset
                 AND b.raw_tail_status IN ('none','half_line')
               )
             )
           )
         ORDER BY sf.thread_id,sf.source_file_id"
    );
    let mut values = source_file_ids.to_vec();
    values.extend([epoch.working_epoch(), parser_version, canonical_algorithm]);
    let mut statement = transaction.prepare(&sql)?;
    for row in statement.query_map(params_from_iter(values), |row| {
        Ok(UsageWorkListRow {
            source_file_id: row.get(0)?,
            owning_thread_id: row.get(1)?,
        })
    })? {
        rows.push(row?);
    }
    Ok(())
}

#[derive(Clone)]
struct SourcePlanRow {
    thread_id: Option<String>,
    device_id: i64,
    inode: i64,
    generation: i64,
    observed_size: i64,
    status: String,
}

fn load_source_plan(
    transaction: &Connection,
    source_file_id: i64,
    requested_parser: i64,
    epoch: SourceUsageEpochState,
) -> StorageResult<UsageSourceScanPlan> {
    let source = transaction
        .query_row(
            "SELECT thread_id,device_id,inode,file_generation,observed_size,file_status
             FROM codex_source_files WHERE source_file_id=?1",
            [source_file_id],
            |row| {
                Ok(SourcePlanRow {
                    thread_id: row.get(0)?,
                    device_id: row.get(1)?,
                    inode: row.get(2)?,
                    generation: row.get(3)?,
                    observed_size: row.get(4)?,
                    status: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| StorageError::invalid_state("usage source does not exist"))?;
    let root_session_id = source
        .thread_id
        .as_ref()
        .and_then(|thread_id| {
            transaction
                .query_row(
                    "SELECT root_session_id FROM threads WHERE thread_id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .transpose()
        })
        .transpose()?
        .flatten();
    let checkpoint = read_usage_checkpoint(transaction, source_file_id)?;
    let state = match read_usage_source_state(transaction, epoch.working_epoch(), source_file_id) {
        Ok(state) => state,
        Err(error) if error.requires_usage_rebuild() => {
            let build = read_build_plan_state(transaction, epoch.build_epoch, source_file_id)?;
            return Ok(UsageSourceScanPlan {
                source_file_id,
                action: UsagePlanAction::RebuildRequired,
                start_offset: 0,
                observed_size: source.observed_size,
                owning_thread_id: source.thread_id,
                root_session_id,
                checkpoint,
                state: None,
                open_turn: None,
                build,
            });
        }
        Err(error) => return Err(error),
    };
    let open_turn = match state.as_ref() {
        Some(state) => read_open_turn(transaction, epoch.working_epoch(), source_file_id, state)?,
        None => None,
    };
    let build = read_build_plan_state(transaction, epoch.build_epoch, source_file_id)?;

    let mut plan = UsageSourceScanPlan {
        source_file_id,
        action: UsagePlanAction::RebuildRequired,
        start_offset: 0,
        observed_size: source.observed_size,
        owning_thread_id: source.thread_id.clone(),
        root_session_id: root_session_id.clone(),
        checkpoint: checkpoint.clone(),
        state: state.clone(),
        open_turn,
        build: build.clone(),
    };

    // Highest-priority global plan conditions.
    if epoch.active_epoch == 0 && epoch.build_epoch.is_none() {
        return Ok(plan);
    }
    if requested_parser != epoch.working_parser_version()
        || canonical_algorithm_for(requested_parser).is_none()
    {
        return Ok(plan);
    }
    if source.thread_id.is_none() || root_session_id.is_none() {
        plan.action = UsagePlanAction::BlockedRelationship;
        return Ok(plan);
    }

    if source.status == "present" {
        if let Some(checkpoint) = &checkpoint
            && checkpoint.committed_offset > source.observed_size
        {
            return Ok(plan);
        }
        // A build member freezes identity/binding/root. Any mismatch is a
        // replacement condition and outranks carry/completion/read plans.
        if let Some(build) = &build
            && (build.expected_file_generation != source.generation
                || build.expected_device_id != source.device_id
                || build.expected_inode != source.inode
                || build.expected_owning_thread_id != source.thread_id
                || build.expected_root_session_id != root_session_id)
        {
            return Ok(plan);
        }
    }

    let matching_state = state.as_ref().is_some_and(|state| {
        state.file_generation == source.generation
            && state.device_id == source.device_id
            && state.inode == source.inode
            && state.usage_parser_version == requested_parser
            && canonical_algorithm_for(requested_parser) == Some(state.canonical_algorithm_version)
            && checkpoint.as_ref().is_some_and(|checkpoint| {
                state.resolved_through_offset == checkpoint.committed_offset
            })
            && state.owning_thread_id == source.thread_id.as_deref().unwrap_or_default()
            && state.root_session_id == root_session_id.as_deref().unwrap_or_default()
            && open_turn_internally_matches(state, plan.open_turn.as_ref())
    });
    let guard_shape_valid = checkpoint.as_ref().is_none_or(|checkpoint| {
        (checkpoint.committed_offset == 0 && checkpoint.guard_hash.is_none())
            || (checkpoint.committed_offset > 0
                && checkpoint
                    .guard_hash
                    .as_ref()
                    .is_some_and(|guard| guard.len() == 32))
    });
    if !guard_shape_valid {
        return Ok(plan);
    }

    // A completed build member has no source work left in this build. Source
    // observation and metadata reconciliation are responsible for invalidating
    // this proof before planning if raw size, identity, binding or presence
    // makes it stale. Without this branch a Rebuilt/Carried row would fall
    // through to RebuildRequired and continuously reset a completed member.
    if let Some(build) = &build
        && matches!(
            build.completion_status,
            UsageBuildCompletion::Rebuilt
                | UsageBuildCompletion::Carried
                | UsageBuildCompletion::Quarantined
        )
    {
        plan.action = UsagePlanAction::Skip;
        return Ok(plan);
    }

    // Verified error recovery is deliberately before carry and normal plans.
    if let Some(checkpoint) = &checkpoint
        && checkpoint.processing_status == CheckpointProcessingStatus::Error
    {
        let verified = checkpoint.committed_offset > 0
            && matching_state
            && source.status == "present"
            && state.as_ref().is_some_and(|state| {
                state.resolved_through_offset == checkpoint.committed_offset
                    && state.usage_parser_version == requested_parser
            });
        if verified {
            plan.start_offset = checkpoint.committed_offset;
            plan.action = if epoch.build_epoch.is_some() {
                UsagePlanAction::BuildFrom
            } else {
                UsagePlanAction::ResumeOwningLive
            };
            return Ok(plan);
        }
        if epoch.build_epoch.is_none()
            && local_replay_safe(
                transaction,
                epoch,
                &source,
                source_file_id,
                checkpoint,
                state.as_ref(),
                root_session_id.as_deref(),
            )?
        {
            plan.action = UsagePlanAction::LocalReplay;
        }
        return Ok(plan);
    }

    if let Some(build) = &build
        && matches!(
            build.completion_status,
            UsageBuildCompletion::Pending | UsageBuildCompletion::Blocked
        )
        && build.carry_phase != UsageCarryPhase::None
    {
        plan.action = UsagePlanAction::ResumeCarry;
        return Ok(plan);
    }

    if let Some(build) = &build
        && matches!(
            build.completion_status,
            UsageBuildCompletion::Pending | UsageBuildCompletion::Blocked
        )
        && build.carry_phase == UsageCarryPhase::None
        && source.status == "missing"
    {
        plan.action = if begin_carry_eligible(
            transaction,
            epoch,
            source_file_id,
            CarryEligibility {
                source: &source,
                root: root_session_id.as_deref(),
                checkpoint: checkpoint.as_ref(),
                working_state: state.as_ref(),
                build,
            },
        )? {
            UsagePlanAction::BeginCarry
        } else {
            UsagePlanAction::BlockedRelationship
        };
        return Ok(plan);
    }

    if source.status != "present" {
        plan.action = UsagePlanAction::BlockedRelationship;
        return Ok(plan);
    }

    // Build completion proof outranks Skip and offset comparisons, including a
    // verified half-line whose checkpoint is below raw size.
    if let (Some(build), Some(checkpoint), Some(state)) = (&build, &checkpoint, &state)
        && matches!(
            build.completion_status,
            UsageBuildCompletion::Pending | UsageBuildCompletion::Blocked
        )
        && build.carry_phase == UsageCarryPhase::None
        && checkpoint.processing_status == CheckpointProcessingStatus::Ready
        && matching_state
        && checkpoint.committed_offset == build.required_through_offset
        && durable_tail_matches_build(
            source.generation,
            source.observed_size,
            checkpoint,
            state,
            build,
        )
    {
        plan.start_offset = checkpoint.committed_offset;
        plan.action = UsagePlanAction::CompleteOnly;
        return Ok(plan);
    }

    // Stable active tail proofs are true zero-body skips.
    if epoch.build_epoch.is_none()
        && let (Some(checkpoint), Some(state)) = (&checkpoint, &state)
        && checkpoint.processing_status == CheckpointProcessingStatus::Ready
        && matching_state
        && durable_tail_matches_source(source.generation, source.observed_size, checkpoint, state)
    {
        match state.raw_tail_status {
            UsageTailStatus::None if checkpoint.committed_offset == source.observed_size => {
                plan.start_offset = checkpoint.committed_offset;
                plan.action = UsagePlanAction::Skip;
                return Ok(plan);
            }
            UsageTailStatus::HalfLine
                if state.raw_tail_start_offset == Some(checkpoint.committed_offset)
                    && checkpoint.committed_offset < source.observed_size =>
            {
                plan.start_offset = checkpoint.committed_offset;
                plan.action = UsagePlanAction::Skip;
                return Ok(plan);
            }
            _ => {}
        }
    }

    // A nonzero checkpoint is never resumed from partial/stale state.
    if let Some(checkpoint) = &checkpoint
        && checkpoint.committed_offset > 0
        && !matching_state
    {
        return Ok(plan);
    }

    if let Some(build) = &build
        && matches!(
            build.completion_status,
            UsageBuildCompletion::Pending | UsageBuildCompletion::Blocked
        )
        && build.carry_phase == UsageCarryPhase::None
    {
        let Some(checkpoint) = &checkpoint else {
            return Ok(plan);
        };
        match checkpoint.processing_status {
            CheckpointProcessingStatus::RebuildRequired
                if checkpoint.committed_offset == 0 && state.is_none() =>
            {
                plan.action = UsagePlanAction::BuildFrom;
                return Ok(plan);
            }
            CheckpointProcessingStatus::Ready if matching_state => {
                plan.start_offset = checkpoint.committed_offset;
                if checkpoint.committed_offset == source.observed_size
                    && state
                        .as_ref()
                        .is_some_and(|state| state.raw_tail_status == UsageTailStatus::Unverified)
                {
                    plan.action = UsagePlanAction::VerifyRawTail;
                } else if checkpoint.committed_offset <= source.observed_size {
                    plan.action = UsagePlanAction::BuildFrom;
                }
                return Ok(plan);
            }
            _ => return Ok(plan),
        }
    }

    // No build: missing checkpoint means a first read from zero. A stale
    // rebuild-required checkpoint may use LocalReplay only under the exact
    // conservative active-epoch proof.
    let Some(checkpoint) = &checkpoint else {
        plan.action = UsagePlanAction::ReadFrom;
        return Ok(plan);
    };
    match checkpoint.processing_status {
        CheckpointProcessingStatus::Pending if checkpoint.committed_offset == 0 => {
            plan.action = UsagePlanAction::ReadFrom;
            return Ok(plan);
        }
        CheckpointProcessingStatus::RebuildRequired => {
            if local_replay_safe(
                transaction,
                epoch,
                &source,
                source_file_id,
                checkpoint,
                state.as_ref(),
                root_session_id.as_deref(),
            )? {
                plan.action = UsagePlanAction::LocalReplay;
            }
            return Ok(plan);
        }
        CheckpointProcessingStatus::Ready => {}
        CheckpointProcessingStatus::Pending | CheckpointProcessingStatus::Error => return Ok(plan),
    }
    if !matching_state && checkpoint.committed_offset > 0 {
        return Ok(plan);
    }
    plan.start_offset = checkpoint.committed_offset;
    if checkpoint.committed_offset == source.observed_size
        && state
            .as_ref()
            .is_some_and(|state| state.raw_tail_status == UsageTailStatus::Unverified)
    {
        plan.action = UsagePlanAction::VerifyRawTail;
    } else if checkpoint.committed_offset < source.observed_size {
        plan.action = if checkpoint.committed_offset == 0 {
            UsagePlanAction::ReadFrom
        } else {
            UsagePlanAction::ResumeOwningLive
        };
    }
    Ok(plan)
}

fn read_build_plan_state(
    transaction: &Connection,
    build_epoch: Option<i64>,
    source_file_id: i64,
) -> StorageResult<Option<UsageBuildPlanState>> {
    let Some(build_epoch) = build_epoch else {
        return Ok(None);
    };
    transaction.query_row(
        "SELECT target_parser_version,expected_file_generation,expected_device_id,expected_inode,
                expected_owning_thread_id,expected_root_session_id,active_committed_offset,
                active_guard_hash,active_state_fingerprint,required_through_offset,observed_raw_size,
                raw_tail_status,raw_tail_start_offset,completion_status,carry_phase
         FROM codex_usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
        params![build_epoch,source_file_id],
        |row| {
            let tail: String = row.get(11)?;
            let completion: String = row.get(13)?;
            let carry: String = row.get(14)?;
            Ok(UsageBuildPlanState {
                build_epoch,
                target_parser_version: row.get(0)?,
                expected_file_generation: row.get(1)?,
                expected_device_id: row.get(2)?,
                expected_inode: row.get(3)?,
                expected_owning_thread_id: row.get(4)?,
                expected_root_session_id: row.get(5)?,
                active_committed_offset: row.get(6)?,
                active_guard_hash: row.get(7)?,
                active_state_fingerprint: row.get(8)?,
                required_through_offset: row.get(9)?,
                observed_raw_size: row.get(10)?,
                raw_tail_status: UsageTailStatus::parse(&tail).map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?,
                raw_tail_start_offset: row.get(12)?,
                completion_status: match completion.as_str() {
                    "pending" => UsageBuildCompletion::Pending,
                    "rebuilt" => UsageBuildCompletion::Rebuilt,
                    "carried" => UsageBuildCompletion::Carried,
                    "blocked" => UsageBuildCompletion::Blocked,
                    "quarantined" => UsageBuildCompletion::Quarantined,
                    _ => return Err(rusqlite::Error::InvalidParameterName("invalid build completion".to_owned())),
                },
                carry_phase: match carry.as_str() {
                    "none" => UsageCarryPhase::None,
                    "occurrences" => UsageCarryPhase::Occurrences,
                    "facts" => UsageCarryPhase::Facts,
                    "markers" => UsageCarryPhase::Markers,
                    "windows" => UsageCarryPhase::Windows,
                    "turns" => UsageCarryPhase::Turns,
                    "anomalies" => UsageCarryPhase::Anomalies,
                    "finalize" => UsageCarryPhase::Finalize,
                    _ => return Err(rusqlite::Error::InvalidParameterName("invalid carry phase".to_owned())),
                },
            })
        },
    ).optional().map_err(StorageError::from)
}

fn open_turn_internally_matches(
    state: &UsageSourceStateWrite,
    open_turn: Option<&crate::codex::ingestion::usage_processor::TurnState>,
) -> bool {
    match (&state.active_turn_key, open_turn) {
        (None, None) => true,
        (Some(key), Some(turn)) => {
            key == &turn.turn_key
                && i64::try_from(turn.start_offset)
                    .is_ok_and(|offset| offset <= state.resolved_through_offset)
        }
        _ => false,
    }
}

fn durable_tail_matches_source(
    generation: i64,
    observed_size: i64,
    checkpoint: &UsageCheckpointExpectation,
    state: &UsageSourceStateWrite,
) -> bool {
    state.file_generation == generation
        && state.observed_raw_size == observed_size
        && state.resolved_through_offset == checkpoint.committed_offset
        && match state.raw_tail_status {
            UsageTailStatus::None => {
                checkpoint.committed_offset == observed_size
                    && state.raw_tail_start_offset.is_none()
            }
            UsageTailStatus::HalfLine => {
                state.raw_tail_start_offset == Some(checkpoint.committed_offset)
                    && checkpoint.committed_offset < observed_size
            }
            UsageTailStatus::Unverified => false,
        }
}

fn durable_tail_matches_build(
    generation: i64,
    observed_size: i64,
    checkpoint: &UsageCheckpointExpectation,
    state: &UsageSourceStateWrite,
    build: &UsageBuildPlanState,
) -> bool {
    build.expected_file_generation == generation
        && build.observed_raw_size == observed_size
        && build.required_through_offset == checkpoint.committed_offset
        && state.observed_raw_size == observed_size
        && state.raw_tail_status == build.raw_tail_status
        && state.raw_tail_start_offset == build.raw_tail_start_offset
        && durable_tail_matches_source(generation, observed_size, checkpoint, state)
}

fn local_replay_safe(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    source: &SourcePlanRow,
    source_file_id: i64,
    checkpoint: &UsageCheckpointExpectation,
    state: Option<&UsageSourceStateWrite>,
    root: Option<&str>,
) -> StorageResult<bool> {
    if epoch.build_epoch.is_some()
        || checkpoint.parser_version != epoch.active_parser_version
        || canonical_algorithm_for(epoch.active_parser_version).is_none()
    {
        return Ok(false);
    }
    if let Some(state) = state {
        return Ok(state.file_generation == source.generation
            && state.device_id == source.device_id
            && state.inode == source.inode
            && state.usage_parser_version == epoch.active_parser_version
            && state.canonical_algorithm_version
                == canonical_algorithm_for(epoch.active_parser_version).unwrap_or(-1)
            && Some(state.owning_thread_id.as_str()) == source.thread_id.as_deref()
            && Some(state.root_session_id.as_str()) == root
            && (checkpoint.committed_offset == 0
                || state.resolved_through_offset == checkpoint.committed_offset));
    }
    if checkpoint.committed_offset != 0 {
        return Ok(false);
    }
    let contributed: i64 = transaction.query_row(
        "SELECT
            (SELECT count(*) FROM codex_usage_event_occurrences
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_usage_event_holds
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2)",
        params![epoch.active_epoch, source_file_id],
        |row| row.get(0),
    )?;
    Ok(contributed == 0)
}

struct CarryEligibility<'a> {
    source: &'a SourcePlanRow,
    root: Option<&'a str>,
    checkpoint: Option<&'a UsageCheckpointExpectation>,
    working_state: Option<&'a UsageSourceStateWrite>,
    build: &'a UsageBuildPlanState,
}

fn begin_carry_eligible(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    source_file_id: i64,
    input: CarryEligibility<'_>,
) -> StorageResult<bool> {
    let CarryEligibility {
        source,
        root,
        checkpoint,
        working_state,
        build,
    } = input;
    if epoch.active_epoch <= 0
        || build.target_parser_version != epoch.active_parser_version
        || build.required_through_offset != build.active_committed_offset
        || build.expected_file_generation != source.generation
        || build.expected_device_id != source.device_id
        || build.expected_inode != source.inode
        || build.expected_owning_thread_id.as_deref() != source.thread_id.as_deref()
        || build.expected_root_session_id.as_deref() != root
        || canonical_algorithm_for(build.target_parser_version).is_none()
    {
        return Ok(false);
    }
    let active_state = read_usage_source_state(transaction, epoch.active_epoch, source_file_id)?;
    let Some(active_state) = active_state else {
        return Ok(false);
    };
    let active_fingerprint = match crate::codex::storage::rebuild::active_state_fingerprint(
        transaction,
        epoch.active_epoch,
        source_file_id,
    ) {
        Ok(fingerprint) => fingerprint,
        Err(crate::codex::storage::rebuild::RebuildError::Invalid(
            "usage reconciliation carry version requires rebuild",
        )) => return Ok(false),
        Err(error) => return Err(rebuild_storage_error(error)),
    };
    if active_fingerprint != build.active_state_fingerprint {
        return Ok(false);
    }
    // During a build the shared source checkpoint belongs to the working
    // epoch, so the frozen active boundary/guard in the manifest is the
    // authoritative active checkpoint proof.
    let active_tail_ok = active_state.resolved_through_offset == build.active_committed_offset
        && active_state.file_generation == build.expected_file_generation
        && active_state.device_id == build.expected_device_id
        && active_state.inode == build.expected_inode
        && active_state.owning_thread_id
            == build
                .expected_owning_thread_id
                .as_deref()
                .unwrap_or_default()
        && active_state.root_session_id
            == build
                .expected_root_session_id
                .as_deref()
                .unwrap_or_default()
        && active_state.usage_parser_version == epoch.active_parser_version
        && active_state.canonical_algorithm_version
            == canonical_algorithm_for(epoch.active_parser_version).unwrap_or(-1)
        && active_state.observed_raw_size == build.observed_raw_size
        && active_state.raw_tail_status != UsageTailStatus::Unverified;
    if !active_tail_ok
        || build
            .active_guard_hash
            .as_ref()
            .is_some_and(|g| g.len() != 32)
    {
        return Ok(false);
    }
    let fresh = checkpoint.is_some_and(|cp| {
        cp.processing_status == CheckpointProcessingStatus::RebuildRequired
            && cp.committed_offset == 0
    }) && working_state.is_none();
    let partial = checkpoint
        .is_some_and(|cp| cp.processing_status == CheckpointProcessingStatus::Ready)
        && working_state.is_some_and(|state| {
            state.resolved_through_offset <= build.active_committed_offset
                && state.file_generation == build.expected_file_generation
                && state.usage_parser_version == build.target_parser_version
        });
    Ok(fresh || partial)
}

fn verify_carry_db_proof(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    source_file_id: i64,
    build: &UsageBuildPlanState,
) -> StorageResult<()> {
    if build.target_parser_version != epoch.active_parser_version
        || build.required_through_offset != build.active_committed_offset
        || build.carry_phase == UsageCarryPhase::None
    {
        return Err(StorageError::invalid_state(
            "usage carry frozen proof changed",
        ));
    }
    let active_state = read_usage_source_state(transaction, epoch.active_epoch, source_file_id)?
        .ok_or_else(|| StorageError::invalid_state("active usage source state is missing"))?;
    let fingerprint = crate::codex::storage::rebuild::active_state_fingerprint(
        transaction,
        epoch.active_epoch,
        source_file_id,
    )
    .map_err(rebuild_storage_error)?;
    if fingerprint != build.active_state_fingerprint
        || active_state.resolved_through_offset != build.active_committed_offset
        || active_state.file_generation != build.expected_file_generation
        || active_state.device_id != build.expected_device_id
        || active_state.inode != build.expected_inode
        || Some(active_state.owning_thread_id.as_str())
            != build.expected_owning_thread_id.as_deref()
        || Some(active_state.root_session_id.as_str()) != build.expected_root_session_id.as_deref()
        || active_state.usage_parser_version != epoch.active_parser_version
        || active_state.canonical_algorithm_version
            != canonical_algorithm_for(epoch.active_parser_version).unwrap_or(-1)
    {
        return Err(StorageError::invalid_state(
            "usage carry active proof changed",
        ));
    }
    let checkpoint = read_usage_checkpoint(transaction, source_file_id)?
        .ok_or_else(|| StorageError::invalid_state("usage carry checkpoint is missing"))?;
    if checkpoint.parser_version != epoch.working_parser_version()
        || checkpoint.committed_offset != 0
        || checkpoint.guard_hash.is_some()
        || checkpoint.processing_status != CheckpointProcessingStatus::RebuildRequired
    {
        return Err(StorageError::invalid_state(
            "usage carry checkpoint is resumable",
        ));
    }
    let working_state_count: i64 = transaction.query_row(
        "SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![build.build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    if working_state_count != 0 {
        return Err(StorageError::invalid_state(
            "usage carry retained working source state",
        ));
    }
    Ok(())
}

const CARRY_PAGE_ROWS: i64 = 2048;

fn carry_occurrence_event_ids(
    transaction: &Connection,
    active_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
) -> StorageResult<Vec<String>> {
    let mut statement = transaction.prepare(
        "SELECT DISTINCT event_id FROM codex_usage_event_occurrences
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3
         ORDER BY event_id",
    )?;
    statement
        .query_map(
            params![active_epoch, source_file_id, file_generation],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn carry_occurrence_page(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let (generation, after): (i64, Option<i64>) = transaction.query_row(
        "SELECT expected_file_generation,carry_after_start_offset FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='occurrences'",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT source_start_offset FROM (
             SELECT source_start_offset FROM codex_usage_event_occurrences
              WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
                AND file_generation=?3
             UNION
             SELECT source_start_offset FROM codex_skill_usage_events
              WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
         ) WHERE (?4 IS NULL OR source_start_offset>?4)
         ORDER BY source_start_offset LIMIT ?5",
    )?;
    let rows = statement
        .query_map(
            params![
                active_epoch,
                source_file_id,
                generation,
                after,
                CARRY_PAGE_ROWS + 1
            ],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let copy = &rows[..rows.len().min(CARRY_PAGE_ROWS as usize)];
    for start in copy {
        let event_id: Option<String> = transaction
            .query_row(
                "SELECT event_id FROM codex_usage_event_occurrences
                 WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
                   AND file_generation=?3 AND source_start_offset=?4",
                params![active_epoch, source_file_id, generation, start],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(event_id) = event_id {
            carry_occurrence(
                transaction,
                active_epoch,
                build_epoch,
                source_file_id,
                generation,
                *start,
            )?;
        }
        carry_skill_events_at_offset(
            transaction,
            active_epoch,
            build_epoch,
            source_file_id,
            generation,
            *start,
        )?;
    }
    let next_after = copy.last().copied().or(after);
    let (next_phase, next_cursor) = if has_more {
        ("occurrences", next_after)
    } else {
        ("facts", None)
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_phase=?1,carry_after_start_offset=?2,
                carry_after_fact_event_id=NULL,carry_after_marker_start_offset=NULL,
                carry_after_window_start_offset=NULL,carry_after_turn_key=NULL,
                carry_after_anomaly_id=NULL,updated_at_ms=?3
         WHERE build_epoch=?4 AND source_file_id=?5 AND carry_phase='occurrences'
           AND carry_after_start_offset IS ?6",
        params![
            next_phase,
            next_cursor,
            now_ms,
            build_epoch,
            source_file_id,
            after
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry occurrence cursor CAS failed",
        ));
    }
    Ok(())
}

fn carry_occurrence(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    start_offset: i64,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO codex_usage_event_occurrences(
            source,ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms)
         SELECT 'codex',?1,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms
         FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3
           AND file_generation=?4 AND source_start_offset=?5
           AND NOT EXISTS(SELECT 1 FROM codex_usage_event_occurrences
                          WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3
                            AND file_generation=?4 AND source_start_offset=?5)",
        params![build_epoch, active_epoch, source_file_id, file_generation, start_offset],
    )?;
    let equal: i64 = transaction.query_row(
        "SELECT count(*) FROM codex_usage_event_occurrences a
         JOIN codex_usage_event_occurrences b ON b.source='codex' AND b.ledger_epoch=?2
           AND b.source_file_id=a.source_file_id AND b.file_generation=a.file_generation
           AND b.source_start_offset=a.source_start_offset
         WHERE a.source='codex' AND a.ledger_epoch=?1 AND a.source_file_id=?3
           AND a.file_generation=?5 AND a.source_start_offset=?4
           AND b.source_end_offset=a.source_end_offset AND b.event_id=a.event_id",
        params![
            active_epoch,
            build_epoch,
            source_file_id,
            start_offset,
            file_generation
        ],
        |row| row.get(0),
    )?;
    if equal != 1 {
        return Err(StorageError::usage_conflict(
            "usage carry occurrence conflict",
        ));
    }
    Ok(())
}

fn carry_fact_page_event_ids(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
) -> StorageResult<(Option<String>, Vec<String>, bool)> {
    let (generation, after): (i64, Option<String>) = transaction.query_row(
        "SELECT expected_file_generation,carry_after_fact_event_id FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='facts'",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT DISTINCT f.event_id FROM codex_usage_event_facts f
         WHERE f.source='codex' AND f.ledger_epoch=?1 AND (?3 IS NULL OR f.event_id>?3)
           AND (EXISTS(
                SELECT 1 FROM codex_usage_event_occurrences o
                WHERE o.source=f.source AND o.ledger_epoch=f.ledger_epoch
                  AND o.source_file_id=?2 AND o.file_generation=?4
                  AND o.event_id=f.event_id)
             OR EXISTS(
                SELECT 1 FROM codex_compaction_markers m
                WHERE m.source=f.source AND m.ledger_epoch=f.ledger_epoch
                  AND m.source_file_id=?2 AND m.file_generation=?4
                  AND m.resolved_event_id=f.event_id))
         ORDER BY f.event_id LIMIT ?5",
    )?;
    let rows = statement
        .query_map(
            params![
                active_epoch,
                source_file_id,
                after,
                generation,
                CARRY_PAGE_ROWS + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let event_ids = rows
        .into_iter()
        .take(CARRY_PAGE_ROWS as usize)
        .collect::<Vec<_>>();
    Ok((after, event_ids, has_more))
}

fn carry_fact_page(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    event_ids: &[String],
    has_more: bool,
    after: Option<String>,
    now_ms: i64,
) -> StorageResult<()> {
    let generation: i64 = transaction.query_row(
        "SELECT expected_file_generation FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='facts'",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    for event_id in event_ids {
        let canonical_exists: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_events
             WHERE source='codex' AND source_epoch=?1 AND event_id=?2)",
            params![build_epoch, event_id],
            |row| row.get(0),
        )?;
        if canonical_exists != 1 {
            return Err(StorageError::usage_conflict(
                "carried fact canonical event is missing",
            ));
        }
        transaction.execute(
            "INSERT INTO codex_usage_event_holds(
                source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
             VALUES ('codex',?1,?2,?3,?4,'carry')
             ON CONFLICT(source,ledger_epoch,source_file_id,file_generation,event_id)
             DO UPDATE SET hold_reason='carry'",
            params![build_epoch, source_file_id, generation, event_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO codex_usage_event_facts(
                source,ledger_epoch,event_id,owning_thread_id,response_id,evidence_kind,operation)
             SELECT source,?1,event_id,owning_thread_id,response_id,evidence_kind,operation
             FROM codex_usage_event_facts
             WHERE source='codex' AND ledger_epoch=?2 AND event_id=?3",
            params![build_epoch, active_epoch, event_id],
        )?;
        transaction.execute(
            "UPDATE codex_usage_event_facts AS build_fact
             SET operation='compaction'
             WHERE build_fact.source='codex' AND build_fact.ledger_epoch=?1
               AND build_fact.event_id=?3 AND build_fact.operation='response'
               AND EXISTS (
                 SELECT 1 FROM codex_usage_event_facts active_fact
                 WHERE active_fact.source='codex' AND active_fact.ledger_epoch=?2
                   AND active_fact.event_id=?3 AND active_fact.operation='compaction'
                   AND active_fact.owning_thread_id=build_fact.owning_thread_id
                   AND active_fact.response_id IS build_fact.response_id
                   AND active_fact.evidence_kind=build_fact.evidence_kind
               )",
            params![build_epoch, active_epoch, event_id],
        )?;
        let identical: i64 = transaction.query_row(
            "SELECT count(*) FROM codex_usage_event_facts a
             JOIN codex_usage_event_facts b
               ON b.source=a.source AND b.ledger_epoch=?2 AND b.event_id=a.event_id
             WHERE a.source='codex' AND a.ledger_epoch=?1 AND a.event_id=?3
               AND b.owning_thread_id=a.owning_thread_id
               AND b.response_id IS a.response_id AND b.evidence_kind=a.evidence_kind
               AND (b.operation=a.operation
                    OR (a.operation='response' AND b.operation='compaction'))",
            params![active_epoch, build_epoch, event_id],
            |row| row.get(0),
        )?;
        if identical != 1 {
            return Err(StorageError::usage_conflict("usage carry fact conflict"));
        }
    }
    let next_after = event_ids.last().cloned().or(after.clone());
    let (next_phase, next_cursor) = if has_more {
        ("facts", next_after)
    } else {
        ("markers", None)
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_phase=?1,
                carry_after_start_offset=NULL,carry_after_fact_event_id=?2,
                carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,
                carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?3
         WHERE build_epoch=?4 AND source_file_id=?5 AND carry_phase='facts'
           AND carry_after_fact_event_id IS ?6",
        params![
            next_phase,
            next_cursor,
            now_ms,
            build_epoch,
            source_file_id,
            after
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry fact cursor CAS failed",
        ));
    }
    Ok(())
}

fn carry_marker_page(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let (generation, after): (i64, Option<i64>) = transaction.query_row(
        "SELECT expected_file_generation,carry_after_marker_start_offset
         FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='markers'",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT source_start_offset FROM codex_compaction_markers
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3 AND (?4 IS NULL OR source_start_offset>?4)
         ORDER BY source_start_offset LIMIT ?5",
    )?;
    let rows = statement
        .query_map(
            params![
                active_epoch,
                source_file_id,
                generation,
                after,
                CARRY_PAGE_ROWS + 1
            ],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let offsets = rows
        .into_iter()
        .take(CARRY_PAGE_ROWS as usize)
        .collect::<Vec<_>>();
    for offset in &offsets {
        transaction.execute(
            "INSERT OR IGNORE INTO codex_compaction_markers(
                source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                reasoning_effort,response_id,resolved_event_id,unknown_reason)
             SELECT source,?1,source_file_id,file_generation,source_start_offset,
                source_end_offset,owning_thread_id,root_session_id,occurred_at_ms,model,
                reasoning_effort,response_id,resolved_event_id,unknown_reason
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?4 AND source_start_offset=?5",
            params![
                build_epoch,
                active_epoch,
                source_file_id,
                generation,
                offset
            ],
        )?;
        let identical: i64 = transaction.query_row(
            "SELECT count(*) FROM codex_compaction_markers a
             JOIN codex_compaction_markers b
               ON b.source=a.source AND b.ledger_epoch=?2
              AND b.source_file_id=a.source_file_id AND b.file_generation=a.file_generation
              AND b.source_start_offset=a.source_start_offset
             WHERE a.source='codex' AND a.ledger_epoch=?1 AND a.source_file_id=?3
               AND a.file_generation=?4 AND a.source_start_offset=?5
               AND b.source_end_offset=a.source_end_offset
               AND b.owning_thread_id=a.owning_thread_id AND b.root_session_id=a.root_session_id
               AND b.occurred_at_ms IS a.occurred_at_ms AND b.model IS a.model
               AND b.reasoning_effort IS a.reasoning_effort AND b.response_id IS a.response_id
               AND b.resolved_event_id IS a.resolved_event_id
               AND b.unknown_reason IS a.unknown_reason",
            params![
                active_epoch,
                build_epoch,
                source_file_id,
                generation,
                offset
            ],
            |row| row.get(0),
        )?;
        if identical != 1 {
            return Err(StorageError::usage_conflict("usage carry marker conflict"));
        }
    }
    let next_after = offsets.last().copied().or(after);
    let (next_phase, next_cursor) = if has_more {
        ("markers", next_after)
    } else {
        ("windows", None)
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_phase=?1,
                carry_after_start_offset=NULL,carry_after_fact_event_id=NULL,
                carry_after_marker_start_offset=?2,carry_after_window_start_offset=NULL,
                carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?3
         WHERE build_epoch=?4 AND source_file_id=?5 AND carry_phase='markers'
           AND carry_after_marker_start_offset IS ?6",
        params![
            next_phase,
            next_cursor,
            now_ms,
            build_epoch,
            source_file_id,
            after
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry marker cursor CAS failed",
        ));
    }
    Ok(())
}

fn carry_window_page(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let (generation, after): (i64, Option<i64>) = transaction.query_row(
        "SELECT expected_file_generation,carry_after_window_start_offset
         FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='windows'",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT source_start_offset,state_json FROM codex_usage_reconciliation_windows
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3 AND (?4 IS NULL OR source_start_offset>?4)
         ORDER BY source_start_offset LIMIT ?5",
    )?;
    let rows = statement
        .query_map(
            params![
                active_epoch,
                source_file_id,
                generation,
                after,
                CARRY_PAGE_ROWS + 1
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let windows = rows
        .into_iter()
        .take(CARRY_PAGE_ROWS as usize)
        .collect::<Vec<_>>();
    for (offset, state_json) in &windows {
        canonical_window_state(state_json)?;
        transaction.execute(
            "INSERT OR IGNORE INTO codex_usage_reconciliation_windows(
                source,ledger_epoch,source_file_id,file_generation,source_start_offset,
                source_end_offset,owning_thread_id,turn_key,state_json)
             SELECT source,?1,source_file_id,file_generation,source_start_offset,
                source_end_offset,owning_thread_id,turn_key,state_json
             FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?4 AND source_start_offset=?5",
            params![
                build_epoch,
                active_epoch,
                source_file_id,
                generation,
                offset
            ],
        )?;
        let identical: i64 = transaction.query_row(
            "SELECT count(*) FROM codex_usage_reconciliation_windows a
             JOIN codex_usage_reconciliation_windows b
               ON b.source=a.source AND b.ledger_epoch=?2
              AND b.source_file_id=a.source_file_id AND b.file_generation=a.file_generation
              AND b.source_start_offset=a.source_start_offset
             WHERE a.source='codex' AND a.ledger_epoch=?1 AND a.source_file_id=?3
               AND a.file_generation=?4 AND a.source_start_offset=?5
               AND b.source_end_offset=a.source_end_offset
               AND b.owning_thread_id=a.owning_thread_id AND b.turn_key IS a.turn_key
               AND b.state_json=a.state_json",
            params![
                active_epoch,
                build_epoch,
                source_file_id,
                generation,
                offset
            ],
            |row| row.get(0),
        )?;
        if identical != 1 {
            return Err(StorageError::usage_conflict("usage carry window conflict"));
        }
    }
    let next_after = windows.last().map(|(offset, _)| *offset).or(after);
    let (next_phase, next_cursor) = if has_more {
        ("windows", next_after)
    } else {
        ("turns", None)
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_phase=?1,
                carry_after_start_offset=NULL,carry_after_fact_event_id=NULL,
                carry_after_marker_start_offset=NULL,carry_after_window_start_offset=?2,
                carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?3
         WHERE build_epoch=?4 AND source_file_id=?5 AND carry_phase='windows'
           AND carry_after_window_start_offset IS ?6",
        params![
            next_phase,
            next_cursor,
            now_ms,
            build_epoch,
            source_file_id,
            after
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry window cursor CAS failed",
        ));
    }
    Ok(())
}

fn carry_turn_page(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let (generation, after): (i64, Option<String>) = transaction.query_row(
        "SELECT expected_file_generation,carry_after_turn_key FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='turns'",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT turn_key FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3 AND (?4 IS NULL OR turn_key>?4)
         ORDER BY turn_key LIMIT ?5",
    )?;
    let rows = statement
        .query_map(
            params![
                active_epoch,
                source_file_id,
                generation,
                after,
                CARRY_PAGE_ROWS + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let copy = &rows[..rows.len().min(CARRY_PAGE_ROWS as usize)];
    for turn_key in copy {
        carry_turn(
            transaction,
            active_epoch,
            build_epoch,
            source_file_id,
            generation,
            turn_key,
        )?;
    }
    let next_after = copy.last().cloned().or(after.clone());
    let (next_phase, next_cursor) = if has_more {
        ("turns", next_after)
    } else {
        ("anomalies", None)
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_phase=?1,carry_after_turn_key=?2,
                carry_after_start_offset=NULL,carry_after_fact_event_id=NULL,
                carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,
                carry_after_anomaly_id=NULL,updated_at_ms=?3
         WHERE build_epoch=?4 AND source_file_id=?5 AND carry_phase='turns'
           AND carry_after_turn_key IS ?6",
        params![
            next_phase,
            next_cursor,
            now_ms,
            build_epoch,
            source_file_id,
            after
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry Turn cursor CAS failed",
        ));
    }
    Ok(())
}

fn carry_turn(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    turn_key: &str,
) -> StorageResult<()> {
    let build_exists: bool = transaction
        .query_row(
            "SELECT 1 FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND turn_key=?4",
            params![build_epoch, source_file_id, file_generation, turn_key],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if build_exists {
        let compatible: i64 = transaction.query_row(
            "SELECT count(*) FROM codex_turns a JOIN codex_turns b
               ON b.ledger_epoch=?2 AND b.source_file_id=a.source_file_id
              AND b.file_generation=a.file_generation AND b.turn_key=a.turn_key
             WHERE a.ledger_epoch=?1 AND a.source_file_id=?3
               AND a.file_generation=?5 AND a.turn_key=?4
               AND b.thread_id=a.thread_id AND b.raw_turn_id IS a.raw_turn_id
               AND b.started_at_ms IS a.started_at_ms AND b.start_offset=a.start_offset
               AND b.start_total_input_tokens IS a.start_total_input_tokens
               AND b.start_total_cached_tokens IS a.start_total_cached_tokens
               AND b.start_total_cache_write_tokens IS a.start_total_cache_write_tokens
               AND b.start_total_output_tokens IS a.start_total_output_tokens
               AND b.start_total_reasoning_tokens IS a.start_total_reasoning_tokens
               AND b.start_total_total_tokens IS a.start_total_total_tokens
               AND b.start_total_fingerprint IS a.start_total_fingerprint
               AND (b.status='open' OR (b.status=a.status AND b.end_offset IS a.end_offset AND b.ended_at_ms IS a.ended_at_ms))
               AND b.accounted_candidate_count<=a.accounted_candidate_count
               AND b.state_through_offset<=a.state_through_offset
               AND b.unresolved_model_seen<=a.unresolved_model_seen
               AND b.unresolved_reasoning_effort_seen<=a.unresolved_reasoning_effort_seen
               AND b.block_start_missing<=a.block_start_missing AND b.block_time_missing<=a.block_time_missing
               AND b.block_reset<=a.block_reset AND b.block_ownership_gap<=a.block_ownership_gap
               AND b.block_parser_gap<=a.block_parser_gap AND b.block_required_invalid<=a.block_required_invalid
               AND b.block_model_unresolved<=a.block_model_unresolved
               AND ((b.model_state='none') OR (b.model_state='single' AND
                    ((a.model_state='single' AND b.single_model=a.single_model) OR a.model_state='mixed'))
                    OR (b.model_state='mixed' AND a.model_state='mixed'))
               AND ((b.reasoning_effort_state='none') OR (b.reasoning_effort_state='single' AND
                    ((a.reasoning_effort_state='single'
                        AND b.single_reasoning_effort=a.single_reasoning_effort)
                     OR a.reasoning_effort_state='mixed'))
                    OR (b.reasoning_effort_state='mixed' AND a.reasoning_effort_state='mixed'))",
            params![active_epoch, build_epoch, source_file_id, turn_key, file_generation],
            |row| row.get(0),
        )?;
        if compatible != 1 {
            return Err(StorageError::usage_conflict(
                "usage carry Turn seed conflict",
            ));
        }
        transaction.execute(
            "DELETE FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND turn_key=?4",
            params![build_epoch, source_file_id, file_generation, turn_key],
        )?;
    }
    let changed = transaction.execute(
        "INSERT INTO codex_turns SELECT ?1,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,
            started_at_ms,ended_at_ms,start_offset,end_offset,status,
            start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
            start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,
            start_total_fingerprint,
            last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
            last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,
            accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
            accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
            accounted_candidate_count,model_state,single_model,unresolved_model_seen,
            reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,compensation_allowed,
            block_start_missing,block_time_missing,block_reset,block_ownership_gap,block_parser_gap,
            block_required_invalid,block_model_unresolved,quality_status,state_through_offset,updated_at_ms
         FROM codex_turns WHERE ledger_epoch=?2 AND source_file_id=?3
           AND file_generation=?4 AND turn_key=?5",
        params![build_epoch, active_epoch, source_file_id, file_generation, turn_key],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry active Turn is missing",
        ));
    }
    Ok(())
}

fn carry_anomaly_page(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let (generation, after): (i64, Option<String>) = transaction.query_row(
        "SELECT expected_file_generation,carry_after_anomaly_id FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='anomalies'",
        params![build_epoch, source_file_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = transaction.prepare(
        "SELECT anomaly_id FROM codex_ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3 AND (?4 IS NULL OR anomaly_id>?4)
         ORDER BY anomaly_id LIMIT ?5",
    )?;
    let rows = statement
        .query_map(
            params![
                active_epoch,
                source_file_id,
                generation,
                after,
                CARRY_PAGE_ROWS + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let copy = &rows[..rows.len().min(CARRY_PAGE_ROWS as usize)];
    for anomaly_id in copy {
        carry_anomaly(
            transaction,
            active_epoch,
            build_epoch,
            source_file_id,
            generation,
            anomaly_id,
        )?;
    }
    let next_after = copy.last().cloned().or(after.clone());
    let (next_phase, next_cursor) = if has_more {
        ("anomalies", next_after)
    } else {
        ("finalize", None)
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET carry_phase=?1,carry_after_anomaly_id=?2,
                carry_after_start_offset=NULL,carry_after_fact_event_id=NULL,
                carry_after_marker_start_offset=NULL,carry_after_window_start_offset=NULL,
                carry_after_turn_key=NULL,updated_at_ms=?3
         WHERE build_epoch=?4 AND source_file_id=?5 AND carry_phase='anomalies'
           AND carry_after_anomaly_id IS ?6",
        params![
            next_phase,
            next_cursor,
            now_ms,
            build_epoch,
            source_file_id,
            after
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry anomaly cursor CAS failed",
        ));
    }
    Ok(())
}

fn carry_anomaly(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    anomaly_id: &str,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO codex_ingest_anomalies(
            ledger_epoch,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
            file_generation,source_start_offset,anomaly_type,severity,details_json,resolved)
         SELECT ?1,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
            file_generation,source_start_offset,anomaly_type,severity,details_json,resolved
         FROM codex_ingest_anomalies WHERE ledger_epoch=?2 AND source_file_id=?3
           AND file_generation=?5 AND anomaly_id=?4
           AND NOT EXISTS(SELECT 1 FROM codex_ingest_anomalies WHERE ledger_epoch=?1 AND anomaly_id=?4)",
        params![build_epoch, active_epoch, source_file_id, anomaly_id, file_generation],
    )?;
    let equal: i64 = transaction.query_row(
        "SELECT count(*) FROM codex_ingest_anomalies a JOIN codex_ingest_anomalies b
           ON b.ledger_epoch=?2 AND b.anomaly_id=a.anomaly_id
         WHERE a.ledger_epoch=?1 AND a.source_file_id=?3 AND a.anomaly_id=?4
           AND a.file_generation=?5 AND b.file_generation=?5
           AND b.occurred_at_ms IS a.occurred_at_ms AND b.thread_id IS a.thread_id
           AND b.source_file_id IS a.source_file_id AND b.file_generation IS a.file_generation
           AND b.source_start_offset IS a.source_start_offset AND b.anomaly_type=a.anomaly_type
           AND b.severity=a.severity AND b.details_json=a.details_json AND b.resolved=a.resolved",
        params![
            active_epoch,
            build_epoch,
            source_file_id,
            anomaly_id,
            file_generation
        ],
        |row| row.get(0),
    )?;
    if equal != 1 {
        return Err(StorageError::usage_conflict("usage carry anomaly conflict"));
    }
    Ok(())
}

fn finalize_carry(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    source_file_id: i64,
    build: &UsageBuildPlanState,
    now_ms: i64,
) -> StorageResult<CarryStepOutcome> {
    verify_carry_sets(
        transaction,
        epoch.active_epoch,
        build.build_epoch,
        source_file_id,
    )?;
    let active_state = read_usage_source_state(transaction, epoch.active_epoch, source_file_id)?
        .ok_or_else(|| StorageError::invalid_state("active usage source state is missing"))?;
    let source: (String, i64, i64, i64, i64) = transaction.query_row(
        "SELECT file_status,file_generation,device_id,inode,observed_size FROM codex_source_files WHERE source_file_id=?1",
        [source_file_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    )?;
    if source.1 != build.expected_file_generation
        || source.2 != build.expected_device_id
        || source.3 != build.expected_inode
    {
        return Err(StorageError::invalid_state(
            "usage carry source identity changed",
        ));
    }
    let present = source.0 == "present";
    // Carry reuses the frozen *active* prefix. A partial BuildFrom seed is
    // allowed to have an unverified working tail; that cannot erase the
    // independently durable active tail proof. Conversely, a present source
    // whose raw size changed no longer has the same active raw view and must
    // not later finalize as carried if it disappears again.
    let active_tail_verified = active_state.raw_tail_status != UsageTailStatus::Unverified
        && active_state.observed_raw_size == build.observed_raw_size;
    let can_carry_missing = !present
        && active_tail_verified
        && build.required_through_offset == build.active_committed_offset;

    let mut restored = active_state.clone();
    restored.usage_parser_version = epoch.working_parser_version();
    restored.canonical_algorithm_version = canonical_algorithm_for(epoch.working_parser_version())
        .ok_or_else(|| StorageError::invalid_state("usage canonical parser mapping is missing"))?;
    restored.updated_at_ms = now_ms;
    if present {
        restored.observed_raw_size = source.4;
        if source.4 != active_state.observed_raw_size {
            restored.raw_tail_status = UsageTailStatus::Unverified;
            restored.raw_tail_start_offset = None;
        }
    } else if !active_tail_verified {
        restored.observed_raw_size = source.4;
        restored.raw_tail_status = UsageTailStatus::Unverified;
        restored.raw_tail_start_offset = None;
    }
    write_source_state_row(transaction, build.build_epoch, source_file_id, &restored)?;
    let changed = transaction.execute(
        "UPDATE codex_source_checkpoints SET parser_version=?1,committed_offset=?2,guard_hash=?3,
                processing_status='ready',last_successful_scan_at_ms=?4,last_error_code=NULL
         WHERE source_file_id=?5 AND consumer_kind='usage' AND processing_status='rebuild_required'
           AND committed_offset=0 AND guard_hash IS NULL",
        params![
            epoch.working_parser_version(),
            build.active_committed_offset,
            build.active_guard_hash,
            now_ms,
            source_file_id
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry checkpoint finalize CAS failed",
        ));
    }
    let (completion, error, completed_generation, completed_offset, outcome) = if present {
        (
            "pending",
            None::<&str>,
            None::<i64>,
            None::<i64>,
            CarryStepOutcome::FinalizedPresent,
        )
    } else if can_carry_missing {
        (
            "carried",
            None,
            Some(build.expected_file_generation),
            Some(build.active_committed_offset),
            CarryStepOutcome::FinalizedMissing,
        )
    } else {
        (
            "blocked",
            Some("SOURCE_MISSING_WITH_UNVERIFIED_TAIL"),
            None,
            None,
            CarryStepOutcome::FinalizedMissing,
        )
    };
    let changed = transaction.execute(
        "UPDATE codex_usage_build_sources SET completion_status=?1,completion_error_code=?2,
                completed_generation=?3,completed_through_offset=?4,
                carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                carry_after_fact_event_id=NULL,carry_after_marker_start_offset=NULL,
                carry_after_window_start_offset=NULL,carry_after_turn_key=NULL,
                carry_after_anomaly_id=NULL,updated_at_ms=?5
         WHERE build_epoch=?6 AND source_file_id=?7 AND carry_phase='finalize'",
        params![
            completion,
            error,
            completed_generation,
            completed_offset,
            now_ms,
            build.build_epoch,
            source_file_id
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry finalize manifest CAS failed",
        ));
    }
    transaction.execute(
        "DELETE FROM codex_usage_event_holds
         WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3 AND hold_reason='carry'",
        params![
            build.build_epoch,
            source_file_id,
            build.expected_file_generation
        ],
    )?;
    Ok(outcome)
}

fn verify_carry_sets(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
) -> StorageResult<()> {
    let generation: i64 = transaction.query_row(
        "SELECT expected_file_generation FROM codex_usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='finalize'",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    verify_carry_canonical_events(transaction, build_epoch)?;
    let occurrence_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM codex_usage_event_occurrences
              WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3 AND file_generation=?4
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM codex_usage_event_occurrences
              WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3 AND file_generation=?4))
        + (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM codex_usage_event_occurrences
              WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3 AND file_generation=?4
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM codex_usage_event_occurrences
              WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3 AND file_generation=?4))",
        params![active_epoch, build_epoch, source_file_id, generation],
        |row| row.get(0),
    )?;
    if occurrence_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry occurrence set mismatch",
        ));
    }
    let skill_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3
               AND file_generation=?4
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?4))
        + (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?4
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3
               AND file_generation=?4))",
        params![active_epoch, build_epoch, source_file_id, generation],
        |row| row.get(0),
    )?;
    if skill_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry Skill set mismatch",
        ));
    }
    let marker_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,
                    response_id,resolved_event_id,unknown_reason
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3 AND file_generation=?4
             EXCEPT
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,
                    response_id,resolved_event_id,unknown_reason
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3 AND file_generation=?4))
        + (SELECT count(*) FROM (
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,
                    response_id,resolved_event_id,unknown_reason
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3 AND file_generation=?4
             EXCEPT
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,root_session_id,occurred_at_ms,model,reasoning_effort,
                    response_id,resolved_event_id,unknown_reason
             FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3 AND file_generation=?4))",
        params![active_epoch, build_epoch, source_file_id, generation],
        |row| row.get(0),
    )?;
    if marker_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry Compaction marker set mismatch",
        ));
    }
    let window_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,turn_key,state_json
             FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3 AND file_generation=?4
             EXCEPT
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,turn_key,state_json
             FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3 AND file_generation=?4))
        + (SELECT count(*) FROM (
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,turn_key,state_json
             FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?2 AND source_file_id=?3 AND file_generation=?4
             EXCEPT
             SELECT source,file_generation,source_start_offset,source_end_offset,
                    owning_thread_id,turn_key,state_json
             FROM codex_usage_reconciliation_windows
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?3 AND file_generation=?4))",
        params![active_epoch, build_epoch, source_file_id, generation],
        |row| row.get(0),
    )?;
    if window_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry reconciliation window set mismatch",
        ));
    }
    let fact_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM codex_usage_event_facts a
           WHERE a.source='codex' AND a.ledger_epoch=?1
             AND (EXISTS(
                   SELECT 1 FROM codex_usage_event_occurrences o
                   WHERE o.source=a.source AND o.ledger_epoch=a.ledger_epoch
                     AND o.source_file_id=?3 AND o.file_generation=?4 AND o.event_id=a.event_id)
               OR EXISTS(
                   SELECT 1 FROM codex_compaction_markers m
                   WHERE m.source=a.source AND m.ledger_epoch=a.ledger_epoch
                     AND m.source_file_id=?3 AND m.file_generation=?4
                     AND m.resolved_event_id=a.event_id))
             AND NOT EXISTS (
               SELECT 1 FROM codex_usage_event_facts b
               WHERE b.source=a.source AND b.ledger_epoch=?2 AND b.event_id=a.event_id
                 AND b.owning_thread_id=a.owning_thread_id
                 AND b.response_id IS a.response_id AND b.evidence_kind=a.evidence_kind
                 AND (a.operation='response' OR b.operation='compaction')))
        + (SELECT count(*) FROM codex_usage_event_facts b
           WHERE b.source='codex' AND b.ledger_epoch=?2
             AND (EXISTS(
                   SELECT 1 FROM codex_usage_event_occurrences o
                   WHERE o.source=b.source AND o.ledger_epoch=b.ledger_epoch
                     AND o.source_file_id=?3 AND o.file_generation=?4 AND o.event_id=b.event_id)
               OR EXISTS(
                   SELECT 1 FROM codex_compaction_markers m
                   WHERE m.source=b.source AND m.ledger_epoch=b.ledger_epoch
                     AND m.source_file_id=?3 AND m.file_generation=?4
                     AND m.resolved_event_id=b.event_id))
             AND NOT EXISTS (
               SELECT 1 FROM codex_usage_event_facts a
               WHERE a.source=b.source AND a.ledger_epoch=?1 AND a.event_id=b.event_id
                 AND a.owning_thread_id=b.owning_thread_id
                 AND a.response_id IS b.response_id AND a.evidence_kind=b.evidence_kind
                 AND (a.operation='response' OR b.operation='compaction')))",
        params![active_epoch, build_epoch, source_file_id, generation],
        |row| row.get(0),
    )?;
    if fact_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry fact set mismatch",
        ));
    }
    let hold_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM codex_usage_event_facts f
           WHERE f.source='codex' AND f.ledger_epoch=?1
             AND (EXISTS (
                   SELECT 1 FROM codex_usage_event_occurrences o
                   WHERE o.source=f.source AND o.ledger_epoch=f.ledger_epoch
                     AND o.source_file_id=?3 AND o.file_generation=?4 AND o.event_id=f.event_id)
               OR EXISTS (
                   SELECT 1 FROM codex_compaction_markers m
                   WHERE m.source=f.source AND m.ledger_epoch=f.ledger_epoch
                     AND m.source_file_id=?3 AND m.file_generation=?4
                     AND m.resolved_event_id=f.event_id))
             AND NOT EXISTS (
                   SELECT 1 FROM codex_usage_event_holds h
                   WHERE h.source='codex' AND h.ledger_epoch=?2 AND h.source_file_id=?3
                     AND h.file_generation=?4 AND h.event_id=f.event_id AND h.hold_reason='carry'))
        + (SELECT count(*) FROM codex_usage_event_holds h
           WHERE h.source='codex' AND h.ledger_epoch=?2 AND h.source_file_id=?3
             AND h.file_generation=?4 AND h.hold_reason='carry'
             AND NOT EXISTS (
               SELECT 1 FROM codex_usage_event_facts f
               WHERE f.source=h.source AND f.ledger_epoch=?1 AND f.event_id=h.event_id
                 AND (EXISTS (
                       SELECT 1 FROM codex_usage_event_occurrences o
                       WHERE o.source=f.source AND o.ledger_epoch=f.ledger_epoch
                         AND o.source_file_id=?3 AND o.file_generation=?4 AND o.event_id=f.event_id)
                   OR EXISTS (
                       SELECT 1 FROM codex_compaction_markers m
                       WHERE m.source=f.source AND m.ledger_epoch=f.ledger_epoch
                         AND m.source_file_id=?3 AND m.file_generation=?4
                         AND m.resolved_event_id=f.event_id))))",
        params![active_epoch, build_epoch, source_file_id, generation],
        |row| row.get(0),
    )?;
    if hold_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry hold set mismatch",
        ));
    }
    // Compare complete active/build rows through deterministic fingerprints in
    // Rust so that updated_at_ms and ledger_epoch are the only excluded fields.
    let active_codex_turns = carry_table_fingerprint(
        transaction,
        "codex_turns",
        active_epoch,
        source_file_id,
        generation,
        &["ledger_epoch", "updated_at_ms"],
    )?;
    let build_codex_turns = carry_table_fingerprint(
        transaction,
        "codex_turns",
        build_epoch,
        source_file_id,
        generation,
        &["ledger_epoch", "updated_at_ms"],
    )?;
    if active_codex_turns != build_codex_turns {
        return Err(StorageError::usage_conflict(
            "usage carry Turn set mismatch",
        ));
    }
    let active_anomalies = carry_table_fingerprint(
        transaction,
        "codex_ingest_anomalies",
        active_epoch,
        source_file_id,
        generation,
        &["ledger_epoch", "detected_at_ms"],
    )?;
    let build_anomalies = carry_table_fingerprint(
        transaction,
        "codex_ingest_anomalies",
        build_epoch,
        source_file_id,
        generation,
        &["ledger_epoch", "detected_at_ms"],
    )?;
    if active_anomalies != build_anomalies {
        return Err(StorageError::usage_conflict(
            "usage carry anomaly set mismatch",
        ));
    }
    Ok(())
}

fn verify_carry_canonical_events(transaction: &Connection, build_epoch: i64) -> StorageResult<()> {
    let extra: Option<String> = transaction
        .query_row(
            "SELECT build.event_id
             FROM usage_events build
             WHERE build.source='codex' AND build.source_epoch=?1
               AND NOT EXISTS (
                   SELECT 1 FROM codex_usage_event_occurrences occurrence
                   WHERE occurrence.source='codex' AND occurrence.ledger_epoch=?1
                     AND occurrence.event_id=build.event_id
               ) AND NOT EXISTS (
                   SELECT 1 FROM codex_compaction_markers marker
                   WHERE marker.source='codex' AND marker.ledger_epoch=?1
                     AND marker.resolved_event_id=build.event_id
               ) AND NOT EXISTS (
                   SELECT 1 FROM codex_usage_event_holds hold
                   WHERE hold.source='codex' AND hold.ledger_epoch=?1
                     AND hold.event_id=build.event_id
               )
             ORDER BY build.event_id
             LIMIT 1",
            [build_epoch],
            |row| row.get(0),
        )
        .optional()?;
    if extra.is_some() {
        return Err(StorageError::usage_conflict(
            "usage carry canonical event set contains an unexpected seed",
        ));
    }
    Ok(())
}

fn carry_table_fingerprint(
    transaction: &Connection,
    table: &str,
    epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    excluded: &[&str],
) -> StorageResult<(i64, Vec<u8>)> {
    if !matches!(table, "codex_turns" | "codex_ingest_anomalies") {
        return Err(StorageError::invalid_state(
            "invalid carry fingerprint table",
        ));
    }
    let mut columns = Vec::new();
    let pragma = format!("PRAGMA table_info({table})");
    let mut statement = transaction.prepare(&pragma)?;
    for row in statement.query_map([], |row| row.get::<_, String>(1))? {
        let column = row?;
        if !excluded.contains(&column.as_str()) {
            columns.push(column);
        }
    }
    let select = columns
        .iter()
        .map(|column| format!("quote({column})"))
        .collect::<Vec<_>>()
        .join("||'|'||");
    let order = if table == "codex_turns" {
        "turn_key"
    } else {
        "anomaly_id"
    };
    let sql = format!(
        "SELECT {select} FROM {table} WHERE ledger_epoch=?1 AND source_file_id=?2
         AND file_generation=?3 ORDER BY {order}"
    );
    let mut statement = transaction.prepare(&sql)?;
    let rows = statement
        .query_map(params![epoch, source_file_id, file_generation], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"usage-carry-set-v1\0");
    for row in &rows {
        hasher.update(&(row.len() as u64).to_be_bytes());
        hasher.update(row.as_bytes());
    }
    Ok((
        i64::try_from(rows.len()).unwrap_or(i64::MAX),
        hasher.finalize().as_bytes().to_vec(),
    ))
}

fn read_usage_checkpoint(
    transaction: &Connection,
    source_file_id: i64,
) -> StorageResult<Option<UsageCheckpointExpectation>> {
    transaction
        .query_row(
            "SELECT parser_version,committed_offset,guard_hash,processing_status
             FROM codex_source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_file_id],
            |row| {
                let status: String = row.get(3)?;
                let processing_status = CheckpointProcessingStatus::try_from(status.as_str())
                    .map_err(super::to_domain_sql_error)?;
                Ok(UsageCheckpointExpectation {
                    parser_version: row.get(0)?,
                    committed_offset: row.get(1)?,
                    guard_hash: row.get(2)?,
                    processing_status,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn read_usage_source_state(
    transaction: &Connection,
    epoch: i64,
    source_file_id: i64,
) -> StorageResult<Option<UsageSourceStateWrite>> {
    let mut state = transaction
        .query_row(
            "SELECT file_generation,device_id,inode,usage_parser_version,
                canonical_algorithm_version,resolved_through_offset,observed_raw_size,
                raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
                previous_total_input_tokens,previous_total_cached_tokens,
                previous_total_cache_write_tokens,previous_total_output_tokens,
                previous_total_reasoning_tokens,previous_total_total_tokens,previous_total_fingerprint,
                previous_total_offset,chain_state,chain_block_reason,active_turn_key,
                active_model,active_model_offset,active_reasoning_effort,active_reasoning_effort_offset,
                updated_at_ms,reconciliation_state_json
             FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![epoch, source_file_id],
            |row| {
                let tail: String = row.get(7)?;
                let continuation: String = row.get(11)?;
                let chain: String = row.get(20)?;
                let reason: Option<String> = row.get(21)?;
                let previous_input: Option<i64> = row.get(12)?;
                let previous_total = match previous_input {
                    None => None,
                    Some(input_tokens) => Some(UsageSnapshot {
                        vector: NormalizedTokenUsage::new(
                            input_tokens,
                            row.get(13)?,
                            row.get(14)?,
                            row.get(15)?,
                            row.get(16)?,
                            row.get(17)?,
                        )
                        .map_err(super::to_domain_sql_error)?,
                        fingerprint: row.get(18)?,
                    }),
                };
                Ok(UsageSourceStateWrite {
                    file_generation: row.get(0)?,
                    device_id: row.get(1)?,
                    inode: row.get(2)?,
                    usage_parser_version: row.get(3)?,
                    canonical_algorithm_version: row.get(4)?,
                    resolved_through_offset: row.get(5)?,
                    observed_raw_size: row.get(6)?,
                    raw_tail_status: UsageTailStatus::parse(&tail)
                        .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?,
                    raw_tail_start_offset: row.get(8)?,
                    owning_thread_id: row.get(9)?,
                    root_session_id: row.get(10)?,
                    continuation_state: UsageContinuationState::parse(&continuation)
                        .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?,
                    previous_total,
                    previous_total_offset: row.get(19)?,
                    chain_state: match (chain.as_str(), reason.as_deref()) {
                        ("continuous", None) => UsageChainState::Continuous,
                        ("interrupted", Some(reason)) => UsageChainState::Interrupted(
                            UsageGapReason::parse(reason)
                                .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?,
                        ),
                        _ => return Err(rusqlite::Error::InvalidParameterName(
                            "invalid usage chain state".to_owned(),
                        )),
                    },
                    active_turn_key: row.get(22)?,
                    active_model: row.get(23)?,
                    active_model_offset: row.get(24)?,
                    active_reasoning_effort: row.get(25)?,
                    active_reasoning_effort_offset: row.get(26)?,
                    reconciliation_state_json: row.get(28)?,
                    updated_at_ms: row.get(27)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)?;
    if let Some(state) = &mut state {
        state.reconciliation_state_json =
            canonical_reconciliation_state(&state.reconciliation_state_json)?;
    }
    Ok(state)
}

fn read_open_turn(
    transaction: &Connection,
    epoch: i64,
    source_file_id: i64,
    state: &UsageSourceStateWrite,
) -> StorageResult<Option<crate::codex::ingestion::usage_processor::TurnState>> {
    let mut statement = transaction.prepare(
        "SELECT turn_key,raw_turn_id,started_at_ms,start_offset,
                start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
                start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,
                last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
                last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,
                accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
                accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
                accounted_candidate_count,model_state,single_model,unresolved_model_seen,
                reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
                block_start_missing,block_time_missing,block_reset,block_ownership_gap,
                block_parser_gap,block_required_invalid,block_model_unresolved,state_through_offset,
                thread_id,file_generation
         FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2 AND status='open' ORDER BY turn_key",
    )?;
    let mut rows = statement.query(params![epoch, source_file_id])?;
    let Some(row) = rows.next()? else {
        return if state.active_turn_key.is_none() {
            Ok(None)
        } else {
            Err(StorageError::invalid_state("active Turn row is missing"))
        };
    };
    let vector =
        |base: usize, row: &rusqlite::Row<'_>| -> rusqlite::Result<Option<NormalizedTokenUsage>> {
            let input: Option<i64> = row.get(base)?;
            let Some(input) = input else {
                return Ok(None);
            };
            Ok(Some(
                NormalizedTokenUsage::new(
                    input,
                    row.get(base + 1)?,
                    row.get(base + 2)?,
                    row.get(base + 3)?,
                    row.get(base + 4)?,
                    row.get(base + 5)?,
                )
                .map_err(super::to_domain_sql_error)?,
            ))
        };
    let start_total = vector(4, row)?;
    let last_total = vector(11, row)?;
    let accounted = NormalizedTokenUsage::new(
        row.get(18)?,
        row.get(19)?,
        row.get(20)?,
        row.get(21)?,
        row.get(22)?,
        row.get(23)?,
    )
    .map_err(super::to_domain_sql_error)?;
    let model_state: String = row.get(26)?;
    let single_model: Option<String> = row.get(27)?;
    let reasoning_effort_state: String = row.get(29)?;
    let single_reasoning_effort: Option<String> = row.get(30)?;
    let turn = crate::codex::ingestion::usage_processor::TurnState {
        turn_key: row.get(0)?,
        raw_turn_id: row.get(1)?,
        started_at_ms: row.get(2)?,
        start_offset: u64::try_from(row.get::<_, i64>(3)?).map_err(|_| {
            rusqlite::Error::InvalidParameterName("invalid Turn start offset".to_owned())
        })?,
        start_total,
        last_total,
        accounted,
        accounted_candidate_count: u64::try_from(row.get::<_, i64>(25)?).map_err(|_| {
            rusqlite::Error::InvalidParameterName("invalid Turn candidate count".to_owned())
        })?,
        model_state: match (model_state.as_str(), single_model) {
            ("none", None) => crate::codex::ingestion::usage_processor::TurnModelState::None,
            ("single", Some(model)) => {
                crate::codex::ingestion::usage_processor::TurnModelState::Single(model)
            }
            ("mixed", None) => crate::codex::ingestion::usage_processor::TurnModelState::Mixed,
            _ => {
                return Err(StorageError::invalid_state(
                    "invalid persisted Turn model state",
                ));
            }
        },
        unresolved_model_seen: row.get::<_, i64>(28)? != 0,
        reasoning_effort_state: match (reasoning_effort_state.as_str(), single_reasoning_effort) {
            ("none", None) => {
                crate::codex::ingestion::usage_processor::TurnReasoningEffortState::None
            }
            ("single", Some(effort)) => {
                crate::codex::ingestion::usage_processor::TurnReasoningEffortState::Single(effort)
            }
            ("mixed", None) => {
                crate::codex::ingestion::usage_processor::TurnReasoningEffortState::Mixed
            }
            _ => {
                return Err(StorageError::invalid_state(
                    "invalid persisted Turn reasoning-effort state",
                ));
            }
        },
        unresolved_reasoning_effort_seen: row.get::<_, i64>(31)? != 0,
        blocks: crate::codex::ingestion::usage_processor::CompensationBlocks {
            start_missing: row.get::<_, i64>(32)? != 0,
            time_missing: row.get::<_, i64>(33)? != 0,
            reset: row.get::<_, i64>(34)? != 0,
            ownership_gap: row.get::<_, i64>(35)? != 0,
            parser_gap: row.get::<_, i64>(36)? != 0,
            required_invalid: row.get::<_, i64>(37)? != 0,
            model_unresolved: row.get::<_, i64>(38)? != 0,
        },
    };
    let state_through: i64 = row.get(39)?;
    let thread_id: String = row.get(40)?;
    let generation: i64 = row.get(41)?;
    if rows.next()?.is_some()
        || state.active_turn_key.as_deref() != Some(turn.turn_key.as_str())
        || state_through > state.resolved_through_offset
        || thread_id != state.owning_thread_id
        || generation != state.file_generation
    {
        return Err(StorageError::invalid_state(
            "persisted open Turn is inconsistent",
        ));
    }
    Ok(Some(turn))
}

fn validate_batch(batch: &UsageCommitBatch) -> StorageResult<()> {
    if batch.ledger_epoch <= 0 || batch.usage_parser_version < 0 || batch.sources.is_empty() {
        return Err(StorageError::invalid_state(
            "invalid or empty usage commit batch",
        ));
    }
    if batch.thread_id.is_empty() || batch.root_session_id.is_empty() {
        return Err(StorageError::invalid_state(
            "usage group relationship is missing",
        ));
    }
    let mut ids = HashSet::new();
    let mut adapter_bytes = 0i64;
    let mut adapter_lines = 0i64;
    let mut write_units = 0i64;
    for source in &batch.sources {
        if !ids.insert(source.source_file_id) {
            return Err(StorageError::invalid_state("duplicate usage source commit"));
        }
        validate_source_payload(batch, source)?;
        adapter_bytes = adapter_bytes
            .checked_add(source.source_bytes_consumed - source.replayed_prefix_bytes)
            .ok_or_else(|| StorageError::invalid_state("usage group byte count overflow"))?;
        adapter_lines = adapter_lines
            .checked_add(source.complete_line_count - source.replayed_prefix_lines)
            .ok_or_else(|| StorageError::invalid_state("usage group line count overflow"))?;
        write_units = write_units
            .checked_add(source.write_unit_count)
            .ok_or_else(|| StorageError::invalid_state("usage group write-unit count overflow"))?;
    }
    let ordinary = adapter_bytes <= MAX_USAGE_BATCH_BYTES as i64
        && adapter_lines <= MAX_USAGE_BATCH_LINES as i64
        && write_units <= MAX_USAGE_BATCH_WRITE_UNITS as i64;
    let exclusive_progress = batch.sources.len() == 1 && {
        let source = &batch.sources[0];
        let source_adapter_bytes = source.source_bytes_consumed - source.replayed_prefix_bytes;
        let source_adapter_lines = source.complete_line_count - source.replayed_prefix_lines;
        (source_adapter_lines == 1
            && source_adapter_bytes <= MAX_LEGAL_LINE_BYTES as i64
            && source.write_unit_count <= MAX_USAGE_BATCH_WRITE_UNITS as i64)
            || oversized_exclusive_progress(batch, source)
    };
    if !(ordinary || exclusive_progress) {
        return Err(StorageError::invalid_state(
            "usage Thread group exceeds fixed batch budget",
        ));
    }
    Ok(())
}

fn oversized_exclusive_progress(batch: &UsageCommitBatch, source: &UsageSourceCommit) -> bool {
    let Some(adapter_bytes) = source
        .source_bytes_consumed
        .checked_sub(source.replayed_prefix_bytes)
    else {
        return false;
    };
    let Some(adapter_lines) = source
        .complete_line_count
        .checked_sub(source.replayed_prefix_lines)
    else {
        return false;
    };
    if adapter_lines != 1 || adapter_bytes <= MAX_LEGAL_LINE_BYTES as i64 {
        return false;
    }

    let patch = &source.patch;
    let no_other_writes = patch.events.is_empty()
        && patch.occurrences.is_empty()
        && patch.facts.is_empty()
        && patch.marker_upserts.is_empty()
        && patch.window_upserts.is_empty()
        && patch.hold_upserts.is_empty()
        && patch.turn_rewrites.is_empty()
        && patch.delete_event_ids.is_empty()
        && patch.delete_markers.is_empty()
        && patch.delete_windows.is_empty()
        && patch.delete_holds.is_empty();
    if !no_other_writes {
        return false;
    }

    match patch.turn_upserts.as_slice() {
        [] => source.write_unit_count == 0,
        [turn] if source.write_unit_count == 1 => {
            source.updated_state.chain_state
                == UsageChainState::Interrupted(UsageGapReason::Oversized)
                && turn.source_file_id == source.source_file_id
                && turn.file_generation == source.expected_file_generation
                && turn.thread_id == batch.thread_id
                && turn.status == UsageTurnStatus::Open
                && turn.ended_at_ms.is_none()
                && turn.end_offset.is_none()
                && turn.blocks.parser_gap
                && turn.quality_status == "partial"
                && turn.state_through_offset == source.last_complete_offset
                && source
                    .expected_state
                    .as_ref()
                    .is_some_and(|state| {
                        state.file_generation == source.expected_file_generation
                            && state.resolved_through_offset == source.batch_start_offset
                            && state.active_turn_key.as_deref() == Some(turn.turn_key.as_str())
                    })
                && source.updated_state.active_turn_key.as_deref()
                    == Some(turn.turn_key.as_str())
                && source.updated_state.resolved_through_offset == source.last_complete_offset
        }
        _ => false,
    }
}

fn validate_source_payload(
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let counts = patch_counts(&source.patch)?;
    if source.source_file_id <= 0
        || source.expected_file_generation <= 0
        || source.batch_start_offset < 0
        || source.fixed_observed_raw_size < 0
        || source.last_complete_offset < source.batch_start_offset
        || source.last_complete_offset > source.fixed_observed_raw_size
        || source.source_bytes_consumed != source.last_complete_offset - source.batch_start_offset
        || source.complete_line_count < source.replayed_prefix_lines
        || source.source_bytes_consumed < source.replayed_prefix_bytes
        || source.canonical_event_count != counts.0
        || source.occurrence_count != counts.1
        || source.evidence_write_count != counts.2
        || source.write_unit_count != counts.3
        || source.committed_at_ms < 0
        || source.expected_checkpoint.parser_version != batch.usage_parser_version
        || (source.expected_checkpoint.committed_offset == 0)
            != source.expected_checkpoint.guard_hash.is_none()
        || source
            .expected_checkpoint
            .guard_hash
            .as_ref()
            .is_some_and(|guard| guard.len() != 32)
    {
        return Err(StorageError::invalid_state(
            "invalid usage batch count or boundary",
        ));
    }
    let adapter_bytes = source.source_bytes_consumed - source.replayed_prefix_bytes;
    let adapter_lines = source.complete_line_count - source.replayed_prefix_lines;
    let ordinary = adapter_bytes <= MAX_USAGE_BATCH_BYTES as i64
        && adapter_lines <= MAX_USAGE_BATCH_LINES as i64
        && source.write_unit_count <= MAX_USAGE_BATCH_WRITE_UNITS as i64;
    let legal_single = adapter_lines == 1
        && adapter_bytes <= MAX_LEGAL_LINE_BYTES as i64
        && source.write_unit_count <= MAX_USAGE_BATCH_WRITE_UNITS as i64;
    let oversized_only = oversized_exclusive_progress(batch, source);
    if !(ordinary || legal_single || oversized_only) {
        return Err(StorageError::invalid_state(
            "usage batch exceeds fixed budget",
        ));
    }
    match (
        source.fixed_view_exhausted,
        source.tail_status,
        source.tail_start_offset,
    ) {
        (false, UsageTailStatus::Unverified, None) => {}
        (true, UsageTailStatus::None, None)
            if source.last_complete_offset == source.fixed_observed_raw_size => {}
        (true, UsageTailStatus::HalfLine, Some(start))
            if start == source.last_complete_offset && start < source.fixed_observed_raw_size => {}
        _ => return Err(StorageError::invalid_state("invalid fixed-view tail proof")),
    }
    if (!source.local_replay
        && source.batch_start_offset != source.expected_checkpoint.committed_offset)
        || (source.local_replay && source.batch_start_offset != 0)
        || source.updated_state.file_generation != source.expected_file_generation
        || source.updated_state.usage_parser_version != batch.usage_parser_version
        || source.updated_state.canonical_algorithm_version
            != canonical_algorithm_for(batch.usage_parser_version).unwrap_or(-1)
        || source.updated_state.resolved_through_offset != source.last_complete_offset
        || source.updated_state.observed_raw_size != source.fixed_observed_raw_size
        || source.updated_state.raw_tail_status != source.tail_status
        || source.updated_state.raw_tail_start_offset != source.tail_start_offset
        || source.updated_state.owning_thread_id != batch.thread_id
        || source.updated_state.root_session_id != batch.root_session_id
        || (source.last_complete_offset == 0) != source.next_guard_hash.is_none()
        || source
            .next_guard_hash
            .as_ref()
            .is_some_and(|guard| guard.len() != 32)
    {
        return Err(StorageError::invalid_state(
            "usage state/checkpoint payload mismatch",
        ));
    }
    if source.local_replay {
        if !source.fixed_view_exhausted || source.tail_status == UsageTailStatus::Unverified {
            return Err(StorageError::invalid_state(
                "LocalReplay must prove the entire fixed source in one batch",
            ));
        }
    } else {
        match (&source.expected_state, source.batch_start_offset) {
            (None, 0) => {}
            (Some(state), offset)
                if offset > 0
                    && state.resolved_through_offset == offset
                    && state.file_generation == source.expected_file_generation
                    && state.usage_parser_version == batch.usage_parser_version
                    && state.owning_thread_id == batch.thread_id
                    && state.root_session_id == batch.root_session_id => {}
            _ => {
                return Err(StorageError::invalid_state(
                    "usage resume state does not match checkpoint",
                ));
            }
        }
    }
    for skill in &source.skill_events {
        if skill.source_file_id != source.source_file_id
            || skill.file_generation != source.expected_file_generation
            || skill.thread_id != batch.thread_id
            || skill.root_session_id != batch.root_session_id
            || skill.occurred_at_ms < 0
            || skill.skill_name.is_empty()
            || skill.skill_name.len() > 128
            || skill.skill_name.chars().any(char::is_control)
            || skill
                .model
                .as_ref()
                .is_some_and(|model| model.trim().is_empty() || model.chars().any(char::is_control))
            || skill.source_start_offset < source.batch_start_offset
            || skill.source_end_offset > source.last_complete_offset
            || skill.source_end_offset <= skill.source_start_offset
        {
            return Err(StorageError::invalid_state("invalid Skill usage event"));
        }
    }
    for event in &source.patch.events {
        event
            .usage
            .validate()
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        if !valid_hash_id(&event.event_id)
            || event.thread_id != batch.thread_id
            || event.root_session_id != batch.root_session_id
        {
            return Err(StorageError::invalid_state("invalid canonical usage event"));
        }
    }
    for occurrence in &source.patch.occurrences {
        if occurrence.source_file_id <= 0
            || occurrence.file_generation <= 0
            || occurrence.source_start_offset < 0
            || occurrence.source_end_offset <= occurrence.source_start_offset
            || !valid_hash_id(&occurrence.event_id)
        {
            return Err(StorageError::invalid_state(
                "invalid usage event occurrence",
            ));
        }
    }
    validate_patch_keys(&source.patch)?;
    canonical_reconciliation_state(&source.updated_state.reconciliation_state_json)?;
    if source.reconciliation_expected_fingerprint.len() != 32 {
        return Err(StorageError::invalid_state(
            "invalid reconciliation context proof",
        ));
    }
    Ok(())
}

fn patch_counts(patch: &ReconciliationPatchWrite) -> StorageResult<(i64, i64, i64, i64)> {
    let count = |length: usize| {
        i64::try_from(length)
            .map_err(|_| StorageError::invalid_state("usage patch count exceeds SQLite INTEGER"))
    };
    let canonical = count(patch.events.len())?;
    let occurrences = count(patch.occurrences.len())?;
    let evidence = [
        patch.facts.len(),
        patch.marker_upserts.len(),
        patch.window_upserts.len(),
        patch.hold_upserts.len(),
        patch.turn_upserts.len(),
        patch.turn_rewrites.len(),
    ]
    .into_iter()
    .try_fold(0_i64, |total, value| {
        total
            .checked_add(count(value)?)
            .ok_or_else(|| StorageError::invalid_state("usage patch count overflow"))
    })?;
    let deletions = [
        patch.delete_event_ids.len(),
        patch.delete_markers.len(),
        patch.delete_windows.len(),
        patch.delete_holds.len(),
    ]
    .into_iter()
    .try_fold(0_i64, |total, value| {
        total
            .checked_add(count(value)?)
            .ok_or_else(|| StorageError::invalid_state("usage patch count overflow"))
    })?;
    let units = canonical
        .checked_add(occurrences)
        .and_then(|total| total.checked_add(evidence))
        .and_then(|total| total.checked_add(deletions))
        .ok_or_else(|| StorageError::invalid_state("usage patch count overflow"))?;
    Ok((canonical, occurrences, evidence, units))
}

fn validate_patch_keys(patch: &ReconciliationPatchWrite) -> StorageResult<()> {
    let unique = |values: &mut Vec<String>| {
        values.sort();
        values.dedup();
    };
    let mut event_ids = patch
        .events
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<Vec<_>>();
    if event_ids.iter().any(|id| !valid_hash_id(id)) {
        return Err(StorageError::invalid_state("invalid canonical event ID"));
    }
    let original = event_ids.len();
    unique(&mut event_ids);
    if event_ids.len() != original {
        return Err(StorageError::invalid_state(
            "duplicate canonical event write",
        ));
    }
    let mut delete_ids = patch.delete_event_ids.clone();
    let original = delete_ids.len();
    unique(&mut delete_ids);
    if delete_ids.len() != original || delete_ids.iter().any(|id| !valid_hash_id(id)) {
        return Err(StorageError::invalid_state(
            "invalid canonical event deletion",
        ));
    }

    let occurrence_keys = patch
        .occurrences
        .iter()
        .map(|row| {
            (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
            )
        })
        .collect::<BTreeSet<_>>();
    if occurrence_keys.len() != patch.occurrences.len() {
        return Err(StorageError::invalid_state(
            "duplicate occurrence patch key",
        ));
    }
    let fact_ids = patch
        .facts
        .iter()
        .map(|fact| fact.event_id.as_str())
        .collect::<BTreeSet<_>>();
    if fact_ids.len() != patch.facts.len() {
        return Err(StorageError::invalid_state(
            "duplicate usage fact patch key",
        ));
    }
    for fact in &patch.facts {
        if !valid_hash_id(&fact.event_id)
            || fact.owning_thread_id.is_empty()
            || fact.response_id.as_ref().is_some_and(String::is_empty)
            || (fact.evidence_kind == EvidenceKind::Legacy
                && (fact.response_id.is_some() || fact.operation == CodexOperation::Compaction))
            || (fact.evidence_kind == EvidenceKind::Explicit && fact.response_id.is_none())
        {
            return Err(StorageError::invalid_state("invalid usage event fact"));
        }
    }
    let marker_keys = patch
        .marker_upserts
        .iter()
        .map(|row| {
            (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
            )
        })
        .collect::<BTreeSet<_>>();
    if marker_keys.len() != patch.marker_upserts.len()
        || patch.marker_upserts.iter().any(|marker| {
            marker.source_file_id <= 0
                || marker.file_generation <= 0
                || marker.source_start_offset < 0
                || marker.source_end_offset <= marker.source_start_offset
                || marker.owning_thread_id.is_empty()
                || marker.root_session_id.is_empty()
                || marker.model.as_ref().is_some_and(String::is_empty)
                || marker.response_id.as_ref().is_some_and(String::is_empty)
                || marker
                    .resolved_event_id
                    .as_ref()
                    .is_some_and(|id| !valid_hash_id(id))
        })
    {
        return Err(StorageError::invalid_state(
            "invalid Compaction marker patch",
        ));
    }
    let window_keys = patch
        .window_upserts
        .iter()
        .map(|row| {
            (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
            )
        })
        .collect::<BTreeSet<_>>();
    if window_keys.len() != patch.window_upserts.len() {
        return Err(StorageError::invalid_state(
            "duplicate reconciliation window patch key",
        ));
    }
    for window in &patch.window_upserts {
        if window.source_file_id <= 0
            || window.file_generation <= 0
            || window.source_start_offset < 0
            || window.source_end_offset <= window.source_start_offset
            || window.owning_thread_id.is_empty()
        {
            return Err(StorageError::invalid_state(
                "invalid reconciliation window patch",
            ));
        }
        canonical_window_state(&window.state_json)?;
    }
    let hold_keys = patch
        .hold_upserts
        .iter()
        .map(|row| {
            (
                row.source_file_id,
                row.file_generation,
                row.event_id.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    if hold_keys.len() != patch.hold_upserts.len()
        || patch.hold_upserts.iter().any(|hold| {
            hold.source_file_id <= 0 || hold.file_generation <= 0 || !valid_hash_id(&hold.event_id)
        })
    {
        return Err(StorageError::invalid_state("invalid usage hold patch"));
    }
    validate_unique_private_keys(&patch.delete_markers, "marker deletion")?;
    validate_unique_private_keys(&patch.delete_windows, "window deletion")?;
    if patch.delete_holds.iter().collect::<BTreeSet<_>>().len() != patch.delete_holds.len()
        || patch
            .delete_holds
            .iter()
            .any(|(source_id, generation, event_id)| {
                *source_id <= 0 || *generation <= 0 || !valid_hash_id(event_id)
            })
    {
        return Err(StorageError::invalid_state("invalid usage hold deletion"));
    }
    let turn_keys = patch
        .turn_upserts
        .iter()
        .chain(
            patch
                .turn_rewrites
                .iter()
                .map(|rewrite| &rewrite.replacement),
        )
        .map(|turn| {
            (
                turn.source_file_id,
                turn.file_generation,
                turn.turn_key.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let total_turn_writes = patch.turn_upserts.len() + patch.turn_rewrites.len();
    if turn_keys.len() != total_turn_writes {
        return Err(StorageError::invalid_state("duplicate Turn patch key"));
    }
    for turn in patch.turn_upserts.iter().chain(
        patch
            .turn_rewrites
            .iter()
            .flat_map(|rewrite| [&rewrite.expected, &rewrite.replacement]),
    ) {
        validate_turn_write(turn)?;
    }
    for rewrite in &patch.turn_rewrites {
        if turn_primary_key(&rewrite.expected) != turn_primary_key(&rewrite.replacement)
            || rewrite.expected.thread_id != rewrite.replacement.thread_id
        {
            return Err(StorageError::invalid_state("Turn rewrite key changed"));
        }
        let mut replacement_with_expected_derived_fields = rewrite.replacement.clone();
        replacement_with_expected_derived_fields.accounted = rewrite.expected.accounted.clone();
        replacement_with_expected_derived_fields.accounted_candidate_count =
            rewrite.expected.accounted_candidate_count;
        replacement_with_expected_derived_fields.quality_status = rewrite.expected.quality_status;
        replacement_with_expected_derived_fields.updated_at_ms = rewrite.expected.updated_at_ms;
        if replacement_with_expected_derived_fields != rewrite.expected {
            return Err(StorageError::invalid_state(
                "Turn rewrite changed historical state",
            ));
        }
    }
    if patch.delete_markers.iter().any(|key| {
        marker_keys.contains(&(
            key.source_file_id,
            key.file_generation,
            key.source_start_offset,
        ))
    }) || patch.delete_windows.iter().any(|key| {
        window_keys.contains(&(
            key.source_file_id,
            key.file_generation,
            key.source_start_offset,
        ))
    }) || patch
        .delete_holds
        .iter()
        .any(|(source_id, generation, event_id)| {
            hold_keys.contains(&(*source_id, *generation, event_id.as_str()))
        })
    {
        return Err(StorageError::invalid_state(
            "patch contains conflicting delete and upsert",
        ));
    }
    Ok(())
}

fn validate_unique_private_keys(keys: &[UsagePrivateRowKey], name: &str) -> StorageResult<()> {
    if keys.iter().any(|key| {
        key.source_file_id <= 0 || key.file_generation <= 0 || key.source_start_offset < 0
    }) || keys.iter().collect::<BTreeSet<_>>().len() != keys.len()
    {
        return Err(StorageError::invalid_state(format!("invalid {name}")));
    }
    Ok(())
}

fn turn_primary_key(turn: &UsageTurnWrite) -> (i64, i64, &str) {
    (turn.source_file_id, turn.file_generation, &turn.turn_key)
}

fn validate_turn_write(turn: &UsageTurnWrite) -> StorageResult<()> {
    if turn.source_file_id <= 0
        || turn.file_generation <= 0
        || turn.thread_id.is_empty()
        || turn.turn_key.is_empty()
        || turn.start_offset < 0
        || turn.end_offset.is_some_and(|end| end <= turn.start_offset)
        || turn.ended_at_ms.is_some_and(|time| time < 0)
        || turn.started_at_ms.is_some_and(|time| time < 0)
        || turn.state_through_offset < 0
        || turn.updated_at_ms < 0
        || !matches!(turn.quality_status, "complete" | "partial" | "conflict")
    {
        return Err(StorageError::invalid_state("invalid Turn write"));
    }
    for snapshot in [
        turn.start_total.as_ref(),
        turn.last_total.as_ref(),
        Some(&turn.accounted),
    ]
    .into_iter()
    .flatten()
    {
        snapshot
            .vector
            .validate()
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        if snapshot.fingerprint != crate::codex::normalization::usage_fingerprint(&snapshot.vector)
        {
            return Err(StorageError::invalid_state(
                "Turn usage fingerprint mismatch",
            ));
        }
    }
    Ok(())
}

fn valid_hash_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_group_relationship(
    transaction: &Connection,
    thread_id: &str,
    root_session_id: &str,
) -> StorageResult<()> {
    let row: Option<(String, Option<String>)> = transaction
        .query_row(
            "SELECT source,root_session_id FROM threads WHERE thread_id=?1",
            [thread_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((source, root)) = row else {
        return Err(StorageError::invalid_state("usage thread does not exist"));
    };
    let root_source: Option<String> = transaction
        .query_row(
            "SELECT source FROM threads WHERE thread_id=?1",
            [root_session_id],
            |row| row.get(0),
        )
        .optional()?;
    if root.as_deref() != Some(root_session_id)
        || source != "codex"
        || root_source.as_deref() != Some(source.as_str())
    {
        return Err(StorageError::invalid_state(
            "usage root relationship is not confirmed",
        ));
    }
    Ok(())
}

fn validate_source_preconditions(
    transaction: &Connection,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let current: Option<(Option<String>, i64, i64, i64, i64, String)> = transaction
        .query_row(
            "SELECT thread_id,file_generation,device_id,inode,observed_size,file_status
             FROM codex_source_files WHERE source_file_id=?1",
            [source.source_file_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((thread, generation, device, inode, observed, status)) = current else {
        return Err(StorageError::invalid_state("usage source disappeared"));
    };
    if thread != source.expected_previous_thread_id
        || thread.as_deref() != Some(batch.thread_id.as_str())
        || generation != source.expected_file_generation
        || device != source.updated_state.device_id
        || inode != source.updated_state.inode
        || observed != source.fixed_observed_raw_size
        || status != "present"
    {
        return Err(StorageError::invalid_state("usage source CAS failed"));
    }
    let checkpoint = read_usage_checkpoint(transaction, source.source_file_id)?;
    if source.expected_checkpoint_missing {
        if checkpoint.is_some() {
            return Err(StorageError::invalid_state("usage checkpoint CAS failed"));
        }
    } else if checkpoint.as_ref() != Some(&source.expected_checkpoint) {
        return Err(StorageError::invalid_state("usage checkpoint CAS failed"));
    }
    let epoch = read_epoch(transaction)?.working_epoch();
    let persisted_state = read_usage_source_state(transaction, epoch, source.source_file_id)?;
    if persisted_state != source.expected_state {
        return Err(StorageError::invalid_state("usage source state CAS failed"));
    }
    Ok(())
}

fn validate_reconciliation_context(
    transaction: &Connection,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let context = crate::codex::ingestion::usage_processor::UsageContext {
        source_file_id: source.source_file_id,
        file_generation: source.expected_file_generation,
        owning_thread_id: batch.thread_id.clone(),
        root_session_id: batch.root_session_id.clone(),
    };
    let basic_proof = UsageReconciliationBasicProof {
        device_id: source.updated_state.device_id,
        inode: source.updated_state.inode,
        observed_raw_size: source.fixed_observed_raw_size,
        expected_checkpoint: (!source.expected_checkpoint_missing)
            .then(|| source.expected_checkpoint.clone()),
        expected_state: source.expected_state.clone(),
    };
    let frozen = read_reconciliation_context(
        transaction,
        batch.ledger_epoch,
        context,
        source.reconciliation_request.clone(),
        &basic_proof,
    )?;
    if frozen.context.expected_fingerprint != source.reconciliation_expected_fingerprint {
        return Err(StorageError::usage_conflict(
            "Codex reconciliation context changed during processing",
        ));
    }
    let known_dependency_occurrences = frozen
        .context
        .affected_turns
        .values()
        .flat_map(|affected| affected.compensation_occurrences.iter())
        .chain(frozen.window_proposals.values().flat_map(|proposals| {
            proposals
                .iter()
                .flat_map(|proposal| proposal.occurrences.iter())
        }))
        .chain(frozen.response_occurrences.values().flatten())
        .map(|occurrence| {
            (
                occurrence.source_file_id,
                occurrence.file_generation,
                occurrence.source_start_offset,
                occurrence.source_end_offset,
                occurrence.event_id.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let rewritten_turns = source
        .patch
        .turn_rewrites
        .iter()
        .map(|rewrite| {
            (
                rewrite.expected.source_file_id,
                rewrite.expected.file_generation,
                rewrite.expected.turn_key.clone(),
                rewrite.expected.thread_id.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let retargetable_compensation_ranges = frozen
        .context
        .affected_turns
        .values()
        .filter(|affected| {
            rewritten_turns.contains(&(
                affected.snapshot.key.source_file_id,
                affected.snapshot.key.file_generation,
                affected.snapshot.key.turn_key.clone(),
                affected.snapshot.owning_thread_id.clone(),
            )) && source.patch.events.iter().any(|event| {
                event.kind == EventKind::TurnCompensation
                    && event.thread_id == affected.snapshot.owning_thread_id
                    && event.turn_key.as_deref() == Some(affected.snapshot.key.turn_key.as_str())
            })
        })
        .flat_map(|affected| affected.compensation_occurrences.iter())
        .map(|occurrence| {
            (
                occurrence.source_file_id,
                occurrence.file_generation,
                occurrence.source_start_offset,
                occurrence.source_end_offset,
            )
        })
        .collect::<BTreeSet<_>>();
    for occurrence in &source.patch.occurrences {
        let range = (
            occurrence.source_file_id,
            occurrence.file_generation,
            occurrence.source_start_offset as u64,
            occurrence.source_end_offset as u64,
        );
        let retargeted_compensation = retargetable_compensation_ranges.contains(&range);
        if occurrence.source_file_id == source.source_file_id
            && occurrence.file_generation == source.expected_file_generation
        {
            if occurrence.source_start_offset < source.batch_start_offset
                || occurrence.source_end_offset > source.last_complete_offset
            {
                if !retargeted_compensation {
                    return Err(StorageError::invalid_state(
                        "current-source occurrence falls outside the committed chunk",
                    ));
                }
            }
        } else if !known_dependency_occurrences.contains(&(
            occurrence.source_file_id,
            occurrence.file_generation,
            occurrence.source_start_offset as u64,
            occurrence.source_end_offset as u64,
            occurrence.event_id.as_str(),
        )) && !retargeted_compensation
        {
            return Err(StorageError::usage_conflict(
                "cross-source occurrence is absent from frozen reconciliation dependencies",
            ));
        }
    }
    Ok(())
}

fn prepare_local_replay(
    transaction: &Connection,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let epoch = read_epoch(transaction)?;
    if epoch.build_epoch.is_some()
        || batch.ledger_epoch != epoch.active_epoch
        || epoch.active_epoch == 0
    {
        return Err(StorageError::invalid_state(
            "LocalReplay is only valid in the active epoch",
        ));
    }
    if source.expected_checkpoint_missing {
        let facts: i64 = transaction.query_row(
            "SELECT
                (SELECT count(*) FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_usage_event_holds WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2)",
            params![batch.ledger_epoch, source.source_file_id],
            |row| row.get(0),
        )?;
        if facts != 0 {
            return Err(StorageError::invalid_state(
                "LocalReplay missing-checkpoint proof failed",
            ));
        }
    } else if !local_replay_safe(
        transaction,
        epoch,
        &SourcePlanRow {
            thread_id: Some(batch.thread_id.clone()),
            device_id: source.updated_state.device_id,
            inode: source.updated_state.inode,
            generation: source.expected_file_generation,
            observed_size: source.fixed_observed_raw_size,
            status: "present".to_owned(),
        },
        source.source_file_id,
        &source.expected_checkpoint,
        source.expected_state.as_ref(),
        Some(batch.root_session_id.as_str()),
    )? {
        return Err(StorageError::invalid_state(
            "LocalReplay safety proof failed",
        ));
    }

    transaction.execute(
        "INSERT OR IGNORE INTO codex_usage_event_holds(
            source,ledger_epoch,source_file_id,file_generation,event_id,hold_reason)
         SELECT 'codex',?1,?2,?3,event_id,'replay' FROM (
             SELECT event_id FROM codex_usage_event_occurrences
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3
             UNION
             SELECT resolved_event_id AS event_id FROM codex_compaction_markers
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND resolved_event_id IS NOT NULL
         )",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_compaction_markers WHERE source='codex' AND ledger_epoch=?1
           AND source_file_id=?2 AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_reconciliation_windows WHERE source='codex' AND ledger_epoch=?1
           AND source_file_id=?2 AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_event_occurrences WHERE source='codex' AND ledger_epoch=?1
           AND source_file_id=?2 AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    transaction.execute(
        "DELETE FROM codex_usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2
           AND file_generation=?3",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
    )?;
    // Keep canonical rows until replay candidates have been compared. This is
    // required so a deterministic event ID with a different payload remains a
    // hard conflict even when this source owned the only occurrence. Orphans
    // are removed only after every source in the owning-Thread group has been
    // replayed successfully.
    Ok(())
}

fn capture_affected_canonical_visibility(
    transaction: &Connection,
    batch: &UsageCommitBatch,
) -> StorageResult<HashSet<String>> {
    let mut ids = HashSet::new();
    for source in &batch.sources {
        for event in &source.patch.events {
            ids.insert(event.event_id.clone());
        }
        ids.extend(source.patch.delete_event_ids.iter().cloned());
        if source.local_replay {
            let mut statement = transaction.prepare(
                "SELECT event_id FROM codex_usage_event_occurrences
                 WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
                 UNION SELECT resolved_event_id FROM codex_compaction_markers
                 WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
                   AND resolved_event_id IS NOT NULL
                 UNION SELECT event_id FROM codex_usage_event_holds
                 WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2",
            )?;
            for row in statement
                .query_map(params![batch.ledger_epoch, source.source_file_id], |row| {
                    row.get::<_, String>(0)
                })?
            {
                ids.insert(row?);
            }
        }
    }
    let mut visible = HashSet::new();
    for event_id in ids {
        let exists: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_events WHERE source='codex' AND source_epoch=?1 AND event_id=?2)",
            params![batch.ledger_epoch, event_id],
            |row| row.get(0),
        )?;
        if exists != 0 {
            visible.insert(event_id);
        } else {
            // Prefix absent IDs so the comparison helper can also remember
            // which candidate IDs were part of the affected set.
            visible.insert(format!("\0{event_id}"));
        }
    }
    Ok(visible)
}

fn affected_canonical_visibility_changed(
    transaction: &Connection,
    ledger_epoch: i64,
    before: &HashSet<String>,
) -> StorageResult<bool> {
    for encoded in before {
        let (was_visible, event_id) = if let Some(id) = encoded.strip_prefix('\0') {
            (false, id)
        } else {
            (true, encoded.as_str())
        };
        let is_visible: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM usage_events WHERE source='codex' AND source_epoch=?1 AND event_id=?2)",
            rusqlite::params![ledger_epoch, event_id],
            |row| row.get(0),
        )?;
        if (is_visible != 0) != was_visible {
            return Ok(true);
        }
    }
    Ok(false)
}

fn local_replay_orphan_ids(
    transaction: &Connection,
    ledger_epoch: i64,
) -> StorageResult<Vec<String>> {
    let mut statement = transaction.prepare(
        "SELECT event_id FROM usage_events
         WHERE source='codex' AND source_epoch=?1 AND NOT EXISTS (
             SELECT 1 FROM codex_usage_event_occurrences o
             WHERE o.source='codex' AND o.ledger_epoch=?1 AND o.event_id=usage_events.event_id
         ) AND NOT EXISTS (
             SELECT 1 FROM codex_compaction_markers m
             WHERE m.source='codex' AND m.ledger_epoch=?1
               AND m.resolved_event_id=usage_events.event_id
         ) AND NOT EXISTS (
             SELECT 1 FROM codex_usage_event_holds h
             WHERE h.source='codex' AND h.ledger_epoch=?1
               AND h.event_id=usage_events.event_id
         )",
    )?;
    let ids = statement
        .query_map([ledger_epoch], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::from)?;
    drop(statement);
    strip_orphan_window_references(transaction, ledger_epoch, &ids)?;
    Ok(ids)
}

fn write_or_compare_skill_event(
    transaction: &Connection,
    epoch: i64,
    source: &UsageSourceCommit,
    event: &SkillUsageEventWrite,
) -> StorageResult<()> {
    let existing: Option<(i64, i64, String, String, Option<String>)> = transaction
        .query_row(
            "SELECT source_end_offset,occurred_at_ms,thread_id,root_session_id,model
             FROM codex_skill_usage_events
             WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
               AND source_start_offset=?4 AND skill_name=?5",
            params![
                epoch,
                event.source_file_id,
                event.file_generation,
                event.source_start_offset,
                event.skill_name
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let expected = (
        event.source_end_offset,
        event.occurred_at_ms,
        event.thread_id.clone(),
        event.root_session_id.clone(),
        event.model.clone(),
    );
    if let Some(existing) = existing {
        if existing == expected {
            return Ok(());
        }
        return Err(StorageError::usage_conflict("Skill usage event conflict"));
    }
    transaction.execute(
        "INSERT INTO codex_skill_usage_events(
            ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
            occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            epoch,
            source.source_file_id,
            source.expected_file_generation,
            event.source_start_offset,
            event.source_end_offset,
            event.occurred_at_ms,
            event.thread_id,
            event.root_session_id,
            event.model,
            event.skill_name,
            source.committed_at_ms
        ],
    )?;
    Ok(())
}

fn skill_source_fingerprint(
    transaction: &Connection,
    epoch: i64,
    source_file_id: i64,
) -> StorageResult<Vec<u8>> {
    let mut statement = transaction.prepare(
        "SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                thread_id,root_session_id,model,skill_name
         FROM codex_skill_usage_events
         WHERE ledger_epoch=?1 AND source_file_id=?2
         ORDER BY file_generation,source_start_offset,skill_name",
    )?;
    let mut rows = statement.query(params![epoch, source_file_id])?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"skill-usage-source-v1\0");
    while let Some(row) = rows.next()? {
        let values = [
            row.get::<_, i64>(0)?.to_string(),
            row.get::<_, i64>(1)?.to_string(),
            row.get::<_, i64>(2)?.to_string(),
            row.get::<_, i64>(3)?.to_string(),
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?
                .unwrap_or_else(|| "\0".to_owned()),
            row.get::<_, String>(7)?,
        ];
        for value in values {
            hasher.update(&(value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
    Ok(hasher.finalize().as_bytes().to_vec())
}

fn capture_skill_visibility(
    transaction: &Connection,
    batch: &UsageCommitBatch,
) -> StorageResult<Vec<(i64, Vec<u8>)>> {
    batch
        .sources
        .iter()
        .map(|source| {
            Ok((
                source.source_file_id,
                skill_source_fingerprint(transaction, batch.ledger_epoch, source.source_file_id)?,
            ))
        })
        .collect()
}

fn affected_skill_visibility_changed(
    transaction: &Connection,
    epoch: i64,
    before: &[(i64, Vec<u8>)],
) -> StorageResult<bool> {
    for (source_file_id, fingerprint) in before {
        if &skill_source_fingerprint(transaction, epoch, *source_file_id)? != fingerprint {
            return Ok(true);
        }
    }
    Ok(false)
}

fn carry_skill_events_at_offset(
    transaction: &Connection,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    start_offset: i64,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO codex_skill_usage_events(
            ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
            occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms)
         SELECT ?1,source_file_id,file_generation,source_start_offset,source_end_offset,
            occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms
         FROM codex_skill_usage_events a
         WHERE a.ledger_epoch=?2 AND a.source_file_id=?3 AND a.file_generation=?4
           AND a.source_start_offset=?5
           AND NOT EXISTS(
             SELECT 1 FROM codex_skill_usage_events b
             WHERE b.ledger_epoch=?1 AND b.source_file_id=a.source_file_id
               AND b.file_generation=a.file_generation
               AND b.source_start_offset=a.source_start_offset AND b.skill_name=a.skill_name)",
        params![
            build_epoch,
            active_epoch,
            source_file_id,
            file_generation,
            start_offset
        ],
    )?;
    let diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3
               AND file_generation=?5 AND source_start_offset=?4
             EXCEPT
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?5 AND source_start_offset=?4))
        + (SELECT count(*) FROM (
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3
               AND file_generation=?5 AND source_start_offset=?4
             EXCEPT
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM codex_skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3
               AND file_generation=?5 AND source_start_offset=?4))",
        params![active_epoch, build_epoch, source_file_id, start_offset, file_generation],
        |row| row.get(0),
    )?;
    if diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry Skill event conflict",
        ));
    }
    Ok(())
}

fn write_or_compare_occurrence(
    transaction: &Connection,
    epoch: i64,
    source: &UsageSourceCommit,
    occurrence: &UsageOccurrenceWrite,
    explicitly_deleted_event_ids: &BTreeSet<String>,
) -> StorageResult<()> {
    if occurrence.source_file_id <= 0
        || occurrence.file_generation <= 0
        || occurrence.source_start_offset < 0
        || occurrence.source_end_offset <= occurrence.source_start_offset
    {
        return Err(StorageError::invalid_state(
            "invalid occurrence physical key",
        ));
    }
    let existing: Option<(String, i64)> = transaction
        .query_row(
            "SELECT event_id,source_end_offset FROM codex_usage_event_occurrences
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
                AND source_start_offset=?4",
            params![
                epoch,
                occurrence.source_file_id,
                occurrence.file_generation,
                occurrence.source_start_offset
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing == (occurrence.event_id.clone(), occurrence.source_end_offset) {
            return Ok(());
        }
        if !explicitly_deleted_event_ids.contains(&existing.0) {
            return Err(StorageError::usage_conflict(
                "usage occurrence identity changed without explicit deletion",
            ));
        }
        transaction.execute(
            "UPDATE codex_usage_event_occurrences
             SET event_id=?5,source_end_offset=?6,created_at_ms=?7
             WHERE source='codex' AND ledger_epoch=?1 AND source_file_id=?2
               AND file_generation=?3 AND source_start_offset=?4",
            params![
                epoch,
                occurrence.source_file_id,
                occurrence.file_generation,
                occurrence.source_start_offset,
                occurrence.event_id,
                occurrence.source_end_offset,
                source.committed_at_ms
            ],
        )?;
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO codex_usage_event_occurrences (
            source,ledger_epoch,source_file_id,file_generation,source_start_offset,
            source_end_offset,event_id,created_at_ms
         ) VALUES ('codex',?1,?2,?3,?4,?5,?6,?7)",
        params![
            epoch,
            occurrence.source_file_id,
            occurrence.file_generation,
            occurrence.source_start_offset,
            occurrence.source_end_offset,
            occurrence.event_id,
            source.committed_at_ms
        ],
    )?;
    Ok(())
}

fn snapshot_columns(snapshot: Option<&UsageSnapshot>) -> SnapshotColumns {
    match snapshot {
        Some(snapshot) => (
            Some(snapshot.vector.input_tokens),
            Some(snapshot.vector.cached_tokens),
            snapshot.vector.cache_write_tokens,
            Some(snapshot.vector.output_tokens),
            Some(snapshot.vector.reasoning_tokens),
            Some(snapshot.vector.total_tokens),
            Some(snapshot.fingerprint.clone()),
        ),
        None => (None, None, None, None, None, None, None),
    }
}

fn read_usage_turn_write(
    connection: &Connection,
    epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    turn_key: &str,
) -> StorageResult<Option<UsageTurnWrite>> {
    let row = connection
        .query_row(
            "SELECT source_file_id,file_generation,turn_key,thread_id,raw_turn_id,started_at_ms,
                    ended_at_ms,start_offset,end_offset,status,
                    start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
                    start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,start_total_fingerprint,
                    last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
                    last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,last_total_fingerprint,
                    accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
                    accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
                    accounted_candidate_count,model_state,single_model,unresolved_model_seen,
                    reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
                    compensation_allowed,block_start_missing,block_time_missing,block_reset,
                    block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,
                    quality_status,state_through_offset,updated_at_ms
             FROM codex_turns
             WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3 AND turn_key=?4",
            params![epoch,source_file_id,file_generation,turn_key],
            |row| {
                Ok((
                    read_persisted_turn_snapshot(row)?,
                    row.get::<_, i64>(48)?,
                ))
            },
        )
        .optional()?;
    row.map(|(snapshot, updated_at_ms)| usage_turn_write_from_snapshot(snapshot, updated_at_ms))
        .transpose()
}

fn usage_turn_write_from_snapshot(
    snapshot: crate::codex::ingestion::usage_processor::PersistedTurnSnapshot,
    updated_at_ms: i64,
) -> StorageResult<UsageTurnWrite> {
    use crate::codex::ingestion::usage_processor::{
        PersistedTurnStatus, TurnModelState, TurnReasoningEffortState,
    };

    let model_state = match snapshot.state.model_state {
        TurnModelState::None => UsageTurnModelState::None,
        TurnModelState::Single(model) => UsageTurnModelState::Single(model),
        TurnModelState::Mixed => UsageTurnModelState::Mixed,
    };
    let reasoning_effort_state = match snapshot.state.reasoning_effort_state {
        TurnReasoningEffortState::None => UsageTurnReasoningEffortState::None,
        TurnReasoningEffortState::Single(effort) => UsageTurnReasoningEffortState::Single(effort),
        TurnReasoningEffortState::Mixed => UsageTurnReasoningEffortState::Mixed,
    };
    let usage_snapshot = |value: &NormalizedTokenUsage| UsageSnapshot {
        vector: value.clone(),
        fingerprint: crate::codex::normalization::usage_fingerprint(value).to_vec(),
    };
    Ok(UsageTurnWrite {
        source_file_id: snapshot.key.source_file_id,
        file_generation: snapshot.key.file_generation,
        thread_id: snapshot.owning_thread_id,
        turn_key: snapshot.state.turn_key,
        raw_turn_id: snapshot.state.raw_turn_id,
        started_at_ms: snapshot.state.started_at_ms,
        ended_at_ms: snapshot.ended_at_ms,
        start_offset: i64::try_from(snapshot.state.start_offset)
            .map_err(|_| StorageError::invalid_state("Turn start offset exceeds storage"))?,
        end_offset: snapshot
            .end_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StorageError::invalid_state("Turn end offset exceeds storage"))?,
        status: match snapshot.status {
            PersistedTurnStatus::Open => UsageTurnStatus::Open,
            PersistedTurnStatus::Completed => UsageTurnStatus::Completed,
            PersistedTurnStatus::Aborted => UsageTurnStatus::Aborted,
            PersistedTurnStatus::Failed => UsageTurnStatus::Failed,
        },
        start_total: snapshot.state.start_total.as_ref().map(usage_snapshot),
        last_total: snapshot.state.last_total.as_ref().map(usage_snapshot),
        accounted: usage_snapshot(&snapshot.state.accounted),
        accounted_candidate_count: i64::try_from(snapshot.state.accounted_candidate_count)
            .map_err(|_| StorageError::invalid_state("Turn candidate count exceeds storage"))?,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen: snapshot.state.unresolved_reasoning_effort_seen,
        unresolved_model_seen: snapshot.state.unresolved_model_seen,
        blocks: UsageCompensationBlocks {
            start_missing: snapshot.state.blocks.start_missing,
            time_missing: snapshot.state.blocks.time_missing,
            reset: snapshot.state.blocks.reset,
            ownership_gap: snapshot.state.blocks.ownership_gap,
            parser_gap: snapshot.state.blocks.parser_gap,
            required_invalid: snapshot.state.blocks.required_invalid,
            model_unresolved: snapshot.state.blocks.model_unresolved,
        },
        quality_status: match snapshot.quality_status.as_str() {
            "complete" => "complete",
            "partial" => "partial",
            "conflict" => "conflict",
            _ => return Err(StorageError::invalid_state("invalid Turn quality status")),
        },
        state_through_offset: i64::try_from(snapshot.state_through_offset)
            .map_err(|_| StorageError::invalid_state("Turn state offset exceeds storage"))?,
        updated_at_ms,
    })
}

fn write_turn_rewrite(
    transaction: &Connection,
    ledger_epoch: i64,
    rewrite: &UsageTurnRewriteWrite,
) -> StorageResult<()> {
    let expected = &rewrite.expected;
    let replacement = &rewrite.replacement;
    if expected.source_file_id <= 0
        || expected.file_generation <= 0
        || expected.thread_id.is_empty()
        || expected.turn_key.is_empty()
        || (
            expected.source_file_id,
            expected.file_generation,
            expected.thread_id.as_str(),
            expected.turn_key.as_str(),
        ) != (
            replacement.source_file_id,
            replacement.file_generation,
            replacement.thread_id.as_str(),
            replacement.turn_key.as_str(),
        )
    {
        return Err(StorageError::invalid_state("invalid full Turn rewrite key"));
    }
    let Some(current) = read_usage_turn_write(
        transaction,
        ledger_epoch,
        expected.source_file_id,
        expected.file_generation,
        &expected.turn_key,
    )?
    else {
        return Err(StorageError::usage_conflict(
            "Turn rewrite expected row is missing",
        ));
    };
    let mut expected_with_current_timestamp = expected.clone();
    expected_with_current_timestamp.updated_at_ms = current.updated_at_ms;
    if current != expected_with_current_timestamp || current.thread_id != expected.thread_id {
        return Err(StorageError::usage_conflict(
            "Turn rewrite expected snapshot changed",
        ));
    }
    let deleted = transaction.execute(
        "DELETE FROM codex_turns
         WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3 AND turn_key=?4",
        params![
            ledger_epoch,
            expected.source_file_id,
            expected.file_generation,
            expected.turn_key
        ],
    )?;
    if deleted != 1 {
        return Err(StorageError::usage_conflict(
            "Turn rewrite CAS delete failed",
        ));
    }
    write_turn(
        transaction,
        ledger_epoch,
        replacement.source_file_id,
        replacement.file_generation,
        &replacement.thread_id,
        replacement,
    )
}

fn write_turn(
    transaction: &Connection,
    ledger_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    thread_id: &str,
    turn: &UsageTurnWrite,
) -> StorageResult<()> {
    if turn.source_file_id != source_file_id
        || turn.file_generation != file_generation
        || turn.thread_id != thread_id
    {
        return Err(StorageError::invalid_state("Turn write key mismatch"));
    }
    let start = snapshot_columns(turn.start_total.as_ref());
    let last = snapshot_columns(turn.last_total.as_ref());
    let accounted = snapshot_columns(Some(&turn.accounted));
    let compensation_allowed = turn.blocks == UsageCompensationBlocks::default();
    let changed = transaction.execute(
        "INSERT INTO codex_turns (
            ledger_epoch,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,
            started_at_ms,ended_at_ms,start_offset,end_offset,status,
            start_total_input_tokens,start_total_cached_tokens,start_total_cache_write_tokens,
            start_total_output_tokens,start_total_reasoning_tokens,start_total_total_tokens,
            start_total_fingerprint,
            last_total_input_tokens,last_total_cached_tokens,last_total_cache_write_tokens,
            last_total_output_tokens,last_total_reasoning_tokens,last_total_total_tokens,
            last_total_fingerprint,
            accounted_input_tokens,accounted_cached_tokens,accounted_cache_write_tokens,
            accounted_output_tokens,accounted_reasoning_tokens,accounted_total_tokens,accounted_fingerprint,
            accounted_candidate_count,model_state,single_model,unresolved_model_seen,
            reasoning_effort_state,single_reasoning_effort,unresolved_reasoning_effort_seen,
            compensation_allowed,block_start_missing,block_time_missing,block_reset,
            block_ownership_gap,block_parser_gap,block_required_invalid,block_model_unresolved,
            quality_status,state_through_offset,updated_at_ms
         ) VALUES (
            ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
            ?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,
            ?34,?35,?36,?37,?38,?39,?40,?41,?42,?43,?44,?45,?46,?47,?48,?49,?50)
         ON CONFLICT(ledger_epoch,source_file_id,file_generation,turn_key) DO UPDATE SET
            raw_turn_id=excluded.raw_turn_id,started_at_ms=excluded.started_at_ms,
            ended_at_ms=excluded.ended_at_ms,end_offset=excluded.end_offset,status=excluded.status,
            last_total_input_tokens=excluded.last_total_input_tokens,
            last_total_cached_tokens=excluded.last_total_cached_tokens,
            last_total_cache_write_tokens=excluded.last_total_cache_write_tokens,
            last_total_output_tokens=excluded.last_total_output_tokens,
            last_total_reasoning_tokens=excluded.last_total_reasoning_tokens,
            last_total_total_tokens=excluded.last_total_total_tokens,
            last_total_fingerprint=excluded.last_total_fingerprint,
            accounted_input_tokens=excluded.accounted_input_tokens,
            accounted_cached_tokens=excluded.accounted_cached_tokens,
            accounted_cache_write_tokens=excluded.accounted_cache_write_tokens,
            accounted_output_tokens=excluded.accounted_output_tokens,
            accounted_reasoning_tokens=excluded.accounted_reasoning_tokens,
            accounted_total_tokens=excluded.accounted_total_tokens,
            accounted_fingerprint=excluded.accounted_fingerprint,
            accounted_candidate_count=excluded.accounted_candidate_count,
            model_state=excluded.model_state,single_model=excluded.single_model,
            unresolved_model_seen=excluded.unresolved_model_seen,
            reasoning_effort_state=excluded.reasoning_effort_state,
            single_reasoning_effort=excluded.single_reasoning_effort,
            unresolved_reasoning_effort_seen=excluded.unresolved_reasoning_effort_seen,
            compensation_allowed=excluded.compensation_allowed,
            block_start_missing=excluded.block_start_missing,block_time_missing=excluded.block_time_missing,
            block_reset=excluded.block_reset,block_ownership_gap=excluded.block_ownership_gap,
            block_parser_gap=excluded.block_parser_gap,block_required_invalid=excluded.block_required_invalid,
            block_model_unresolved=excluded.block_model_unresolved,quality_status=excluded.quality_status,
            state_through_offset=excluded.state_through_offset,updated_at_ms=excluded.updated_at_ms
         WHERE codex_turns.thread_id=excluded.thread_id
            AND codex_turns.start_offset=excluded.start_offset
            AND codex_turns.raw_turn_id IS excluded.raw_turn_id
            AND codex_turns.started_at_ms IS excluded.started_at_ms
            AND codex_turns.start_total_input_tokens IS excluded.start_total_input_tokens
            AND codex_turns.start_total_cached_tokens IS excluded.start_total_cached_tokens
            AND codex_turns.start_total_cache_write_tokens IS excluded.start_total_cache_write_tokens
            AND codex_turns.start_total_output_tokens IS excluded.start_total_output_tokens
            AND codex_turns.start_total_reasoning_tokens IS excluded.start_total_reasoning_tokens
            AND codex_turns.start_total_total_tokens IS excluded.start_total_total_tokens
            AND codex_turns.start_total_fingerprint IS excluded.start_total_fingerprint
            AND (codex_turns.status='open' OR codex_turns.status=excluded.status)
            AND codex_turns.block_start_missing <= excluded.block_start_missing
            AND codex_turns.block_time_missing <= excluded.block_time_missing
            AND codex_turns.block_reset <= excluded.block_reset
            AND codex_turns.block_ownership_gap <= excluded.block_ownership_gap
            AND codex_turns.block_parser_gap <= excluded.block_parser_gap
            AND codex_turns.block_required_invalid <= excluded.block_required_invalid
            AND codex_turns.block_model_unresolved <= excluded.block_model_unresolved
            AND codex_turns.unresolved_model_seen <= excluded.unresolved_model_seen
            AND codex_turns.unresolved_reasoning_effort_seen <= excluded.unresolved_reasoning_effort_seen
            -- Reasoning-effort Turn summary is monotonic: none -> single(same value) -> mixed.
            -- The existing durable state must never be replaced by a less informative state.
            AND (
                codex_turns.reasoning_effort_state='none'
                OR (
                    codex_turns.reasoning_effort_state='single'
                    AND (
                        (
                            excluded.reasoning_effort_state='single'
                            AND codex_turns.single_reasoning_effort=excluded.single_reasoning_effort
                        )
                        OR excluded.reasoning_effort_state='mixed'
                    )
                )
                OR (
                    codex_turns.reasoning_effort_state='mixed'
                    AND excluded.reasoning_effort_state='mixed'
                )
            )
            AND codex_turns.accounted_candidate_count <= excluded.accounted_candidate_count
            AND codex_turns.state_through_offset <= excluded.state_through_offset",
        params![
            ledger_epoch,source_file_id,file_generation,turn.turn_key,
            thread_id,turn.raw_turn_id,turn.started_at_ms,turn.ended_at_ms,turn.start_offset,
            turn.end_offset,turn.status.as_str(),start.0,start.1,start.2,start.3,start.4,start.5,
            start.6,last.0,last.1,last.2,last.3,last.4,last.5,last.6,
            accounted.0,accounted.1,accounted.2,accounted.3,accounted.4,accounted.5,accounted.6,
            turn.accounted_candidate_count,turn.model_state.as_str(),
            turn.model_state.single_model(),i64::from(turn.unresolved_model_seen),
            turn.reasoning_effort_state.as_str(),turn.reasoning_effort_state.single_effort(),
            i64::from(turn.unresolved_reasoning_effort_seen),
            i64::from(compensation_allowed),i64::from(turn.blocks.start_missing),
            i64::from(turn.blocks.time_missing),i64::from(turn.blocks.reset),
            i64::from(turn.blocks.ownership_gap),i64::from(turn.blocks.parser_gap),
            i64::from(turn.blocks.required_invalid),i64::from(turn.blocks.model_unresolved),
            turn.quality_status,turn.state_through_offset,turn.updated_at_ms
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::usage_conflict("usage Turn conflict"));
    }
    Ok(())
}

pub(super) fn write_anomaly(
    transaction: &Connection,
    ledger_epoch: i64,
    thread_id: &str,
    source_file_id: i64,
    file_generation: i64,
    anomaly: &UsageAnomalyWrite,
) -> StorageResult<()> {
    if !valid_hash_id(&anomaly.anomaly_id) {
        return Err(StorageError::invalid_state(
            "invalid deterministic anomaly id",
        ));
    }
    let expected = (
        anomaly.occurred_at_ms,
        thread_id.to_owned(),
        source_file_id,
        file_generation,
        anomaly.source_start_offset,
        anomaly.kind.as_str().to_owned(),
        if anomaly.severity_error {
            "error".to_owned()
        } else {
            "warning".to_owned()
        },
        anomaly
            .turn_key
            .as_ref()
            .map(|turn_key| serde_json::json!({"turn_key": turn_key}).to_string())
            .unwrap_or_else(|| "{}".to_owned()),
    );
    let existing: Option<ExistingAnomaly> = transaction
        .query_row(
            "SELECT occurred_at_ms,thread_id,source_file_id,file_generation,
                    source_start_offset,anomaly_type,severity,details_json
                 FROM codex_ingest_anomalies WHERE ledger_epoch=?1 AND anomaly_id=?2",
            params![ledger_epoch, anomaly.anomaly_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        let existing_kind = UsageAnomalyKind::parse(&existing.5)?;
        if existing.0 == expected.0
            && existing.1 == expected.1
            && existing.2 == expected.2
            && existing.3 == expected.3
            && existing.4 == expected.4
            && existing_kind == anomaly.kind
            && existing.6 == expected.6
            && existing.7 == expected.7
        {
            return Ok(());
        }
        return Err(StorageError::usage_conflict(
            "deterministic anomaly conflict",
        ));
    }
    transaction.execute(
        "INSERT INTO codex_ingest_anomalies (
            ledger_epoch,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
            file_generation,source_start_offset,anomaly_type,severity,details_json,resolved
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0)",
        params![
            ledger_epoch,
            anomaly.anomaly_id,
            anomaly.detected_at_ms,
            anomaly.occurred_at_ms,
            thread_id,
            source_file_id,
            file_generation,
            anomaly.source_start_offset,
            anomaly.kind.as_str(),
            if anomaly.severity_error {
                "error"
            } else {
                "warning"
            },
            expected.7,
        ],
    )?;
    Ok(())
}

fn write_source_state(
    transaction: &Connection,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    write_source_state_row(
        transaction,
        batch.ledger_epoch,
        source.source_file_id,
        &source.updated_state,
    )
}

fn write_source_state_row(
    transaction: &Connection,
    ledger_epoch: i64,
    source_file_id: i64,
    state: &UsageSourceStateWrite,
) -> StorageResult<()> {
    let reconciliation_state_json =
        canonical_reconciliation_state(&state.reconciliation_state_json)?;
    let previous = snapshot_columns(state.previous_total.as_ref());
    let (chain, reason) = match state.chain_state {
        UsageChainState::Continuous => ("continuous", None),
        UsageChainState::Interrupted(reason) => ("interrupted", Some(reason.as_str())),
    };
    transaction.execute(
        "INSERT INTO codex_usage_source_states (
            ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,
            canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
            raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
            previous_total_input_tokens,previous_total_cached_tokens,
            previous_total_cache_write_tokens,previous_total_output_tokens,
            previous_total_reasoning_tokens,previous_total_total_tokens,
            previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
            active_turn_key,active_model,active_model_offset,active_reasoning_effort,
            active_reasoning_effort_offset,updated_at_ms,reconciliation_state_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
            ?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31)
         ON CONFLICT(ledger_epoch,source_file_id) DO UPDATE SET
            file_generation=excluded.file_generation,device_id=excluded.device_id,inode=excluded.inode,
            usage_parser_version=excluded.usage_parser_version,
            canonical_algorithm_version=excluded.canonical_algorithm_version,
            resolved_through_offset=excluded.resolved_through_offset,
            observed_raw_size=excluded.observed_raw_size,raw_tail_status=excluded.raw_tail_status,
            raw_tail_start_offset=excluded.raw_tail_start_offset,
            owning_thread_id=excluded.owning_thread_id,root_session_id=excluded.root_session_id,
            continuation_state=excluded.continuation_state,
            previous_total_input_tokens=excluded.previous_total_input_tokens,
            previous_total_cached_tokens=excluded.previous_total_cached_tokens,
            previous_total_cache_write_tokens=excluded.previous_total_cache_write_tokens,
            previous_total_output_tokens=excluded.previous_total_output_tokens,
            previous_total_reasoning_tokens=excluded.previous_total_reasoning_tokens,
            previous_total_total_tokens=excluded.previous_total_total_tokens,
            previous_total_fingerprint=excluded.previous_total_fingerprint,
            previous_total_offset=excluded.previous_total_offset,chain_state=excluded.chain_state,
            chain_block_reason=excluded.chain_block_reason,active_turn_key=excluded.active_turn_key,
            active_model=excluded.active_model,active_model_offset=excluded.active_model_offset,
            active_reasoning_effort=excluded.active_reasoning_effort,
            active_reasoning_effort_offset=excluded.active_reasoning_effort_offset,
            updated_at_ms=excluded.updated_at_ms,
            reconciliation_state_json=excluded.reconciliation_state_json",
        params![
            ledger_epoch,source_file_id,state.file_generation,state.device_id,state.inode,
            state.usage_parser_version,state.canonical_algorithm_version,state.resolved_through_offset,
            state.observed_raw_size,state.raw_tail_status.as_str(),state.raw_tail_start_offset,
            state.owning_thread_id,state.root_session_id,state.continuation_state.as_str(),
            previous.0,previous.1,previous.2,previous.3,previous.4,previous.5,previous.6,
            state.previous_total_offset,chain,reason,state.active_turn_key,state.active_model,
            state.active_model_offset,state.active_reasoning_effort,state.active_reasoning_effort_offset,
            state.updated_at_ms,reconciliation_state_json
        ],
    )?;
    Ok(())
}

fn canonical_reconciliation_state(json: &str) -> StorageResult<String> {
    use crate::codex::ingestion::usage_processor::{CarryError, ReconciliationCarry};

    let carry = ReconciliationCarry::from_json(json).map_err(|error| match error {
        CarryError::UnsupportedVersion => {
            StorageError::usage_conflict("usage reconciliation carry version requires rebuild")
        }
        CarryError::Invalid => StorageError::invalid_state("invalid usage reconciliation carry"),
    })?;
    let canonical = carry
        .to_json()
        .map_err(|_| StorageError::invalid_state("invalid usage reconciliation carry"))?;
    if canonical != json {
        return Err(StorageError::invalid_state(
            "usage reconciliation carry is not canonical",
        ));
    }
    Ok(canonical)
}

fn write_usage_checkpoint(
    transaction: &Connection,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    if source.expected_checkpoint_missing {
        let changed = transaction.execute(
            "INSERT INTO codex_source_checkpoints(
                source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                processing_status,last_successful_scan_at_ms,last_error_code
             ) VALUES (?1,'usage',?2,?3,?4,'ready',?5,NULL)",
            params![
                source.source_file_id,
                batch.usage_parser_version,
                source.last_complete_offset,
                source.next_guard_hash,
                source.committed_at_ms,
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "usage checkpoint insert failed",
            ));
        }
        return Ok(());
    }
    let changed = transaction.execute(
        "UPDATE codex_source_checkpoints SET parser_version=?3,committed_offset=?4,guard_hash=?5,
            processing_status='ready',last_successful_scan_at_ms=?6,last_error_code=NULL
         WHERE source_file_id=?1 AND consumer_kind='usage' AND parser_version=?2
            AND committed_offset=?7 AND processing_status=?8
            AND guard_hash IS ?9",
        params![
            source.source_file_id,
            source.expected_checkpoint.parser_version,
            batch.usage_parser_version,
            source.last_complete_offset,
            source.next_guard_hash,
            source.committed_at_ms,
            source.expected_checkpoint.committed_offset,
            source.expected_checkpoint.processing_status.as_str(),
            source.expected_checkpoint.guard_hash
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage checkpoint changed during commit",
        ));
    }
    Ok(())
}

fn update_build_progress(
    transaction: &Connection,
    epoch: SourceUsageEpochState,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let Some(build_epoch) = epoch.build_epoch else {
        return Ok(());
    };
    let final_proof = source.fixed_view_exhausted
        && matches!(
            source.tail_status,
            UsageTailStatus::None | UsageTailStatus::HalfLine
        );
    let changed = if final_proof {
        transaction.execute(
            "UPDATE codex_usage_build_sources SET
                required_through_offset=MAX(required_through_offset,?4),
                raw_tail_status=?5,raw_tail_start_offset=?6,
                completion_status='rebuilt',completion_error_code=NULL,
                completed_generation=?8,completed_through_offset=?4,
                carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                carry_after_fact_event_id=NULL,carry_after_marker_start_offset=NULL,
                carry_after_window_start_offset=NULL,carry_after_turn_key=NULL,
                carry_after_anomaly_id=NULL,updated_at_ms=?7
             WHERE build_epoch=?1 AND source_file_id=?2 AND target_parser_version=?3
                AND expected_file_generation=?8 AND required_generation=?8
                AND observed_raw_size=?9 AND completion_status IN ('pending','blocked')
                AND carry_phase='none' AND ?4 >= required_through_offset",
            params![
                build_epoch,
                source.source_file_id,
                batch.usage_parser_version,
                source.last_complete_offset,
                source.tail_status.as_str(),
                source.tail_start_offset,
                source.committed_at_ms,
                source.expected_file_generation,
                source.fixed_observed_raw_size
            ],
        )?
    } else {
        transaction.execute(
            "UPDATE codex_usage_build_sources SET
                required_through_offset=MAX(required_through_offset,?4),
                raw_tail_status='unverified',raw_tail_start_offset=NULL,
                completion_status=CASE WHEN completion_status='blocked' THEN 'pending' ELSE completion_status END,
                completion_error_code=NULL,updated_at_ms=?7
             WHERE build_epoch=?1 AND source_file_id=?2 AND target_parser_version=?3
                AND expected_file_generation=?8 AND required_generation=?8
                AND observed_raw_size=?9 AND completion_status IN ('pending','blocked')
                AND carry_phase='none'",
            params![
                build_epoch,
                source.source_file_id,
                batch.usage_parser_version,
                source.last_complete_offset,
                UsageTailStatus::Unverified.as_str(),
                Option::<i64>::None,
                source.committed_at_ms,
                source.expected_file_generation,
                source.fixed_observed_raw_size
            ],
        )?
    };
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage build manifest progress CAS failed",
        ));
    }
    Ok(())
}

fn verify_source_postconditions(
    transaction: &Connection,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let values: (i64, i64, String, i64, String, String, i64) = transaction.query_row(
        "SELECT c.parser_version,c.committed_offset,c.processing_status,
            s.resolved_through_offset,s.owning_thread_id,s.root_session_id,s.file_generation
         FROM codex_source_checkpoints c JOIN codex_usage_source_states s
           ON s.source_file_id=c.source_file_id AND s.ledger_epoch=?2
         WHERE c.source_file_id=?1 AND c.consumer_kind='usage'",
        params![source.source_file_id, batch.ledger_epoch],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;
    if values
        != (
            batch.usage_parser_version,
            source.last_complete_offset,
            "ready".to_owned(),
            source.last_complete_offset,
            batch.thread_id.clone(),
            batch.root_session_id.clone(),
            source.expected_file_generation,
        )
    {
        return Err(StorageError::invalid_state(
            "usage commit postcondition failed",
        ));
    }
    let (open_count, active_turn): (i64, Option<String>) = transaction.query_row(
        "SELECT count(*),min(turn_key) FROM codex_turns WHERE ledger_epoch=?1 AND source_file_id=?2
            AND file_generation=?3 AND status='open'",
        params![
            batch.ledger_epoch,
            source.source_file_id,
            source.expected_file_generation
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if open_count > 1 || active_turn != source.updated_state.active_turn_key {
        return Err(StorageError::invalid_state(
            "usage open Turn state mismatch",
        ));
    }
    Ok(())
}

pub(super) fn reconcile_usage_metadata_change(
    source_tx: &mut CodexWriteTxn<'_>,
    thread_id: &str,
    previous_root: Option<&str>,
    next_root: Option<&str>,
    binding_changed_source_ids: &[i64],
) -> StorageResult<()> {
    let root_changed = previous_root != next_root;
    if !root_changed && binding_changed_source_ids.is_empty() {
        return Ok(());
    }
    let next_root =
        if root_changed {
            Some(next_root.ok_or_else(|| {
                StorageError::invalid_state("confirmed usage root cannot be cleared")
            })?)
        } else {
            next_root
        };
    let (active_epoch, build_epoch, build_parser): (i64, Option<i64>, Option<i64>) = source_tx
        .with_private_state(|transaction| {
            transaction
                .query_row(
                    "SELECT active_epoch,build_epoch,build_parser_version
                     FROM source_usage_epochs WHERE source='codex'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(StorageError::from)
        })?;

    if root_changed && (active_epoch > 0 || build_epoch.is_some()) {
        let next_root = next_root.expect("root_changed requires a confirmed next root");
        ensure_usage_root_materialized(source_tx, next_root)?;
    }

    // Active facts are stable user-visible data and may be reconciled in
    // place because only the confirmed root relation changed. This is in the
    // caller's metadata transaction, so revision advances at most once.
    if root_changed && active_epoch > 0 {
        let next_root = next_root.expect("root_changed requires a confirmed next root");
        source_tx
            .rebind_usage_root_no_revision(UsageWriteTarget::Active, thread_id, next_root)
            .map_err(storage_error_from_codex)?;
        source_tx.with_private_state(|transaction| {
            transaction.execute(
                "UPDATE codex_skill_usage_events SET root_session_id=?1
                 WHERE ledger_epoch=?2 AND thread_id=?3",
                params![next_root, active_epoch, thread_id],
            )?;
            transaction.execute(
                "UPDATE codex_usage_source_states SET root_session_id=?1
                 WHERE ledger_epoch=?2 AND owning_thread_id=?3",
                params![next_root, active_epoch, thread_id],
            )?;
            transaction.execute(
                "UPDATE codex_compaction_markers SET root_session_id=?1
                 WHERE source='codex' AND ledger_epoch=?2 AND owning_thread_id=?3",
                params![next_root, active_epoch, thread_id],
            )?;
            Ok::<_, StorageError>(())
        })?;
    }

    if let Some(build_epoch) = build_epoch {
        source_tx.with_private_state(|transaction| -> Result<(), StorageError> {
        let parser =
            build_parser.ok_or_else(|| StorageError::invalid_state("invalid build pair"))?;
        let active_epoch_for_build = active_epoch;
        let mut invalidated = binding_changed_source_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if root_changed {
            let mut statement = transaction.prepare(
                "SELECT DISTINCT b.source_file_id
                 FROM codex_usage_build_sources b
                 JOIN codex_source_files sf ON sf.source_file_id=b.source_file_id
                 WHERE b.build_epoch=?1
                   AND (b.expected_owning_thread_id=?2 OR sf.thread_id=?2)",
            )?;
            for row in
                statement.query_map(params![build_epoch, thread_id], |row| row.get::<_, i64>(0))?
            {
                invalidated.insert(row?);
            }
        }
        if !invalidated.is_empty() {
            let mut present = std::collections::BTreeSet::new();
            let mut statement = transaction.prepare(
                "SELECT source_file_id FROM codex_source_files WHERE file_status='present' ORDER BY source_file_id",
            )?;
            for row in statement.query_map([], |row| row.get::<_, i64>(0))? {
                present.insert(row?);
            }
            crate::codex::storage::rebuild::replace_build_preserving_all_members_tx(
                transaction,
                active_epoch_for_build,
                build_epoch,
                parser,
                &present,
                &invalidated,
                now_ms_for_transaction(),
            )
            .map_err(rebuild_storage_error)?;
        }
        Ok::<_, StorageError>(())
        })?;
        crate::codex::storage::rebuild::delete_orphan_build_events(source_tx, build_epoch)
            .map_err(storage_error_from_codex)?;
    }
    Ok(())
}

fn storage_error_from_codex(error: CodexStorageError) -> StorageError {
    match error {
        CodexStorageError::Storage(error) => error,
        CodexStorageError::Sqlite(error) => StorageError::sqlite(error),
        CodexStorageError::Source(error) => StorageError::from(error),
        other => StorageError::invalid_state(other.to_string()),
    }
}

fn ensure_usage_root_materialized(
    source_tx: &mut CodexWriteTxn<'_>,
    root_session_id: &str,
) -> StorageResult<()> {
    source_tx.with_private_state(|transaction| {
        let related_source: Option<String> = transaction
            .query_row(
                "SELECT source FROM threads WHERE thread_id=?1",
                [root_session_id],
                |row| row.get(0),
            )
            .optional()?;
        match related_source.as_deref() {
            Some("codex") => Ok(()),
            Some(source) => Err(StorageError::invalid_state(format!(
                "usage root {root_session_id} belongs to source {source}"
            ))),
            None => Err(StorageError::invalid_state(format!(
                "usage root {root_session_id} is not materialized"
            ))),
        }
    })
}

fn rebuild_storage_error(error: crate::codex::storage::rebuild::RebuildError) -> StorageError {
    match error {
        crate::codex::storage::rebuild::RebuildError::Sql(error) => StorageError::sqlite(error),
        crate::codex::storage::rebuild::RebuildError::Invalid(message)
        | crate::codex::storage::rebuild::RebuildError::Cas(message) => {
            StorageError::usage_conflict(message)
        }
    }
}

fn now_ms_for_transaction() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "usage/tests.rs"]
mod tests;
