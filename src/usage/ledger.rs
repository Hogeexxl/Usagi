//! Deep Spec04 usage ledger facade.
//!
//! Scanner-facing code deals only in scan plans/chunks and the aggregate
//! reader API. SQLite row shapes and canonical persistence details stay behind
//! this module.

use std::collections::BTreeSet;

use rusqlite::TransactionBehavior;

use crate::{
    cost::{BundledPricingRepository, CostEstimator},
    domain::{CheckpointProcessingStatus, SourceUsageEpochState},
    storage::{self, Ledger},
};

use super::{
    aggregate::{
        AggregateError, AggregateReader, FilterOptions, ModelUsageRows,
        SessionDetail as AggregateSessionDetail, SessionPageRequest as AggregateSessionPageRequest,
        SessionSortField, SessionSortOrder, SessionUsageRow, SummaryQuery, TimeRange, UsageFilter,
        UsageSummary,
    },
    normalized::{NormalizedTokenUsage, USAGE_PARSER_VERSION},
    pipeline::{
        CheckpointExpectation, CheckpointStatus, PipelineDisposition, PipelineError, PlanAction,
        SourceContinuationState, SourceStateProof, TailStatus, UsagePipeline, UsagePipelinePlan,
        UsageSourceCommitDto,
    },
    processor::{
        Anomaly, AnomalyCode, ClosedTurn, CompensationBlocks, EventKind, GapKind, TurnEndStatus,
        TurnModelState, TurnReasoningEffortState, TurnState, UsageSourceState,
    },
    rebuild::{
        ActivationOutcome, ActiveQuarantineState, BuildSnapshot, RebuildError, RebuildLedger,
    },
};

type TurnCommon = (
    Option<storage::usage::UsageSnapshot>,
    Option<storage::usage::UsageSnapshot>,
    storage::usage::UsageSnapshot,
    storage::usage::UsageTurnModelState,
    storage::usage::UsageTurnReasoningEffortState,
    bool,
    storage::usage::UsageCompensationBlocks,
    &'static str,
);

