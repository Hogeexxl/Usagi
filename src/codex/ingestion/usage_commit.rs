//! Conversion of processor output into the Codex storage commit DTO.
//!
//! Cost estimation belongs to this boundary: private storage receives a
//! complete derived-cost value and never resolves pricing itself.

use crate::{
    codex::{
        normalization::{USAGE_PARSER_VERSION, usage_fingerprint},
        storage::usage as storage_usage,
    },
    cost::{
        BundledPricingRepository, CostEstimateOutcome, CostEstimator, granularity_for_event_kind,
    },
    usage::event::EventKind,
};

use super::{
    usage_pipeline::{
        CheckpointStatus, SourceContinuationState, SourceStateProof, TailStatus,
        UsageSourceCommitDto,
    },
    usage_processor::{
        Anomaly, AnomalyCode, ClosedTurn, CompensationBlocks, GapKind, TurnEndStatus,
        TurnModelState, TurnReasoningEffortState, TurnState,
    },
};

type BuildResult<T> = Result<T, &'static str>;
type TurnCommon = (
    Option<storage_usage::UsageSnapshot>,
    Option<storage_usage::UsageSnapshot>,
    storage_usage::UsageSnapshot,
    storage_usage::UsageTurnModelState,
    storage_usage::UsageTurnReasoningEffortState,
    bool,
    storage_usage::UsageCompensationBlocks,
    &'static str,
);

pub(crate) fn commit_group(
    storage: &crate::codex::storage::CodexStorage<'_>,
    dtos: Vec<UsageSourceCommitDto>,
) -> Result<storage_usage::UsageCommitOutcome, crate::codex::storage::CodexStorageError> {
    let batch = build_batch(dtos)
        .map_err(|error| crate::codex::storage::CodexStorageError::InvalidBindingState)?;
    storage.commit_group(batch)
}

pub(crate) fn build_batch(
    dtos: Vec<UsageSourceCommitDto>,
) -> BuildResult<storage_usage::UsageCommitBatch> {
    let mut iter = dtos.into_iter();
    let first = iter.next().ok_or("empty usage Thread group")?;
    let (epoch, parser, thread, root, source) = source_commit(first)?;
    let mut sources = vec![source];
    for dto in iter {
        let (next_epoch, next_parser, next_thread, next_root, source) = source_commit(dto)?;
        if (next_epoch, next_parser, next_thread, next_root)
            != (epoch, parser, thread.clone(), root.clone())
        {
            return Err("usage Thread group facts do not match");
        }
        sources.push(source);
    }
    Ok(storage_usage::UsageCommitBatch {
        ledger_epoch: epoch,
        usage_parser_version: parser,
        thread_id: thread,
        root_session_id: root,
        sources,
    })
}

