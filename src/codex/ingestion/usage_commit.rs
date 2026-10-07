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
        Anomaly, AnomalyCode, CompensationBlocks, GapKind, LegacyWindowWrite, MarkerUnknownReason,
        PersistedTurnSnapshot, PersistedTurnStatus, TurnModelState, TurnReasoningEffortState,
        TurnRewrite, UsageEventHoldReason,
    },
};

type BuildResult<T> = Result<T, &'static str>;

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
    mut dto: UsageSourceCommitDto,
) -> BuildResult<(i64, i64, String, String, storage_usage::UsageSourceCommit)> {
    if dto.parser_version != USAGE_PARSER_VERSION {
        return Err("unexpected usage parser version");
    }
    dto.patch.fold();
    let expected_state = dto.expected_state.as_ref().map(source_state).transpose()?;
    let updated_state = source_state(&dto.updated_state)?;
    let counts = dto
        .patch
        .counts()
        .ok_or("usage reconciliation patch count overflow")?;
    if (
        dto.canonical_event_count,
        dto.occurrence_count,
        dto.evidence_write_count,
        dto.write_unit_count,
    ) != (
        counts.canonical_event_count,
        counts.occurrence_count,
        counts.evidence_write_count,
        counts.write_unit_count,
    ) {
        return Err("usage reconciliation patch counts do not match DTO");
    }

    let pricing = BundledPricingRepository::new();
    let estimator = CostEstimator::new();
    let events = dto
        .patch
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
        .patch
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
    let facts = dto
        .patch
        .facts
        .iter()
        .map(|fact| storage_usage::UsageEventFactWrite {
            event_id: fact.event_id.clone(),
            owning_thread_id: fact.owning_thread_id.clone(),
            response_id: fact.response_id.clone(),
            evidence_kind: fact.evidence_kind,
            operation: fact.operation,
        })
        .collect::<Vec<_>>();
    let marker_upserts = dto
        .patch
        .marker_updates
        .iter()
        .map(marker_write)
        .collect::<BuildResult<Vec<_>>>()?;
    let window_upserts = dto
        .patch
        .window_updates
        .iter()
        .map(window_write)
        .collect::<BuildResult<Vec<_>>>()?;
    let turn_upserts = dto
        .patch
        .turn_upserts
        .iter()
        .map(|turn| persisted_turn(turn, dto.committed_at_ms))
        .collect::<BuildResult<Vec<_>>>()?;
    let turn_rewrites = dto
        .patch
        .turn_rewrites
        .iter()
        .map(|rewrite| turn_rewrite(rewrite, dto.committed_at_ms))
        .collect::<BuildResult<Vec<_>>>()?;
    let delete_markers = dto
        .patch
        .delete_markers
        .iter()
        .map(|key| {
            Ok(storage_usage::UsagePrivateRowKey {
                source_file_id: key.source_file_id,
                file_generation: key.file_generation,
                source_start_offset: i64::try_from(key.source_start_offset)
                    .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
            })
        })
        .collect::<BuildResult<Vec<_>>>()?;
    let delete_windows = dto
        .patch
        .delete_windows
        .iter()
        .map(|key| {
            Ok(storage_usage::UsagePrivateRowKey {
                source_file_id: key.source_file_id,
                file_generation: key.file_generation,
                source_start_offset: i64::try_from(key.source_start_offset)
                    .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
            })
        })
        .collect::<BuildResult<Vec<_>>>()?;
    let delete_holds = dto
        .patch
        .delete_holds
        .iter()
        .map(|key| {
            (
                key.source_file_id,
                key.file_generation,
                key.event_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    let hold_upserts = dto
        .patch
        .hold_updates
        .iter()
        .map(|hold| storage_usage::UsageEventHoldWrite {
            source_file_id: hold.source_file_id,
            file_generation: hold.file_generation,
            event_id: hold.event_id.clone(),
            hold_reason: match hold.hold_reason {
                UsageEventHoldReason::Replay => storage_usage::UsageEventHoldReason::Replay,
                UsageEventHoldReason::Carry => storage_usage::UsageEventHoldReason::Carry,
            },
        })
        .collect::<Vec<_>>();
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
        .patch
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
        canonical_event_count: i64::try_from(dto.canonical_event_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        occurrence_count: i64::try_from(dto.occurrence_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        evidence_write_count: i64::try_from(dto.evidence_write_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        write_unit_count: i64::try_from(dto.write_unit_count)
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
        patch: storage_usage::ReconciliationPatchWrite {
            delete_event_ids: dto.patch.delete_event_ids,
            delete_markers,
            delete_windows,
            delete_holds,
            events,
            occurrences,
            facts,
            marker_upserts,
            window_upserts,
            hold_upserts,
            turn_upserts,
            turn_rewrites,
            anomalies,
            ..storage_usage::ReconciliationPatchWrite::default()
        },
        skill_events,
        reconciliation_request: dto.reconciliation_request,
        reconciliation_expected_fingerprint: dto.reconciliation_expected_fingerprint,
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
        reconciliation_state_json: value
            .processor_state
            .reconciliation_carry
            .to_json()
            .map_err(|_| "invalid usage reconciliation carry")?,
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

fn persisted_turn(
    snapshot: &PersistedTurnSnapshot,
    updated_at: i64,
) -> BuildResult<storage_usage::UsageTurnWrite> {
    let turn = &snapshot.state;
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
    Ok(storage_usage::UsageTurnWrite {
        source_file_id: snapshot.key.source_file_id,
        file_generation: snapshot.key.file_generation,
        thread_id: snapshot.owning_thread_id.clone(),
        turn_key: turn.turn_key.clone(),
        raw_turn_id: turn.raw_turn_id.clone(),
        started_at_ms: turn.started_at_ms,
        ended_at_ms: snapshot.ended_at_ms,
        start_offset: i64::try_from(turn.start_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        end_offset: snapshot
            .end_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        status: match snapshot.status {
            PersistedTurnStatus::Open => storage_usage::UsageTurnStatus::Open,
            PersistedTurnStatus::Completed => storage_usage::UsageTurnStatus::Completed,
            PersistedTurnStatus::Aborted => storage_usage::UsageTurnStatus::Aborted,
            PersistedTurnStatus::Failed => storage_usage::UsageTurnStatus::Failed,
        },
        start_total: turn.start_total.as_ref().map(snapshot_usage),
        last_total: turn.last_total.as_ref().map(snapshot_usage),
        accounted: snapshot_usage(&turn.accounted),
        accounted_candidate_count: i64::try_from(turn.accounted_candidate_count)
            .map_err(|_| "usage count exceeds SQLite INTEGER")?,
        model_state,
        reasoning_effort_state: reasoning,
        unresolved_reasoning_effort_seen: turn.unresolved_reasoning_effort_seen,
        unresolved_model_seen: turn.unresolved_model_seen,
        blocks,
        quality_status: match snapshot.quality_status.as_str() {
            "complete" => "complete",
            "partial" => "partial",
            "conflict" => "conflict",
            _ => return Err("invalid persisted turn quality status"),
        },
        state_through_offset: i64::try_from(snapshot.state_through_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        updated_at_ms: updated_at,
    })
}

fn turn_rewrite(
    rewrite: &TurnRewrite,
    updated_at: i64,
) -> BuildResult<storage_usage::UsageTurnRewriteWrite> {
    Ok(storage_usage::UsageTurnRewriteWrite {
        expected: persisted_turn(&rewrite.expected, updated_at)?,
        replacement: persisted_turn(&rewrite.replacement, updated_at)?,
    })
}

fn snapshot_usage(value: &crate::usage::NormalizedTokenUsage) -> storage_usage::UsageSnapshot {
    storage_usage::UsageSnapshot {
        vector: value.clone(),
        fingerprint: usage_fingerprint(value).to_vec(),
    }
}

fn marker_write(
    marker: &super::usage_processor::CompactionMarkerWrite,
) -> BuildResult<storage_usage::UsageCompactionMarkerWrite> {
    Ok(storage_usage::UsageCompactionMarkerWrite {
        source_file_id: marker.source_file_id,
        file_generation: marker.file_generation,
        source_start_offset: i64::try_from(marker.source_start_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        source_end_offset: i64::try_from(marker.source_end_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        owning_thread_id: marker.owning_thread_id.clone(),
        root_session_id: marker.root_session_id.clone(),
        occurred_at_ms: marker.occurred_at_ms,
        model: marker.model.clone(),
        reasoning_effort: marker.reasoning_effort.clone(),
        response_id: marker.response_id.clone(),
        resolved_event_id: marker.resolved_event_id.clone(),
        unknown_reason: marker.unknown_reason.map(|reason| match reason {
            MarkerUnknownReason::UsageMissing => "usage_missing",
            MarkerUnknownReason::IdentityMissing => "identity_missing",
            MarkerUnknownReason::UsageInvalid => "usage_invalid",
            MarkerUnknownReason::TimeMissing => "time_missing",
            MarkerUnknownReason::ModelUnresolved => "model_unresolved",
        }),
    })
}

fn window_write(
    window: &LegacyWindowWrite,
) -> BuildResult<storage_usage::UsageReconciliationWindowWrite> {
    Ok(storage_usage::UsageReconciliationWindowWrite {
        source_file_id: window.source_file_id,
        file_generation: window.file_generation,
        source_start_offset: i64::try_from(window.source_start_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        source_end_offset: i64::try_from(window.source_end_offset)
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        owning_thread_id: window.owning_thread_id.clone(),
        turn_key: window.turn_key.clone(),
        state_json: window
            .state
            .to_json()
            .map_err(|_| "invalid usage reconciliation window")?,
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
    anomaly_write_for(
        anomaly,
        dto.source_file_id,
        dto.expected_file_generation,
        &dto.owning_thread_id,
        dto.committed_at_ms,
    )
}

pub(super) fn anomaly_write_for(
    anomaly: &Anomaly,
    source_file_id: i64,
    file_generation: i64,
    owning_thread_id: &str,
    detected_at_ms: i64,
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
        AnomalyCode::ReconciliationPatchTooLarge => {
            storage_usage::UsageAnomalyKind::ReconciliationPatchTooLarge
        }
        AnomalyCode::ResponseUsageConflict => {
            storage_usage::UsageAnomalyKind::ResponseUsageConflict
        }
        AnomalyCode::ResponseOwnershipMismatch => {
            storage_usage::UsageAnomalyKind::ResponseOwnershipMismatch
        }
        AnomalyCode::CompactionIdentityMismatch => {
            storage_usage::UsageAnomalyKind::CompactionIdentityMismatch
        }
        AnomalyCode::LegacyCoverageAmbiguous => {
            storage_usage::UsageAnomalyKind::LegacyCoverageAmbiguous
        }
        AnomalyCode::ThreadUsageMismatch => storage_usage::UsageAnomalyKind::ThreadUsageMismatch,
    };
    let mut encoder = AnomalyEncoder::new(b"usage-anomaly-v1");
    encoder.byte(anomaly_code(anomaly.code));
    encoder.i64(source_file_id);
    encoder.i64(file_generation);
    encoder.optional_u64(anomaly.source_start_offset);
    encoder.text(owning_thread_id);
    encoder.optional_text(anomaly.turn_key.as_deref());
    let anomaly_id = encoder.finish();
    Ok(storage_usage::UsageAnomalyWrite {
        anomaly_id,
        detected_at_ms,
        occurred_at_ms: None,
        kind,
        severity_error: matches!(
            anomaly.code,
            AnomalyCode::RequiredTotalInvalid
                | AnomalyCode::ArithmeticOverflow
                | AnomalyCode::ReconciliationPatchTooLarge
                | AnomalyCode::ResponseUsageConflict
                | AnomalyCode::ResponseOwnershipMismatch
                | AnomalyCode::CompactionIdentityMismatch
                | AnomalyCode::LegacyCoverageAmbiguous
        ),
        source_start_offset: anomaly
            .source_start_offset
            .map(i64::try_from)
            .transpose()
            .map_err(|_| "usage offset exceeds SQLite INTEGER")?,
        turn_key: anomaly.turn_key.clone(),
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
        AnomalyCode::ReconciliationPatchTooLarge => 11,
        AnomalyCode::ResponseUsageConflict => 12,
        AnomalyCode::ResponseOwnershipMismatch => 13,
        AnomalyCode::CompactionIdentityMismatch => 14,
        AnomalyCode::LegacyCoverageAmbiguous => 15,
        AnomalyCode::ThreadUsageMismatch => 16,
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