#[derive(Debug)]
pub enum UsageLedgerError {
    Storage(storage::StorageError),
    Pipeline(PipelineError),
    Rebuild(RebuildError),
    Aggregate(AggregateError),
    Invalid(&'static str),
    StaleDataRevision,
}

impl From<storage::StorageError> for UsageLedgerError {
    fn from(value: storage::StorageError) -> Self {
        Self::Storage(value)
    }
}
impl From<PipelineError> for UsageLedgerError {
    fn from(value: PipelineError) -> Self {
        Self::Pipeline(value)
    }
}
impl From<RebuildError> for UsageLedgerError {
    fn from(value: RebuildError) -> Self {
        Self::Rebuild(value)
    }
}
impl From<AggregateError> for UsageLedgerError {
    fn from(value: AggregateError) -> Self {
        Self::Aggregate(value)
    }
}

impl UsageLedgerError {
    pub(crate) fn requires_rebuild(&self) -> bool {
        matches!(self, Self::Storage(error) if error.requires_usage_rebuild())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageSnapshot<T> {
    pub data_revision: i64,
    pub value: T,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionSnapshot {
    pub data_revision: i64,
    pub sort_index: Vec<crate::usage::aggregate::SessionSortIndexItem>,
    pub rows: Vec<SessionUsageRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRowsSnapshot {
    pub data_revision: i64,
    pub rows: Vec<SessionUsageRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionDetailSnapshot {
    pub data_revision: i64,
    pub value: AggregateSessionDetail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageSourceScanPlan {
    pub source_file_id: i64,
    pub action: PlanAction,
    pub start_offset: u64,
    pub observed_size: u64,
    pub owning_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub checkpoint: Option<CheckpointExpectation>,
    pub state: Option<SourceStateProof>,
    pub build: Option<UsageBuildScanProof>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageBuildScanProof {
    pub build_epoch: i64,
    pub expected_file_generation: i64,
    pub expected_device_id: i64,
    pub expected_inode: i64,
    pub active_committed_offset: u64,
    pub active_guard_hash: Option<Vec<u8>>,
    pub required_through_offset: u64,
    pub observed_raw_size: u64,
    pub raw_tail_status: TailStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageScanState {
    pub epoch: SourceUsageEpochState,
    pub plans: Vec<UsageSourceScanPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkList {
    pub(crate) epoch: SourceUsageEpochState,
    pub(crate) threads: Vec<UsageWorkThread>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageWorkThread {
    pub(crate) thread_id: String,
    pub(crate) source_file_ids: Vec<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageCommitOutcome {
    pub sources_committed: usize,
    pub events_inserted: usize,
    pub events_deduplicated: usize,
    pub data_revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarryStepOutcome {
    Progress,
    FinalizedMissing,
    FinalizedPresent,
}

pub struct UsageLedger<'a> {
    ledger: &'a Ledger,
}

impl<'a> UsageLedger<'a> {
    pub const fn new(ledger: &'a Ledger) -> Self {
        Self { ledger }
    }

    pub fn load_scan_state(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
    ) -> Result<UsageScanState, UsageLedgerError> {
        let raw = self
            .ledger
            .load_usage_scan_state(source_file_ids, parser_version)?;
        let plans = raw
            .plans
            .into_iter()
            .map(convert_plan)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(UsageScanState {
            epoch: raw.epoch,
            plans,
        })
    }

    pub(crate) fn load_work_list(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
    ) -> Result<UsageWorkList, UsageLedgerError> {
        let raw = self
            .ledger
            .load_usage_work_list(source_file_ids, parser_version)?;
        Ok(convert_work_list(raw))
    }

    pub(crate) fn load_scan_state_exact(
        &self,
        source_file_ids: &[i64],
        parser_version: i64,
        expected_epoch: SourceUsageEpochState,
    ) -> Result<UsageScanState, UsageLedgerError> {
        let raw = self.ledger.load_usage_scan_state_exact(
            source_file_ids,
            parser_version,
            expected_epoch,
        )?;
        let plans = raw
            .plans
            .into_iter()
            .map(convert_plan)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(UsageScanState {
            epoch: raw.epoch,
            plans,
        })
    }

    pub fn process_chunk<I>(
        &self,
        plan: UsagePipelinePlan,
        lines: I,
        tail: super::pipeline::FixedViewTail,
        next_guard_hash: Option<Vec<u8>>,
        metadata_needs_rebuild: bool,
        committed_at_ms: i64,
    ) -> Result<PipelineDisposition, UsageLedgerError>
    where
        I: IntoIterator,
        I::Item: Into<super::pipeline::ClassifiedUsageItem>,
    {
        Ok(UsagePipeline::process_chunk(
            plan,
            lines,
            tail,
            next_guard_hash,
            metadata_needs_rebuild,
            committed_at_ms,
        )?)
    }

    pub fn commit(
        &self,
        dto: UsageSourceCommitDto,
    ) -> Result<UsageCommitOutcome, UsageLedgerError> {
        self.commit_group(vec![dto])
    }

    /// Commit one bounded owning-Thread group atomically. Every source commit
    /// must describe the same working epoch/parser and the same confirmed
    /// owning/root relationship. Storage revalidates those facts and all
    /// source CAS preconditions inside one IMMEDIATE transaction.
    pub fn commit_group(
        &self,
        dtos: Vec<UsageSourceCommitDto>,
    ) -> Result<UsageCommitOutcome, UsageLedgerError> {
        let batch = commit_batch(dtos)?;
        let outcome = self.ledger.commit_usage(&batch)?;
        Ok(UsageCommitOutcome {
            sources_committed: outcome.sources_committed,
            events_inserted: outcome.events_inserted,
            events_deduplicated: outcome.events_deduplicated,
            data_revision: outcome.data_revision,
        })
    }

    pub fn begin_rebuild(
        &self,
        parser_version: i64,
        present_source_ids: impl IntoIterator<Item = i64>,
        now_ms: i64,
    ) -> Result<BuildSnapshot, UsageLedgerError> {
        let present = present_source_ids.into_iter().collect::<BTreeSet<_>>();
        let present = present.into_iter().collect::<Vec<_>>();
        let mut connection = self.ledger.connection()?;
        Ok(
            RebuildLedger::new(&mut connection).begin_or_resume(
                parser_version,
                &present,
                now_ms,
            )?,
        )
    }

    pub fn replace_build_sources(
        &self,
        parser_version: i64,
        present_source_ids: impl IntoIterator<Item = i64>,
        invalidated_source_ids: impl IntoIterator<Item = i64>,
        now_ms: i64,
    ) -> Result<(), UsageLedgerError> {
        let present = present_source_ids.into_iter().collect::<BTreeSet<_>>();
        let invalidated = invalidated_source_ids.into_iter().collect::<BTreeSet<_>>();
        let mut connection = self.ledger.connection()?;
        let mut source_tx = crate::source::SourceWriteTxn::begin_legacy_codex(
            "legacy-codex-usage:replace_build_sources",
            &mut connection,
            Some(self.ledger.revision_publisher()),
        )
        .map_err(storage::StorageError::sqlite)?;
        source_tx.apply_codex_replace_build_sources(
            parser_version,
            &present,
            &invalidated,
            now_ms,
        )?;
        source_tx
            .commit()
            .map_err(|error| storage::StorageError::invalid_state(error.to_string()))?;
        Ok(())
    }

    pub fn begin_carry(&self, source_file_id: i64, now_ms: i64) -> Result<(), UsageLedgerError> {
        self.ledger.begin_usage_carry(source_file_id, now_ms)?;
        Ok(())
    }

    pub fn resume_carry(
        &self,
        source_file_id: i64,
        now_ms: i64,
    ) -> Result<CarryStepOutcome, UsageLedgerError> {
        Ok(
            match self.ledger.resume_usage_carry(source_file_id, now_ms)? {
                storage::usage::CarryStepOutcome::Progress => CarryStepOutcome::Progress,
                storage::usage::CarryStepOutcome::FinalizedMissing => {
                    CarryStepOutcome::FinalizedMissing
                }
                storage::usage::CarryStepOutcome::FinalizedPresent => {
                    CarryStepOutcome::FinalizedPresent
                }
            },
        )
    }

    pub fn complete_only(&self, source_file_id: i64, now_ms: i64) -> Result<(), UsageLedgerError> {
        self.ledger
            .complete_usage_build_source(source_file_id, now_ms)?;
        Ok(())
    }

    pub fn quarantine_thread(
        &self,
        thread_id: &str,
        error_code: &str,
        now_ms: i64,
    ) -> Result<usize, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let root: Option<String> = connection
            .query_row(
                "SELECT root_session_id FROM threads WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .map_err(storage::StorageError::sqlite)?;
        let root = root.ok_or(UsageLedgerError::Invalid("thread has no root session"))?;
        Ok(RebuildLedger::new(&mut connection).quarantine_session(&root, error_code, now_ms)?)
    }

    pub fn active_quarantine_state(&self) -> Result<ActiveQuarantineState, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        Ok(RebuildLedger::new(&mut connection).active_quarantine_state()?)
    }

    pub fn activate_rebuild(
        &self,
        build_epoch: i64,
        complete_present_source_ids: &[i64],
    ) -> Result<ActivationOutcome, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let current: Option<i64> = connection
            .query_row(
                "SELECT build_epoch FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| row.get(0),
            )
            .map_err(storage::StorageError::sqlite)?;
        if current != Some(build_epoch) {
            return Err(UsageLedgerError::Invalid("usage build epoch changed"));
        }
        let outcome = RebuildLedger::new(&mut connection).activate(complete_present_source_ids)?;
        let status_revision: i64 = connection
            .query_row(
                "SELECT status_revision FROM app_meta WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(storage::StorageError::sqlite)?;
        self.ledger
            .publish_revisions(outcome.data_revision, status_revision);
        Ok(outcome)
    }

    pub fn cleanup_inactive(&self, max_rows: usize) -> Result<usize, UsageLedgerError> {
        Ok(self.ledger.cleanup_inactive_usage(max_rows)?)
    }

    pub fn summary(&self, query: SummaryQuery) -> Result<UsageSummary, UsageLedgerError> {
        let connection = self.ledger.connection()?;
        Ok(AggregateReader::new(&connection).summary(query)?)
    }

    pub fn sessions(
        &self,
        range: TimeRange,
        request: AggregateSessionPageRequest,
    ) -> Result<super::aggregate::SessionUsagePage, UsageLedgerError> {
        let connection = self.ledger.connection()?;
        Ok(AggregateReader::new(&connection).sessions(range, request)?)
    }

    pub fn models(&self, range: TimeRange) -> Result<ModelUsageRows, UsageLedgerError> {
        let connection = self.ledger.connection()?;
        Ok(AggregateReader::new(&connection).models(range)?)
    }

    /// Freeze data_revision in the same SQLite read
    /// transaction as the Spec 04 aggregate query.
    pub fn summary_snapshot(
        &self,
        query: SummaryQuery,
    ) -> Result<UsageSnapshot<UsageSummary>, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage::StorageError::sqlite)?;
        let data_revision = snapshot_meta(&transaction)?;
        let value = AggregateReader::new(&transaction).summary(query)?;
        transaction
            .commit()
            .map_err(storage::StorageError::sqlite)?;
        Ok(UsageSnapshot {
            data_revision,
            value,
        })
    }

    /// Freeze revision and compute the complete Session snapshot in one
    /// Deferred read transaction.  `seed_*` only selects the initial <=60
    /// complete rows; the lightweight index always covers the full scope.
    pub fn sessions_snapshot(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        seed_sort_field: SessionSortField,
        seed_sort_order: SessionSortOrder,
    ) -> Result<SessionSnapshot, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage::StorageError::sqlite)?;
        let data_revision = snapshot_meta(&transaction)?;
        let value = AggregateReader::new(&transaction).session_snapshot(
            range,
            &filter,
            seed_sort_field,
            seed_sort_order,
        )?;
        transaction
            .commit()
            .map_err(storage::StorageError::sqlite)?;
        Ok(SessionSnapshot {
            data_revision,
            sort_index: value.sort_index,
            rows: value.rows,
        })
    }

    pub fn session_rows_snapshot(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        expected_data_revision: Option<i64>,
        root_session_ids: Vec<String>,
    ) -> Result<SessionRowsSnapshot, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage::StorageError::sqlite)?;
        let data_revision = snapshot_meta(&transaction)?;
        if expected_data_revision.is_some_and(|expected| expected != data_revision) {
            return Err(UsageLedgerError::StaleDataRevision);
        }
        let rows =
            AggregateReader::new(&transaction).session_rows(range, &filter, &root_session_ids)?;
        transaction
            .commit()
            .map_err(storage::StorageError::sqlite)?;
        Ok(SessionRowsSnapshot {
            data_revision,
            rows,
        })
    }

    pub fn session_detail_snapshot(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        expected_data_revision: Option<i64>,
        root_session_id: String,
    ) -> Result<SessionDetailSnapshot, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage::StorageError::sqlite)?;
        let data_revision = snapshot_meta(&transaction)?;
        if expected_data_revision.is_some_and(|expected| expected != data_revision) {
            return Err(UsageLedgerError::StaleDataRevision);
        }
        let value =
            AggregateReader::new(&transaction).session_detail(range, &filter, &root_session_id)?;
        transaction
            .commit()
            .map_err(storage::StorageError::sqlite)?;
        Ok(SessionDetailSnapshot {
            data_revision,
            value,
        })
    }

    pub fn models_snapshot(
        &self,
        range: TimeRange,
    ) -> Result<UsageSnapshot<ModelUsageRows>, UsageLedgerError> {
        self.models_snapshot_filtered(range, &UsageFilter::default())
    }

    pub fn models_snapshot_filtered(
        &self,
        range: TimeRange,
        filter: &UsageFilter,
    ) -> Result<UsageSnapshot<ModelUsageRows>, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage::StorageError::sqlite)?;
        let data_revision = snapshot_meta(&transaction)?;
        let value = AggregateReader::new(&transaction).models_filtered(range, filter)?;
        transaction
            .commit()
            .map_err(storage::StorageError::sqlite)?;
        Ok(UsageSnapshot {
            data_revision,
            value,
        })
    }

