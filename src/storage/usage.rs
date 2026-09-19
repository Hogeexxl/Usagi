//! Atomic persistence seam for usage ingestion batches.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter};

use crate::domain::{CheckpointProcessingStatus, UsageEpochState};
use crate::usage::normalized::{NormalizedTokenUsage, canonical_algorithm_for};

use super::{Ledger, Result as StorageResult, StorageError};

pub(crate) const MAX_USAGE_BATCH_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_USAGE_BATCH_LINES: u64 = 4096;
pub(crate) const MAX_USAGE_BATCH_CANDIDATES: u64 = 2048;
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
type ExistingAnomaly = (Option<i64>, String, i64, i64, Option<i64>, String, String);

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
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageEventKind {
    Normal,
    Recovered,
    TurnCompensation,
}

impl UsageEventKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Recovered => "recovered",
            Self::TurnCompensation => "turn_compensation",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageEventWrite {
    pub event_id: String,
    pub kind: UsageEventKind,
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub turn_key: Option<String>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub estimated_cost_nanos_usd: Option<i64>,
    pub usage: NormalizedTokenUsage,
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
    pub candidate_count: i64,
    pub replayed_prefix_bytes: i64,
    pub replayed_prefix_lines: i64,
    pub fixed_view_exhausted: bool,
    pub tail_status: UsageTailStatus,
    pub tail_start_offset: Option<i64>,
    pub events: Vec<UsageEventWrite>,
    pub occurrences: Vec<UsageOccurrenceWrite>,
    pub skill_events: Vec<SkillUsageEventWrite>,
    pub turns: Vec<UsageTurnWrite>,
    pub anomalies: Vec<UsageAnomalyWrite>,
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
pub(crate) struct UsageSourcePlan {
    pub source_file_id: i64,
    pub action: UsagePlanAction,
    pub start_offset: i64,
    pub observed_size: i64,
    pub owning_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub checkpoint: Option<UsageCheckpointExpectation>,
    pub state: Option<UsageSourceStateWrite>,
    pub open_turn: Option<crate::usage::processor::TurnState>,
    pub build: Option<UsageBuildPlanState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageScanState {
    pub epoch: UsageEpochState,
    pub plans: Vec<UsageSourcePlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkListRow {
    pub source_file_id: i64,
    pub owning_thread_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkListState {
    pub epoch: UsageEpochState,
    pub rows: Vec<UsageWorkListRow>,
}

impl Ledger {
    pub(crate) fn load_usage_work_list(
        &self,
        present_source_ids: &[i64],
        parser_version: i64,
    ) -> StorageResult<UsageWorkListState> {
        validate_usage_source_ids(present_source_ids)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let epoch = read_epoch(&transaction)?;
        let canonical = canonical_algorithm_for(epoch.working_parser_version());
        let mut rows = Vec::new();

        // Epoch zero without a build and a parser transition are global
        // control states.  The caller handles the transition before asking
        // for executable source work.
        if !(epoch.active_epoch == 0 && epoch.build_epoch.is_none())
            && parser_version == epoch.working_parser_version()
            && let Some(canonical) = canonical
        {
            let mut query_ids = present_source_ids.to_vec();
            if let Some(build_epoch) = epoch.build_epoch {
                // Build members are part of the candidate universe even when
                // a source is currently missing from the present discovery
                // set; this is required for BeginCarry/ResumeCarry.
                let mut statement = transaction.prepare(
                    "SELECT source_file_id FROM usage_build_sources
                     WHERE build_epoch=?1 ORDER BY source_file_id",
                )?;
                for row in statement.query_map([build_epoch], |row| row.get::<_, i64>(0))? {
                    query_ids.push(row?);
                }
            }
            query_ids.sort_unstable();
            query_ids.dedup();

            const MAX_WORKLIST_BIND_IDS: usize = 900;
            for chunk in query_ids.chunks(MAX_WORKLIST_BIND_IDS) {
                if epoch.build_epoch.is_some() {
                    load_usage_build_work_list_chunk(
                        &transaction,
                        epoch,
                        parser_version,
                        canonical,
                        chunk,
                        &mut rows,
                    )?;
                } else {
                    load_usage_stable_work_list_chunk(
                        &transaction,
                        epoch,
                        parser_version,
                        canonical,
                        chunk,
                        &mut rows,
                    )?;
                }
            }
        }

        rows.sort_by(|left, right| {
            left.owning_thread_id
                .cmp(&right.owning_thread_id)
                .then_with(|| left.source_file_id.cmp(&right.source_file_id))
        });
        rows.dedup_by_key(|row| row.source_file_id);
        transaction.commit()?;
        Ok(UsageWorkListState { epoch, rows })
    }

    pub(crate) fn load_usage_scan_state_exact(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
        expected_epoch: UsageEpochState,
    ) -> StorageResult<UsageScanState> {
        validate_usage_source_ids(source_file_ids)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let epoch = read_epoch(&transaction)?;
        if epoch != expected_epoch {
            return Err(StorageError::invalid_state(
                "usage epoch changed while loading exact plans",
            ));
        }
        let mut plans = Vec::with_capacity(source_file_ids.len());
        for &source_file_id in source_file_ids {
            plans.push(load_source_plan(
                &transaction,
                source_file_id,
                parser_version,
                epoch,
            )?);
        }
        plans.sort_by_key(|plan| plan.source_file_id);
        transaction.commit()?;
        Ok(UsageScanState { epoch, plans })
    }

    pub(crate) fn load_usage_scan_state(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
    ) -> StorageResult<UsageScanState> {
        validate_usage_source_ids(source_file_ids)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let epoch = read_epoch(&transaction)?;
        let mut plans = Vec::with_capacity(source_file_ids.len());
        for &source_file_id in source_file_ids {
            plans.push(load_source_plan(
                &transaction,
                source_file_id,
                parser_version,
                epoch,
            )?);
        }
        plans.sort_by_key(|plan| plan.source_file_id);
        transaction.commit()?;
        Ok(UsageScanState { epoch, plans })
    }

    pub(crate) fn commit_usage(
        &self,
        batch: &UsageCommitBatch,
    ) -> StorageResult<UsageCommitOutcome> {
        validate_batch(batch)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        super::metadata::ensure_source_ready(&transaction, self)?;
        let epoch = read_epoch(&transaction)?;
        if batch.ledger_epoch != epoch.working_epoch()
            || batch.usage_parser_version != epoch.working_parser_version()
            || canonical_algorithm_for(batch.usage_parser_version).is_none()
        {
            return Err(StorageError::invalid_state(
                "usage working epoch or parser changed",
            ));
        }
        validate_group_relationship(&transaction, &batch.thread_id, &batch.root_session_id)?;
        let canonical_before = capture_affected_canonical_visibility(&transaction, batch)?;
        let skills_before = capture_skill_visibility(&transaction, batch)?;
        let has_local_replay = batch.sources.iter().any(|source| source.local_replay);

        let mut inserted = 0usize;
        let mut deduplicated = 0usize;
        for source in &batch.sources {
            validate_source_preconditions(&transaction, batch, source)?;
            if source.local_replay {
                prepare_local_replay(&transaction, batch, source)?;
            }
            for (event, occurrence) in source.events.iter().zip(&source.occurrences) {
                match write_or_compare_event(&transaction, batch.ledger_epoch, source, event)? {
                    CanonicalWrite::Inserted => inserted += 1,
                    CanonicalWrite::Duplicate => deduplicated += 1,
                }
                write_or_compare_occurrence(&transaction, batch.ledger_epoch, source, occurrence)?;
            }
            for skill in &source.skill_events {
                write_or_compare_skill_event(&transaction, batch.ledger_epoch, source, skill)?;
            }
            for turn in &source.turns {
                write_turn(
                    &transaction,
                    batch.ledger_epoch,
                    source.source_file_id,
                    source.expected_file_generation,
                    &batch.thread_id,
                    turn,
                )?;
            }
            for anomaly in &source.anomalies {
                write_anomaly(
                    &transaction,
                    batch.ledger_epoch,
                    &batch.thread_id,
                    source.source_file_id,
                    source.expected_file_generation,
                    anomaly,
                )?;
            }
            write_source_state(&transaction, batch, source)?;
            write_usage_checkpoint(&transaction, batch, source)?;
            update_build_progress(&transaction, epoch, batch, source)?;
            verify_source_postconditions(&transaction, batch, source)?;
        }
        if has_local_replay {
            cleanup_local_replay_orphans(&transaction, batch.ledger_epoch)?;
        }
        let token_visibility_changed = affected_canonical_visibility_changed(
            &transaction,
            batch.ledger_epoch,
            &canonical_before,
        )?;
        let skill_visibility_changed =
            affected_skill_visibility_changed(&transaction, batch.ledger_epoch, &skills_before)?;
        let canonical_changed = token_visibility_changed || skill_visibility_changed;

        let active_epoch = epoch.active_epoch;
        let current_revision: i64 =
            transaction.query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })?;
        let data_revision = if canonical_changed && batch.ledger_epoch == active_epoch {
            let next = current_revision
                .checked_add(1)
                .ok_or_else(|| StorageError::invalid_state("data revision overflow"))?;
            let changed = transaction.execute(
                "UPDATE app_meta SET data_revision=?1 WHERE id=1 AND data_revision=?2",
                params![next, current_revision],
            )?;
            if changed != 1 {
                return Err(StorageError::invalid_state("app meta revision changed"));
            }
            next
        } else {
            current_revision
        };
        let status_revision: i64 = transaction.query_row(
            "SELECT status_revision FROM app_meta WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        if canonical_changed && batch.ledger_epoch == active_epoch {
            self.publish_revisions(data_revision, status_revision);
        }
        Ok(UsageCommitOutcome {
            sources_committed: batch.sources.len(),
            events_inserted: inserted,
            events_deduplicated: deduplicated,
            data_revision,
        })
    }

    pub(crate) fn begin_usage_carry(&self, source_file_id: i64, now_ms: i64) -> StorageResult<()> {
        if now_ms < 0 {
            return Err(StorageError::invalid_state("negative carry time"));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let epoch = read_epoch(&transaction)?;
        let build_epoch = epoch
            .build_epoch
            .ok_or_else(|| StorageError::invalid_state("usage carry requires a build"))?;
        let parser = epoch.working_parser_version();
        let plan = load_source_plan(&transaction, source_file_id, parser, epoch)?;
        if plan.action != UsagePlanAction::BeginCarry {
            return Err(StorageError::invalid_state(
                "usage source is not eligible for BeginCarry",
            ));
        }
        let build = plan
            .build
            .as_ref()
            .ok_or_else(|| StorageError::invalid_state("usage carry manifest is missing"))?;
        verify_carry_canonical_events(&transaction, build_epoch)?;

        let partial_seed = plan.checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.processing_status == CheckpointProcessingStatus::Ready
        });
        if partial_seed {
            transaction.execute(
                "DELETE FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
                params![build_epoch, source_file_id],
            )?;
        }
        let changed = transaction.execute(
            "UPDATE source_checkpoints SET parser_version=?1,committed_offset=0,guard_hash=NULL,
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
            "UPDATE usage_build_sources SET carry_from_epoch=?1,carry_phase='occurrences',
                    carry_after_start_offset=NULL,carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,
                    completion_status='pending',completion_error_code=NULL,
                    completed_generation=NULL,completed_through_offset=NULL,updated_at_ms=?2
             WHERE build_epoch=?3 AND source_file_id=?4
               AND carry_phase='none' AND completion_status IN ('pending','blocked')
               AND required_through_offset=?5 AND active_committed_offset=?5",
            params![epoch.active_epoch, now_ms, build_epoch, source_file_id, build.active_committed_offset],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "usage carry manifest CAS failed",
            ));
        }
        let working_state_count: i64 = transaction.query_row(
            "SELECT count(*) FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![build_epoch, source_file_id],
            |row| row.get(0),
        )?;
        if working_state_count != 0 {
            return Err(StorageError::invalid_state(
                "carry-in-progress retained working source state",
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn resume_usage_carry(
        &self,
        source_file_id: i64,
        now_ms: i64,
    ) -> StorageResult<CarryStepOutcome> {
        if now_ms < 0 {
            return Err(StorageError::invalid_state("negative carry time"));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let epoch = read_epoch(&transaction)?;
        let build_epoch = epoch
            .build_epoch
            .ok_or_else(|| StorageError::invalid_state("usage carry requires a build"))?;
        let parser = epoch.working_parser_version();
        let plan = load_source_plan(&transaction, source_file_id, parser, epoch)?;
        if plan.action != UsagePlanAction::ResumeCarry {
            return Err(StorageError::invalid_state(
                "usage source is not in ResumeCarry",
            ));
        }
        let build = plan
            .build
            .as_ref()
            .ok_or_else(|| StorageError::invalid_state("usage carry manifest is missing"))?;
        verify_carry_db_proof(&transaction, epoch, source_file_id, build)?;

        let phase = build.carry_phase;
        let outcome = match phase {
            UsageCarryPhase::Occurrences => {
                carry_occurrence_page(
                    &transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )?;
                CarryStepOutcome::Progress
            }
            UsageCarryPhase::Turns => {
                carry_turn_page(
                    &transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )?;
                CarryStepOutcome::Progress
            }
            UsageCarryPhase::Anomalies => {
                carry_anomaly_page(
                    &transaction,
                    epoch.active_epoch,
                    build_epoch,
                    source_file_id,
                    now_ms,
                )?;
                CarryStepOutcome::Progress
            }
            UsageCarryPhase::Finalize => {
                finalize_carry(&transaction, epoch, source_file_id, build, now_ms)?
            }
            UsageCarryPhase::None => {
                return Err(StorageError::invalid_state(
                    "usage carry cursor is not initialized",
                ));
            }
        };
        transaction.commit()?;
        Ok(outcome)
    }

    pub(crate) fn complete_usage_build_source(
        &self,
        source_file_id: i64,
        now_ms: i64,
    ) -> StorageResult<()> {
        if now_ms < 0 {
            return Err(StorageError::invalid_state("negative completion time"));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let epoch = read_epoch(&transaction)?;
        let build_epoch = epoch
            .build_epoch
            .ok_or_else(|| StorageError::invalid_state("CompleteOnly requires a build"))?;
        let plan = load_source_plan(
            &transaction,
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
            "UPDATE usage_build_sources SET completion_status='rebuilt',completion_error_code=NULL,
                    completed_generation=required_generation,completed_through_offset=required_through_offset,
                    carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                    carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?1
             WHERE build_epoch=?2 AND source_file_id=?3 AND carry_phase='none'
               AND completion_status IN ('pending','blocked')
               AND required_generation=?4 AND required_through_offset=?5",
            params![now_ms, build_epoch, source_file_id, build.expected_file_generation, build.required_through_offset],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "CompleteOnly manifest CAS failed",
            ));
        }
        crate::usage::rebuild::verify_completion_row_for_storage(
            &transaction,
            build_epoch,
            source_file_id,
        )
        .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn cleanup_inactive_usage(&self, max_rows: usize) -> StorageResult<usize> {
        if max_rows == 0 {
            return Ok(0);
        }
        let limit = i64::try_from(max_rows)
            .map_err(|_| StorageError::invalid_state("cleanup row limit is too large"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (active, build): (i64, Option<i64>) = transaction.query_row(
            "SELECT usage_active_epoch,usage_build_epoch FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let excluded_build = build.unwrap_or(-1);
        let statements = [
            "DELETE FROM usage_session_quarantine_sources WHERE rowid IN (
                SELECT rowid FROM usage_session_quarantine_sources
                WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
                ORDER BY ledger_epoch,rowid LIMIT ?3)",
            "DELETE FROM usage_session_quarantine WHERE rowid IN (
                SELECT q.rowid FROM usage_session_quarantine q
                WHERE q.ledger_epoch<>?1 AND q.ledger_epoch<>?2
                  AND NOT EXISTS (
                    SELECT 1 FROM usage_session_quarantine_sources qs
                    WHERE qs.ledger_epoch=q.ledger_epoch AND qs.root_session_id=q.root_session_id)
                ORDER BY q.ledger_epoch,q.rowid LIMIT ?3)",
            "DELETE FROM usage_event_occurrences WHERE rowid IN (
                SELECT rowid FROM usage_event_occurrences WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
                ORDER BY ledger_epoch,rowid LIMIT ?3)",
            "DELETE FROM usage_events WHERE rowid IN (
                SELECT e.rowid FROM usage_events e WHERE e.ledger_epoch<>?1 AND e.ledger_epoch<>?2
                  AND NOT EXISTS (SELECT 1 FROM usage_event_occurrences o
                                  WHERE o.ledger_epoch=e.ledger_epoch AND o.event_id=e.event_id)
                ORDER BY e.ledger_epoch,e.rowid LIMIT ?3)",
            "DELETE FROM skill_usage_events WHERE rowid IN (
                SELECT rowid FROM skill_usage_events WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
                ORDER BY ledger_epoch,rowid LIMIT ?3)",
            "DELETE FROM turns WHERE rowid IN (
                SELECT rowid FROM turns WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
                ORDER BY ledger_epoch,rowid LIMIT ?3)",
            "DELETE FROM ingest_anomalies WHERE rowid IN (
                SELECT rowid FROM ingest_anomalies WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
                ORDER BY ledger_epoch,rowid LIMIT ?3)",
            "DELETE FROM usage_source_states WHERE rowid IN (
                SELECT rowid FROM usage_source_states WHERE ledger_epoch<>?1 AND ledger_epoch<>?2
                ORDER BY ledger_epoch,rowid LIMIT ?3)",
        ];
        let mut deleted = 0usize;
        for sql in statements {
            if deleted >= max_rows {
                break;
            }
            let remaining = i64::try_from(max_rows - deleted)
                .map_err(|_| StorageError::invalid_state("cleanup row limit is too large"))?;
            let count =
                transaction.execute(sql, params![active, excluded_build, remaining.min(limit)])?;
            deleted += count;
            if count > 0 {
                // Preserve FK-safe phase ordering across bounded cleanup calls.
                break;
            }
        }
        transaction.commit()?;
        Ok(deleted)
    }
}