fn source_commit(
    dto: UsageSourceCommitDto,
) -> BuildResult<(i64, i64, String, String, storage_usage::UsageSourceCommit)> {
    if dto.parser_version != USAGE_PARSER_VERSION {
        return Err("unexpected usage parser version");
    }
    let expected_state = dto.expected_state.as_ref().map(source_state).transpose()?;
    let updated_state = source_state(&dto.updated_state)?;
    let mut turns = dto
        .closed_turns
        .iter()
        .map(|turn| closed_turn(turn, dto.last_complete_offset, dto.committed_at_ms))
        .collect::<BuildResult<Vec<_>>>()?;
    if let Some(turn) = dto.open_turn.as_ref() {
        turns.push(open_turn(
            turn,
            dto.last_complete_offset,
            dto.committed_at_ms,
        )?);
    }

    let pricing = BundledPricingRepository::new();
    let estimator = CostEstimator::new();
    let events = dto
        .events
        .iter()
        .map(|event| {
            let estimated_cost_nanos_usd = estimate_cost(
                &pricing,
                &estimator,
                &event.model,
                event.occurred_at_ms,
                event.kind,
                &event.usage,
            )?;
            Ok(storage_usage::UsageEventWrite {
                event_id: event.event_id.clone(),
                kind: event.kind,
                occurred_at_ms: event.occurred_at_ms,
                thread_id: event.thread_id.clone(),
                root_session_id: event.root_session_id.clone(),
                turn_key: event.turn_key.clone(),
                model: event.model.clone(),
                reasoning_effort: event.reasoning_effort.clone(),
                estimated_cost_nanos_usd,
                usage: event.usage.clone(),
                created_at_ms: dto.committed_at_ms,
            })
        })
        .collect::<BuildResult<Vec<_>>>()?;
    let occurrences = dto
        .occurrences
        .iter()
        .map(|occurrence| {
            Ok(storage_usage::UsageOccurrenceWrite {
                source_file_id: occurrence.source_file_id,
                file_generation: occurrence.file_generation,
                source_start_offset: i64::try_from(occurrence.source_start_offset)
                    .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
                source_end_offset: i64::try_from(occurrence.source_end_offset)
                    .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
                event_id: occurrence.event_id.clone(),
            })
        })
        .collect::<BuildResult<Vec<_>>>()?;
    let skill_events = dto
        .skill_events
        .iter()
        .map(|event| {
            Ok(storage_usage::SkillUsageEventWrite {
                occurred_at_ms: event.occurred_at_ms,
                thread_id: event.thread_id.clone(),
                root_session_id: event.root_session_id.clone(),
                model: event.model.clone(),
                skill_name: event.skill_name.clone(),
                source_file_id: event.source_file_id,
                file_generation: event.file_generation,
                source_start_offset: i64::try_from(event.source_start_offset)
                    .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
                source_end_offset: i64::try_from(event.source_end_offset)
                    .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
            })
        })
        .collect::<BuildResult<Vec<_>>>()?;
    let anomalies = dto
        .anomalies
        .iter()
        .map(|anomaly| anomaly_write(anomaly, &dto))
        .collect::<BuildResult<Vec<_>>>()?;
    let source = storage_usage::UsageSourceCommit {
        source_file_id: dto.source_file_id,
        expected_file_generation: dto.expected_file_generation,
        expected_previous_thread_id: dto.expected_previous_thread_id,
        expected_checkpoint: storage_usage::UsageCheckpointExpectation {
            parser_version: dto.expected_checkpoint.parser_version,
            committed_offset: i64::try_from(dto.expected_checkpoint.committed_offset)
                .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
            guard_hash: dto.expected_checkpoint.guard_hash,
            processing_status: checkpoint_status(dto.expected_checkpoint.status),
        },
        expected_checkpoint_missing: dto.expected_checkpoint_missing,
        expected_state,
        local_replay: dto.local_replay,
        batch_start_offset: i64::try_from(dto.batch_start_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        fixed_observed_raw_size: i64::try_from(dto.fixed_observed_raw_size)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        last_complete_offset: i64::try_from(dto.last_complete_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        source_bytes_consumed: i64::try_from(dto.source_bytes_consumed)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        complete_line_count: i64::try_from(dto.complete_line_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        candidate_count: i64::try_from(dto.candidate_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        replayed_prefix_bytes: i64::try_from(dto.replayed_prefix_bytes)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        replayed_prefix_lines: i64::try_from(dto.replayed_prefix_lines)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        fixed_view_exhausted: dto.fixed_view_exhausted,
        tail_status: tail_status(dto.tail_status),
        tail_start_offset: dto
            .tail_start_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        events,
        occurrences,
        skill_events,
        turns,
        anomalies,
        updated_state,
        next_guard_hash: dto.next_guard_hash,
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

fn estimate_cost(
    pricing: &BundledPricingRepository,
    estimator: &CostEstimator,
    model: &str,
    occurred_at_ms: i64,
    kind: EventKind,
    usage: &crate::usage::NormalizedTokenUsage,
) -> BuildResult<Option<i64>> {
    let Some(model_pricing) = pricing.resolve(model, occurred_at_ms) else {
        return Ok(None);
    };
    match estimator
        .estimate_with(usage, model_pricing, granularity_for_event_kind(kind))
        .map_err(|_| "usage cost estimation failed")?
    {
        CostEstimateOutcome::Known(cost) => Ok(Some(cost.total_nanos_usd)),
        CostEstimateOutcome::Unknown(_) => Ok(None),
    }
}

fn checkpoint_status(status: CheckpointStatus) -> crate::codex::domain::CheckpointProcessingStatus {
    match status {
        CheckpointStatus::Pending => crate::codex::domain::CheckpointProcessingStatus::Pending,
        CheckpointStatus::Ready => crate::codex::domain::CheckpointProcessingStatus::Ready,
        CheckpointStatus::Error => crate::codex::domain::CheckpointProcessingStatus::Error,
        CheckpointStatus::RebuildRequired => {
            crate::codex::domain::CheckpointProcessingStatus::RebuildRequired
        }
    }
}

fn tail_status(status: TailStatus) -> storage_usage::UsageTailStatus {
    match status {
        TailStatus::Unverified => storage_usage::UsageTailStatus::Unverified,
        TailStatus::None => storage_usage::UsageTailStatus::None,
        TailStatus::HalfLine => storage_usage::UsageTailStatus::HalfLine,
    }
}

fn snapshot(value: &crate::usage::NormalizedTokenUsage) -> storage_usage::UsageSnapshot {
    storage_usage::UsageSnapshot {
        vector: value.clone(),
        fingerprint: usage_fingerprint(value).to_vec(),
    }
}

fn source_state(value: &SourceStateProof) -> BuildResult<storage_usage::UsageSourceStateWrite> {
    Ok(storage_usage::UsageSourceStateWrite {
        file_generation: value.file_generation,
        device_id: value.device_id,
        inode: value.inode,
        usage_parser_version: value.parser_version,
        canonical_algorithm_version: value.canonical_algorithm_version,
        resolved_through_offset: i64::try_from(value.resolved_through_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        observed_raw_size: i64::try_from(value.observed_raw_size)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        raw_tail_status: tail_status(value.raw_tail_status),
        raw_tail_start_offset: value
            .raw_tail_start_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        owning_thread_id: value.owning_thread_id.clone(),
        root_session_id: value.root_session_id.clone(),
        continuation_state: match value.continuation_state {
            SourceContinuationState::ReplayedAncestor => {
                storage_usage::UsageContinuationState::ReplayedAncestor
            }
            SourceContinuationState::OwningLive => {
                storage_usage::UsageContinuationState::OwningLive
            }
        },
        previous_total: value.processor_state.previous_total.as_ref().map(snapshot),
        previous_total_offset: value
            .processor_state
            .previous_total_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        chain_state: chain_state(value.processor_state.chain_state),
        active_turn_key: value
            .processor_state
            .open_turn
            .as_ref()
            .map(|turn| turn.turn_key.clone()),
        active_model: value.processor_state.active_model.clone(),
        active_model_offset: value
            .active_model_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        active_reasoning_effort: value.processor_state.active_reasoning_effort.clone(),
        active_reasoning_effort_offset: value
            .active_reasoning_effort_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        updated_at_ms: value.updated_at_ms,
    })
}

fn chain_state(state: super::usage_processor::ChainState) -> storage_usage::UsageChainState {
    match state {
        super::usage_processor::ChainState::Continuous => {
            storage_usage::UsageChainState::Continuous
        }
        super::usage_processor::ChainState::Interrupted(reason) => {
            storage_usage::UsageChainState::Interrupted(match reason {
                GapKind::Malformed => storage_usage::UsageGapReason::Malformed,
                GapKind::Oversized => storage_usage::UsageGapReason::Oversized,
                GapKind::RequiredInvalid => storage_usage::UsageGapReason::TotalInvalid,
                GapKind::Ownership => storage_usage::UsageGapReason::OwnershipGap,
                GapKind::Parser => storage_usage::UsageGapReason::ParserGap,
            })
        }
    }
}

fn turn_common(turn: &TurnState, through: u64, updated_at: i64) -> BuildResult<TurnCommon> {
    let model_state = match &turn.model_state {
        TurnModelState::None => storage_usage::UsageTurnModelState::None,
        TurnModelState::Single(value) => storage_usage::UsageTurnModelState::Single(value.clone()),
        TurnModelState::Mixed => storage_usage::UsageTurnModelState::Mixed,
    };
    let reasoning = match &turn.reasoning_effort_state {
        TurnReasoningEffortState::None => storage_usage::UsageTurnReasoningEffortState::None,
        TurnReasoningEffortState::Single(value) => {
            storage_usage::UsageTurnReasoningEffortState::Single(value.clone())
        }
        TurnReasoningEffortState::Mixed => storage_usage::UsageTurnReasoningEffortState::Mixed,
    };
    let blocks = blocks(turn.blocks);
    let quality = if blocks == storage_usage::UsageCompensationBlocks::default()
        && !turn.unresolved_model_seen
    {
        "complete"
    } else {
        "partial"
    };
    let _ = (through, updated_at);
    Ok((
        turn.start_total.as_ref().map(snapshot),
        turn.last_total.as_ref().map(snapshot),
        snapshot(&turn.accounted),
        model_state,
        reasoning,
        turn.unresolved_reasoning_effort_seen,
        blocks,
        quality,
    ))
}

fn open_turn(
    turn: &TurnState,
    through: u64,
    updated_at: i64,
) -> BuildResult<storage_usage::UsageTurnWrite> {
    let (
        start_total,
        last_total,
        accounted,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        blocks,
        quality_status,
    ) = turn_common(turn, through, updated_at)?;
    Ok(storage_usage::UsageTurnWrite {
        turn_key: turn.turn_key.clone(),
        raw_turn_id: turn.raw_turn_id.clone(),
        started_at_ms: turn.started_at_ms,
        ended_at_ms: None,
        start_offset: i64::try_from(turn.start_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        end_offset: None,
        status: storage_usage::UsageTurnStatus::Open,
        start_total,
        last_total,
        accounted,
        accounted_candidate_count: i64::try_from(turn.accounted_candidate_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        unresolved_model_seen: turn.unresolved_model_seen,
        blocks,
        quality_status,
        state_through_offset: i64::try_from(through)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        updated_at_ms: updated_at,
    })
}

fn closed_turn(
    turn: &ClosedTurn,
    through: u64,
    updated_at: i64,
) -> BuildResult<storage_usage::UsageTurnWrite> {
    let (
        start_total,
        last_total,
        accounted,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        blocks,
        quality_status,
    ) = turn_common(&turn.turn, through, updated_at)?;
    Ok(storage_usage::UsageTurnWrite {
        turn_key: turn.turn.turn_key.clone(),
        raw_turn_id: turn.turn.raw_turn_id.clone(),
        started_at_ms: turn.turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms,
        start_offset: i64::try_from(turn.turn.start_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        end_offset: Some(
            i64::try_from(turn.end_offset).map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        ),
        status: match turn.status {
            TurnEndStatus::Completed => storage_usage::UsageTurnStatus::Completed,
            TurnEndStatus::Aborted => storage_usage::UsageTurnStatus::Aborted,
            TurnEndStatus::Failed => storage_usage::UsageTurnStatus::Failed,
        },
        start_total,
        last_total,
        accounted,
        accounted_candidate_count: i64::try_from(turn.turn.accounted_candidate_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        model_state,
        reasoning_effort_state,
        unresolved_reasoning_effort_seen,
        unresolved_model_seen: turn.turn.unresolved_model_seen,
        blocks,
        quality_status,
        state_through_offset: i64::try_from(through)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        updated_at_ms: updated_at,
    })
}

fn blocks(value: CompensationBlocks) -> storage_usage::UsageCompensationBlocks {
    storage_usage::UsageCompensationBlocks {
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
) -> BuildResult<storage_usage::UsageAnomalyWrite> {
    let kind = match anomaly.code {
        AnomalyCode::UsageTimeMissing => storage_usage::UsageAnomalyKind::UsageTimeMissing,
        AnomalyCode::RequiredTotalInvalid => storage_usage::UsageAnomalyKind::RequiredTotalInvalid,
        AnomalyCode::LastUsageInvalid => storage_usage::UsageAnomalyKind::LastUsageInvalid,
        AnomalyCode::TotalChainReset => storage_usage::UsageAnomalyKind::TotalChainReset,
        AnomalyCode::CacheWriteChainDecrease => {
            storage_usage::UsageAnomalyKind::CacheWriteChainDecrease
        }
        AnomalyCode::TurnAccountedExceedsTotal => {
            storage_usage::UsageAnomalyKind::TurnAccountedExceedsTotal
        }
        AnomalyCode::TurnCacheWriteDeltaNegative => {
            storage_usage::UsageAnomalyKind::TurnCacheWriteDeltaNegative
        }
        AnomalyCode::TurnIdMismatch => storage_usage::UsageAnomalyKind::TurnIdMismatch,
        AnomalyCode::TurnReplaced => storage_usage::UsageAnomalyKind::TurnReplaced,
        AnomalyCode::ArithmeticOverflow => storage_usage::UsageAnomalyKind::ArithmeticOverflow,
    };
    let mut encoder = AnomalyEncoder::new(b"usage-anomaly-v1");
    encoder.byte(anomaly_code(anomaly.code));
    encoder.i64(dto.source_file_id);
    encoder.i64(dto.expected_file_generation);
    encoder.optional_u64(anomaly.source_start_offset);
    encoder.text(&dto.owning_thread_id);
    encoder.optional_text(anomaly.turn_key.as_deref());
    let anomaly_id = encoder.finish();
    Ok(storage_usage::UsageAnomalyWrite {
        anomaly_id,
        detected_at_ms: dto.committed_at_ms,
        occurred_at_ms: None,
        kind,
        severity_error: matches!(
            anomaly.code,
            AnomalyCode::RequiredTotalInvalid | AnomalyCode::ArithmeticOverflow
        ),
        source_start_offset: anomaly
            .source_start_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
    })
}

const fn anomaly_code(code: AnomalyCode) -> u8 {
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
    fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}