    pub fn filter_options_snapshot(
        &self,
    ) -> Result<UsageSnapshot<FilterOptions>, UsageLedgerError> {
        let mut connection = self.ledger.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage::StorageError::sqlite)?;
        let data_revision = snapshot_meta(&transaction)?;
        let value = AggregateReader::new(&transaction).filter_options()?;
        transaction
            .commit()
            .map_err(storage::StorageError::sqlite)?;
        Ok(UsageSnapshot {
            data_revision,
            value,
        })
    }
}

fn snapshot_meta(connection: &rusqlite::Connection) -> Result<i64, UsageLedgerError> {
    let value = connection
        .query_row(
            "SELECT data_revision FROM app_meta WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage::StorageError::sqlite)?;
    if value < 0 {
        return Err(UsageLedgerError::Invalid("invalid usage snapshot metadata"));
    }
    Ok(value)
}

fn convert_work_list(raw: storage::usage::UsageWorkListState) -> UsageWorkList {
    let mut rows = raw.rows;
    rows.sort_by(|left, right| {
        left.owning_thread_id
            .cmp(&right.owning_thread_id)
            .then_with(|| left.source_file_id.cmp(&right.source_file_id))
    });

    let mut threads: Vec<UsageWorkThread> = Vec::new();
    for row in rows {
        match threads.last_mut() {
            Some(thread) if thread.thread_id == row.owning_thread_id => {
                if thread.source_file_ids.last().copied() != Some(row.source_file_id) {
                    thread.source_file_ids.push(row.source_file_id);
                }
            }
            _ => threads.push(UsageWorkThread {
                thread_id: row.owning_thread_id,
                source_file_ids: vec![row.source_file_id],
            }),
        }
    }

    UsageWorkList {
        epoch: raw.epoch,
        threads,
    }
}

fn convert_plan(
    plan: storage::usage::UsageSourcePlan,
) -> Result<UsageSourceScanPlan, UsageLedgerError> {
    let state = plan
        .state
        .as_ref()
        .map(|state| source_state_from_storage(state, plan.open_turn.clone()))
        .transpose()?;
    Ok(UsageSourceScanPlan {
        source_file_id: plan.source_file_id,
        action: match plan.action {
            storage::usage::UsagePlanAction::ReadFrom => PlanAction::ReadFrom,
            storage::usage::UsagePlanAction::BuildFrom => PlanAction::BuildFrom,
            storage::usage::UsagePlanAction::LocalReplay => PlanAction::LocalReplay,
            storage::usage::UsagePlanAction::ResumeOwningLive => PlanAction::ResumeOwningLive,
            storage::usage::UsagePlanAction::VerifyRawTail => PlanAction::VerifyRawTail,
            storage::usage::UsagePlanAction::CompleteOnly => PlanAction::CompleteOnly,
            storage::usage::UsagePlanAction::BeginCarry => PlanAction::BeginCarry,
            storage::usage::UsagePlanAction::ResumeCarry => PlanAction::ResumeCarry,
            storage::usage::UsagePlanAction::Skip => PlanAction::Skip,
            storage::usage::UsagePlanAction::BlockedRelationship => PlanAction::BlockedRelationship,
            storage::usage::UsagePlanAction::RebuildRequired => PlanAction::RebuildRequired,
        },
        start_offset: i64_to_u64(plan.start_offset)?,
        observed_size: i64_to_u64(plan.observed_size)?,
        owning_thread_id: plan.owning_thread_id,
        root_session_id: plan.root_session_id,
        checkpoint: plan.checkpoint.map(checkpoint_from_storage).transpose()?,
        state,
        build: match plan.build {
            Some(build) => Some(UsageBuildScanProof {
                build_epoch: build.build_epoch,
                expected_file_generation: build.expected_file_generation,
                expected_device_id: build.expected_device_id,
                expected_inode: build.expected_inode,
                active_committed_offset: i64_to_u64(build.active_committed_offset)?,
                active_guard_hash: build.active_guard_hash,
                required_through_offset: i64_to_u64(build.required_through_offset)?,
                observed_raw_size: i64_to_u64(build.observed_raw_size)?,
                raw_tail_status: tail_from_storage(build.raw_tail_status),
            }),
            None => None,
        },
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "preserve the established crate planning seam"
)]
pub(crate) fn pipeline_plan(
    scan: &UsageScanState,
    source: &UsageSourceScanPlan,
    file_generation: i64,
    device_id: i64,
    inode: i64,
    fixed_observed_size: u64,
    read_start_offset: u64,
    replayed_prefix_bytes_before_chunk: u64,
    replayed_prefix_lines_before_chunk: u64,
) -> Result<UsagePipelinePlan, UsageLedgerError> {
    let checkpoint = match source.checkpoint.clone() {
        Some(value) => value,
        None => CheckpointExpectation {
            parser_version: scan.epoch.working_parser_version(),
            committed_offset: 0,
            guard_hash: None,
            status: CheckpointStatus::Ready,
        },
    };
    Ok(UsagePipelinePlan {
        ledger_epoch: scan.epoch.working_epoch(),
        parser_version: scan.epoch.working_parser_version(),
        source_file_id: source.source_file_id,
        file_generation,
        device_id,
        inode,
        action: source.action,
        start_offset: source.start_offset,
        read_start_offset,
        fixed_observed_size,
        owning_thread_id: source.owning_thread_id.clone(),
        root_session_id: source.root_session_id.clone(),
        checkpoint,
        state: source.state.clone(),
        allow_replay_tail: false,
        replayed_prefix_bytes_before_chunk,
        replayed_prefix_lines_before_chunk,
    })
}

fn checkpoint_from_storage(
    value: storage::usage::UsageCheckpointExpectation,
) -> Result<CheckpointExpectation, UsageLedgerError> {
    Ok(CheckpointExpectation {
        parser_version: value.parser_version,
        committed_offset: i64_to_u64(value.committed_offset)?,
        guard_hash: value.guard_hash,
        status: match value.processing_status {
            CheckpointProcessingStatus::Pending => CheckpointStatus::Pending,
            CheckpointProcessingStatus::Ready => CheckpointStatus::Ready,
            CheckpointProcessingStatus::Error => CheckpointStatus::Error,
            CheckpointProcessingStatus::RebuildRequired => CheckpointStatus::RebuildRequired,
        },
    })
}

fn source_state_from_storage(
    value: &storage::usage::UsageSourceStateWrite,
    open_turn: Option<TurnState>,
) -> Result<SourceStateProof, UsageLedgerError> {
    Ok(SourceStateProof {
        file_generation: value.file_generation,
        device_id: value.device_id,
        inode: value.inode,
        parser_version: value.usage_parser_version,
        canonical_algorithm_version: value.canonical_algorithm_version,
        resolved_through_offset: i64_to_u64(value.resolved_through_offset)?,
        observed_raw_size: i64_to_u64(value.observed_raw_size)?,
        raw_tail_status: tail_from_storage(value.raw_tail_status),
        raw_tail_start_offset: value.raw_tail_start_offset.map(i64_to_u64).transpose()?,
        owning_thread_id: value.owning_thread_id.clone(),
        root_session_id: value.root_session_id.clone(),
        continuation_state: match value.continuation_state {
            storage::usage::UsageContinuationState::ReplayedAncestor => {
                SourceContinuationState::ReplayedAncestor
            }
            storage::usage::UsageContinuationState::OwningLive => {
                SourceContinuationState::OwningLive
            }
        },
        processor_state: UsageSourceState {
            chain_state: match value.chain_state {
                storage::usage::UsageChainState::Continuous => {
                    super::processor::ChainState::Continuous
                }
                storage::usage::UsageChainState::Interrupted(reason) => {
                    super::processor::ChainState::Interrupted(match reason {
                        storage::usage::UsageGapReason::Malformed => GapKind::Malformed,
                        storage::usage::UsageGapReason::Oversized => GapKind::Oversized,
                        storage::usage::UsageGapReason::TotalInvalid => GapKind::RequiredInvalid,
                        storage::usage::UsageGapReason::OwnershipGap => GapKind::Ownership,
                        storage::usage::UsageGapReason::ParserGap => GapKind::Parser,
                    })
                }
            },
            previous_total: value
                .previous_total
                .as_ref()
                .map(|snapshot| snapshot.vector.clone()),
            previous_total_offset: value.previous_total_offset.map(i64_to_u64).transpose()?,
            active_model: value.active_model.clone(),
            active_reasoning_effort: value.active_reasoning_effort.clone(),
            open_turn,
        },
        active_model_offset: value.active_model_offset.map(i64_to_u64).transpose()?,
        active_reasoning_effort_offset: value
            .active_reasoning_effort_offset
            .map(i64_to_u64)
            .transpose()?,
        updated_at_ms: value.updated_at_ms,
    })
}

fn source_state_to_storage(
    value: &SourceStateProof,
) -> Result<storage::usage::UsageSourceStateWrite, UsageLedgerError> {
    Ok(storage::usage::UsageSourceStateWrite {
        file_generation: value.file_generation,
        device_id: value.device_id,
        inode: value.inode,
        usage_parser_version: value.parser_version,
        canonical_algorithm_version: value.canonical_algorithm_version,
        resolved_through_offset: u64_to_i64(value.resolved_through_offset)?,
        observed_raw_size: u64_to_i64(value.observed_raw_size)?,
        raw_tail_status: tail_to_storage(value.raw_tail_status),
        raw_tail_start_offset: value.raw_tail_start_offset.map(u64_to_i64).transpose()?,
        owning_thread_id: value.owning_thread_id.clone(),
        root_session_id: value.root_session_id.clone(),
        continuation_state: match value.continuation_state {
            SourceContinuationState::ReplayedAncestor => {
                storage::usage::UsageContinuationState::ReplayedAncestor
            }
            SourceContinuationState::OwningLive => {
                storage::usage::UsageContinuationState::OwningLive
            }
        },
        previous_total: value.processor_state.previous_total.as_ref().map(snapshot),
        previous_total_offset: value
            .processor_state
            .previous_total_offset
            .map(u64_to_i64)
            .transpose()?,
        chain_state: match value.processor_state.chain_state {
            super::processor::ChainState::Continuous => storage::usage::UsageChainState::Continuous,
            super::processor::ChainState::Interrupted(reason) => {
                storage::usage::UsageChainState::Interrupted(match reason {
                    GapKind::Malformed => storage::usage::UsageGapReason::Malformed,
                    GapKind::Oversized => storage::usage::UsageGapReason::Oversized,
                    GapKind::RequiredInvalid => storage::usage::UsageGapReason::TotalInvalid,
                    GapKind::Ownership => storage::usage::UsageGapReason::OwnershipGap,
                    GapKind::Parser => storage::usage::UsageGapReason::ParserGap,
                })
            }
        },
        active_turn_key: value
            .processor_state
            .open_turn
            .as_ref()
            .map(|turn| turn.turn_key.clone()),
        active_model: value.processor_state.active_model.clone(),
        active_model_offset: value.active_model_offset.map(u64_to_i64).transpose()?,
        active_reasoning_effort: value.processor_state.active_reasoning_effort.clone(),
        active_reasoning_effort_offset: value
            .active_reasoning_effort_offset
            .map(u64_to_i64)
            .transpose()?,
        updated_at_ms: value.updated_at_ms,
    })
}

fn commit_batch(
    dtos: Vec<UsageSourceCommitDto>,
) -> Result<storage::usage::UsageCommitBatch, UsageLedgerError> {
    let mut iter = dtos.into_iter();
    let first = iter
        .next()
        .ok_or(UsageLedgerError::Invalid("empty usage Thread group"))?;
    let (ledger_epoch, usage_parser_version, thread_id, root_session_id, first_source) =
        source_commit(first)?;
    let mut sources = vec![first_source];
    for dto in iter {
        let (epoch, parser, thread, root, source) = source_commit(dto)?;
        if epoch != ledger_epoch
            || parser != usage_parser_version
            || thread != thread_id
            || root != root_session_id
        {
            return Err(UsageLedgerError::Invalid(
                "usage Thread group facts do not match",
            ));
        }
        sources.push(source);
    }
    Ok(storage::usage::UsageCommitBatch {
        ledger_epoch,
        usage_parser_version,
        thread_id,
        root_session_id,
        sources,
    })
}

fn source_commit(
    dto: UsageSourceCommitDto,
) -> Result<(i64, i64, String, String, storage::usage::UsageSourceCommit), UsageLedgerError> {
    if dto.parser_version != USAGE_PARSER_VERSION {
        return Err(UsageLedgerError::Invalid("unexpected usage parser version"));
    }
    let expected_state = dto
        .expected_state
        .as_ref()
        .map(source_state_to_storage)
        .transpose()?;
    let updated_state = source_state_to_storage(&dto.updated_state)?;
    let mut turns = dto
        .closed_turns
        .iter()
        .map(|turn| closed_turn_write(turn, dto.last_complete_offset, dto.committed_at_ms))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(turn) = dto.open_turn.as_ref() {
        turns.push(open_turn_write(
            turn,
            dto.last_complete_offset,
            dto.committed_at_ms,
        )?);
    }
    let pricing_repository = BundledPricingRepository::new();
    let cost_estimator = CostEstimator::new();
    let events = dto
        .events
        .iter()
        .map(|event| {
            let kind = match event.kind {
                EventKind::Normal => storage::usage::UsageEventKind::Normal,
                EventKind::Recovered => storage::usage::UsageEventKind::Recovered,
                EventKind::TurnCompensation => storage::usage::UsageEventKind::TurnCompensation,
            };
            let estimated_cost_nanos_usd = storage::cost::estimate_event_cost(
                &pricing_repository,
                &cost_estimator,
                &event.model,
                event.occurred_at_ms,
                storage::cost::granularity_for_event_kind(kind),
                &event.usage,
            )?;
            Ok(storage::usage::UsageEventWrite {
                event_id: event.event_id.clone(),
                kind,
                occurred_at_ms: event.occurred_at_ms,
                thread_id: event.thread_id.clone(),
                root_session_id: event.root_session_id.clone(),
                turn_key: event.turn_key.clone(),
                model: event.model.clone(),
                reasoning_effort: event.reasoning_effort.clone(),
                estimated_cost_nanos_usd,
                usage: event.usage.clone(),
            })
        })
        .collect::<Result<Vec<_>, UsageLedgerError>>()?;
    let occurrences = dto
        .occurrences
        .iter()
        .map(|occurrence| {
            Ok(storage::usage::UsageOccurrenceWrite {
                source_file_id: occurrence.source_file_id,
                file_generation: occurrence.file_generation,
                source_start_offset: u64_to_i64(occurrence.source_start_offset)?,
                source_end_offset: u64_to_i64(occurrence.source_end_offset)?,
                event_id: occurrence.event_id.clone(),
            })
        })
        .collect::<Result<Vec<_>, UsageLedgerError>>()?;
    let skill_events = dto
        .skill_events
        .iter()
        .map(|event| {
            Ok(storage::usage::SkillUsageEventWrite {
                occurred_at_ms: event.occurred_at_ms,
                thread_id: event.thread_id.clone(),
                root_session_id: event.root_session_id.clone(),
                model: event.model.clone(),
                skill_name: event.skill_name.clone(),
                source_file_id: event.source_file_id,
                file_generation: event.file_generation,
                source_start_offset: u64_to_i64(event.source_start_offset)?,
                source_end_offset: u64_to_i64(event.source_end_offset)?,
            })
        })
        .collect::<Result<Vec<_>, UsageLedgerError>>()?;
    let anomalies = dto
        .anomalies
        .iter()
        .map(|anomaly| anomaly_write(anomaly, &dto))
        .collect::<Result<Vec<_>, _>>()?;
    let source = storage::usage::UsageSourceCommit {
        source_file_id: dto.source_file_id,
        expected_file_generation: dto.expected_file_generation,
        expected_previous_thread_id: dto.expected_previous_thread_id.clone(),
        expected_checkpoint: storage::usage::UsageCheckpointExpectation {
            parser_version: dto.expected_checkpoint.parser_version,
            committed_offset: u64_to_i64(dto.expected_checkpoint.committed_offset)?,
            guard_hash: dto.expected_checkpoint.guard_hash.clone(),
            processing_status: match dto.expected_checkpoint.status {
                CheckpointStatus::Pending => CheckpointProcessingStatus::Pending,
                CheckpointStatus::Ready => CheckpointProcessingStatus::Ready,
                CheckpointStatus::Error => CheckpointProcessingStatus::Error,
                CheckpointStatus::RebuildRequired => CheckpointProcessingStatus::RebuildRequired,
            },
        },
        expected_checkpoint_missing: dto.expected_checkpoint_missing,
        expected_state,
        local_replay: dto.local_replay,
        batch_start_offset: u64_to_i64(dto.batch_start_offset)?,
        fixed_observed_raw_size: u64_to_i64(dto.fixed_observed_raw_size)?,
        last_complete_offset: u64_to_i64(dto.last_complete_offset)?,
        source_bytes_consumed: u64_to_i64(dto.source_bytes_consumed)?,
        complete_line_count: u64_to_i64(dto.complete_line_count)?,
        candidate_count: u64_to_i64(dto.candidate_count)?,
        replayed_prefix_bytes: u64_to_i64(dto.replayed_prefix_bytes)?,
        replayed_prefix_lines: u64_to_i64(dto.replayed_prefix_lines)?,
        fixed_view_exhausted: dto.fixed_view_exhausted,
        tail_status: tail_to_storage(dto.tail_status),
        tail_start_offset: dto.tail_start_offset.map(u64_to_i64).transpose()?,
        events,
        occurrences,
        skill_events,
        turns,
        anomalies,
        updated_state,
        next_guard_hash: dto.next_guard_hash.clone(),
        committed_at_ms: dto.committed_at_ms,
    };
    Ok((
        dto.ledger_epoch,
        dto.parser_version,
        dto.owning_thread_id,
        dto.root_session_id,
        source,
    ))
}

fn snapshot(vector: &NormalizedTokenUsage) -> storage::usage::UsageSnapshot {
    storage::usage::UsageSnapshot {
        vector: vector.clone(),
        fingerprint: vector.fingerprint().to_vec(),
    }
}

fn turn_common(
    turn: &TurnState,
    state_through_offset: u64,
    updated_at_ms: i64,
) -> Result<TurnCommon, UsageLedgerError> {
    let model_state = match &turn.model_state {
        TurnModelState::None => storage::usage::UsageTurnModelState::None,
        TurnModelState::Single(model) => storage::usage::UsageTurnModelState::Single(model.clone()),
        TurnModelState::Mixed => storage::usage::UsageTurnModelState::Mixed,
    };
    let blocks = blocks(turn.blocks);
    let reasoning_effort_state = match &turn.reasoning_effort_state {
        TurnReasoningEffortState::None => storage::usage::UsageTurnReasoningEffortState::None,
        TurnReasoningEffortState::Single(effort) => {
            storage::usage::UsageTurnReasoningEffortState::Single(effort.clone())
        }
        TurnReasoningEffortState::Mixed => storage::usage::UsageTurnReasoningEffortState::Mixed,
    };
    let quality = if blocks == storage::usage::UsageCompensationBlocks::default()
        && !turn.unresolved_model_seen
    {
        "complete"
    } else {
        "partial"
    };
    let _ = state_through_offset;
    let _ = updated_at_ms;
    Ok((
        turn.start_total.as_ref().map(snapshot),
        turn.last_total.as_ref().map(snapshot),
        snapshot(&turn.accounted),
        model_state,
        reasoning_effort_state,
        turn.unresolved_reasoning_effort_seen,
        blocks,
        quality,
    ))
}

fn open_turn_write(
    turn: &TurnState,
    state_through_offset: u64,
    updated_at_ms: i64,
) -> Result<storage::usage::UsageTurnWrite, UsageLedgerError> {
    let (
        start_total,
        last_total,
        accounted,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        blocks,
        quality_status,
    ) = turn_common(turn, state_through_offset, updated_at_ms)?;
    Ok(storage::usage::UsageTurnWrite {
        turn_key: turn.turn_key.clone(),
        raw_turn_id: turn.raw_turn_id.clone(),
        started_at_ms: turn.started_at_ms,
        ended_at_ms: None,
        start_offset: u64_to_i64(turn.start_offset)?,
        end_offset: None,
        status: storage::usage::UsageTurnStatus::Open,
        start_total,
        last_total,
        accounted,
        accounted_candidate_count: u64_to_i64(turn.accounted_candidate_count)?,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        unresolved_model_seen: turn.unresolved_model_seen,
        blocks,
        quality_status,
        state_through_offset: u64_to_i64(state_through_offset)?,
        updated_at_ms,
    })
}

fn closed_turn_write(
    turn: &ClosedTurn,
    state_through_offset: u64,
    updated_at_ms: i64,
) -> Result<storage::usage::UsageTurnWrite, UsageLedgerError> {
    let (
        start_total,
        last_total,
        accounted,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        blocks,
        quality_status,
    ) = turn_common(&turn.turn, state_through_offset, updated_at_ms)?;
    Ok(storage::usage::UsageTurnWrite {
        turn_key: turn.turn.turn_key.clone(),
        raw_turn_id: turn.turn.raw_turn_id.clone(),
        started_at_ms: turn.turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms,
        start_offset: u64_to_i64(turn.turn.start_offset)?,
        end_offset: Some(u64_to_i64(turn.end_offset)?),
        status: match turn.status {
            TurnEndStatus::Completed => storage::usage::UsageTurnStatus::Completed,
            TurnEndStatus::Aborted => storage::usage::UsageTurnStatus::Aborted,
            TurnEndStatus::Failed => storage::usage::UsageTurnStatus::Failed,
        },
        start_total,
        last_total,
        accounted,
        accounted_candidate_count: u64_to_i64(turn.turn.accounted_candidate_count)?,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        unresolved_model_seen: turn.turn.unresolved_model_seen,
        blocks,
        quality_status,
        state_through_offset: u64_to_i64(state_through_offset)?,
        updated_at_ms,
    })
}

fn blocks(value: CompensationBlocks) -> storage::usage::UsageCompensationBlocks {
    storage::usage::UsageCompensationBlocks {
        start_missing: value.start_missing,
        time_missing: value.time_missing,
        reset: value.reset,
        ownership_gap: value.ownership_gap,
        parser_gap: value.parser_gap,
        required_invalid: value.required_invalid,
        model_unresolved: value.model_unresolved,
    }
}

fn anomaly_write(
    anomaly: &Anomaly,
    dto: &UsageSourceCommitDto,
) -> Result<storage::usage::UsageAnomalyWrite, UsageLedgerError> {
    let kind = match anomaly.code {
        AnomalyCode::UsageTimeMissing => storage::usage::UsageAnomalyKind::UsageTimeMissing,
        AnomalyCode::RequiredTotalInvalid => storage::usage::UsageAnomalyKind::RequiredTotalInvalid,
        AnomalyCode::LastUsageInvalid => storage::usage::UsageAnomalyKind::LastUsageInvalid,
        AnomalyCode::TotalChainReset => storage::usage::UsageAnomalyKind::TotalChainReset,
        AnomalyCode::CacheWriteChainDecrease => {
            storage::usage::UsageAnomalyKind::CacheWriteChainDecrease
        }
        AnomalyCode::TurnAccountedExceedsTotal => {
            storage::usage::UsageAnomalyKind::TurnAccountedExceedsTotal
        }
        AnomalyCode::TurnCacheWriteDeltaNegative => {
            storage::usage::UsageAnomalyKind::TurnCacheWriteDeltaNegative
        }
        AnomalyCode::TurnIdMismatch => storage::usage::UsageAnomalyKind::TurnIdMismatch,
        AnomalyCode::TurnReplaced => storage::usage::UsageAnomalyKind::TurnReplaced,
        AnomalyCode::ArithmeticOverflow => storage::usage::UsageAnomalyKind::ArithmeticOverflow,
    };

    // The anomaly ID follows the same unambiguous, versioned binary-encoding
    // rule as usage event IDs. Detection time and serialization details are
    // deliberately excluded so retries and archive copies converge.
    let facts_fingerprint = anomaly_facts_fingerprint(anomaly);
    let mut encoder = AnomalyEncoder::new(b"usage-anomaly-v1");
    encoder.byte(anomaly_code_number(anomaly.code));
    encoder.i64(dto.source_file_id);
    encoder.i64(dto.expected_file_generation);
    encoder.optional_u64(anomaly.source_start_offset);
    encoder.text(&dto.owning_thread_id);
    encoder.optional_text(anomaly.turn_key.as_deref());
    encoder.bytes(&facts_fingerprint);
    let anomaly_id = encoder.finish();

    Ok(storage::usage::UsageAnomalyWrite {
        anomaly_id,
        detected_at_ms: dto.committed_at_ms,
        occurred_at_ms: None,
        kind,
        severity_error: matches!(
            anomaly.code,
            AnomalyCode::RequiredTotalInvalid | AnomalyCode::ArithmeticOverflow
        ),
        source_start_offset: anomaly.source_start_offset.map(u64_to_i64).transpose()?,
    })
}

fn anomaly_facts_fingerprint(anomaly: &Anomaly) -> [u8; 32] {
    // Source offset and Turn key are already top-level anomaly-ID fields.
    // The facts fingerprint is deliberately restricted to the whitelisted
    // anomaly facts defined by Spec04; the current processor exposes only
    // the stable anomaly/error code.
    let mut encoder = AnomalyEncoder::new(b"usage-anomaly-facts-v1");
    encoder.byte(anomaly_code_number(anomaly.code));
    encoder.digest()
}

const fn anomaly_code_number(code: AnomalyCode) -> u8 {
    match code {
        AnomalyCode::UsageTimeMissing => 0,
        AnomalyCode::RequiredTotalInvalid => 1,
        AnomalyCode::LastUsageInvalid => 2,
        AnomalyCode::TotalChainReset => 3,
        AnomalyCode::CacheWriteChainDecrease => 4,
        AnomalyCode::TurnAccountedExceedsTotal => 6,
        AnomalyCode::TurnCacheWriteDeltaNegative => 7,
        AnomalyCode::TurnIdMismatch => 8,
        AnomalyCode::TurnReplaced => 9,
        AnomalyCode::ArithmeticOverflow => 10,
    }
}

struct AnomalyEncoder(blake3::Hasher);

impl AnomalyEncoder {
    fn new(tag: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(tag.len() as u64).to_be_bytes());
        hasher.update(tag);
        Self(hasher)
    }

    fn byte(&mut self, value: u8) {
        self.0.update(&[value]);
    }

    fn i64(&mut self, value: i64) {
        self.0.update(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_be_bytes());
    }

    fn text(&mut self, value: &str) {
        self.u64(value.len() as u64);
        self.0.update(value.as_bytes());
    }

    fn optional_text(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.text(value);
            }
            None => self.byte(0),
        }
    }

    fn optional_u64(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.u64(value);
            }
            None => self.byte(0),
        }
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.update(value);
    }

    fn digest(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }

    fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