fn read_epoch(transaction: &Transaction<'_>) -> StorageResult<UsageEpochState> {
    let values: (i64, Option<i64>, i64, Option<i64>) = transaction.query_row(
        "SELECT usage_active_epoch, usage_build_epoch, usage_parser_version,
                usage_build_parser_version FROM app_meta WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    UsageEpochState::new(values.0, values.1, values.2, values.3)
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
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
         JOIN source_files sf ON sf.source_file_id=i.source_file_id
         LEFT JOIN source_checkpoints cp
           ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'
         LEFT JOIN usage_source_states st
           ON st.ledger_epoch=?{epoch_bind} AND st.source_file_id=sf.source_file_id
         LEFT JOIN threads th ON th.thread_id=sf.thread_id
         WHERE sf.file_status='present'
           AND sf.thread_id IS NOT NULL
           AND th.root_session_id IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM usage_session_quarantine_sources qs
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
                    SELECT 1 FROM turns t
                    WHERE t.ledger_epoch=?{epoch_bind}
                      AND t.source_file_id=sf.source_file_id
                      AND t.status='open'))
                 OR
                 (st.active_turn_key IS NOT NULL
                  AND EXISTS (
                    SELECT 1 FROM turns t
                    WHERE t.ledger_epoch=?{epoch_bind}
                      AND t.source_file_id=sf.source_file_id
                      AND t.status='open'
                      AND t.turn_key=st.active_turn_key
                      AND t.state_through_offset<=st.resolved_through_offset
                      AND t.thread_id=sf.thread_id
                      AND t.file_generation=sf.file_generation)
                  AND NOT EXISTS (
                    SELECT 1 FROM turns t
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
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
         JOIN source_files sf ON sf.source_file_id=i.source_file_id
         LEFT JOIN usage_build_sources b
           ON b.build_epoch=?{build_bind} AND b.source_file_id=sf.source_file_id
         LEFT JOIN source_checkpoints cp
           ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'
         LEFT JOIN usage_source_states st
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
    transaction: &Transaction<'_>,
    source_file_id: i64,
    requested_parser: i64,
    epoch: UsageEpochState,
) -> StorageResult<UsageSourcePlan> {
    let source = transaction
        .query_row(
            "SELECT thread_id,device_id,inode,file_generation,observed_size,file_status
             FROM source_files WHERE source_file_id=?1",
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
    let state = read_usage_source_state(transaction, epoch.working_epoch(), source_file_id)?;
    let open_turn = match state.as_ref() {
        Some(state) => read_open_turn(transaction, epoch.working_epoch(), source_file_id, state)?,
        None => None,
    };
    let build = read_build_plan_state(transaction, epoch.build_epoch, source_file_id)?;

    let mut plan = UsageSourcePlan {
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
    transaction: &Transaction<'_>,
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
         FROM usage_build_sources WHERE build_epoch=?1 AND source_file_id=?2",
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
    open_turn: Option<&crate::usage::processor::TurnState>,
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
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
            (SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2)
          + (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2)",
        params![epoch.active_epoch,source_file_id],
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
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
    let active_fingerprint = crate::usage::rebuild::active_state_fingerprint(
        transaction,
        epoch.active_epoch,
        source_file_id,
    )
    .map_err(|error| StorageError::invalid_state(error.to_string()))?;
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
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
    let fingerprint = crate::usage::rebuild::active_state_fingerprint(
        transaction,
        epoch.active_epoch,
        source_file_id,
    )
    .map_err(|error| StorageError::invalid_state(error.to_string()))?;
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
        "SELECT count(*) FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
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

fn carry_occurrence_page(
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let after: Option<i64> = transaction.query_row(
        "SELECT carry_after_start_offset FROM usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='occurrences'",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    let mut statement = transaction.prepare(
        "SELECT source_start_offset FROM (
             SELECT source_start_offset FROM usage_event_occurrences
              WHERE ledger_epoch=?1 AND source_file_id=?2
             UNION
             SELECT source_start_offset FROM skill_usage_events
              WHERE ledger_epoch=?1 AND source_file_id=?2
         ) WHERE (?3 IS NULL OR source_start_offset>?3)
         ORDER BY source_start_offset LIMIT ?4",
    )?;
    let rows = statement
        .query_map(
            params![active_epoch, source_file_id, after, CARRY_PAGE_ROWS + 1],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = rows.len() > CARRY_PAGE_ROWS as usize;
    let copy = &rows[..rows.len().min(CARRY_PAGE_ROWS as usize)];
    for start in copy {
        let event_id: Option<String> = transaction
            .query_row(
                "SELECT event_id FROM usage_event_occurrences
                 WHERE ledger_epoch=?1 AND source_file_id=?2 AND source_start_offset=?3",
                params![active_epoch, source_file_id, start],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(event_id) = event_id {
            carry_canonical_event(transaction, active_epoch, build_epoch, &event_id)?;
            carry_occurrence(
                transaction,
                active_epoch,
                build_epoch,
                source_file_id,
                *start,
            )?;
        }
        carry_skill_events_at_offset(
            transaction,
            active_epoch,
            build_epoch,
            source_file_id,
            *start,
        )?;
    }
    let next_after = copy.last().copied().or(after);
    let (next_phase, next_cursor) = if has_more {
        ("occurrences", next_after)
    } else {
        ("turns", None)
    };
    let changed = transaction.execute(
        "UPDATE usage_build_sources SET carry_phase=?1,carry_after_start_offset=?2,updated_at_ms=?3
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

fn carry_canonical_event(
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    event_id: &str,
) -> StorageResult<()> {
    let inserted = transaction.execute(
        "INSERT INTO usage_events(
            ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
            reasoning_effort,input_tokens,cached_tokens,cache_write_tokens,
            output_tokens,reasoning_tokens,total_tokens,quality_status,estimated_cost_nanos_usd,
            source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms)
         SELECT ?1,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
            reasoning_effort,input_tokens,cached_tokens,cache_write_tokens,
            output_tokens,reasoning_tokens,total_tokens,quality_status,estimated_cost_nanos_usd,
            source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms
         FROM usage_events WHERE ledger_epoch=?2 AND event_id=?3
           AND NOT EXISTS(SELECT 1 FROM usage_events WHERE ledger_epoch=?1 AND event_id=?3)",
        params![build_epoch, active_epoch, event_id],
    )?;
    if inserted > 1 {
        return Err(StorageError::invalid_state(
            "usage carry canonical insert was not unique",
        ));
    }
    let active_exists: i64 = transaction.query_row(
        "SELECT count(*) FROM usage_events WHERE ledger_epoch=?1 AND event_id=?2",
        params![active_epoch, event_id],
        |row| row.get(0),
    )?;
    if active_exists != 1 {
        return Err(StorageError::invalid_state(
            "usage carry canonical source event is missing",
        ));
    }
    let equal: i64 = transaction.query_row(
        "SELECT count(*) FROM usage_events a JOIN usage_events b ON b.ledger_epoch=?2 AND b.event_id=a.event_id
         WHERE a.ledger_epoch=?1 AND a.event_id=?3
           AND b.event_kind=a.event_kind AND b.occurred_at_ms=a.occurred_at_ms
           AND b.thread_id=a.thread_id AND b.root_session_id=a.root_session_id
               AND b.turn_key IS a.turn_key AND b.model=a.model
               AND b.reasoning_effort IS a.reasoning_effort
           AND b.input_tokens=a.input_tokens AND b.cached_tokens=a.cached_tokens
           AND b.cache_write_tokens IS a.cache_write_tokens
           AND b.output_tokens=a.output_tokens
           AND b.reasoning_tokens=a.reasoning_tokens AND b.total_tokens=a.total_tokens
           AND b.quality_status=a.quality_status",
        params![active_epoch, build_epoch, event_id],
        |row| row.get(0),
    )?;
    if equal != 1 {
        return Err(StorageError::usage_conflict(
            "usage carry canonical event conflict",
        ));
    }
    Ok(())
}

fn carry_occurrence(
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    start_offset: i64,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO usage_event_occurrences(
            ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms)
         SELECT ?1,source_file_id,file_generation,source_start_offset,source_end_offset,event_id,created_at_ms
         FROM usage_event_occurrences WHERE ledger_epoch=?2 AND source_file_id=?3 AND source_start_offset=?4
           AND NOT EXISTS(SELECT 1 FROM usage_event_occurrences
                          WHERE ledger_epoch=?1 AND source_file_id=?3
                            AND file_generation=usage_event_occurrences.file_generation
                            AND source_start_offset=?4)",
        params![build_epoch, active_epoch, source_file_id, start_offset],
    )?;
    let equal: i64 = transaction.query_row(
        "SELECT count(*) FROM usage_event_occurrences a
         JOIN usage_event_occurrences b ON b.ledger_epoch=?2
           AND b.source_file_id=a.source_file_id AND b.file_generation=a.file_generation
           AND b.source_start_offset=a.source_start_offset
         WHERE a.ledger_epoch=?1 AND a.source_file_id=?3 AND a.source_start_offset=?4
           AND b.source_end_offset=a.source_end_offset AND b.event_id=a.event_id",
        params![active_epoch, build_epoch, source_file_id, start_offset],
        |row| row.get(0),
    )?;
    if equal != 1 {
        return Err(StorageError::usage_conflict(
            "usage carry occurrence conflict",
        ));
    }
    Ok(())
}

fn carry_turn_page(
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let after: Option<String> = transaction.query_row(
        "SELECT carry_after_turn_key FROM usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='turns'",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    let mut statement = transaction.prepare(
        "SELECT turn_key FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2
           AND (?3 IS NULL OR turn_key>?3) ORDER BY turn_key LIMIT ?4",
    )?;
    let rows = statement
        .query_map(
            params![active_epoch, source_file_id, after, CARRY_PAGE_ROWS + 1],
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
        "UPDATE usage_build_sources SET carry_phase=?1,carry_after_turn_key=?2,updated_at_ms=?3
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
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    turn_key: &str,
) -> StorageResult<()> {
    let build_exists: bool = transaction
        .query_row(
            "SELECT 1 FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2 AND turn_key=?3",
            params![build_epoch, source_file_id, turn_key],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if build_exists {
        let compatible: i64 = transaction.query_row(
            "SELECT count(*) FROM turns a JOIN turns b
               ON b.ledger_epoch=?2 AND b.source_file_id=a.source_file_id
              AND b.file_generation=a.file_generation AND b.turn_key=a.turn_key
             WHERE a.ledger_epoch=?1 AND a.source_file_id=?3 AND a.turn_key=?4
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
            params![active_epoch, build_epoch, source_file_id, turn_key],
            |row| row.get(0),
        )?;
        if compatible != 1 {
            return Err(StorageError::usage_conflict(
                "usage carry Turn seed conflict",
            ));
        }
        transaction.execute(
            "DELETE FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2 AND turn_key=?3",
            params![build_epoch, source_file_id, turn_key],
        )?;
    }
    let changed = transaction.execute(
        "INSERT INTO turns SELECT ?1,source_file_id,file_generation,turn_key,thread_id,raw_turn_id,
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
         FROM turns WHERE ledger_epoch=?2 AND source_file_id=?3 AND turn_key=?4",
        params![build_epoch, active_epoch, source_file_id, turn_key],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "usage carry active Turn is missing",
        ));
    }
    Ok(())
}

fn carry_anomaly_page(
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    now_ms: i64,
) -> StorageResult<()> {
    let after: Option<String> = transaction.query_row(
        "SELECT carry_after_anomaly_id FROM usage_build_sources
         WHERE build_epoch=?1 AND source_file_id=?2 AND carry_phase='anomalies'",
        params![build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    let mut statement = transaction.prepare(
        "SELECT anomaly_id FROM ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2
           AND (?3 IS NULL OR anomaly_id>?3) ORDER BY anomaly_id LIMIT ?4",
    )?;
    let rows = statement
        .query_map(
            params![active_epoch, source_file_id, after, CARRY_PAGE_ROWS + 1],
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
        "UPDATE usage_build_sources SET carry_phase=?1,carry_after_anomaly_id=?2,updated_at_ms=?3
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
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    anomaly_id: &str,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO ingest_anomalies(
            ledger_epoch,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
            file_generation,source_start_offset,anomaly_type,severity,details_json,resolved)
         SELECT ?1,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
            file_generation,source_start_offset,anomaly_type,severity,details_json,resolved
         FROM ingest_anomalies WHERE ledger_epoch=?2 AND source_file_id=?3 AND anomaly_id=?4
           AND NOT EXISTS(SELECT 1 FROM ingest_anomalies WHERE ledger_epoch=?1 AND anomaly_id=?4)",
        params![build_epoch, active_epoch, source_file_id, anomaly_id],
    )?;
    let equal: i64 = transaction.query_row(
        "SELECT count(*) FROM ingest_anomalies a JOIN ingest_anomalies b
           ON b.ledger_epoch=?2 AND b.anomaly_id=a.anomaly_id
         WHERE a.ledger_epoch=?1 AND a.source_file_id=?3 AND a.anomaly_id=?4
           AND b.occurred_at_ms IS a.occurred_at_ms AND b.thread_id IS a.thread_id
           AND b.source_file_id IS a.source_file_id AND b.file_generation IS a.file_generation
           AND b.source_start_offset IS a.source_start_offset AND b.anomaly_type=a.anomaly_type
           AND b.severity=a.severity AND b.details_json=a.details_json AND b.resolved=a.resolved",
        params![active_epoch, build_epoch, source_file_id, anomaly_id],
        |row| row.get(0),
    )?;
    if equal != 1 {
        return Err(StorageError::usage_conflict("usage carry anomaly conflict"));
    }
    Ok(())
}

fn finalize_carry(
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
        "SELECT file_status,file_generation,device_id,inode,observed_size FROM source_files WHERE source_file_id=?1",
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
        "UPDATE source_checkpoints SET parser_version=?1,committed_offset=?2,guard_hash=?3,
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
        "UPDATE usage_build_sources SET completion_status=?1,completion_error_code=?2,
                completed_generation=?3,completed_through_offset=?4,
                carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?5
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
    Ok(outcome)
}

fn verify_carry_sets(
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
) -> StorageResult<()> {
    verify_carry_canonical_events(transaction, build_epoch)?;
    let occurrence_diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM usage_event_occurrences
              WHERE ledger_epoch=?1 AND source_file_id=?3
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM usage_event_occurrences
              WHERE ledger_epoch=?2 AND source_file_id=?3))
        + (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM usage_event_occurrences
              WHERE ledger_epoch=?2 AND source_file_id=?3
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,event_id FROM usage_event_occurrences
              WHERE ledger_epoch=?1 AND source_file_id=?3))",
        params![active_epoch, build_epoch, source_file_id],
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
             FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3))
        + (SELECT count(*) FROM (
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3
             EXCEPT
             SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3))",
        params![active_epoch, build_epoch, source_file_id],
        |row| row.get(0),
    )?;
    if skill_diff != 0 {
        return Err(StorageError::usage_conflict(
            "usage carry Skill set mismatch",
        ));
    }
    // Compare complete active/build rows through deterministic fingerprints in
    // Rust so that updated_at_ms and ledger_epoch are the only excluded fields.
    let active_turns = carry_table_fingerprint(
        transaction,
        "turns",
        active_epoch,
        source_file_id,
        &["ledger_epoch", "updated_at_ms"],
    )?;
    let build_turns = carry_table_fingerprint(
        transaction,
        "turns",
        build_epoch,
        source_file_id,
        &["ledger_epoch", "updated_at_ms"],
    )?;
    if active_turns != build_turns {
        return Err(StorageError::usage_conflict(
            "usage carry Turn set mismatch",
        ));
    }
    let active_anomalies = carry_table_fingerprint(
        transaction,
        "ingest_anomalies",
        active_epoch,
        source_file_id,
        &["ledger_epoch", "detected_at_ms"],
    )?;
    let build_anomalies = carry_table_fingerprint(
        transaction,
        "ingest_anomalies",
        build_epoch,
        source_file_id,
        &["ledger_epoch", "detected_at_ms"],
    )?;
    if active_anomalies != build_anomalies {
        return Err(StorageError::usage_conflict(
            "usage carry anomaly set mismatch",
        ));
    }
    Ok(())
}

fn verify_carry_canonical_events(
    transaction: &Transaction<'_>,
    build_epoch: i64,
) -> StorageResult<()> {
    let extra: Option<String> = transaction
        .query_row(
            "SELECT build.event_id
             FROM usage_events build
             WHERE build.ledger_epoch=?1
               AND NOT EXISTS (
                   SELECT 1 FROM usage_event_occurrences occurrence
                   WHERE occurrence.ledger_epoch=?1
                     AND occurrence.event_id=build.event_id
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
    transaction: &Transaction<'_>,
    table: &str,
    epoch: i64,
    source_file_id: i64,
    excluded: &[&str],
) -> StorageResult<(i64, Vec<u8>)> {
    if !matches!(table, "turns" | "ingest_anomalies") {
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
    let order = if table == "turns" {
        "turn_key"
    } else {
        "anomaly_id"
    };
    let sql = format!(
        "SELECT {select} FROM {table} WHERE ledger_epoch=?1 AND source_file_id=?2 ORDER BY {order}"
    );
    let mut statement = transaction.prepare(&sql)?;
    let rows = statement
        .query_map(params![epoch, source_file_id], |row| {
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
    transaction: &Transaction<'_>,
    source_file_id: i64,
) -> StorageResult<Option<UsageCheckpointExpectation>> {
    transaction
        .query_row(
            "SELECT parser_version,committed_offset,guard_hash,processing_status
             FROM source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'",
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
    transaction: &Transaction<'_>,
    epoch: i64,
    source_file_id: i64,
) -> StorageResult<Option<UsageSourceStateWrite>> {
    transaction
        .query_row(
            "SELECT file_generation,device_id,inode,usage_parser_version,
                canonical_algorithm_version,resolved_through_offset,observed_raw_size,
                raw_tail_status,raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
                previous_total_input_tokens,previous_total_cached_tokens,
                previous_total_cache_write_tokens,previous_total_output_tokens,
                previous_total_reasoning_tokens,previous_total_total_tokens,previous_total_fingerprint,
                previous_total_offset,chain_state,chain_block_reason,active_turn_key,
                active_model,active_model_offset,active_reasoning_effort,active_reasoning_effort_offset,updated_at_ms
             FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
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
                    updated_at_ms: row.get(27)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)
}

fn read_open_turn(
    transaction: &Transaction<'_>,
    epoch: i64,
    source_file_id: i64,
    state: &UsageSourceStateWrite,
) -> StorageResult<Option<crate::usage::processor::TurnState>> {
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
         FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2 AND status='open' ORDER BY turn_key",
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
    let turn = crate::usage::processor::TurnState {
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
            ("none", None) => crate::usage::processor::TurnModelState::None,
            ("single", Some(model)) => crate::usage::processor::TurnModelState::Single(model),
            ("mixed", None) => crate::usage::processor::TurnModelState::Mixed,
            _ => {
                return Err(StorageError::invalid_state(
                    "invalid persisted Turn model state",
                ));
            }
        },
        unresolved_model_seen: row.get::<_, i64>(28)? != 0,
        reasoning_effort_state: match (reasoning_effort_state.as_str(), single_reasoning_effort) {
            ("none", None) => crate::usage::processor::TurnReasoningEffortState::None,
            ("single", Some(effort)) => {
                crate::usage::processor::TurnReasoningEffortState::Single(effort)
            }
            ("mixed", None) => crate::usage::processor::TurnReasoningEffortState::Mixed,
            _ => {
                return Err(StorageError::invalid_state(
                    "invalid persisted Turn reasoning-effort state",
                ));
            }
        },
        unresolved_reasoning_effort_seen: row.get::<_, i64>(31)? != 0,
        blocks: crate::usage::processor::CompensationBlocks {
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
    let mut candidates = 0i64;
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
        candidates = candidates
            .checked_add(source.candidate_count)
            .ok_or_else(|| StorageError::invalid_state("usage group candidate count overflow"))?;
    }
    let ordinary = adapter_bytes <= MAX_USAGE_BATCH_BYTES as i64
        && adapter_lines <= MAX_USAGE_BATCH_LINES as i64
        && candidates <= MAX_USAGE_BATCH_CANDIDATES as i64;
    let exclusive_progress = batch.sources.len() == 1 && {
        let source = &batch.sources[0];
        let source_adapter_bytes = source.source_bytes_consumed - source.replayed_prefix_bytes;
        let source_adapter_lines = source.complete_line_count - source.replayed_prefix_lines;
        (source_adapter_lines == 1
            && source_adapter_bytes <= MAX_LEGAL_LINE_BYTES as i64
            && source.candidate_count <= MAX_USAGE_BATCH_CANDIDATES as i64)
            || (source_adapter_lines == 1
                && source_adapter_bytes > MAX_LEGAL_LINE_BYTES as i64
                && source.candidate_count == 0
                && source.events.is_empty())
    };
    if !(ordinary || exclusive_progress) {
        return Err(StorageError::invalid_state(
            "usage Thread group exceeds fixed batch budget",
        ));
    }
    Ok(())
}

fn validate_source_payload(
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    if source.source_file_id <= 0
        || source.expected_file_generation <= 0
        || source.batch_start_offset < 0
        || source.fixed_observed_raw_size < 0
        || source.last_complete_offset < source.batch_start_offset
        || source.last_complete_offset > source.fixed_observed_raw_size
        || source.source_bytes_consumed != source.last_complete_offset - source.batch_start_offset
        || source.complete_line_count < source.replayed_prefix_lines
        || source.source_bytes_consumed < source.replayed_prefix_bytes
        || source.candidate_count < 0
        || source.candidate_count as usize != source.occurrences.len()
        || source.events.len() != source.occurrences.len()
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
        && source.candidate_count <= MAX_USAGE_BATCH_CANDIDATES as i64;
    let legal_single = adapter_lines == 1
        && adapter_bytes <= MAX_LEGAL_LINE_BYTES as i64
        && source.candidate_count <= MAX_USAGE_BATCH_CANDIDATES as i64;
    let oversized_only = adapter_lines == 1
        && adapter_bytes > MAX_LEGAL_LINE_BYTES as i64
        && source.candidate_count == 0
        && source.events.is_empty();
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
    for (event, occurrence) in source.events.iter().zip(&source.occurrences) {
        event
            .usage
            .validate()
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        if !valid_hash_id(&event.event_id)
            || event.thread_id != batch.thread_id
            || event.root_session_id != batch.root_session_id
            || occurrence.source_file_id != source.source_file_id
            || occurrence.file_generation != source.expected_file_generation
            || occurrence.event_id != event.event_id
            || occurrence.source_start_offset < source.batch_start_offset
            || occurrence.source_end_offset > source.last_complete_offset
            || occurrence.source_end_offset <= occurrence.source_start_offset
        {
            return Err(StorageError::invalid_state(
                "invalid usage event occurrence",
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
    transaction: &Transaction<'_>,
    thread_id: &str,
    root_session_id: &str,
) -> StorageResult<()> {
    let root: Option<String> = transaction
        .query_row(
            "SELECT root_session_id FROM threads WHERE thread_id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if root.as_deref() != Some(root_session_id) {
        return Err(StorageError::invalid_state(
            "usage root relationship is not confirmed",
        ));
    }
    Ok(())
}

fn validate_source_preconditions(
    transaction: &Transaction<'_>,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let current: Option<(Option<String>, i64, i64, i64, i64, String)> = transaction
        .query_row(
            "SELECT thread_id,file_generation,device_id,inode,observed_size,file_status
             FROM source_files WHERE source_file_id=?1",
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

fn prepare_local_replay(
    transaction: &Transaction<'_>,
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
                (SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2) +
                (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2)",
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
        "DELETE FROM usage_event_occurrences WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![batch.ledger_epoch, source.source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![batch.ledger_epoch, source.source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![batch.ledger_epoch, source.source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM ingest_anomalies WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![batch.ledger_epoch, source.source_file_id],
    )?;
    transaction.execute(
        "DELETE FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
        params![batch.ledger_epoch, source.source_file_id],
    )?;
    // Keep canonical rows until replay candidates have been compared. This is
    // required so a deterministic event ID with a different payload remains a
    // hard conflict even when this source owned the only occurrence. Orphans
    // are removed only after every source in the owning-Thread group has been
    // replayed successfully.
    Ok(())
}

fn capture_affected_canonical_visibility(
    transaction: &Transaction<'_>,
    batch: &UsageCommitBatch,
) -> StorageResult<HashSet<String>> {
    let mut ids = HashSet::new();
    for source in &batch.sources {
        for event in &source.events {
            ids.insert(event.event_id.clone());
        }
        if source.local_replay {
            let mut statement = transaction.prepare(
                "SELECT event_id FROM usage_event_occurrences
                 WHERE ledger_epoch=?1 AND source_file_id=?2",
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
            "SELECT EXISTS(SELECT 1 FROM usage_events WHERE ledger_epoch=?1 AND event_id=?2)",
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
    transaction: &Transaction<'_>,
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
            "SELECT EXISTS(SELECT 1 FROM usage_events WHERE ledger_epoch=?1 AND event_id=?2)",
            rusqlite::params![ledger_epoch, event_id],
            |row| row.get(0),
        )?;
        if (is_visible != 0) != was_visible {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cleanup_local_replay_orphans(
    transaction: &Transaction<'_>,
    ledger_epoch: i64,
) -> StorageResult<()> {
    transaction.execute(
        "DELETE FROM usage_events
         WHERE ledger_epoch=?1 AND NOT EXISTS (
             SELECT 1 FROM usage_event_occurrences o
             WHERE o.ledger_epoch=usage_events.ledger_epoch AND o.event_id=usage_events.event_id
         )",
        [ledger_epoch],
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
enum CanonicalWrite {
    Inserted,
    Duplicate,
}

#[derive(PartialEq, Eq)]
struct CanonicalEventRow {
    kind: String,
    occurred_at_ms: i64,
    thread_id: String,
    root_session_id: String,
    turn_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    input_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: Option<i64>,
    output_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    quality_status: String,
}

fn write_or_compare_event(
    transaction: &Transaction<'_>,
    epoch: i64,
    source: &UsageSourceCommit,
    event: &UsageEventWrite,
) -> StorageResult<CanonicalWrite> {
    let existing: Option<CanonicalEventRow> = transaction
        .query_row(
            "SELECT event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,reasoning_effort,
                input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status
             FROM usage_events WHERE ledger_epoch=?1 AND event_id=?2",
            params![epoch, event.event_id],
            |row| {
                Ok(CanonicalEventRow {
                    kind: row.get(0)?,
                    occurred_at_ms: row.get(1)?,
                    thread_id: row.get(2)?,
                    root_session_id: row.get(3)?,
                    turn_key: row.get(4)?,
                    model: row.get(5)?,
                    reasoning_effort: row.get(6)?,
                    input_tokens: row.get(7)?,
                    cached_tokens: row.get(8)?,
                    cache_write_tokens: row.get(9)?,
                    output_tokens: row.get(10)?,
                    reasoning_tokens: row.get(11)?,
                    total_tokens: row.get(12)?,
                    quality_status: row.get(13)?,
                })
            },
        )
        .optional()?;
    let quality = if event.usage.cache_write_tokens.is_none() {
        "partial"
    } else {
        "complete"
    };
    let canonical = CanonicalEventRow {
        kind: event.kind.as_str().to_owned(),
        occurred_at_ms: event.occurred_at_ms,
        thread_id: event.thread_id.clone(),
        root_session_id: event.root_session_id.clone(),
        turn_key: event.turn_key.clone(),
        model: event.model.clone(),
        reasoning_effort: event.reasoning_effort.clone(),
        input_tokens: event.usage.input_tokens,
        cached_tokens: event.usage.cached_tokens,
        cache_write_tokens: event.usage.cache_write_tokens,
        output_tokens: event.usage.output_tokens,
        reasoning_tokens: event.usage.reasoning_tokens,
        total_tokens: event.usage.total_tokens,
        quality_status: quality.to_owned(),
    };
    if let Some(existing) = existing {
        if existing == canonical {
            return Ok(CanonicalWrite::Duplicate);
        }
        return Err(StorageError::usage_conflict(
            "canonical usage event conflict",
        ));
    }
    let occurrence = source
        .occurrences
        .iter()
        .find(|occurrence| occurrence.event_id == event.event_id)
        .ok_or_else(|| StorageError::invalid_state("event occurrence is missing"))?;
    transaction.execute(
        "INSERT INTO usage_events (
            ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
            turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,
            output_tokens,reasoning_tokens,total_tokens,
            quality_status,source_file_id,file_generation,source_start_offset,
            source_end_offset,created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
            ?17,?18,?19,?20,?21,?22)",
        params![
            epoch,
            event.event_id,
            event.kind.as_str(),
            event.occurred_at_ms,
            event.thread_id,
            event.root_session_id,
            event.turn_key,
            event.model,
            event.reasoning_effort,
            event.estimated_cost_nanos_usd,
            event.usage.input_tokens,
            event.usage.cached_tokens,
            event.usage.cache_write_tokens,
            event.usage.output_tokens,
            event.usage.reasoning_tokens,
            event.usage.total_tokens,
            quality,
            source.source_file_id,
            source.expected_file_generation,
            occurrence.source_start_offset,
            occurrence.source_end_offset,
            source.committed_at_ms
        ],
    )?;
    Ok(CanonicalWrite::Inserted)
}

fn write_or_compare_skill_event(
    transaction: &Transaction<'_>,
    epoch: i64,
    source: &UsageSourceCommit,
    event: &SkillUsageEventWrite,
) -> StorageResult<()> {
    let existing: Option<(i64, i64, String, String, Option<String>)> = transaction
        .query_row(
            "SELECT source_end_offset,occurred_at_ms,thread_id,root_session_id,model
             FROM skill_usage_events
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
        "INSERT INTO skill_usage_events(
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
    transaction: &Transaction<'_>,
    epoch: i64,
    source_file_id: i64,
) -> StorageResult<Vec<u8>> {
    let mut statement = transaction.prepare(
        "SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                thread_id,root_session_id,model,skill_name
         FROM skill_usage_events
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
    transaction: &Transaction<'_>,
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
    transaction: &Transaction<'_>,
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
    transaction: &Transaction<'_>,
    active_epoch: i64,
    build_epoch: i64,
    source_file_id: i64,
    start_offset: i64,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO skill_usage_events(
            ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
            occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms)
         SELECT ?1,source_file_id,file_generation,source_start_offset,source_end_offset,
            occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms
         FROM skill_usage_events a
         WHERE a.ledger_epoch=?2 AND a.source_file_id=?3 AND a.source_start_offset=?4
           AND NOT EXISTS(
             SELECT 1 FROM skill_usage_events b
             WHERE b.ledger_epoch=?1 AND b.source_file_id=a.source_file_id
               AND b.file_generation=a.file_generation
               AND b.source_start_offset=a.source_start_offset AND b.skill_name=a.skill_name)",
        params![build_epoch, active_epoch, source_file_id, start_offset],
    )?;
    let diff: i64 = transaction.query_row(
        "SELECT
          (SELECT count(*) FROM (
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3 AND source_start_offset=?4
             EXCEPT
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3 AND source_start_offset=?4))
        + (SELECT count(*) FROM (
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?2 AND source_file_id=?3 AND source_start_offset=?4
             EXCEPT
             SELECT file_generation,source_end_offset,occurred_at_ms,thread_id,root_session_id,model,skill_name
             FROM skill_usage_events WHERE ledger_epoch=?1 AND source_file_id=?3 AND source_start_offset=?4))",
        params![active_epoch, build_epoch, source_file_id, start_offset],
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
    transaction: &Transaction<'_>,
    epoch: i64,
    source: &UsageSourceCommit,
    occurrence: &UsageOccurrenceWrite,
) -> StorageResult<()> {
    let existing: Option<(String, i64)> = transaction
        .query_row(
            "SELECT event_id,source_end_offset FROM usage_event_occurrences
             WHERE ledger_epoch=?1 AND source_file_id=?2 AND file_generation=?3
                AND source_start_offset=?4",
            params![
                epoch,
                source.source_file_id,
                source.expected_file_generation,
                occurrence.source_start_offset
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing == (occurrence.event_id.clone(), occurrence.source_end_offset) {
            return Ok(());
        }
        return Err(StorageError::usage_conflict("usage occurrence conflict"));
    }
    transaction.execute(
        "INSERT INTO usage_event_occurrences (
            ledger_epoch,source_file_id,file_generation,source_start_offset,
            source_end_offset,event_id,created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            epoch,
            source.source_file_id,
            source.expected_file_generation,
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

fn write_turn(
    transaction: &Transaction<'_>,
    ledger_epoch: i64,
    source_file_id: i64,
    file_generation: i64,
    thread_id: &str,
    turn: &UsageTurnWrite,
) -> StorageResult<()> {
    let start = snapshot_columns(turn.start_total.as_ref());
    let last = snapshot_columns(turn.last_total.as_ref());
    let accounted = snapshot_columns(Some(&turn.accounted));
    let compensation_allowed = turn.blocks == UsageCompensationBlocks::default();
    let changed = transaction.execute(
        "INSERT INTO turns (
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
         WHERE turns.thread_id=excluded.thread_id
            AND turns.start_offset=excluded.start_offset
            AND turns.raw_turn_id IS excluded.raw_turn_id
            AND turns.started_at_ms IS excluded.started_at_ms
            AND turns.start_total_input_tokens IS excluded.start_total_input_tokens
            AND turns.start_total_cached_tokens IS excluded.start_total_cached_tokens
            AND turns.start_total_cache_write_tokens IS excluded.start_total_cache_write_tokens
            AND turns.start_total_output_tokens IS excluded.start_total_output_tokens
            AND turns.start_total_reasoning_tokens IS excluded.start_total_reasoning_tokens
            AND turns.start_total_total_tokens IS excluded.start_total_total_tokens
            AND turns.start_total_fingerprint IS excluded.start_total_fingerprint
            AND (turns.status='open' OR turns.status=excluded.status)
            AND turns.block_start_missing <= excluded.block_start_missing
            AND turns.block_time_missing <= excluded.block_time_missing
            AND turns.block_reset <= excluded.block_reset
            AND turns.block_ownership_gap <= excluded.block_ownership_gap
            AND turns.block_parser_gap <= excluded.block_parser_gap
            AND turns.block_required_invalid <= excluded.block_required_invalid
            AND turns.block_model_unresolved <= excluded.block_model_unresolved
            AND turns.unresolved_model_seen <= excluded.unresolved_model_seen
            AND turns.unresolved_reasoning_effort_seen <= excluded.unresolved_reasoning_effort_seen
            -- Reasoning-effort Turn summary is monotonic: none -> single(same value) -> mixed.
            -- The existing durable state must never be replaced by a less informative state.
            AND (
                turns.reasoning_effort_state='none'
                OR (
                    turns.reasoning_effort_state='single'
                    AND (
                        (
                            excluded.reasoning_effort_state='single'
                            AND turns.single_reasoning_effort=excluded.single_reasoning_effort
                        )
                        OR excluded.reasoning_effort_state='mixed'
                    )
                )
                OR (
                    turns.reasoning_effort_state='mixed'
                    AND excluded.reasoning_effort_state='mixed'
                )
            )
            AND turns.accounted_candidate_count <= excluded.accounted_candidate_count
            AND turns.state_through_offset <= excluded.state_through_offset",
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

fn write_anomaly(
    transaction: &Transaction<'_>,
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
    );
    let existing: Option<ExistingAnomaly> = transaction
        .query_row(
            "SELECT occurred_at_ms,thread_id,source_file_id,file_generation,
                    source_start_offset,anomaly_type,severity
                 FROM ingest_anomalies WHERE ledger_epoch=?1 AND anomaly_id=?2",
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
                ))
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing == expected {
            return Ok(());
        }
        return Err(StorageError::usage_conflict(
            "deterministic anomaly conflict",
        ));
    }
    transaction.execute(
        "INSERT INTO ingest_anomalies (
            ledger_epoch,anomaly_id,detected_at_ms,occurred_at_ms,thread_id,source_file_id,
            file_generation,source_start_offset,anomaly_type,severity,details_json,resolved
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'{}',0)",
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
            }
        ],
    )?;
    Ok(())
}

fn write_source_state(
    transaction: &Transaction<'_>,
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
    transaction: &Transaction<'_>,
    ledger_epoch: i64,
    source_file_id: i64,
    state: &UsageSourceStateWrite,
) -> StorageResult<()> {
    let previous = snapshot_columns(state.previous_total.as_ref());
    let (chain, reason) = match state.chain_state {
        UsageChainState::Continuous => ("continuous", None),
        UsageChainState::Interrupted(reason) => ("interrupted", Some(reason.as_str())),
    };
    transaction.execute(
        "INSERT INTO usage_source_states (
            ledger_epoch,source_file_id,file_generation,device_id,inode,usage_parser_version,
            canonical_algorithm_version,resolved_through_offset,observed_raw_size,raw_tail_status,
            raw_tail_start_offset,owning_thread_id,root_session_id,continuation_state,
            previous_total_input_tokens,previous_total_cached_tokens,
            previous_total_cache_write_tokens,previous_total_output_tokens,
            previous_total_reasoning_tokens,previous_total_total_tokens,
            previous_total_fingerprint,previous_total_offset,chain_state,chain_block_reason,
            active_turn_key,active_model,active_model_offset,active_reasoning_effort,
            active_reasoning_effort_offset,updated_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
            ?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)
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
            updated_at_ms=excluded.updated_at_ms",
        params![
            ledger_epoch,source_file_id,state.file_generation,state.device_id,state.inode,
            state.usage_parser_version,state.canonical_algorithm_version,state.resolved_through_offset,
            state.observed_raw_size,state.raw_tail_status.as_str(),state.raw_tail_start_offset,
            state.owning_thread_id,state.root_session_id,state.continuation_state.as_str(),
            previous.0,previous.1,previous.2,previous.3,previous.4,previous.5,previous.6,
            state.previous_total_offset,chain,reason,state.active_turn_key,state.active_model,
            state.active_model_offset,state.active_reasoning_effort,state.active_reasoning_effort_offset,
            state.updated_at_ms
        ],
    )?;
    Ok(())
}

fn write_usage_checkpoint(
    transaction: &Transaction<'_>,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    if source.expected_checkpoint_missing {
        let changed = transaction.execute(
            "INSERT INTO source_checkpoints(
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
        "UPDATE source_checkpoints SET parser_version=?3,committed_offset=?4,guard_hash=?5,
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
    transaction: &Transaction<'_>,
    epoch: UsageEpochState,
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
            "UPDATE usage_build_sources SET
                required_through_offset=MAX(required_through_offset,?4),
                raw_tail_status=?5,raw_tail_start_offset=?6,
                completion_status='rebuilt',completion_error_code=NULL,
                completed_generation=?8,completed_through_offset=?4,
                carry_from_epoch=NULL,carry_phase='none',carry_after_start_offset=NULL,
                carry_after_turn_key=NULL,carry_after_anomaly_id=NULL,updated_at_ms=?7
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
            "UPDATE usage_build_sources SET
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
    transaction: &Transaction<'_>,
    batch: &UsageCommitBatch,
    source: &UsageSourceCommit,
) -> StorageResult<()> {
    let values: (i64, i64, String, i64, String, String, i64) = transaction.query_row(
        "SELECT c.parser_version,c.committed_offset,c.processing_status,
            s.resolved_through_offset,s.owning_thread_id,s.root_session_id,s.file_generation
         FROM source_checkpoints c JOIN usage_source_states s
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
        "SELECT count(*),min(turn_key) FROM turns WHERE ledger_epoch=?1 AND source_file_id=?2
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
    transaction: &Transaction<'_>,
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
    let (active_epoch, build_epoch, build_parser): (i64, Option<i64>, Option<i64>) = transaction
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,usage_build_parser_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

    // Active facts are stable user-visible data and may be reconciled in
    // place because only the confirmed root relation changed. This is in the
    // caller's metadata transaction, so revision advances at most once.
    if root_changed && active_epoch > 0 {
        let next_root = next_root.expect("root_changed requires a confirmed next root");
        transaction.execute(
            "UPDATE usage_events SET root_session_id=?1
             WHERE ledger_epoch=?2 AND thread_id=?3",
            params![next_root, active_epoch, thread_id],
        )?;
        transaction.execute(
            "UPDATE skill_usage_events SET root_session_id=?1
             WHERE ledger_epoch=?2 AND thread_id=?3",
            params![next_root, active_epoch, thread_id],
        )?;
        transaction.execute(
            "UPDATE usage_source_states SET root_session_id=?1
             WHERE ledger_epoch=?2 AND owning_thread_id=?3",
            params![next_root, active_epoch, thread_id],
        )?;
    }

    if let Some(build_epoch) = build_epoch {
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
                 FROM usage_build_sources b
                 JOIN source_files sf ON sf.source_file_id=b.source_file_id
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
                "SELECT source_file_id FROM source_files WHERE file_status='present' ORDER BY source_file_id",
            )?;
            for row in statement.query_map([], |row| row.get::<_, i64>(0))? {
                present.insert(row?);
            }
            crate::usage::rebuild::replace_build_preserving_all_members_tx(
                transaction,
                active_epoch_for_build,
                build_epoch,
                parser,
                &present,
                &invalidated,
                now_ms_for_transaction(),
            )
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        }
    }
    Ok(())
}

fn now_ms_for_transaction() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::storage::LedgerOptions;

    mod spec04_p2;
    mod usage_incremental_scan;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        ledger: Ledger,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "usagi-storage-usage-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("codex")).unwrap();
            let ledger = Ledger::open(LedgerOptions::new(
                root.join("mu.sqlite3"),
                root.join("codex"),
            ))
            .unwrap();
            {
                let connection = ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=?1 WHERE id=1",
                        [crate::usage::USAGE_PARSER_VERSION],
                    )
                    .unwrap();
                for (thread_id, parent, root_id, role) in [
                    ("root", None, Some("root"), "main"),
                    ("child", Some("root"), Some("root"), "subagent"),
                    ("unresolved", None, None, "unknown"),
                    ("other-root", None, Some("other-root"), "main"),
                ] {
                    connection
                        .execute(
                            "INSERT INTO threads (
                                thread_id,parent_thread_id,root_session_id,agent_role,project_kind,archived,
                                metadata_quality_status,metadata_resolved_at_ms
                             ) VALUES (?1,?2,?3,?4,'unknown',0,'complete',1)",
                            params![thread_id, parent, root_id, role],
                        )
                        .unwrap();
                }
            }
            Self { root, ledger }
        }

        fn add_source(&self, id: i64, thread_id: Option<&str>, device: i64) {
            let connection = self.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO source_files (
                        source_file_id,thread_id,current_path,source_area,device_id,inode,
                        file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                     ) VALUES (?1,?2,?3,'sessions',?4,?5,1,100,1,'present',1)",
                    params![
                        id,
                        thread_id,
                        format!("/tmp/usage-{id}.jsonl"),
                        device,
                        device
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO source_checkpoints (
                     source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                     processing_status,last_successful_scan_at_ms,last_error_code
                     ) VALUES (?1,'metadata',1,80,?2,'ready',1,NULL),
                              (?1,'usage',?3,0,NULL,'pending',NULL,NULL)",
                    params![id, vec![8_u8; 32], crate::usage::USAGE_PARSER_VERSION],
                )
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn vector() -> NormalizedTokenUsage {
        NormalizedTokenUsage::new(10, 2, Some(3), 4, 1, 14).unwrap()
    }

    fn state(
        thread_id: &str,
        root_id: &str,
        device: i64,
        offset: i64,
        active_turn: bool,
    ) -> UsageSourceStateWrite {
        let value = vector();
        UsageSourceStateWrite {
            file_generation: 1,
            device_id: device,
            inode: device,
            usage_parser_version: crate::usage::USAGE_PARSER_VERSION,
            canonical_algorithm_version: crate::usage::USAGE_CANONICAL_ALGORITHM_VERSION,
            resolved_through_offset: offset,
            observed_raw_size: 100,
            raw_tail_status: UsageTailStatus::Unverified,
            raw_tail_start_offset: None,
            owning_thread_id: thread_id.to_owned(),
            root_session_id: root_id.to_owned(),
            continuation_state: UsageContinuationState::OwningLive,
            previous_total: Some(UsageSnapshot {
                fingerprint: value.fingerprint().to_vec(),
                vector: value,
            }),
            previous_total_offset: Some(offset),
            chain_state: UsageChainState::Continuous,
            active_turn_key: active_turn.then(|| "turn".to_owned()),
            active_model: Some("model".to_owned()),
            active_model_offset: Some(0),
            active_reasoning_effort: None,
            active_reasoning_effort_offset: None,
            updated_at_ms: 10,
        }
    }

    fn source_commit(
        source_id: i64,
        device: i64,
        thread_id: &str,
        root_id: &str,
        event_id: char,
        with_auxiliary_rows: bool,
    ) -> UsageSourceCommit {
        let event_id = event_id.to_string().repeat(64);
        let value = vector();
        let snapshot = UsageSnapshot {
            fingerprint: value.fingerprint().to_vec(),
            vector: value.clone(),
        };
        UsageSourceCommit {
            source_file_id: source_id,
            expected_file_generation: 1,
            expected_previous_thread_id: Some(thread_id.to_owned()),
            expected_checkpoint: UsageCheckpointExpectation {
                parser_version: crate::usage::USAGE_PARSER_VERSION,
                committed_offset: 0,
                guard_hash: None,
                processing_status: CheckpointProcessingStatus::Pending,
            },
            expected_checkpoint_missing: false,
            expected_state: None,
            local_replay: false,
            batch_start_offset: 0,
            fixed_observed_raw_size: 100,
            last_complete_offset: 20,
            source_bytes_consumed: 20,
            complete_line_count: 1,
            candidate_count: 1,
            replayed_prefix_bytes: 0,
            replayed_prefix_lines: 0,
            fixed_view_exhausted: false,
            tail_status: UsageTailStatus::Unverified,
            tail_start_offset: None,
            events: vec![UsageEventWrite {
                event_id: event_id.clone(),
                kind: UsageEventKind::Normal,
                occurred_at_ms: 5,
                thread_id: thread_id.to_owned(),
                root_session_id: root_id.to_owned(),
                turn_key: None,
                model: "model".to_owned(),
                reasoning_effort: None,
                estimated_cost_nanos_usd: None,
                usage: value.clone(),
            }],
            occurrences: vec![UsageOccurrenceWrite {
                source_file_id: source_id,
                file_generation: 1,
                source_start_offset: 0,
                source_end_offset: 20,
                event_id,
            }],
            skill_events: Vec::new(),
            turns: with_auxiliary_rows
                .then(|| UsageTurnWrite {
                    turn_key: "turn".to_owned(),
                    raw_turn_id: None,
                    started_at_ms: Some(1),
                    ended_at_ms: None,
                    start_offset: 0,
                    end_offset: None,
                    status: UsageTurnStatus::Open,
                    start_total: None,
                    last_total: Some(snapshot.clone()),
                    accounted: snapshot.clone(),
                    accounted_candidate_count: 1,
                    model_state: UsageTurnModelState::Single("model".to_owned()),
                    reasoning_effort_state: UsageTurnReasoningEffortState::None,
                    unresolved_reasoning_effort_seen: false,
                    unresolved_model_seen: false,
                    blocks: UsageCompensationBlocks {
                        start_missing: true,
                        ..UsageCompensationBlocks::default()
                    },
                    quality_status: "partial",
                    state_through_offset: 20,
                    updated_at_ms: 10,
                })
                .into_iter()
                .collect(),
            anomalies: with_auxiliary_rows
                .then(|| UsageAnomalyWrite {
                    anomaly_id: "b".repeat(64),
                    detected_at_ms: 10,
                    occurred_at_ms: Some(5),
                    kind: UsageAnomalyKind::TurnReplaced,
                    severity_error: false,
                    source_start_offset: Some(0),
                })
                .into_iter()
                .collect(),
            updated_state: state(thread_id, root_id, device, 20, with_auxiliary_rows),
            next_guard_hash: Some(vec![9; 32]),
            committed_at_ms: 10,
        }
    }

    fn batch(thread_id: &str, root_id: &str, source: UsageSourceCommit) -> UsageCommitBatch {
        UsageCommitBatch {
            ledger_epoch: 1,
            usage_parser_version: crate::usage::USAGE_PARSER_VERSION,
            thread_id: thread_id.to_owned(),
            root_session_id: root_id.to_owned(),
            sources: vec![source],
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the test helper mirrors one continuation commit's proof fields"
    )]
    fn continuation_source(
        source: &mut UsageSourceCommit,
        expected_state: UsageSourceStateWrite,
        expected_offset: i64,
        expected_guard_hash: Vec<u8>,
        next_offset: i64,
        next_guard_hash: Vec<u8>,
        accounted_candidate_count: i64,
        usage: NormalizedTokenUsage,
        reasoning_effort_state: UsageTurnReasoningEffortState,
        unresolved_reasoning_effort_seen: bool,
    ) {
        source.expected_checkpoint = UsageCheckpointExpectation {
            parser_version: crate::usage::USAGE_PARSER_VERSION,
            committed_offset: expected_offset,
            guard_hash: Some(expected_guard_hash),
            processing_status: CheckpointProcessingStatus::Ready,
        };
        source.expected_state = Some(expected_state);
        source.batch_start_offset = expected_offset;
        source.last_complete_offset = next_offset;
        source.source_bytes_consumed = next_offset - expected_offset;
        source.next_guard_hash = Some(next_guard_hash);
        source.committed_at_ms = next_offset;

        source.events[0].occurred_at_ms = next_offset;
        source.events[0].turn_key = Some("turn".to_owned());
        source.events[0].reasoning_effort = match &reasoning_effort_state {
            UsageTurnReasoningEffortState::Single(value) => Some(value.clone()),
            UsageTurnReasoningEffortState::None | UsageTurnReasoningEffortState::Mixed => None,
        };
        source.events[0].usage = usage.clone();
        source.occurrences[0].source_start_offset = expected_offset;
        source.occurrences[0].source_end_offset = next_offset;

        let snapshot = UsageSnapshot {
            fingerprint: usage.fingerprint().to_vec(),
            vector: usage,
        };
        let turn = &mut source.turns[0];
        turn.last_total = Some(snapshot.clone());
        turn.accounted = snapshot;
        turn.accounted_candidate_count = accounted_candidate_count;
        turn.reasoning_effort_state = reasoning_effort_state.clone();
        turn.unresolved_reasoning_effort_seen = unresolved_reasoning_effort_seen;
        turn.state_through_offset = next_offset;
        turn.updated_at_ms = next_offset;

        let thread_id = source.updated_state.owning_thread_id.clone();
        let root_id = source.updated_state.root_session_id.clone();
        let device = source.updated_state.device_id;
        source.updated_state = state(&thread_id, &root_id, device, next_offset, true);
        source.updated_state.active_reasoning_effort = match &reasoning_effort_state {
            UsageTurnReasoningEffortState::Single(value) => Some(value.clone()),
            UsageTurnReasoningEffortState::None | UsageTurnReasoningEffortState::Mixed => None,
        };
        let effort_offset = source
            .updated_state
            .active_reasoning_effort
            .as_ref()
            .map(|_| next_offset);
        source.updated_state.active_reasoning_effort_offset = effort_offset;
        source.updated_state.updated_at_ms = next_offset;
    }

    fn durable_turn_snapshot(
        transaction: &Transaction<'_>,
        source_file_id: i64,
    ) -> (String, Option<String>, i64, i64, i64, i64) {
        transaction
            .query_row(
                "SELECT reasoning_effort_state,single_reasoning_effort,
                        accounted_total_tokens,accounted_candidate_count,
                        state_through_offset,updated_at_ms
                 FROM turns
                 WHERE ledger_epoch=1 AND source_file_id=?1
                   AND file_generation=1 AND turn_key='turn'",
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
            .unwrap()
    }

    #[test]
    fn atomic_commit_duplicate_occurrence_and_conflict_matrix() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        let first = batch(
            "child",
            "root",
            source_commit(1, 11, "child", "root", 'a', true),
        );
        let outcome = fixture.ledger.commit_usage(&first).unwrap();
        assert_eq!(
            (
                outcome.events_inserted,
                outcome.events_deduplicated,
                outcome.data_revision
            ),
            (1, 0, 2)
        );
        let connection = fixture.ledger.connection().unwrap();
        let facts: (i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events),
                    (SELECT count(*) FROM usage_event_occurrences),
                    (SELECT count(*) FROM turns),
                    (SELECT count(*) FROM ingest_anomalies),
                    (SELECT count(*) FROM usage_source_states),
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='metadata')",
                [],
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
            .unwrap();
        assert_eq!(facts, (1, 1, 1, 1, 1, 80));
        drop(connection);

        fixture.add_source(2, Some("child"), 12);
        let duplicate = batch(
            "child",
            "root",
            source_commit(2, 12, "child", "root", 'a', false),
        );
        let outcome = fixture.ledger.commit_usage(&duplicate).unwrap();
        assert_eq!(
            (
                outcome.events_inserted,
                outcome.events_deduplicated,
                outcome.data_revision
            ),
            (0, 1, 2)
        );
        let connection = fixture.ledger.connection().unwrap();
        let counts: (i64, i64) = connection
            .query_row("SELECT (SELECT count(*) FROM usage_events),(SELECT count(*) FROM usage_event_occurrences)", [], |row| Ok((row.get(0)?,row.get(1)?)))
            .unwrap();
        assert_eq!(counts, (1, 2));
        drop(connection);

        fixture.add_source(3, Some("child"), 13);
        let mut conflict = source_commit(3, 13, "child", "root", 'a', true);
        conflict.events[0].model = "conflicting-model".to_owned();
        assert!(
            fixture
                .ledger
                .commit_usage(&batch("child", "root", conflict))
                .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        let rolled_back: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_event_occurrences WHERE source_file_id=3),
                    (SELECT count(*) FROM usage_source_states WHERE source_file_id=3),
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=3 AND consumer_kind='usage')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(rolled_back, (0, 0, 0));
        drop(connection);

        fixture.add_source(4, Some("child"), 14);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_event_occurrences (
                        ledger_epoch,source_file_id,file_generation,source_start_offset,
                        source_end_offset,event_id,created_at_ms
                     ) VALUES (1,4,1,0,19,?1,1)",
                    ["a".repeat(64)],
                )
                .unwrap();
        }
        assert!(
            fixture
                .ledger
                .commit_usage(&batch(
                    "child",
                    "root",
                    source_commit(4, 14, "child", "root", 'a', false),
                ))
                .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        let occurrence_end_and_checkpoint: (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT source_end_offset FROM usage_event_occurrences WHERE source_file_id=4),
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=4 AND consumer_kind='usage')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(occurrence_end_and_checkpoint, (19, 0));
        drop(connection);

        fixture.add_source(5, Some("child"), 15);
        fixture.add_source(6, Some("child"), 16);
        let first_in_group = source_commit(5, 15, "child", "root", 'e', false);
        let mut stale_second = source_commit(6, 16, "child", "root", 'f', false);
        stale_second.expected_file_generation = 2;
        let atomic_group = UsageCommitBatch {
            ledger_epoch: 1,
            usage_parser_version: crate::usage::USAGE_PARSER_VERSION,
            thread_id: "child".to_owned(),
            root_session_id: "root".to_owned(),
            sources: vec![first_in_group, stale_second],
        };
        assert!(fixture.ledger.commit_usage(&atomic_group).is_err());
        let connection = fixture.ledger.connection().unwrap();
        let group_state: (i64, i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events WHERE event_id=?1),
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=5 AND consumer_kind='usage'),
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=6 AND consumer_kind='usage')",
                ["e".repeat(64)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(group_state, (0, 0, 0));
    }

    #[test]
    fn t_mu03_c02_durable_effort_round_trip_restart_and_fingerprint() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        let mut committed = source_commit(1, 11, "child", "root", 'e', true);
        committed.events[0].reasoning_effort = Some("high".to_owned());
        committed.updated_state.active_reasoning_effort = Some("high".to_owned());
        committed.updated_state.active_reasoning_effort_offset = Some(10);
        let turn = committed.turns.first_mut().unwrap();
        turn.reasoning_effort_state = UsageTurnReasoningEffortState::Single("high".to_owned());
        turn.unresolved_reasoning_effort_seen = false;
        if let Err(error) = fixture
            .ledger
            .commit_usage(&batch("child", "root", committed))
        {
            panic!(
                "durable effort commit failed: {} ({:?})",
                error,
                std::error::Error::source(&error)
            );
        }

        let scan = fixture
            .ledger
            .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
            .unwrap();
        let state = scan.plans[0].state.as_ref().unwrap();
        assert_eq!(state.active_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(state.active_reasoning_effort_offset, Some(10));
        let open_turn = scan.plans[0].open_turn.as_ref().unwrap();
        assert_eq!(
            open_turn.reasoning_effort_state,
            crate::usage::processor::TurnReasoningEffortState::Single("high".to_owned())
        );
        assert!(!open_turn.unresolved_reasoning_effort_seen);

        let fingerprint_before = {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let value = crate::usage::rebuild::active_state_fingerprint(&transaction, 1, 1)
                .unwrap()
                .unwrap();
            transaction.commit().unwrap();
            value
        };
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states
                     SET active_reasoning_effort='medium',active_reasoning_effort_offset=11
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        let fingerprint_after = {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let value = crate::usage::rebuild::active_state_fingerprint(&transaction, 1, 1)
                .unwrap()
                .unwrap();
            transaction.commit().unwrap();
            value
        };
        assert_ne!(fingerprint_before, fingerprint_after);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states
                     SET active_reasoning_effort='high',active_reasoning_effort_offset=10
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }

        let reopened = Ledger::open(LedgerOptions::new(
            fixture.root.join("mu.sqlite3"),
            fixture.root.join("codex"),
        ))
        .unwrap();
        let restarted = reopened
            .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
            .unwrap();
        let restarted_state = restarted.plans[0].state.as_ref().unwrap();
        assert_eq!(
            restarted_state.active_reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(restarted_state.active_reasoning_effort_offset, Some(10));
        assert_eq!(
            restarted.plans[0]
                .open_turn
                .as_ref()
                .unwrap()
                .reasoning_effort_state,
            crate::usage::processor::TurnReasoningEffortState::Single("high".to_owned())
        );
        let connection = reopened.connection().unwrap();
        let persisted: (Option<String>, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT reasoning_effort,estimated_cost_nanos_usd,
                        (SELECT unresolved_reasoning_effort_seen FROM turns
                         WHERE ledger_epoch=1 AND source_file_id=1 AND turn_key='turn')
                 FROM usage_events WHERE ledger_epoch=1 AND event_id=?1",
                ["e".repeat(64)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(persisted, (Some("high".to_owned()), None, Some(0)));
    }

    #[test]
    fn t_hf_re01_write_turn_reasoning_effort_monotonic_matrix() {
        let fixture = Fixture::new();
        let cases = [
            (
                UsageTurnReasoningEffortState::None,
                UsageTurnReasoningEffortState::None,
                true,
            ),
            (
                UsageTurnReasoningEffortState::None,
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                true,
            ),
            (
                UsageTurnReasoningEffortState::None,
                UsageTurnReasoningEffortState::Mixed,
                true,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                true,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::Mixed,
                true,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::None,
                false,
            ),
            (
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                UsageTurnReasoningEffortState::Single("medium".to_owned()),
                false,
            ),
            (
                UsageTurnReasoningEffortState::Mixed,
                UsageTurnReasoningEffortState::Mixed,
                true,
            ),
            (
                UsageTurnReasoningEffortState::Mixed,
                UsageTurnReasoningEffortState::None,
                false,
            ),
            (
                UsageTurnReasoningEffortState::Mixed,
                UsageTurnReasoningEffortState::Single("high".to_owned()),
                false,
            ),
        ];

        for (index, (existing_state, incoming_state, allowed)) in cases.into_iter().enumerate() {
            let source_file_id = i64::try_from(index + 1).unwrap();
            fixture.add_source(source_file_id, Some("child"), source_file_id + 10);
            let mut existing = source_commit(
                source_file_id,
                source_file_id + 10,
                "child",
                "root",
                'a',
                true,
            )
            .turns
            .into_iter()
            .next()
            .unwrap();
            existing.reasoning_effort_state = existing_state;
            let mut incoming = existing.clone();
            incoming.reasoning_effort_state = incoming_state.clone();

            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            write_turn(&transaction, 1, source_file_id, 1, "child", &existing).unwrap();
            let before = durable_turn_snapshot(&transaction, source_file_id);
            let result = write_turn(&transaction, 1, source_file_id, 1, "child", &incoming);
            if allowed {
                result.unwrap();
                let after = durable_turn_snapshot(&transaction, source_file_id);
                assert_eq!(after.0, incoming.reasoning_effort_state.as_str().to_owned());
                assert_eq!(
                    after.1,
                    incoming
                        .reasoning_effort_state
                        .single_effort()
                        .map(str::to_owned)
                );
            } else {
                let error = result.unwrap_err();
                assert!(error.requires_usage_rebuild());
                assert_eq!(durable_turn_snapshot(&transaction, source_file_id), before);
            }
            transaction.commit().unwrap();
        }
    }

    #[test]
    fn t_hf_re02_same_open_turn_none_to_single_high_across_commits() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);

        let first = source_commit(1, 11, "child", "root", 'a', true);
        let first_state = first.updated_state.clone();
        fixture
            .ledger
            .commit_usage(&batch("child", "root", first))
            .unwrap();
        let first_snapshot = {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .unwrap();
            let snapshot = durable_turn_snapshot(&transaction, 1);
            transaction.commit().unwrap();
            snapshot
        };
        assert_eq!(first_snapshot, ("none".to_owned(), None, 14, 1, 20, 10));

        let mut second = source_commit(1, 11, "child", "root", 'b', true);
        let usage = NormalizedTokenUsage::new(20, 4, Some(5), 8, 2, 28).unwrap();
        continuation_source(
            &mut second,
            first_state,
            20,
            vec![9; 32],
            40,
            vec![10; 32],
            2,
            usage,
            UsageTurnReasoningEffortState::Single("high".to_owned()),
            false,
        );
        fixture
            .ledger
            .commit_usage(&batch("child", "root", second))
            .unwrap();

        let mut connection = fixture.ledger.connection().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        let final_snapshot = durable_turn_snapshot(&transaction, 1);
        assert_eq!(
            final_snapshot,
            ("single".to_owned(), Some("high".to_owned()), 28, 2, 40, 40)
        );
        let checkpoint_and_state: (i64, i64) = transaction
            .query_row(
                "SELECT
                    (SELECT committed_offset FROM source_checkpoints
                     WHERE source_file_id=1 AND consumer_kind='usage'),
                    (SELECT resolved_through_offset FROM usage_source_states
                     WHERE ledger_epoch=1 AND source_file_id=1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(checkpoint_and_state, (40, 40));
        transaction.commit().unwrap();
    }

    #[test]
    fn t_hf_re03_same_open_turn_none_single_high_mixed_across_commits() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);

        let first = source_commit(1, 11, "child", "root", 'a', true);
        let first_state = first.updated_state.clone();
        fixture
            .ledger
            .commit_usage(&batch("child", "root", first))
            .unwrap();

        let mut second = source_commit(1, 11, "child", "root", 'b', true);
        continuation_source(
            &mut second,
            first_state,
            20,
            vec![9; 32],
            40,
            vec![10; 32],
            2,
            NormalizedTokenUsage::new(20, 4, Some(5), 8, 2, 28).unwrap(),
            UsageTurnReasoningEffortState::Single("high".to_owned()),
            false,
        );
        let second_state = second.updated_state.clone();
        fixture
            .ledger
            .commit_usage(&batch("child", "root", second))
            .unwrap();
        let second_scan = fixture
            .ledger
            .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
            .unwrap();
        assert!(
            !second_scan.plans[0]
                .open_turn
                .as_ref()
                .unwrap()
                .unresolved_reasoning_effort_seen
        );

        let mut third = source_commit(1, 11, "child", "root", 'c', true);
        continuation_source(
            &mut third,
            second_state,
            40,
            vec![10; 32],
            60,
            vec![11; 32],
            3,
            NormalizedTokenUsage::new(30, 6, Some(7), 12, 3, 42).unwrap(),
            UsageTurnReasoningEffortState::Mixed,
            true,
        );
        third.events[0].reasoning_effort = Some("medium".to_owned());
        fixture
            .ledger
            .commit_usage(&batch("child", "root", third))
            .unwrap();

        let mut connection = fixture.ledger.connection().unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        let final_snapshot = durable_turn_snapshot(&transaction, 1);
        assert_eq!(final_snapshot, ("mixed".to_owned(), None, 42, 3, 60, 60));
        let unresolved: i64 = transaction
            .query_row(
                "SELECT unresolved_reasoning_effort_seen FROM turns
                 WHERE ledger_epoch=1 AND source_file_id=1 AND turn_key='turn'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unresolved, 1);
        let compensation_count: i64 = transaction
            .query_row(
                "SELECT count(*) FROM usage_events
                 WHERE ledger_epoch=1 AND event_kind='turn_compensation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if compensation_count > 0 {
            let compensation_effort: Option<String> = transaction
                .query_row(
                    "SELECT reasoning_effort FROM usage_events
                     WHERE ledger_epoch=1 AND event_kind='turn_compensation'
                     LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(compensation_effort, None);
        }
        transaction.commit().unwrap();
    }

    #[test]
    fn t_mu03_b04_carry_preserves_cost_and_identity_ignores_derived_cost() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("child"), 12);
        let mut first_source = source_commit(1, 11, "child", "root", 'a', true);
        first_source.events[0].reasoning_effort = Some("high".to_owned());
        first_source.events[0].estimated_cost_nanos_usd = Some(5_725_000);
        first_source.updated_state.active_reasoning_effort = Some("high".to_owned());
        first_source.updated_state.active_reasoning_effort_offset = Some(10);
        first_source.turns[0].reasoning_effort_state =
            UsageTurnReasoningEffortState::Single("high".to_owned());
        fixture
            .ledger
            .commit_usage(&batch("child", "root", first_source))
            .unwrap();
        let mut duplicate_source = source_commit(2, 12, "child", "root", 'a', true);
        duplicate_source.events[0].reasoning_effort = Some("high".to_owned());
        duplicate_source.events[0].estimated_cost_nanos_usd = None;
        duplicate_source.updated_state.active_reasoning_effort = Some("high".to_owned());
        duplicate_source
            .updated_state
            .active_reasoning_effort_offset = Some(10);
        duplicate_source.turns[0].reasoning_effort_state =
            UsageTurnReasoningEffortState::Single("high".to_owned());
        duplicate_source.anomalies[0].anomaly_id = "c".repeat(64);
        let duplicate = match fixture
            .ledger
            .commit_usage(&batch("child", "root", duplicate_source))
        {
            Ok(value) => value,
            Err(error) => panic!(
                "carry duplicate seed commit failed: {} ({:?})",
                error,
                std::error::Error::source(&error)
            ),
        };
        assert_eq!(
            (duplicate.events_inserted, duplicate.events_deduplicated),
            (0, 1)
        );
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_files SET observed_size=20 WHERE source_file_id IN (1,2)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states SET observed_raw_size=20,raw_tail_status='none',raw_tail_start_offset=NULL
                     WHERE ledger_epoch=1 AND source_file_id IN (1,2)",
                    [],
                )
                .unwrap();
        }
        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 30)
                .unwrap();
        }
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_files SET file_status='missing' WHERE source_file_id=2",
                    [],
                )
                .unwrap();
        }
        fixture.ledger.begin_usage_carry(2, 31).unwrap();
        fixture.ledger.resume_usage_carry(2, 32).unwrap();
        fixture.ledger.resume_usage_carry(2, 33).unwrap();
        let connection = fixture.ledger.connection().unwrap();
        let proof: (i64, i64, String, String, Option<String>, i64) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM usage_events WHERE ledger_epoch=2 AND event_id=?1),
                    (SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=2 AND source_file_id=2 AND event_id=?1),
                    (SELECT carry_phase FROM usage_build_sources WHERE build_epoch=2 AND source_file_id=2),
                    (SELECT reasoning_effort_state FROM turns
                     WHERE ledger_epoch=2 AND source_file_id=2 AND turn_key='turn'),
                    (SELECT single_reasoning_effort FROM turns
                     WHERE ledger_epoch=2 AND source_file_id=2 AND turn_key='turn'),
                    (SELECT unresolved_reasoning_effort_seen FROM turns
                     WHERE ledger_epoch=2 AND source_file_id=2 AND turn_key='turn')",
                ["a".repeat(64)],
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
            .unwrap();
        assert_eq!(
            proof,
            (
                1,
                1,
                "anomalies".into(),
                "single".into(),
                Some("high".into()),
                0
            )
        );
        let copied_effort: Option<String> = connection
            .query_row(
                "SELECT reasoning_effort FROM usage_events
                 WHERE ledger_epoch=2 AND event_id=?1",
                ["a".repeat(64)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(copied_effort.as_deref(), Some("high"));
        let copied_cost: Option<i64> = connection
            .query_row(
                "SELECT estimated_cost_nanos_usd FROM usage_events
                 WHERE ledger_epoch=2 AND event_id=?1",
                ["a".repeat(64)],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(copied_cost, Some(5_725_000));
    }

    #[test]
    fn storage_rejects_contradictory_fixed_view_tail_proofs_without_checkpoint_progress() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);

        let mut exhausted_unverified = source_commit(1, 11, "child", "root", 'x', false);
        exhausted_unverified.fixed_view_exhausted = true;
        let invalid = batch("child", "root", exhausted_unverified);
        assert!(fixture.ledger.commit_usage(&invalid).is_err());

        let mut early_none = source_commit(1, 11, "child", "root", 'y', false);
        early_none.fixed_view_exhausted = true;
        early_none.tail_status = UsageTailStatus::None;
        early_none.updated_state.raw_tail_status = UsageTailStatus::None;
        assert!(
            fixture
                .ledger
                .commit_usage(&batch("child", "root", early_none))
                .is_err()
        );

        let connection = fixture.ledger.connection().unwrap();
        let proof: (i64, i64) = connection
            .query_row(
                "SELECT c.committed_offset,
                        (SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=1 AND source_file_id=1)
                 FROM source_checkpoints c
                 WHERE c.source_file_id=1 AND c.consumer_kind='usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(proof, (0, 0));
    }

    #[test]
    fn plan_and_verified_error_recovery_matrix_preserves_metadata_boundary() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("unresolved"), 12);
        let initial = fixture
            .ledger
            .load_usage_scan_state(&[2, 1], crate::usage::USAGE_PARSER_VERSION)
            .unwrap();
        assert_eq!(initial.plans[0].action, UsagePlanAction::ReadFrom);
        assert_eq!(
            initial.plans[1].action,
            UsagePlanAction::BlockedRelationship
        );

        let first = batch(
            "child",
            "root",
            source_commit(1, 11, "child", "root", 'a', false),
        );
        fixture.ledger.commit_usage(&first).unwrap();
        let resumed = fixture
            .ledger
            .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
            .unwrap();
        assert_eq!(resumed.plans[0].action, UsagePlanAction::ResumeOwningLive);
        assert_eq!(resumed.plans[0].start_offset, 20);
        assert_eq!(
            fixture
                .ledger
                .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION + 1)
                .unwrap()
                .plans[0]
                .action,
            UsagePlanAction::RebuildRequired
        );

        let expected_state = resumed.plans[0].state.clone().unwrap();
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_checkpoints SET processing_status='error',last_error_code='USAGE_FAILED'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .unwrap();
        }
        let mut recovery = source_commit(1, 11, "child", "root", 'c', false);
        recovery.expected_checkpoint = UsageCheckpointExpectation {
            parser_version: crate::usage::USAGE_PARSER_VERSION,
            committed_offset: 20,
            guard_hash: Some(vec![9; 32]),
            processing_status: CheckpointProcessingStatus::Error,
        };
        recovery.expected_state = Some(expected_state);
        recovery.batch_start_offset = 20;
        recovery.last_complete_offset = 40;
        recovery.source_bytes_consumed = 20;
        recovery.occurrences[0].source_start_offset = 20;
        recovery.occurrences[0].source_end_offset = 40;
        recovery.updated_state.resolved_through_offset = 40;
        recovery.updated_state.previous_total_offset = Some(40);
        let outcome = fixture
            .ledger
            .commit_usage(&batch("child", "root", recovery))
            .unwrap();
        assert_eq!(outcome.data_revision, 3);
        let connection = fixture.ledger.connection().unwrap();
        let boundaries: (i64, i64, String) = connection
            .query_row(
                "SELECT
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='metadata'),
                    (SELECT committed_offset FROM source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='usage'),
                    (SELECT processing_status FROM source_checkpoints
                        WHERE source_file_id=1 AND consumer_kind='usage')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(boundaries, (80, 40, "ready".to_owned()));
        drop(connection);

        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE threads SET parent_thread_id='root',root_session_id='root',
                        agent_role='subagent' WHERE thread_id='unresolved'",
                    [],
                )
                .unwrap();
        }
        assert_eq!(
            fixture
                .ledger
                .load_usage_scan_state(&[2], crate::usage::USAGE_PARSER_VERSION)
                .unwrap()
                .plans[0]
                .action,
            UsagePlanAction::ReadFrom
        );
        fixture
            .ledger
            .commit_usage(&batch(
                "unresolved",
                "root",
                source_commit(2, 12, "unresolved", "root", 'd', false),
            ))
            .unwrap();
        assert_eq!(
            fixture
                .ledger
                .load_usage_scan_state(&[2], crate::usage::USAGE_PARSER_VERSION)
                .unwrap()
                .plans[0]
                .action,
            UsagePlanAction::ResumeOwningLive
        );
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_files SET file_generation=2 WHERE source_file_id=2",
                    [],
                )
                .unwrap();
        }
        assert_eq!(
            fixture
                .ledger
                .load_usage_scan_state(&[2], crate::usage::USAGE_PARSER_VERSION)
                .unwrap()
                .plans[0]
                .action,
            UsagePlanAction::RebuildRequired
        );
    }

    #[test]
    fn thread_groups_isolate_failures_and_root_reconcile_is_atomic_without_build() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("other-root"), 12);
        fixture
            .ledger
            .commit_usage(&batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
            .unwrap();
        let mut stale = source_commit(2, 12, "other-root", "other-root", 'c', false);
        stale.expected_file_generation = 2;
        assert!(
            fixture
                .ledger
                .commit_usage(&batch("other-root", "other-root", stale))
                .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM usage_events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(connection);

        {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection.transaction().unwrap();
            transaction
                .execute(
                    "UPDATE threads SET root_session_id='other-root' WHERE thread_id='child'",
                    [],
                )
                .unwrap();
            reconcile_usage_metadata_change(
                &transaction,
                "child",
                Some("root"),
                Some("other-root"),
                &[],
            )
            .unwrap();
            transaction.commit().unwrap();
        }
        let connection = fixture.ledger.connection().unwrap();
        let roots: (String, String) = connection
            .query_row(
                "SELECT
                    (SELECT root_session_id FROM usage_events WHERE thread_id='child'),
                    (SELECT root_session_id FROM usage_source_states WHERE owning_thread_id='child')",
                [],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )
            .unwrap();
        assert_eq!(roots, ("other-root".to_owned(), "other-root".to_owned()));
        drop(connection);

        fixture.add_source(3, Some("child"), 13);
        fs::create_dir_all(fixture.root.join("other-codex")).unwrap();
        let _changed = Ledger::open(LedgerOptions::new(
            fixture.root.join("mu.sqlite3"),
            fixture.root.join("other-codex"),
        ))
        .unwrap();
        assert!(
            fixture
                .ledger
                .commit_usage(&batch(
                    "child",
                    "other-root",
                    source_commit(3, 13, "child", "other-root", 'e', false),
                ))
                .is_err()
        );
        let connection = fixture.ledger.connection().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT committed_offset FROM source_checkpoints
                     WHERE source_file_id=3 AND consumer_kind='usage'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn root_reconcile_with_build_replaces_only_affected_source_and_preserves_other_progress() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture.add_source(2, Some("other-root"), 12);
        fixture
            .ledger
            .commit_usage(&batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
            .unwrap();
        fixture
            .ledger
            .commit_usage(&batch(
                "other-root",
                "other-root",
                source_commit(2, 12, "other-root", "other-root", 'c', false),
            ))
            .unwrap();

        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1, 2], 20)
                .unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .record_progress(crate::usage::rebuild::SourceProgress {
                    source_file_id: 2,
                    expected_generation: 1,
                    start_offset: 0,
                    last_complete_offset: 100,
                    observed_raw_size: 100,
                    expected_guard_hash: None,
                    guard_hash: Some(vec![4; 32]),
                    tail: crate::usage::rebuild::TailProof::None,
                    updated_at_ms: 21,
                })
                .unwrap();
        }
        let before_other: (String, i64, String, i64, String) = {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .query_row(
                    "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                            c.committed_offset,b.raw_tail_status
                     FROM usage_build_sources b JOIN source_checkpoints c
                       ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                     WHERE b.build_epoch=2 AND b.source_file_id=2",
                    [],
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
                .unwrap()
        };

        {
            let mut connection = fixture.ledger.connection().unwrap();
            let transaction = connection.transaction().unwrap();
            transaction
                .execute(
                    "UPDATE threads SET parent_thread_id='other-root',root_session_id='other-root'
                     WHERE thread_id='child'",
                    [],
                )
                .unwrap();
            reconcile_usage_metadata_change(
                &transaction,
                "child",
                Some("root"),
                Some("other-root"),
                &[],
            )
            .unwrap();
            transaction.commit().unwrap();
        }

        let connection = fixture.ledger.connection().unwrap();
        let active_roots: (String, String) = connection
            .query_row(
                "SELECT
                    (SELECT root_session_id FROM usage_events WHERE ledger_epoch=1 AND thread_id='child'),
                    (SELECT root_session_id FROM usage_source_states WHERE ledger_epoch=1 AND owning_thread_id='child')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(active_roots, ("other-root".into(), "other-root".into()));
        let affected: (Option<String>, String, i64) = connection
            .query_row(
                "SELECT b.expected_root_session_id,c.processing_status,c.committed_offset
                 FROM usage_build_sources b JOIN source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            affected,
            (Some("other-root".into()), "rebuild_required".into(), 0)
        );
        let after_other: (String, i64, String, i64, String) = connection
            .query_row(
                "SELECT b.completion_status,b.required_through_offset,c.processing_status,
                        c.committed_offset,b.raw_tail_status
                 FROM usage_build_sources b JOIN source_checkpoints c
                   ON c.source_file_id=b.source_file_id AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=2",
                [],
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
            .unwrap();
        assert_eq!(after_other, before_other);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM usage_build_sources WHERE build_epoch=2",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn strict_resume_local_replay_and_build_planner_matrix() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture
            .ledger
            .commit_usage(&batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', false),
            ))
            .unwrap();

        let assert_action = |fixture: &Fixture, expected: UsagePlanAction| {
            let plan = fixture
                .ledger
                .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
                .unwrap()
                .plans
                .into_iter()
                .next()
                .unwrap();
            assert_eq!(plan.action, expected);
        };
        assert_action(&fixture, UsagePlanAction::ResumeOwningLive);

        // Every persisted proof used by a non-zero resume is strict. Corrupt
        // one dimension at a time and prove the planner will not reuse it.
        for (column, bad, good) in [
            ("device_id", "12", "11"),
            ("inode", "12", "11"),
            ("canonical_algorithm_version", "1", "5"),
            ("resolved_through_offset", "21", "20"),
        ] {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    &format!(
                        "UPDATE usage_source_states SET {column}={bad} WHERE ledger_epoch=1 AND source_file_id=1"
                    ),
                    [],
                )
                .unwrap();
            drop(connection);
            assert_action(&fixture, UsagePlanAction::RebuildRequired);
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    &format!(
                        "UPDATE usage_source_states SET {column}={good} WHERE ledger_epoch=1 AND source_file_id=1"
                    ),
                    [],
                )
                .unwrap();
        }

        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states SET root_session_id='other-root' WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::RebuildRequired);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states SET root_session_id='root',active_turn_key='missing-turn' WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        assert!(
            fixture
                .ledger
                .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
                .is_err()
        );
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states SET active_turn_key=NULL WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE source_checkpoints SET processing_status='error',last_error_code='USAGE_FAILED' WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::ResumeOwningLive);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_checkpoints SET guard_hash=?1 WHERE source_file_id=1 AND consumer_kind='usage'",
                    [vec![1_u8; 31]],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::RebuildRequired);

        // LocalReplay is allowed only under the same active identity/parser/
        // ownership/canonical proof. A physical identity change promotes the
        // source to a whole-ledger rebuild instead of replaying in place.
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_checkpoints SET processing_status='rebuild_required',committed_offset=20,guard_hash=?1,last_error_code=NULL WHERE source_file_id=1 AND consumer_kind='usage'",
                    [vec![9_u8; 32]],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::LocalReplay);
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_files SET inode=99 WHERE source_file_id=1",
                    [],
                )
                .unwrap();
        }
        assert_action(&fixture, UsagePlanAction::RebuildRequired);

        // A shadow build never applies LocalReplaySafe. It starts from zero,
        // can persist a bounded intermediate batch, and resumes BuildFrom at
        // the committed complete-line boundary.
        let build_fixture = Fixture::new();
        build_fixture.add_source(2, Some("child"), 22);
        {
            let mut connection = build_fixture.ledger.connection().unwrap();
            let snapshot = crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[2], 20)
                .unwrap();
            assert_eq!(snapshot.build_epoch, 2);
        }
        let first_plan = build_fixture
            .ledger
            .load_usage_scan_state(&[2], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans
            .remove(0);
        assert_eq!(first_plan.action, UsagePlanAction::BuildFrom);
        assert_eq!(first_plan.start_offset, 0);

        let mut build_commit = source_commit(2, 22, "child", "root", 'b', false);
        build_commit.expected_checkpoint.processing_status =
            CheckpointProcessingStatus::RebuildRequired;
        build_commit.fixed_observed_raw_size = 100;
        let build_batch = UsageCommitBatch {
            ledger_epoch: 2,
            usage_parser_version: crate::usage::USAGE_PARSER_VERSION,
            thread_id: "child".to_owned(),
            root_session_id: "root".to_owned(),
            sources: vec![build_commit],
        };
        build_fixture.ledger.commit_usage(&build_batch).unwrap();
        let continued = build_fixture
            .ledger
            .load_usage_scan_state(&[2], crate::usage::USAGE_PARSER_VERSION)
            .unwrap()
            .plans
            .remove(0);
        assert_eq!(continued.action, UsagePlanAction::BuildFrom);
        assert_eq!(continued.start_offset, 20);
    }

    #[test]
    fn persistent_partial_seed_carry_is_atomic_restarts_from_first_key_and_rejects_seed_conflict() {
        fn prepare() -> Fixture {
            let fixture = Fixture::new();
            fixture.add_source(1, Some("child"), 11);
            fixture
                .ledger
                .commit_usage(&batch(
                    "child",
                    "root",
                    source_commit(1, 11, "child", "root", 'a', true),
                ))
                .unwrap();
            {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET observed_size=20 WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE usage_source_states SET observed_raw_size=20,raw_tail_status='none',raw_tail_start_offset=NULL WHERE ledger_epoch=1 AND source_file_id=1",
                        [],
                    )
                    .unwrap();
            }
            {
                let mut connection = fixture.ledger.connection().unwrap();
                crate::usage::rebuild::RebuildLedger::new(&mut connection)
                    .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1], 30)
                    .unwrap();
            }
            let mut seed = source_commit(1, 11, "child", "root", 'a', true);
            seed.expected_checkpoint.processing_status =
                CheckpointProcessingStatus::RebuildRequired;
            seed.fixed_observed_raw_size = 20;
            seed.updated_state.observed_raw_size = 20;
            let seed_batch = UsageCommitBatch {
                ledger_epoch: 2,
                usage_parser_version: crate::usage::USAGE_PARSER_VERSION,
                thread_id: "child".to_owned(),
                root_session_id: "root".to_owned(),
                sources: vec![seed],
            };
            fixture.ledger.commit_usage(&seed_batch).unwrap();
            {
                let connection = fixture.ledger.connection().unwrap();
                connection
                    .execute(
                        "UPDATE source_files SET file_status='missing' WHERE source_file_id=1",
                        [],
                    )
                    .unwrap();
            }
            {
                let scan = fixture
                    .ledger
                    .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
                    .unwrap();
                assert_eq!(scan.plans[0].action, UsagePlanAction::BeginCarry);
            }
            fixture
        }

        // Failure while flipping the manifest phase must roll back the state
        // retirement and checkpoint reset from the same BeginCarry transaction.
        let fixture = prepare();
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_begin_carry BEFORE UPDATE OF carry_phase ON usage_build_sources
                     WHEN NEW.carry_phase='occurrences'
                     BEGIN SELECT RAISE(ABORT,'injected carry failure'); END;",
                )
                .unwrap();
        }
        assert!(fixture.ledger.begin_usage_carry(1, 40).is_err());
        {
            let connection = fixture.ledger.connection().unwrap();
            let proof: (String, i64, i64, String) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,
                            (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=2 AND source_file_id=1),
                            b.carry_phase
                     FROM source_checkpoints c JOIN usage_build_sources b ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(proof, ("ready".into(), 20, 1, "none".into()));
            connection
                .execute_batch("DROP TRIGGER fail_begin_carry;")
                .unwrap();
        }

        fixture.ledger.begin_usage_carry(1, 41).unwrap();
        {
            let connection = fixture.ledger.connection().unwrap();
            let proof: (String, i64, i64, String, i64, i64, i64) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,
                            (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=2 AND source_file_id=1),
                            b.carry_phase,
                            (SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=2 AND source_file_id=1),
                            (SELECT count(*) FROM turns WHERE ledger_epoch=2 AND source_file_id=1),
                            (SELECT count(*) FROM ingest_anomalies WHERE ledger_epoch=2 AND source_file_id=1)
                     FROM source_checkpoints c JOIN usage_build_sources b ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
                )
                .unwrap();
            assert_eq!(
                proof,
                (
                    "rebuild_required".into(),
                    0,
                    0,
                    "occurrences".into(),
                    1,
                    1,
                    1
                )
            );
        }
        for _ in 0..8 {
            let outcome = fixture.ledger.resume_usage_carry(1, 50).unwrap();
            if outcome == CarryStepOutcome::FinalizedMissing {
                break;
            }
        }
        {
            let connection = fixture.ledger.connection().unwrap();
            let final_proof: (String, i64, String, String, i64) = connection
                .query_row(
                    "SELECT c.processing_status,c.committed_offset,b.completion_status,b.carry_phase,
                            (SELECT count(*) FROM usage_source_states WHERE ledger_epoch=2 AND source_file_id=1)
                     FROM source_checkpoints c JOIN usage_build_sources b ON b.source_file_id=c.source_file_id
                     WHERE c.source_file_id=1 AND c.consumer_kind='usage' AND b.build_epoch=2",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
                )
                .unwrap();
            assert_eq!(
                final_proof,
                ("ready".into(), 20, "carried".into(), "none".into(), 1)
            );
        }

        // A partial seed is not trusted merely because its key already exists.
        // Resume enumerates active facts from the first key and hard-fails on
        // incompatible seed payload without advancing the durable cursor.
        let conflict = prepare();
        conflict.ledger.begin_usage_carry(1, 60).unwrap();
        {
            let connection = conflict.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE usage_events SET model='seed-conflict' WHERE ledger_epoch=2 AND event_id=?1",
                    ["a".repeat(64)],
                )
                .unwrap();
        }
        assert!(conflict.ledger.resume_usage_carry(1, 61).is_err());
        let connection = conflict.ledger.connection().unwrap();
        let unchanged: (String, Option<i64>, String, i64) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,c.processing_status,c.committed_offset
                 FROM usage_build_sources b JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                 WHERE b.build_epoch=2 AND b.source_file_id=1 AND c.consumer_kind='usage'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            unchanged,
            ("occurrences".into(), None, "rebuild_required".into(), 0)
        );

        // A partial seed may not smuggle a cross-source-provenance orphan
        // canonical event into the build epoch. BeginCarry rejects it before
        // retiring the seed state.
        let orphan = prepare();
        orphan.add_source(2, Some("child"), 22);
        {
            let connection = orphan.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,
                        source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms)
                     SELECT ledger_epoch,?2,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,
                        2,1,source_start_offset,source_end_offset,created_at_ms
                     FROM usage_events WHERE ledger_epoch=2 AND event_id=?1",
                    params!["a".repeat(64), "orphan"],
                )
                .unwrap();
        }
        assert!(orphan.ledger.begin_usage_carry(1, 62).is_err());
        let connection = orphan.ledger.connection().unwrap();
        let unchanged: (String, i64, String, i64) = connection
            .query_row(
                "SELECT c.processing_status,c.committed_offset,b.carry_phase,
                        (SELECT count(*) FROM usage_source_states
                         WHERE ledger_epoch=2 AND source_file_id=1)
                 FROM source_checkpoints c JOIN usage_build_sources b
                   ON b.source_file_id=c.source_file_id
                 WHERE c.source_file_id=1 AND c.consumer_kind='usage'
                   AND b.build_epoch=2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(unchanged, ("ready".into(), 20, "none".into(), 1));

        // The same protection applies if a cross-source-provenance orphan is
        // introduced after the carry cursor starts: finalization remains
        // blocked and atomic.
        let finalize_orphan = prepare();
        finalize_orphan.ledger.begin_usage_carry(1, 65).unwrap();
        finalize_orphan.add_source(2, Some("child"), 22);
        {
            let connection = finalize_orphan.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,
                        source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms)
                     SELECT ledger_epoch,?2,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,
                        2,1,source_start_offset,source_end_offset,created_at_ms
                     FROM usage_events WHERE ledger_epoch=2 AND event_id=?1",
                    params!["a".repeat(64), "late-orphan"],
                )
                .unwrap();
        }
        for step in 0..3_i64 {
            assert!(matches!(
                finalize_orphan.ledger.resume_usage_carry(1, 66 + step),
                Ok(CarryStepOutcome::Progress)
            ));
        }
        assert!(finalize_orphan.ledger.resume_usage_carry(1, 69).is_err());
        let connection = finalize_orphan.ledger.connection().unwrap();
        let blocked: (
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            i64,
            String,
            i64,
        ) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,b.carry_after_turn_key,
                        b.carry_after_anomaly_id,c.committed_offset,c.processing_status,
                        (SELECT count(*) FROM usage_events
                         WHERE ledger_epoch=2 AND event_id='late-orphan')
                 FROM usage_build_sources b
                 JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
                [],
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
            )
            .unwrap();
        assert_eq!(
            blocked,
            (
                "finalize".into(),
                None,
                None,
                None,
                0,
                "rebuild_required".into(),
                1,
            )
        );

        // A same-offset occurrence with a different event id is a durable
        // occurrence conflict, not a seed that can be silently overwritten.
        let occurrence_conflict = prepare();
        occurrence_conflict.ledger.begin_usage_carry(1, 63).unwrap();
        {
            let connection = occurrence_conflict.ledger.connection().unwrap();
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,
                        source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms)
                     SELECT ledger_epoch,?2,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,
                        output_tokens,reasoning_tokens,total_tokens,quality_status,
                        source_file_id,file_generation,source_start_offset,source_end_offset,created_at_ms
                     FROM usage_events WHERE ledger_epoch=2 AND event_id=?1",
                    params!["a".repeat(64), "wrong-event"],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE usage_event_occurrences SET event_id='wrong-event'
                     WHERE ledger_epoch=2 AND source_file_id=1 AND source_start_offset=0",
                    [],
                )
                .unwrap();
        }
        let error = occurrence_conflict
            .ledger
            .resume_usage_carry(1, 64)
            .unwrap_err();
        assert!(error.requires_usage_rebuild());
        let connection = occurrence_conflict.ledger.connection().unwrap();
        let unchanged: (String, Option<i64>, String, i64, String) = connection
            .query_row(
                "SELECT b.carry_phase,b.carry_after_start_offset,
                        c.processing_status,c.committed_offset,o.event_id
                 FROM usage_build_sources b
                 JOIN source_checkpoints c ON c.source_file_id=b.source_file_id
                    AND c.consumer_kind='usage'
                 JOIN usage_event_occurrences o ON o.ledger_epoch=2
                    AND o.source_file_id=1 AND o.source_start_offset=0
                 WHERE b.build_epoch=2 AND b.source_file_id=1",
                [],
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
            .unwrap();
        assert_eq!(
            unchanged,
            (
                "occurrences".into(),
                None,
                "rebuild_required".into(),
                0,
                "wrong-event".into()
            )
        );
    }

    #[test]
    fn active_unverified_tail_rejects_begin_usage_carry() {
        let fixture = Fixture::new();
        fixture.add_source(1, Some("child"), 11);
        fixture
            .ledger
            .commit_usage(&batch(
                "child",
                "root",
                source_commit(1, 11, "child", "root", 'a', true),
            ))
            .unwrap();
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_files SET observed_size=20
                     WHERE source_file_id=1",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE source_checkpoints SET committed_offset=20,guard_hash=?1,
                        processing_status='ready'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [vec![9_u8; 32]],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states SET resolved_through_offset=20,
                        observed_raw_size=20,raw_tail_status='none',raw_tail_start_offset=NULL
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }
        {
            let mut connection = fixture.ledger.connection().unwrap();
            crate::usage::rebuild::RebuildLedger::new(&mut connection)
                .begin_or_resume(crate::usage::USAGE_PARSER_VERSION, &[1], 30)
                .unwrap();
        }
        {
            let connection = fixture.ledger.connection().unwrap();
            connection
                .execute(
                    "UPDATE source_files SET file_status='missing' WHERE source_file_id=1;
                     UPDATE source_checkpoints SET committed_offset=0,guard_hash=NULL,
                        processing_status='rebuild_required'
                     WHERE source_file_id=1 AND consumer_kind='usage'",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE usage_source_states SET raw_tail_status='unverified',
                        raw_tail_start_offset=NULL
                     WHERE ledger_epoch=1 AND source_file_id=1",
                    [],
                )
                .unwrap();
        }

        let plan = fixture
            .ledger
            .load_usage_scan_state(&[1], crate::usage::USAGE_PARSER_VERSION)
            .unwrap();
        assert_eq!(plan.plans[0].action, UsagePlanAction::BlockedRelationship);
        assert!(fixture.ledger.begin_usage_carry(1, 40).is_err());
    }
}