fn i64_to_u64(value: i64) -> Result<u64, UsageLedgerError> {
    u64::try_from(value).map_err(|_| UsageLedgerError::Invalid("negative SQLite offset"))
}

const fn tail_from_storage(value: storage::usage::UsageTailStatus) -> TailStatus {
    match value {
        storage::usage::UsageTailStatus::Unverified => TailStatus::Unverified,
        storage::usage::UsageTailStatus::None => TailStatus::None,
        storage::usage::UsageTailStatus::HalfLine => TailStatus::HalfLine,
    }
}

const fn tail_to_storage(value: TailStatus) -> storage::usage::UsageTailStatus {
    match value {
        TailStatus::Unverified => storage::usage::UsageTailStatus::Unverified,
        TailStatus::None => storage::usage::UsageTailStatus::None,
        TailStatus::HalfLine => storage::usage::UsageTailStatus::HalfLine,
    }
}

fn u64_to_i64(value: u64) -> Result<i64, UsageLedgerError> {
    i64::try_from(value).map_err(|_| UsageLedgerError::Invalid("offset exceeds SQLite INTEGER"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_dto(model: &str, kind: EventKind) -> UsageSourceCommitDto {
        let usage = NormalizedTokenUsage::new(1_000, 200, Some(100), 50, 20, 1_050).unwrap();
        let event_id = "e".repeat(64);
        let state = SourceStateProof {
            file_generation: 1,
            device_id: 1,
            inode: 1,
            parser_version: USAGE_PARSER_VERSION,
            canonical_algorithm_version: crate::usage::USAGE_CANONICAL_ALGORITHM_VERSION,
            resolved_through_offset: 1,
            observed_raw_size: 1,
            raw_tail_status: TailStatus::Unverified,
            raw_tail_start_offset: None,
            owning_thread_id: "thread".to_owned(),
            root_session_id: "root".to_owned(),
            continuation_state: SourceContinuationState::OwningLive,
            processor_state: UsageSourceState::default(),
            active_model_offset: None,
            active_reasoning_effort_offset: None,
            updated_at_ms: 0,
        };
        UsageSourceCommitDto {
            ledger_epoch: 1,
            parser_version: USAGE_PARSER_VERSION,
            source_file_id: 1,
            expected_file_generation: 1,
            expected_previous_thread_id: None,
            expected_checkpoint: CheckpointExpectation {
                parser_version: USAGE_PARSER_VERSION,
                committed_offset: 0,
                guard_hash: None,
                status: CheckpointStatus::Pending,
            },
            expected_checkpoint_missing: false,
            expected_state: None,
            local_replay: false,
            batch_start_offset: 0,
            fixed_observed_raw_size: 1,
            last_complete_offset: 1,
            source_bytes_consumed: 1,
            complete_line_count: 1,
            candidate_count: 1,
            replayed_prefix_bytes: 0,
            replayed_prefix_lines: 0,
            fixed_view_exhausted: true,
            tail_status: TailStatus::None,
            tail_start_offset: None,
            owning_thread_id: "thread".to_owned(),
            root_session_id: "root".to_owned(),
            events: vec![super::super::processor::UsageEvent {
                event_id: event_id.clone(),
                kind,
                occurred_at_ms: 0,
                thread_id: "thread".to_owned(),
                root_session_id: "root".to_owned(),
                turn_key: None,
                model: model.to_owned(),
                reasoning_effort: None,
                usage: usage.clone(),
                previous_total: None,
                current_total: usage,
            }],
            occurrences: vec![super::super::processor::Occurrence {
                source_file_id: 1,
                file_generation: 1,
                source_start_offset: 0,
                source_end_offset: 1,
                event_id,
            }],
            skill_events: Vec::new(),
            closed_turns: Vec::new(),
            open_turn: None,
            anomalies: Vec::new(),
            updated_state: state,
            next_guard_hash: None,
            committed_at_ms: 0,
        }
    }

    #[test]
    fn t_mu03_b04_source_commit_persists_known_and_unknown_costs() {
        let (_, _, _, _, known) =
            source_commit(source_dto("gpt-5.6-sol", EventKind::Normal)).unwrap();
        assert_eq!(known.events[0].estimated_cost_nanos_usd, Some(4_380_000));

        let (_, _, _, _, unknown) =
            source_commit(source_dto("unknown-model", EventKind::Recovered)).unwrap();
        assert_eq!(unknown.events[0].estimated_cost_nanos_usd, None);
    }

    #[test]
    fn t_perf_003_facade_groups_rows_by_thread_and_deduplicates_sources() {
        let epoch = SourceUsageEpochState::new(
            crate::source::SourceId::codex(),
            7,
            None,
            USAGE_PARSER_VERSION,
            None,
        )
        .unwrap();
        let raw = storage::usage::UsageWorkListState {
            epoch: epoch.clone(),
            rows: vec![
                storage::usage::UsageWorkListRow {
                    source_file_id: 20,
                    owning_thread_id: "thread-z".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 3,
                    owning_thread_id: "thread-a".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 11,
                    owning_thread_id: "thread-z".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 3,
                    owning_thread_id: "thread-a".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 2,
                    owning_thread_id: "thread-a".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 7,
                    owning_thread_id: "thread-b".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 20,
                    owning_thread_id: "thread-z".to_owned(),
                },
                storage::usage::UsageWorkListRow {
                    source_file_id: 6,
                    owning_thread_id: "thread-b".to_owned(),
                },
            ],
        };

        let worklist = convert_work_list(raw);

        assert_eq!(worklist.epoch, epoch);
        assert_eq!(
            worklist.threads,
            vec![
                UsageWorkThread {
                    thread_id: "thread-a".to_owned(),
                    source_file_ids: vec![2, 3],
                },
                UsageWorkThread {
                    thread_id: "thread-b".to_owned(),
                    source_file_ids: vec![6, 7],
                },
                UsageWorkThread {
                    thread_id: "thread-z".to_owned(),
                    source_file_ids: vec![11, 20],
                },
            ]
        );
    }
}
