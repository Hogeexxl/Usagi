//! Usage-side orchestration for one fixed rollout-file view.
//!
//! The scanner supplies complete lines together with the ownership decisions
//! made by the metadata parser. This module deliberately owns neither file IO
//! nor SQL transactions: it turns that shared chunk into the single-source DTO
//! consumed by the storage usage commit seam.

use std::cell::OnceCell;

use crate::codex::{
    CodexRolloutParser, CompleteUsageLine, EnvelopeKind, LifecycleKind, NormalizedTokenValue,
    OptionalTokenValue, RecordClassification, RecordOwnership, SkillUsageParser, UsageRawRecord,
};

use super::usage_processor::{
    Anomaly, AnomalyCode, GapKind, Ownership, PendingEvidenceRecord, ProcessResult,
    ReconciliationCarry, ReconciliationContext, ReconciliationPatch, ReconciliationRequest,
    RecordApplyOutcome, ResponseKey, TurnEndStatus, UsageContext, UsageProcessor, UsageRecord,
    UsageSourceState, UsageValue, turn_key_for,
};

/// Provenance emitted by the Codex skill parser and persisted with the usage
/// source commit.  This is ingestion data, not a generic analytics model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillUsageEvent {
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub model: Option<String>,
    pub skill_name: String,
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
    pub source_end_offset: u64,
}

pub const MAX_BATCH_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_BATCH_LINES: u64 = 4096;
pub const MAX_BATCH_WRITE_UNITS: u64 = 2048;
pub const MAX_LEGAL_LINE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanAction {
    ReadFrom,
    BuildFrom,
    LocalReplay,
    AwaitOwningMeta,
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
pub enum CheckpointStatus {
    Pending,
    Ready,
    Error,
    RebuildRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceContinuationState {
    ReplayedAncestor,
    OwningLive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointExpectation {
    pub parser_version: i64,
    pub committed_offset: u64,
    pub guard_hash: Option<Vec<u8>>,
    pub status: CheckpointStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStateProof {
    pub file_generation: i64,
    pub device_id: i64,
    pub inode: i64,
    pub parser_version: i64,
    pub canonical_algorithm_version: i64,
    pub resolved_through_offset: u64,
    pub observed_raw_size: u64,
    pub raw_tail_status: TailStatus,
    pub raw_tail_start_offset: Option<u64>,
    pub owning_thread_id: String,
    pub root_session_id: String,
    pub continuation_state: SourceContinuationState,
    pub processor_state: UsageSourceState,
    pub active_model_offset: Option<u64>,
    pub active_reasoning_effort_offset: Option<u64>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsagePipelinePlan {
    pub ledger_epoch: i64,
    pub parser_version: i64,
    pub source_file_id: i64,
    pub file_generation: i64,
    pub device_id: i64,
    pub inode: i64,
    pub action: PlanAction,
    pub start_offset: u64,
    pub read_start_offset: u64,
    pub fixed_observed_size: u64,
    pub owning_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub checkpoint: CheckpointExpectation,
    pub state: Option<SourceStateProof>,
    /// True only when the metadata safe fact for this exact fixed view proves
    /// that a newly established owner legitimately ends while replaying an ancestor.
    pub allow_replay_tail: bool,
    pub replayed_prefix_bytes_before_chunk: u64,
    pub replayed_prefix_lines_before_chunk: u64,
    pub reconciliation_context: ReconciliationContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailStatus {
    Unverified,
    None,
    HalfLine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedViewTail {
    pub exhausted: bool,
    pub status: TailStatus,
    pub half_line_start: Option<u64>,
}

pub struct ClassifiedUsageLine {
    pub line: CompleteUsageLine,
    pub classification: RecordClassification,
    pub decoded: OnceCell<UsageRawRecord>,
}

pub struct ClassifiedOversizedUsageLine {
    pub start_offset: u64,
    pub end_offset: u64,
    pub classification: RecordClassification,
}

pub enum ClassifiedUsageItem {
    Line(ClassifiedUsageLine),
    Oversized(ClassifiedOversizedUsageLine),
}

impl From<ClassifiedUsageLine> for ClassifiedUsageItem {
    fn from(value: ClassifiedUsageLine) -> Self {
        Self::Line(value)
    }
}

impl From<ClassifiedOversizedUsageLine> for ClassifiedUsageItem {
    fn from(value: ClassifiedOversizedUsageLine) -> Self {
        Self::Oversized(value)
    }
}

impl ClassifiedUsageItem {
    fn start_offset(&self) -> u64 {
        match self {
            Self::Line(value) => value.line.start_offset(),
            Self::Oversized(value) => value.start_offset,
        }
    }

    fn end_offset(&self) -> u64 {
        match self {
            Self::Line(value) => value.line.end_offset(),
            Self::Oversized(value) => value.end_offset,
        }
    }

    fn classification(&self) -> &RecordClassification {
        match self {
            Self::Line(value) => &value.classification,
            Self::Oversized(value) => &value.classification,
        }
    }

    /// Decodes the line once; later callers reuse the cached record.
    fn usage_record(&self, owning_thread_id: &str) -> Option<UsageRecord> {
        let raw = match self {
            Self::Oversized(value) => UsageRawRecord::OversizedComplete {
                start_offset: value.start_offset,
                end_offset: value.end_offset,
            },
            Self::Line(value) => value
                .decoded
                .get_or_init(|| CodexRolloutParser.parse_line(&value.line))
                .clone(),
        };
        normalized_record(
            raw,
            owning_thread_id,
            self.start_offset(),
            self.end_offset(),
        )
    }
}

/// Builds the storage request for one decoded chunk. Each line is decoded at
/// most once; the decoded records stay cached on the items for processing.
pub fn reconciliation_request(
    items: &[ClassifiedUsageItem],
    owning_thread_id: &str,
    current_turn_key: Option<&str>,
    carry: &ReconciliationCarry,
) -> Result<ReconciliationRequest, Anomaly> {
    let mut response_keys = Vec::new();
    let mut turn_keys = Vec::new();
    let mut overflow_offset = carry.open_window_start_offset;
    if let Some(turn_key) = current_turn_key {
        turn_keys.push((owning_thread_id.to_owned(), Some(turn_key.to_owned())));
    }
    for item in items {
        if item.classification().ownership != RecordOwnership::Owning {
            continue;
        }
        overflow_offset = Some(overflow_offset.map_or(item.start_offset(), |offset| {
            offset.min(item.start_offset())
        }));
        let Some(record) = item.usage_record(owning_thread_id) else {
            continue;
        };
        let mut response = |id: &str| {
            response_keys.push(ResponseKey {
                owning_thread_id: owning_thread_id.to_owned(),
                response_id: id.to_owned(),
            });
        };
        match record {
            UsageRecord::ResponseUsage { evidence, .. } => {
                response(&evidence.response_id);
                turn_keys.push((owning_thread_id.to_owned(), evidence.turn_id));
            }
            UsageRecord::Compacted { evidence, .. } => {
                if let Some(id) = &evidence.compaction_response_id {
                    response(id);
                }
                if let Some(latest) = evidence.latest_token_usage_record {
                    response(&latest.response_id);
                    turn_keys.push((owning_thread_id.to_owned(), latest.turn_id));
                }
            }
            UsageRecord::TurnStarted {
                turn_id,
                timestamp_ms,
                start_offset,
                ..
            } => turn_keys.push((
                owning_thread_id.to_owned(),
                Some(turn_key_for(
                    owning_thread_id,
                    turn_id.as_deref(),
                    start_offset,
                    timestamp_ms,
                )),
            )),
            UsageRecord::TurnEnded { turn_id, .. } => {
                turn_keys.push((owning_thread_id.to_owned(), turn_id))
            }
            _ => {}
        }
    }
    response_keys.extend(carry.pending_response_ids.iter().map(|id| ResponseKey {
        owning_thread_id: owning_thread_id.to_owned(),
        response_id: id.clone(),
    }));
    for pending in &carry.pending_evidence {
        match &pending.record {
            PendingEvidenceRecord::ResponseUsage {
                timestamp_ms,
                start_offset,
                evidence,
                ..
            } => {
                response_keys.push(ResponseKey {
                    owning_thread_id: owning_thread_id.to_owned(),
                    response_id: evidence.response_id.clone(),
                });
                turn_keys.push((
                    owning_thread_id.to_owned(),
                    Some(turn_key_for(
                        owning_thread_id,
                        evidence.turn_id.as_deref(),
                        *start_offset,
                        *timestamp_ms,
                    )),
                ));
            }
            PendingEvidenceRecord::Compacted {
                timestamp_ms,
                start_offset,
                evidence,
                ..
            } => {
                if let Some(id) = &evidence.compaction_response_id {
                    response_keys.push(ResponseKey {
                        owning_thread_id: owning_thread_id.to_owned(),
                        response_id: id.clone(),
                    });
                }
                if let Some(latest) = &evidence.latest_token_usage_record {
                    response_keys.push(ResponseKey {
                        owning_thread_id: owning_thread_id.to_owned(),
                        response_id: latest.response_id.clone(),
                    });
                    turn_keys.push((
                        owning_thread_id.to_owned(),
                        Some(turn_key_for(
                            owning_thread_id,
                            latest.turn_id.as_deref(),
                            *start_offset,
                            *timestamp_ms,
                        )),
                    ));
                }
            }
        }
        let start_offset = match &pending.record {
            PendingEvidenceRecord::ResponseUsage { start_offset, .. }
            | PendingEvidenceRecord::Compacted { start_offset, .. } => *start_offset,
        };
        overflow_offset =
            Some(overflow_offset.map_or(start_offset, |offset| offset.min(start_offset)));
    }
    let request = ReconciliationRequest::new(response_keys, turn_keys);
    if request.response_keys.len() + request.owning_turn_keys.len() > 8192 {
        return Err(Anomaly {
            code: AnomalyCode::ReconciliationPatchTooLarge,
            source_start_offset: overflow_offset,
            turn_key: current_turn_key.map(str::to_owned),
        });
    }
    Ok(request)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageSourceCommitDto {
    pub ledger_epoch: i64,
    pub parser_version: i64,
    pub source_file_id: i64,
    pub expected_file_generation: i64,
    pub expected_previous_thread_id: Option<String>,
    pub expected_checkpoint: CheckpointExpectation,
    pub expected_checkpoint_missing: bool,
    pub expected_state: Option<SourceStateProof>,
    pub local_replay: bool,
    pub batch_start_offset: u64,
    pub fixed_observed_raw_size: u64,
    pub last_complete_offset: u64,
    pub source_bytes_consumed: u64,
    pub complete_line_count: u64,
    pub canonical_event_count: u64,
    pub occurrence_count: u64,
    pub evidence_write_count: u64,
    pub write_unit_count: u64,
    pub replayed_prefix_bytes: u64,
    pub replayed_prefix_lines: u64,
    pub fixed_view_exhausted: bool,
    pub tail_status: TailStatus,
    pub tail_start_offset: Option<u64>,
    pub owning_thread_id: String,
    pub root_session_id: String,
    pub patch: ReconciliationPatch,
    pub reconciliation_request: ReconciliationRequest,
    pub reconciliation_expected_fingerprint: Vec<u8>,
    pub skill_events: Vec<SkillUsageEvent>,
    pub updated_state: SourceStateProof,
    pub next_guard_hash: Option<Vec<u8>>,
    pub committed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "preserve the established public pipeline disposition shape"
)]
pub enum PipelineDisposition {
    Commit(UsageSourceCommitDto),
    FatalAnomaly(Anomaly),
    AwaitingOwningMeta,
    Skip,
    BlockedRelationship,
    NeedsRebuild,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineError {
    InvalidPlan,
    InvalidTail,
    /// A single record needs more than `MAX_BATCH_WRITE_UNITS` write units.
    PatchTooLarge,
    CounterOverflow,
}

pub struct UsagePipeline;

impl UsagePipeline {
    pub fn process_chunk<I>(
        plan: UsagePipelinePlan,
        lines: I,
        tail: FixedViewTail,
        next_guard_hash: Option<Vec<u8>>,
        metadata_needs_rebuild: bool,
        committed_at_ms: i64,
    ) -> Result<PipelineDisposition, PipelineError>
    where
        I: IntoIterator,
        I::Item: Into<ClassifiedUsageItem>,
    {
        validate_plan(&plan)?;
        validate_tail(&plan, tail)?;
        if metadata_needs_rebuild {
            return Ok(PipelineDisposition::NeedsRebuild);
        }
        match plan.action {
            PlanAction::Skip => return Ok(PipelineDisposition::Skip),
            PlanAction::BlockedRelationship => {
                return Ok(PipelineDisposition::BlockedRelationship);
            }
            PlanAction::RebuildRequired => return Ok(PipelineDisposition::NeedsRebuild),
            PlanAction::CompleteOnly | PlanAction::BeginCarry | PlanAction::ResumeCarry => {
                return Err(PipelineError::InvalidPlan);
            }
            PlanAction::ReadFrom
            | PlanAction::BuildFrom
            | PlanAction::LocalReplay
            | PlanAction::AwaitOwningMeta
            | PlanAction::ResumeOwningLive
            | PlanAction::VerifyRawTail => {}
        }

        let Some(owning_thread_id) = plan.owning_thread_id.clone() else {
            return Ok(PipelineDisposition::BlockedRelationship);
        };
        let Some(root_session_id) = plan.root_session_id.clone() else {
            return Ok(PipelineDisposition::BlockedRelationship);
        };
        let mut lines = lines.into_iter().map(Into::into).peekable();

        if plan.action == PlanAction::LocalReplay {
            return process_local_replay(
                plan,
                &owning_thread_id,
                &root_session_id,
                &mut lines,
                tail,
                next_guard_hash,
                committed_at_ms,
            );
        }

        if plan.action == PlanAction::AwaitOwningMeta
            || ((plan.action == PlanAction::ReadFrom || plan.action == PlanAction::BuildFrom)
                && plan.start_offset == 0
                && plan.state.is_none())
        {
            return establish_ownership(
                plan,
                &owning_thread_id,
                &root_session_id,
                &mut lines,
                next_guard_hash,
                committed_at_ms,
            );
        }

        let original_proof = plan.state.as_ref().ok_or(PipelineError::InvalidPlan)?;
        let original_state = original_proof.processor_state.clone();
        let mut continuation_state = original_proof.continuation_state;
        let context = UsageContext {
            source_file_id: plan.source_file_id,
            file_generation: plan.file_generation,
            owning_thread_id: owning_thread_id.clone(),
            root_session_id: root_session_id.clone(),
        };
        let mut processor = UsageProcessor::new(
            context.clone(),
            original_state,
            plan.reconciliation_context.clone(),
        );
        let mut active_model_offset = plan
            .state
            .as_ref()
            .and_then(|state| state.active_model_offset);
        let mut active_reasoning_effort_offset = plan
            .state
            .as_ref()
            .and_then(|state| state.active_reasoning_effort_offset);
        let mut skill_events = Vec::new();
        let mut complete_line_count = 0u64;
        let mut last_complete_offset = plan.start_offset;
        let mut patch_too_large_offset = None;

        for item in lines {
            if !matching_item(&item, last_complete_offset, plan.fixed_observed_size) {
                return Ok(PipelineDisposition::NeedsRebuild);
            }
            let start = item.start_offset();
            let end = item.end_offset();
            let line_bytes = end - start;
            let oversized = matches!(item, ClassifiedUsageItem::Oversized(_));
            if !fits_line_budget(
                last_complete_offset - plan.start_offset,
                complete_line_count,
                line_bytes,
                oversized,
            ) {
                break;
            }
            match item.classification().ownership {
                RecordOwnership::UnknownOwnership => {
                    return Ok(PipelineDisposition::NeedsRebuild);
                }
                RecordOwnership::ReplayedAncestor => {
                    if continuation_state != SourceContinuationState::ReplayedAncestor {
                        return Ok(PipelineDisposition::NeedsRebuild);
                    }
                    complete_line_count += 1;
                    last_complete_offset = end;
                    if oversized {
                        break;
                    }
                    continue;
                }
                RecordOwnership::Owning => {
                    continuation_state = SourceContinuationState::OwningLive;
                }
            }
            let skills_before = skill_events.len();
            collect_skill_events(&item, processor.state(), &context, &mut skill_events);
            let Some(record) = item.usage_record(&owning_thread_id) else {
                processor.observe_consumed_offset(end);
                complete_line_count += 1;
                last_complete_offset = end;
                continue;
            };
            let context_record = match &record {
                UsageRecord::TurnContext {
                    model,
                    reasoning_effort,
                    ..
                } => Some((model.is_some(), reasoning_effort.is_some())),
                _ => None,
            };
            if processor.needs_rebuild() {
                return Ok(processor_rebuild_disposition(processor));
            }
            if apply_record(&mut processor, record)? == RecordApplyOutcome::BudgetExceeded {
                skill_events.truncate(skills_before);
                if processor.patch_write_units() == Some(0) {
                    patch_too_large_offset = Some(start);
                }
                break;
            }
            if processor.needs_rebuild() {
                return Ok(processor_rebuild_disposition(processor));
            }
            processor.observe_consumed_offset(end);
            if let Some((has_model, has_effort)) = context_record {
                if has_model {
                    active_model_offset = Some(start);
                }
                // Missing effort is an explicit context boundary and clears
                // the durable source offset instead of inheriting the prior
                // turn.
                active_reasoning_effort_offset = has_effort.then_some(start);
            }
            complete_line_count += 1;
            last_complete_offset = end;
            if oversized {
                break;
            }
        }

        let mut result = processor.finish();
        if let Some(offset) = patch_too_large_offset {
            result.patch.anomalies.push(Anomaly {
                code: AnomalyCode::ReconciliationPatchTooLarge,
                source_start_offset: Some(offset),
                turn_key: result
                    .updated_state
                    .open_turn
                    .as_ref()
                    .map(|turn| turn.turn_key.clone()),
            });
        }
        let effective_tail = if last_complete_offset < plan.fixed_observed_size
            && tail.exhausted
            && tail.status == TailStatus::None
        {
            FixedViewTail {
                exhausted: false,
                status: TailStatus::Unverified,
                half_line_start: None,
            }
        } else {
            tail
        };
        validate_completed_tail(
            last_complete_offset,
            plan.fixed_observed_size,
            effective_tail,
        )?;
        Ok(PipelineDisposition::Commit(commit_dto(
            plan,
            owning_thread_id,
            root_session_id,
            result,
            skill_events,
            last_complete_offset,
            complete_line_count,
            0,
            0,
            effective_tail,
            next_guard_hash,
            committed_at_ms,
            active_model_offset,
            active_reasoning_effort_offset,
            continuation_state,
        )?))
    }
}

fn establish_ownership<I>(
    plan: UsagePipelinePlan,
    owning_thread_id: &str,
    root_session_id: &str,
    lines: &mut std::iter::Peekable<I>,
    next_guard_hash: Option<Vec<u8>>,
    committed_at_ms: i64,
) -> Result<PipelineDisposition, PipelineError>
where
    I: Iterator<Item = ClassifiedUsageItem>,
{
    let context = UsageContext {
        source_file_id: plan.source_file_id,
        file_generation: plan.file_generation,
        owning_thread_id: owning_thread_id.to_owned(),
        root_session_id: root_session_id.to_owned(),
    };
    let mut processor = UsageProcessor::new(
        context.clone(),
        UsageSourceState::default(),
        plan.reconciliation_context.clone(),
    );
    let mut active_model_offset = None;
    let mut active_reasoning_effort_offset = None;
    let mut skill_events = Vec::new();
    let mut last = plan.read_start_offset;
    let mut replayed_bytes = plan.replayed_prefix_bytes_before_chunk;
    let mut replayed_lines = plan.replayed_prefix_lines_before_chunk;
    let mut complete_line_count = replayed_lines;
    let mut ownership_established = false;
    let mut continuation_state = SourceContinuationState::OwningLive;
    let mut patch_too_large_offset = None;

    for item in lines.by_ref() {
        if !matching_item(&item, last, plan.fixed_observed_size) {
            return Ok(PipelineDisposition::NeedsRebuild);
        }
        let start = item.start_offset();
        let end = item.end_offset();
        let bytes = end - start;
        let oversized = matches!(item, ClassifiedUsageItem::Oversized(_));

        if !ownership_established {
            match item.classification().ownership {
                RecordOwnership::ReplayedAncestor => {
                    replayed_bytes = replayed_bytes.saturating_add(bytes);
                    replayed_lines = replayed_lines.saturating_add(1);
                    complete_line_count = complete_line_count.saturating_add(1);
                    last = end;
                    continue;
                }
                RecordOwnership::UnknownOwnership => {
                    return Ok(PipelineDisposition::AwaitingOwningMeta);
                }
                RecordOwnership::Owning => {
                    let ClassifiedUsageItem::Line(line) = &item else {
                        return Ok(PipelineDisposition::AwaitingOwningMeta);
                    };
                    if !matches!(
                        line.classification.envelope,
                        EnvelopeKind::SessionMeta | EnvelopeKind::TurnContext
                    ) {
                        return Ok(PipelineDisposition::AwaitingOwningMeta);
                    }
                    ownership_established = true;
                    continuation_state = SourceContinuationState::OwningLive;
                }
            }
        } else {
            match item.classification().ownership {
                RecordOwnership::UnknownOwnership => {
                    return Ok(PipelineDisposition::NeedsRebuild);
                }
                RecordOwnership::ReplayedAncestor => {
                    if !plan.allow_replay_tail {
                        return Ok(PipelineDisposition::NeedsRebuild);
                    }
                    continuation_state = SourceContinuationState::ReplayedAncestor;
                    replayed_bytes = replayed_bytes.saturating_add(bytes);
                    replayed_lines = replayed_lines.saturating_add(1);
                    complete_line_count = complete_line_count.saturating_add(1);
                    last = end;
                    if oversized {
                        break;
                    }
                    continue;
                }
                RecordOwnership::Owning => {
                    continuation_state = SourceContinuationState::OwningLive;
                }
            }
        }

        if !fits_line_budget(
            last.saturating_sub(plan.start_offset)
                .saturating_sub(replayed_bytes),
            complete_line_count.saturating_sub(replayed_lines),
            bytes,
            oversized,
        ) {
            break;
        }
        let skills_before = skill_events.len();
        collect_skill_events(&item, processor.state(), &context, &mut skill_events);
        if let Some(record) = item.usage_record(owning_thread_id) {
            let context_record = match &record {
                UsageRecord::TurnContext {
                    model,
                    reasoning_effort,
                    ..
                } => Some((model.is_some(), reasoning_effort.is_some())),
                _ => None,
            };
            if apply_record(&mut processor, record)? == RecordApplyOutcome::BudgetExceeded {
                skill_events.truncate(skills_before);
                if processor.patch_write_units() == Some(0) {
                    patch_too_large_offset = Some(start);
                }
                break;
            }
            if processor.needs_rebuild() {
                return Ok(processor_rebuild_disposition(processor));
            }
            if let Some((has_model, has_effort)) = context_record {
                if has_model {
                    active_model_offset = Some(start);
                }
                active_reasoning_effort_offset = has_effort.then_some(start);
            }
        }
        processor.observe_consumed_offset(end);
        complete_line_count = complete_line_count.saturating_add(1);
        last = end;

        // Preserve the historical empty ownership-boundary commit for normal
        // sources. The extended path is entered only for metadata-proven replay EOF.
        if !plan.allow_replay_tail {
            let mut result = processor.finish();
            if let Some(offset) = patch_too_large_offset {
                result.patch.anomalies.push(Anomaly {
                    code: AnomalyCode::ReconciliationPatchTooLarge,
                    source_start_offset: Some(offset),
                    turn_key: result
                        .updated_state
                        .open_turn
                        .as_ref()
                        .map(|turn| turn.turn_key.clone()),
                });
            }
            return Ok(PipelineDisposition::Commit(commit_dto(
                plan,
                owning_thread_id.to_owned(),
                root_session_id.to_owned(),
                result,
                skill_events,
                last,
                complete_line_count,
                replayed_bytes,
                replayed_lines,
                FixedViewTail {
                    exhausted: false,
                    status: TailStatus::Unverified,
                    half_line_start: None,
                },
                next_guard_hash,
                committed_at_ms,
                active_model_offset,
                active_reasoning_effort_offset,
                SourceContinuationState::OwningLive,
            )?));
        }
    }

    if !ownership_established {
        return Ok(PipelineDisposition::AwaitingOwningMeta);
    }
    let tail = FixedViewTail {
        exhausted: last == plan.fixed_observed_size,
        status: if last == plan.fixed_observed_size {
            TailStatus::None
        } else {
            TailStatus::Unverified
        },
        half_line_start: None,
    };
    let mut result = processor.finish();
    if let Some(offset) = patch_too_large_offset {
        result.patch.anomalies.push(Anomaly {
            code: AnomalyCode::ReconciliationPatchTooLarge,
            source_start_offset: Some(offset),
            turn_key: result
                .updated_state
                .open_turn
                .as_ref()
                .map(|turn| turn.turn_key.clone()),
        });
    }
    Ok(PipelineDisposition::Commit(commit_dto(
        plan,
        owning_thread_id.to_owned(),
        root_session_id.to_owned(),
        result,
        skill_events,
        last,
        complete_line_count,
        replayed_bytes,
        replayed_lines,
        tail,
        next_guard_hash,
        committed_at_ms,
        active_model_offset,
        active_reasoning_effort_offset,
        continuation_state,
    )?))
}

fn process_local_replay<I>(
    plan: UsagePipelinePlan,
    owning_thread_id: &str,
    root_session_id: &str,
    lines: &mut std::iter::Peekable<I>,
    tail: FixedViewTail,
    next_guard_hash: Option<Vec<u8>>,
    committed_at_ms: i64,
) -> Result<PipelineDisposition, PipelineError>
where
    I: Iterator<Item = ClassifiedUsageItem>,
{
    if plan.start_offset != 0
        || plan.read_start_offset < plan.start_offset
        || plan.replayed_prefix_bytes_before_chunk != plan.read_start_offset - plan.start_offset
    {
        return Err(PipelineError::InvalidPlan);
    }
    let context = UsageContext {
        source_file_id: plan.source_file_id,
        file_generation: plan.file_generation,
        owning_thread_id: owning_thread_id.to_owned(),
        root_session_id: root_session_id.to_owned(),
    };
    let reconciliation = local_replay_context_view(
        plan.reconciliation_context.clone(),
        plan.source_file_id,
        plan.file_generation,
    );
    let mut processor =
        UsageProcessor::new(context.clone(), UsageSourceState::default(), reconciliation);
    let mut active_model_offset = None;
    let mut active_reasoning_effort_offset = None;
    let mut skill_events = Vec::new();
    let mut last = plan.read_start_offset;
    let mut replayed_bytes = plan.replayed_prefix_bytes_before_chunk;
    let mut replayed_lines = plan.replayed_prefix_lines_before_chunk;
    let mut adapter_lines = 0u64;
    let mut adapter_bytes = 0u64;
    let mut ownership_established = false;
    let mut continuation_state = SourceContinuationState::OwningLive;
    let mut patch_too_large_offset = None;

    while let Some(item) = lines.next() {
        if !matching_item(&item, last, plan.fixed_observed_size) {
            return Ok(PipelineDisposition::NeedsRebuild);
        }
        let start = item.start_offset();
        let end = item.end_offset();
        let bytes = end - start;
        if !ownership_established {
            match item.classification().ownership {
                RecordOwnership::ReplayedAncestor => {
                    replayed_bytes = replayed_bytes.saturating_add(bytes);
                    replayed_lines = replayed_lines.saturating_add(1);
                    last = end;
                    continue;
                }
                RecordOwnership::UnknownOwnership => return Ok(PipelineDisposition::NeedsRebuild),
                RecordOwnership::Owning => {
                    if item.classification().envelope != EnvelopeKind::SessionMeta {
                        return Ok(PipelineDisposition::NeedsRebuild);
                    }
                    ownership_established = true;
                }
            }
        } else {
            match item.classification().ownership {
                RecordOwnership::UnknownOwnership => return Ok(PipelineDisposition::NeedsRebuild),
                RecordOwnership::ReplayedAncestor => {
                    if !plan.allow_replay_tail {
                        return Ok(PipelineDisposition::NeedsRebuild);
                    }
                    continuation_state = SourceContinuationState::ReplayedAncestor;
                    replayed_bytes = replayed_bytes.saturating_add(bytes);
                    replayed_lines = replayed_lines.saturating_add(1);
                    last = end;
                    continue;
                }
                RecordOwnership::Owning => {
                    continuation_state = SourceContinuationState::OwningLive;
                }
            }
        }

        let oversized = matches!(item, ClassifiedUsageItem::Oversized(_));
        if !fits_line_budget(adapter_bytes, adapter_lines, bytes, oversized) {
            return Ok(PipelineDisposition::NeedsRebuild);
        }
        let skills_before = skill_events.len();
        collect_skill_events(&item, processor.state(), &context, &mut skill_events);
        if let Some(record) = item.usage_record(owning_thread_id) {
            let context_record = match &record {
                UsageRecord::TurnContext {
                    model,
                    reasoning_effort,
                    ..
                } => Some((model.is_some(), reasoning_effort.is_some())),
                _ => None,
            };
            if apply_record(&mut processor, record)? == RecordApplyOutcome::BudgetExceeded {
                skill_events.truncate(skills_before);
                if processor.patch_write_units() == Some(0) {
                    patch_too_large_offset = Some(start);
                }
                break;
            }
            if processor.needs_rebuild() {
                return Ok(processor_rebuild_disposition(processor));
            }
            if let Some((has_model, has_effort)) = context_record {
                if has_model {
                    active_model_offset = Some(start);
                }
                active_reasoning_effort_offset = has_effort.then_some(start);
            }
        }
        processor.observe_consumed_offset(end);
        adapter_lines += 1;
        adapter_bytes += bytes;
        last = end;
        if oversized && lines.peek().is_some() {
            return Ok(PipelineDisposition::NeedsRebuild);
        }
    }

    if !ownership_established {
        return Ok(PipelineDisposition::NeedsRebuild);
    }
    let effective_tail =
        if last < plan.fixed_observed_size && tail.exhausted && tail.status == TailStatus::None {
            FixedViewTail {
                exhausted: false,
                status: TailStatus::Unverified,
                half_line_start: None,
            }
        } else {
            tail
        };
    if !effective_tail.exhausted || effective_tail.status == TailStatus::Unverified {
        return Ok(PipelineDisposition::NeedsRebuild);
    }
    validate_completed_tail(last, plan.fixed_observed_size, effective_tail)?;
    let mut result = processor.finish();
    if let Some(offset) = patch_too_large_offset {
        result.patch.anomalies.push(Anomaly {
            code: AnomalyCode::ReconciliationPatchTooLarge,
            source_start_offset: Some(offset),
            turn_key: result
                .updated_state
                .open_turn
                .as_ref()
                .map(|turn| turn.turn_key.clone()),
        });
    }
    Ok(PipelineDisposition::Commit(commit_dto(
        plan,
        owning_thread_id.to_owned(),
        root_session_id.to_owned(),
        result,
        skill_events,
        last,
        replayed_lines + adapter_lines,
        replayed_bytes,
        replayed_lines,
        effective_tail,
        next_guard_hash,
        committed_at_ms,
        active_model_offset,
        active_reasoning_effort_offset,
        continuation_state,
    )?))
}

fn local_replay_context_view(
    mut reconciliation: ReconciliationContext,
    source_file_id: i64,
    file_generation: i64,
) -> ReconciliationContext {
    let is_replayed_source = |source_id: i64, generation: i64| {
        source_id == source_file_id && generation == file_generation
    };

    reconciliation
        .response_occurrences
        .retain(|_, occurrences| {
            occurrences.retain(|occurrence| {
                !is_replayed_source(occurrence.source_file_id, occurrence.file_generation)
            });
            true
        });
    reconciliation
        .markers
        .retain(|marker| !is_replayed_source(marker.source_file_id, marker.file_generation));
    reconciliation
        .windows
        .retain(|key, _| !is_replayed_source(key.source_file_id, key.file_generation));
    reconciliation
        .window_metadata
        .retain(|key, _| !is_replayed_source(key.source_file_id, key.file_generation));
    reconciliation.window_proposals.retain(|key, proposals| {
        if is_replayed_source(key.source_file_id, key.file_generation) {
            return false;
        }
        for proposal in proposals {
            proposal.occurrences.retain(|occurrence| {
                !is_replayed_source(occurrence.source_file_id, occurrence.file_generation)
            });
        }
        true
    });
    reconciliation.affected_turns.retain(|key, affected| {
        if is_replayed_source(key.source_file_id, key.file_generation) {
            return false;
        }
        affected.compensation_occurrences.retain(|occurrence| {
            !is_replayed_source(occurrence.source_file_id, occurrence.file_generation)
        });
        let referenced = affected
            .compensation_occurrences
            .iter()
            .map(|occurrence| occurrence.event_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        affected
            .compensation_events
            .retain(|event| referenced.contains(event.event_id.as_str()));
        true
    });
    reconciliation
}

fn processor_rebuild_disposition(mut processor: UsageProcessor) -> PipelineDisposition {
    let result = processor.finish();
    result
        .patch
        .anomalies
        .into_iter()
        .find(|anomaly| {
            matches!(
                anomaly.code,
                AnomalyCode::ArithmeticOverflow
                    | AnomalyCode::ReconciliationPatchTooLarge
                    | AnomalyCode::ResponseUsageConflict
                    | AnomalyCode::ResponseOwnershipMismatch
                    | AnomalyCode::CompactionIdentityMismatch
                    | AnomalyCode::LegacyCoverageAmbiguous
            )
        })
        .map_or(
            PipelineDisposition::NeedsRebuild,
            PipelineDisposition::FatalAnomaly,
        )
}

fn apply_record(
    processor: &mut UsageProcessor,
    record: UsageRecord,
) -> Result<RecordApplyOutcome, PipelineError> {
    let remaining = match processor.write_units() {
        Some(current) => MAX_BATCH_WRITE_UNITS
            .checked_sub(current)
            .ok_or(PipelineError::PatchTooLarge)?,
        None => 0,
    };
    Ok(processor.try_process_record(record, remaining))
}

fn collect_skill_events(
    item: &ClassifiedUsageItem,
    state: &UsageSourceState,
    context: &UsageContext,
    output: &mut Vec<SkillUsageEvent>,
) {
    if item.classification().ownership != RecordOwnership::Owning {
        return;
    }
    let ClassifiedUsageItem::Line(value) = item else {
        return;
    };
    let Some(evidence) = SkillUsageParser.parse_line(&value.line) else {
        return;
    };
    for skill_name in evidence.skill_names {
        output.push(SkillUsageEvent {
            occurred_at_ms: evidence.occurred_at_ms,
            thread_id: context.owning_thread_id.clone(),
            root_session_id: context.root_session_id.clone(),
            model: state.active_model.clone(),
            skill_name,
            source_file_id: context.source_file_id,
            file_generation: context.file_generation,
            source_start_offset: value.line.start_offset(),
            source_end_offset: value.line.end_offset(),
        });
    }
}

fn matching_item(item: &ClassifiedUsageItem, expected: u64, observed_size: u64) -> bool {
    item.start_offset() == expected
        && item.start_offset() == item.classification().start_offset
        && item.end_offset() == item.classification().end_offset
        && item.end_offset() <= observed_size
}

fn fits_line_budget(consumed: u64, lines: u64, next_line_bytes: u64, oversized: bool) -> bool {
    if oversized {
        return lines == 0;
    }
    if lines == 0 {
        return next_line_bytes <= MAX_LEGAL_LINE_BYTES;
    }
    lines < MAX_BATCH_LINES && consumed + next_line_bytes <= MAX_BATCH_BYTES
}

fn normalized_record(
    raw: UsageRawRecord,
    owning_thread_id: &str,
    start_offset: u64,
    end_offset: u64,
) -> Option<UsageRecord> {
    let ownership = || Ownership::Owning {
        thread_id: owning_thread_id.to_owned(),
    };
    match raw {
        UsageRawRecord::ResponseUsage(record) => Some(UsageRecord::ResponseUsage {
            ownership: ownership(),
            timestamp_ms: record.occurred_at_ms,
            start_offset,
            end_offset,
            evidence: record.evidence,
        }),
        UsageRawRecord::Compacted(record) => Some(UsageRecord::Compacted {
            ownership: ownership(),
            timestamp_ms: record.occurred_at_ms,
            start_offset,
            end_offset,
            evidence: record.evidence,
        }),
        UsageRawRecord::TokenCount(record) => record.info.map(|info| UsageRecord::TokenCount {
            ownership: ownership(),
            timestamp_ms: record.occurred_at_ms,
            start_offset,
            end_offset,
            total: required_value(info.current_total),
            last: optional_value(info.last_usage),
        }),
        UsageRawRecord::TurnContext(record) => Some(UsageRecord::TurnContext {
            ownership: ownership(),
            model: record.model,
            reasoning_effort: record.reasoning_effort,
        }),
        UsageRawRecord::Lifecycle(record) => match record.kind {
            LifecycleKind::Started => Some(UsageRecord::TurnStarted {
                ownership: ownership(),
                turn_id: record.turn_id,
                timestamp_ms: record.occurred_at_ms,
                start_offset,
            }),
            LifecycleKind::Completed | LifecycleKind::Aborted | LifecycleKind::Failed => {
                Some(UsageRecord::TurnEnded {
                    ownership: ownership(),
                    turn_id: record.turn_id,
                    timestamp_ms: record.occurred_at_ms,
                    start_offset,
                    end_offset,
                    status: match record.kind {
                        LifecycleKind::Completed => TurnEndStatus::Completed,
                        LifecycleKind::Aborted => TurnEndStatus::Aborted,
                        LifecycleKind::Failed => TurnEndStatus::Failed,
                        LifecycleKind::Started => unreachable!(),
                    },
                })
            }
        },
        UsageRawRecord::Malformed => Some(UsageRecord::Gap {
            ownership: ownership(),
            kind: GapKind::Malformed,
            start_offset,
            end_offset,
        }),
        UsageRawRecord::OversizedComplete { .. } => Some(UsageRecord::Gap {
            ownership: ownership(),
            kind: GapKind::Oversized,
            start_offset,
            end_offset,
        }),
        UsageRawRecord::Ignored | UsageRawRecord::Unknown => None,
    }
}

fn required_value(value: NormalizedTokenValue) -> UsageValue {
    match value {
        NormalizedTokenValue::Valid(value) => UsageValue::Valid(token_usage(value)),
        NormalizedTokenValue::Invalid(_) => UsageValue::Invalid,
    }
}

fn optional_value(value: OptionalTokenValue) -> UsageValue {
    match value {
        OptionalTokenValue::Missing => UsageValue::Missing,
        OptionalTokenValue::Valid(value) => UsageValue::Valid(token_usage(value)),
        OptionalTokenValue::Invalid(_) => UsageValue::Invalid,
    }
}

fn token_usage(value: crate::usage::NormalizedTokenUsage) -> crate::usage::NormalizedTokenUsage {
    value
}

#[expect(
    clippy::too_many_arguments,
    reason = "preserve the established usage commit DTO seam"
)]
fn commit_dto(
    plan: UsagePipelinePlan,
    owning_thread_id: String,
    root_session_id: String,
    result: ProcessResult,
    skill_events: Vec<SkillUsageEvent>,
    last_complete_offset: u64,
    complete_line_count: u64,
    replayed_prefix_bytes: u64,
    replayed_prefix_lines: u64,
    tail: FixedViewTail,
    next_guard_hash: Option<Vec<u8>>,
    committed_at_ms: i64,
    active_model_offset: Option<u64>,
    active_reasoning_effort_offset: Option<u64>,
    continuation_state: SourceContinuationState,
) -> Result<UsageSourceCommitDto, PipelineError> {
    if result.needs_rebuild {
        return Err(PipelineError::CounterOverflow);
    }
    let counts = result
        .patch
        .counts()
        .ok_or(PipelineError::CounterOverflow)?;
    let source_bytes_consumed = last_complete_offset
        .checked_sub(plan.start_offset)
        .ok_or(PipelineError::InvalidPlan)?;
    let updated_state = SourceStateProof {
        file_generation: plan.file_generation,
        device_id: plan.device_id,
        inode: plan.inode,
        parser_version: plan.parser_version,
        canonical_algorithm_version: crate::codex::normalization::canonical_algorithm_for(
            plan.parser_version,
        )
        .unwrap_or(-1),
        resolved_through_offset: last_complete_offset,
        observed_raw_size: plan.fixed_observed_size,
        raw_tail_status: tail.status,
        raw_tail_start_offset: tail.half_line_start,
        owning_thread_id: owning_thread_id.clone(),
        root_session_id: root_session_id.clone(),
        continuation_state,
        processor_state: result.updated_state.clone(),
        active_model_offset,
        active_reasoning_effort_offset,
        updated_at_ms: committed_at_ms,
    };
    Ok(UsageSourceCommitDto {
        ledger_epoch: plan.ledger_epoch,
        parser_version: plan.parser_version,
        source_file_id: plan.source_file_id,
        expected_file_generation: plan.file_generation,
        expected_previous_thread_id: Some(owning_thread_id.clone()),
        expected_checkpoint_missing: plan.checkpoint.committed_offset == 0
            && plan.checkpoint.guard_hash.is_none()
            && plan.state.is_none()
            && plan.action == PlanAction::ReadFrom,
        expected_checkpoint: plan.checkpoint,
        expected_state: plan.state,
        local_replay: plan.action == PlanAction::LocalReplay,
        batch_start_offset: plan.start_offset,
        fixed_observed_raw_size: plan.fixed_observed_size,
        last_complete_offset,
        source_bytes_consumed,
        complete_line_count,
        canonical_event_count: counts.canonical_event_count,
        occurrence_count: counts.occurrence_count,
        evidence_write_count: counts.evidence_write_count,
        write_unit_count: counts.write_unit_count,
        replayed_prefix_bytes,
        replayed_prefix_lines,
        fixed_view_exhausted: tail.exhausted,
        tail_status: tail.status,
        tail_start_offset: tail.half_line_start,
        owning_thread_id,
        root_session_id,
        patch: result.patch,
        reconciliation_request: plan.reconciliation_context.request,
        reconciliation_expected_fingerprint: plan.reconciliation_context.expected_fingerprint,
        skill_events,
        updated_state,
        next_guard_hash,
        committed_at_ms,
    })
}

fn validate_plan(plan: &UsagePipelinePlan) -> Result<(), PipelineError> {
    let local_replay = plan.action == PlanAction::LocalReplay;
    if plan.ledger_epoch <= 0
        || plan.parser_version < 0
        || crate::codex::normalization::canonical_algorithm_for(plan.parser_version).is_none()
        || plan.source_file_id <= 0
        || plan.file_generation <= 0
        || plan.start_offset > plan.fixed_observed_size
        || plan.read_start_offset > plan.fixed_observed_size
        || plan.checkpoint.parser_version != plan.parser_version
        || (!local_replay && plan.checkpoint.committed_offset != plan.start_offset)
        || (local_replay && plan.start_offset != 0)
        || (plan.checkpoint.committed_offset == 0) != plan.checkpoint.guard_hash.is_none()
        || plan
            .checkpoint
            .guard_hash
            .as_ref()
            .is_some_and(|guard| guard.len() != 32)
    {
        return Err(PipelineError::InvalidPlan);
    }
    match plan.action {
        PlanAction::AwaitOwningMeta
            if plan.start_offset == 0
                && plan.read_start_offset >= plan.start_offset
                && plan.replayed_prefix_bytes_before_chunk
                    == plan.read_start_offset - plan.start_offset => {}
        PlanAction::AwaitOwningMeta => return Err(PipelineError::InvalidPlan),
        PlanAction::LocalReplay
            if plan.start_offset == 0
                && plan.read_start_offset >= plan.start_offset
                && plan.replayed_prefix_bytes_before_chunk
                    == plan.read_start_offset - plan.start_offset => {}
        PlanAction::ReadFrom | PlanAction::BuildFrom
            if plan.start_offset == 0
                && plan.state.is_none()
                && plan.read_start_offset >= plan.start_offset
                && plan.replayed_prefix_bytes_before_chunk
                    == plan.read_start_offset - plan.start_offset => {}
        _ if plan.read_start_offset == plan.start_offset
            && plan.replayed_prefix_bytes_before_chunk == 0
            && plan.replayed_prefix_lines_before_chunk == 0 => {}
        _ => return Err(PipelineError::InvalidPlan),
    }
    if local_replay {
        return match (&plan.state, plan.checkpoint.committed_offset) {
            (None, 0) => Ok(()),
            (Some(state), _)
                if state.file_generation == plan.file_generation
                    && state.device_id == plan.device_id
                    && state.inode == plan.inode
                    && state.parser_version == plan.parser_version
                    && state.canonical_algorithm_version
                        == crate::codex::normalization::canonical_algorithm_for(
                            plan.parser_version,
                        )
                        .unwrap_or(-1)
                    && plan.owning_thread_id.as_deref() == Some(&state.owning_thread_id)
                    && plan.root_session_id.as_deref() == Some(&state.root_session_id) =>
            {
                Ok(())
            }
            _ => Err(PipelineError::InvalidPlan),
        };
    }
    match (&plan.state, plan.start_offset) {
        (None, 0) => Ok(()),
        (Some(state), offset)
            if offset > 0
                && state.file_generation == plan.file_generation
                && state.device_id == plan.device_id
                && state.inode == plan.inode
                && state.parser_version == plan.parser_version
                && state.canonical_algorithm_version
                    == crate::codex::normalization::canonical_algorithm_for(
                        plan.parser_version,
                    )
                    .unwrap_or(-1)
                && state.resolved_through_offset == offset
                && plan.owning_thread_id.as_deref() == Some(&state.owning_thread_id)
                && plan.root_session_id.as_deref() == Some(&state.root_session_id) =>
        {
            Ok(())
        }
        _ => Err(PipelineError::InvalidPlan),
    }
}

fn validate_tail(plan: &UsagePipelinePlan, tail: FixedViewTail) -> Result<(), PipelineError> {
    match (tail.exhausted, tail.status, tail.half_line_start) {
        (false, TailStatus::Unverified, None) => Ok(()),
        (true, TailStatus::None, None) => Ok(()),
        (true, TailStatus::HalfLine, Some(offset)) if offset < plan.fixed_observed_size => Ok(()),
        _ => Err(PipelineError::InvalidTail),
    }
}

fn validate_completed_tail(
    last_complete_offset: u64,
    observed_size: u64,
    tail: FixedViewTail,
) -> Result<(), PipelineError> {
    match (tail.exhausted, tail.status, tail.half_line_start) {
        (false, TailStatus::Unverified, None) => Ok(()),
        (true, TailStatus::None, None) if last_complete_offset == observed_size => Ok(()),
        (true, TailStatus::HalfLine, Some(start))
            if start == last_complete_offset && start < observed_size =>
        {
            Ok(())
        }
        _ => Err(PipelineError::InvalidTail),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::ingestion::usage_processor::ResponseBinding;
    use crate::codex::usage::{CodexOperation, EvidenceKind, ResponseUsageEvidence};
    use crate::usage::event::EventKind;

    const OWNER: &str = "01981111-1111-7111-8111-111111111111";
    const ROOT: &str = "01981111-1111-7111-8111-111111111111";

    #[test]
    fn compaction_parse_pipeline_preserves_evidence_and_offsets() {
        let payload = serde_json::json!({
            "response_id":"response-fixture", "thread_id":OWNER, "session_id":"root-fixture",
            "usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":4,
                "reasoning_output_tokens":1,"total_tokens":14},
            "thread_token_usage":{"input_tokens":20,"cached_input_tokens":4,"output_tokens":8,
                "reasoning_output_tokens":2,"total_tokens":28}
        });
        let mut state = UsageSourceState::default();
        state.active_model = Some("fixture-model".to_owned());
        let context = UsageContext {
            source_file_id: 9,
            file_generation: 1,
            owning_thread_id: OWNER.to_owned(),
            root_session_id: ROOT.to_owned(),
        };
        for (kind, evidence, is_compaction) in [
            ("token_usage_record", payload.clone(), false),
            (
                "compacted",
                serde_json::json!({"compaction_response_id":"response-fixture",
                "latest_token_usage_record":payload}),
                true,
            ),
        ] {
            let bytes = format!(
                "{}\n",
                serde_json::json!({
                    "type":kind,"timestamp":1000,"payload":evidence
                })
            )
            .into_bytes();
            let line = CompleteUsageLine::new(123, bytes).unwrap();
            let end = line.end_offset();
            let record =
                normalized_record(CodexRolloutParser.parse_line(&line), OWNER, 123, end).unwrap();
            let (ownership, timestamp, start, stop) = match &record {
                UsageRecord::ResponseUsage {
                    ownership,
                    timestamp_ms,
                    start_offset,
                    end_offset,
                    evidence,
                } => {
                    assert_eq!(evidence.thread_id.as_deref(), Some(OWNER));
                    assert_eq!(evidence.session_id.as_deref(), Some("root-fixture"));
                    assert!(
                        matches!(&evidence.usage, UsageValue::Valid(usage) if usage.cache_write_tokens.is_none())
                    );
                    (ownership, timestamp_ms, start_offset, end_offset)
                }
                UsageRecord::Compacted {
                    ownership,
                    timestamp_ms,
                    start_offset,
                    end_offset,
                    evidence,
                } => {
                    assert_eq!(
                        evidence.compaction_response_id.as_deref(),
                        Some("response-fixture")
                    );
                    assert!(evidence.latest_token_usage_record.is_some());
                    (ownership, timestamp_ms, start_offset, end_offset)
                }
                _ => panic!("modern evidence must reach the processor"),
            };
            assert_eq!(
                ownership,
                &Ownership::Owning {
                    thread_id: OWNER.to_owned()
                }
            );
            assert_eq!((*timestamp, *start, *stop), (Some(1000), 123, end));
            let mut processor = UsageProcessor::new(
                context.clone(),
                state.clone(),
                ReconciliationContext::default(),
            );
            assert_eq!(
                processor.try_process_record(record, MAX_BATCH_WRITE_UNITS),
                RecordApplyOutcome::Applied
            );
            let result = processor.finish();
            assert_eq!(result.patch.events.len(), 1);
            let event = &result.patch.events[0];
            assert_eq!(event.kind, EventKind::Normal);
            assert_eq!(event.model, "fixture-model");
            assert_eq!(event.occurred_at_ms, 1000);
            assert_eq!(event.usage.input_tokens, 10);
            assert_eq!(event.usage.cached_tokens, 2);
            assert_eq!(event.usage.cache_write_tokens, None);
            assert_eq!(event.usage.output_tokens, 4);
            assert_eq!(event.usage.reasoning_tokens, 1);
            assert_eq!(event.usage.total_tokens, 14);
            assert_eq!(result.patch.occurrences.len(), 1);
            assert_eq!(
                (
                    result.patch.occurrences[0].source_start_offset,
                    result.patch.occurrences[0].source_end_offset,
                    result.patch.occurrences[0].event_id.as_str(),
                ),
                (123, end, event.event_id.as_str())
            );
            assert_eq!(result.patch.facts.len(), 1);
            let fact = &result.patch.facts[0];
            assert_eq!(fact.event_id, event.event_id);
            assert_eq!(fact.owning_thread_id, OWNER);
            assert_eq!(fact.response_id.as_deref(), Some("response-fixture"));
            assert_eq!(fact.evidence_kind, EvidenceKind::Explicit);
            assert_eq!(
                fact.operation,
                if is_compaction {
                    CodexOperation::Compaction
                } else {
                    CodexOperation::Response
                }
            );
            assert_eq!(
                result.patch.marker_updates.len(),
                if is_compaction { 1 } else { 0 }
            );
            if let Some(marker) = result.patch.marker_updates.first() {
                assert_eq!(marker.source_start_offset, 123);
                assert_eq!(marker.source_end_offset, end);
                assert_eq!(marker.response_id.as_deref(), Some("response-fixture"));
                assert_eq!(
                    marker.resolved_event_id.as_deref(),
                    Some(event.event_id.as_str())
                );
                assert_eq!(marker.unknown_reason, None);
            }
            assert_eq!(result.updated_state.active_model, state.active_model);
            assert_eq!(
                result.updated_state.previous_total, state.previous_total,
                "explicit response evidence does not advance the legacy counter"
            );
            assert_eq!(
                result
                    .updated_state
                    .reconciliation_carry
                    .modern_counter_domain,
                Some((OWNER.to_owned(), Some("root-fixture".to_owned())))
            );
            let modern_total = result
                .updated_state
                .reconciliation_carry
                .modern_counter_total
                .as_ref()
                .expect("thread usage is retained as the modern counter anchor");
            assert_eq!(modern_total.input_tokens, 20);
            assert_eq!(modern_total.cached_tokens, 4);
            assert_eq!(modern_total.cache_write_tokens, None);
            assert_eq!(modern_total.output_tokens, 8);
            assert_eq!(modern_total.reasoning_tokens, 2);
            assert_eq!(modern_total.total_tokens, 28);
        }
        assert!(matches!(
            normalized_record(UsageRawRecord::Malformed, OWNER, 123, 456),
            Some(UsageRecord::Gap {
                start_offset: 123,
                end_offset: 456,
                kind: GapKind::Malformed,
                ..
            })
        ));
    }

    fn line(
        start: u64,
        json: &str,
        envelope: EnvelopeKind,
        ownership: RecordOwnership,
    ) -> ClassifiedUsageLine {
        let line = CompleteUsageLine::new(start, format!("{json}\n").into_bytes()).unwrap();
        let classification = RecordClassification {
            start_offset: start,
            end_offset: line.end_offset(),
            envelope,
            ownership,
            response_ownership_mismatch: false,
        };
        ClassifiedUsageLine {
            line,
            classification,
            decoded: OnceCell::new(),
        }
    }

    fn checkpoint(offset: u64) -> CheckpointExpectation {
        CheckpointExpectation {
            parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            committed_offset: offset,
            guard_hash: (offset > 0).then(|| vec![7; 32]),
            status: CheckpointStatus::Ready,
        }
    }

    fn plan(action: PlanAction, start: u64, observed: u64) -> UsagePipelinePlan {
        let processor_state = UsageSourceState::default();
        UsagePipelinePlan {
            ledger_epoch: 1,
            parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
            source_file_id: 9,
            file_generation: 2,
            device_id: 3,
            inode: 4,
            action,
            start_offset: start,
            read_start_offset: start,
            fixed_observed_size: observed,
            owning_thread_id: Some(OWNER.to_owned()),
            root_session_id: Some(ROOT.to_owned()),
            checkpoint: checkpoint(start),
            state: (start > 0).then(|| SourceStateProof {
                file_generation: 2,
                device_id: 3,
                inode: 4,
                parser_version: crate::codex::normalization::USAGE_PARSER_VERSION,
                canonical_algorithm_version:
                    crate::codex::normalization::USAGE_CANONICAL_ALGORITHM_VERSION,
                resolved_through_offset: start,
                observed_raw_size: observed,
                raw_tail_status: TailStatus::Unverified,
                raw_tail_start_offset: None,
                owning_thread_id: OWNER.to_owned(),
                root_session_id: ROOT.to_owned(),
                continuation_state: SourceContinuationState::OwningLive,
                processor_state,
                active_model_offset: None,
                active_reasoning_effort_offset: None,
                updated_at_ms: 0,
            }),
            allow_replay_tail: false,
            replayed_prefix_bytes_before_chunk: 0,
            replayed_prefix_lines_before_chunk: 0,
            reconciliation_context: ReconciliationContext::default(),
        }
    }

    fn token_json(total: i64, last: i64) -> String {
        r#"{"timestamp":1000,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":$TOTAL,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":$TOTAL},"last_token_usage":{"input_tokens":$LAST,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":$LAST}}}}"#
            .replace("$TOTAL", &total.to_string())
            .replace("$LAST", &last.to_string())
    }

    fn turn_context_json_with_effort(model: &str, effort: &str) -> String {
        format!(
            r#"{{"timestamp":1000,"type":"turn_context","payload":{{"turn_id":"{OWNER}","model":"{model}","effort":"{effort}"}}}}"#
        )
    }

    fn turn_context_json_without_effort(model: &str) -> String {
        format!(
            r#"{{"timestamp":1000,"type":"turn_context","payload":{{"turn_id":"{OWNER}","model":"{model}"}}}}"#
        )
    }

    fn response_payload(response_id: &str) -> serde_json::Value {
        serde_json::json!({
            "response_id": response_id,
            "thread_id": OWNER,
            "session_id": ROOT,
            "usage": {
                "input_tokens": 10,
                "cached_input_tokens": 2,
                "output_tokens": 4,
                "reasoning_output_tokens": 1,
                "total_tokens": 14
            }
        })
    }

    fn response_json(response_id: &str, timestamp: i64) -> String {
        serde_json::json!({
            "type": "token_usage_record",
            "timestamp": timestamp,
            "payload": response_payload(response_id)
        })
        .to_string()
    }

    fn compaction_json(response_id: &str, timestamp: i64) -> String {
        serde_json::json!({
            "type": "compacted",
            "timestamp": timestamp,
            "payload": {
                "compaction_response_id": response_id,
                "latest_token_usage_record": response_payload(response_id)
            }
        })
        .to_string()
    }

    fn pipeline_context_chunk(start: u64) -> (Vec<ClassifiedUsageItem>, u64, Vec<u64>) {
        let records = [
            (
                format!(r#"{{"type":"session_meta","payload":{{"id":"{OWNER}"}}}}"#),
                EnvelopeKind::SessionMeta,
            ),
            (
                turn_context_json_with_effort("chunk-model", "low"),
                EnvelopeKind::TurnContext,
            ),
            (
                response_json("persisted-response", 2_000),
                EnvelopeKind::ResponseUsage,
            ),
            (
                serde_json::json!({
                    "type": "thread_settings_applied",
                    "payload": {"model": "ignored-model"}
                })
                .to_string(),
                EnvelopeKind::Ignored,
            ),
            (
                compaction_json("persisted-response", 3_000),
                EnvelopeKind::Compacted,
            ),
            (
                response_json("persisted-response", 4_000),
                EnvelopeKind::ResponseUsage,
            ),
        ];
        let mut cursor = start;
        let mut items = Vec::with_capacity(records.len());
        let mut starts = Vec::with_capacity(records.len());
        for (json, envelope) in records {
            starts.push(cursor);
            let line = line(cursor, &json, envelope, RecordOwnership::Owning);
            cursor = line.line.end_offset();
            items.push(ClassifiedUsageItem::Line(line));
        }
        (items, cursor, starts)
    }

    fn persisted_response_context() -> (ReconciliationContext, ResponseKey) {
        let usage = crate::usage::NormalizedTokenUsage::new(10, 2, None, 4, 1, 14).unwrap();
        let key = ResponseKey {
            owning_thread_id: OWNER.to_owned(),
            response_id: "persisted-response".to_owned(),
        };
        let context = UsageContext {
            source_file_id: 7,
            file_generation: 1,
            owning_thread_id: OWNER.to_owned(),
            root_session_id: ROOT.to_owned(),
        };
        let mut state = UsageSourceState::default();
        state.active_model = Some("persisted-model".to_owned());
        state.active_reasoning_effort = Some("high".to_owned());
        let mut processor = UsageProcessor::new(context, state, ReconciliationContext::default());
        assert_eq!(
            processor.try_process_record(
                UsageRecord::ResponseUsage {
                    ownership: Ownership::Owning {
                        thread_id: OWNER.to_owned(),
                    },
                    timestamp_ms: Some(1_000),
                    start_offset: 10,
                    end_offset: 20,
                    evidence: ResponseUsageEvidence {
                        response_id: key.response_id.clone(),
                        thread_id: Some(OWNER.to_owned()),
                        session_id: Some(ROOT.to_owned()),
                        turn_id: None,
                        usage: UsageValue::Valid(usage),
                        thread_token_usage: UsageValue::Missing,
                    },
                },
                MAX_BATCH_WRITE_UNITS,
            ),
            RecordApplyOutcome::Applied
        );
        let seeded = processor.finish();
        let proposal = seeded.patch.events[0].clone();
        let fact = seeded.patch.facts[0].clone();
        let mut reconciliation = ReconciliationContext::default();
        reconciliation
            .bindings
            .insert(key.clone(), ResponseBinding { proposal, fact });
        reconciliation
            .response_occurrences
            .insert(key.clone(), seeded.patch.occurrences);
        reconciliation.closure_response_keys.insert(key.clone());
        (reconciliation, key)
    }

    #[test]
    fn compaction_pipeline_context_keeps_first_binding_across_all_processor_paths() {
        for (action, start) in [
            (PlanAction::ResumeOwningLive, 10),
            (PlanAction::BuildFrom, 0),
            (PlanAction::LocalReplay, 0),
        ] {
            let (mut reconciliation, response_key) = persisted_response_context();
            let (items, observed, starts) = pipeline_context_chunk(start);
            assert!(items.windows(2).all(|pair| {
                pair[0].end_offset() == pair[1].start_offset()
                    && pair
                        .iter()
                        .all(|item| item.classification().ownership == RecordOwnership::Owning)
            }));
            let request =
                reconciliation_request(&items, OWNER, None, &ReconciliationCarry::default())
                    .unwrap();
            assert_eq!(request.response_keys, vec![response_key.clone()]);
            reconciliation.request = request;
            let mut plan = plan(action, start, observed);
            plan.allow_replay_tail = true;
            plan.reconciliation_context = reconciliation;
            let disposition = UsagePipeline::process_chunk(
                plan,
                items,
                FixedViewTail {
                    exhausted: true,
                    status: TailStatus::None,
                    half_line_start: None,
                },
                Some(vec![5; 32]),
                false,
                5_000,
            )
            .unwrap();
            let PipelineDisposition::Commit(commit) = disposition else {
                panic!("all three paths should commit the frozen SQL-free chunk");
            };
            let binding = &commit.patch.facts[0];
            assert_eq!(
                binding.event_id,
                commit.patch.marker_updates[0]
                    .resolved_event_id
                    .clone()
                    .unwrap()
            );
            assert_eq!(binding.operation, CodexOperation::Compaction);
            assert_eq!(commit.patch.events.len(), 0);
            assert_eq!(commit.patch.occurrences.len(), 3);
            assert_eq!(commit.patch.marker_updates.len(), 1);
            assert_eq!(
                commit.patch.marker_updates[0].source_start_offset,
                starts[4]
            );
            assert!(commit.patch.occurrences.iter().all(|occurrence| {
                starts[2..].contains(&occurrence.source_start_offset)
                    && occurrence.event_id == binding.event_id
            }));
            let persisted = commit
                .reconciliation_request
                .response_keys
                .iter()
                .find(|key| key == &&response_key)
                .expect("request includes the already persisted binding");
            assert_eq!(persisted.response_id, "persisted-response");
            let current_binding = &commit.patch.facts[0];
            assert_eq!(current_binding.evidence_kind, EvidenceKind::Explicit);
            assert_eq!(commit.last_complete_offset, observed);
        }
    }

    #[test]
    fn compaction_pipeline_context_adds_durable_carry_keys_to_the_request() {
        let json = response_json("chunk-response", 2_000);
        let item = ClassifiedUsageItem::Line(line(
            20,
            &json,
            EnvelopeKind::ResponseUsage,
            RecordOwnership::Owning,
        ));
        let evidence = ResponseUsageEvidence {
            response_id: "carry-response".to_owned(),
            thread_id: Some(OWNER.to_owned()),
            session_id: Some(ROOT.to_owned()),
            turn_id: Some("carry-turn".to_owned()),
            usage: UsageValue::Valid(
                crate::usage::NormalizedTokenUsage::new(2, 0, None, 0, 0, 2).unwrap(),
            ),
            thread_token_usage: UsageValue::Missing,
        };
        let carry = ReconciliationCarry {
            pending_response_ids: vec!["pending-id".to_owned(), "carry-response".to_owned()],
            pending_evidence: vec![super::super::usage_processor::PendingUsageEvidence {
                record: PendingEvidenceRecord::ResponseUsage {
                    timestamp_ms: Some(1_000),
                    start_offset: 5,
                    end_offset: 15,
                    evidence,
                },
                model: None,
                reasoning_effort: None,
            }],
            ..ReconciliationCarry::default()
        };
        let request = reconciliation_request(&[item], OWNER, Some("open-turn"), &carry).unwrap();
        assert_eq!(
            request.response_keys,
            vec![
                ResponseKey {
                    owning_thread_id: OWNER.to_owned(),
                    response_id: "carry-response".to_owned(),
                },
                ResponseKey {
                    owning_thread_id: OWNER.to_owned(),
                    response_id: "chunk-response".to_owned(),
                },
                ResponseKey {
                    owning_thread_id: OWNER.to_owned(),
                    response_id: "pending-id".to_owned(),
                },
            ]
        );
        let expected_turn = turn_key_for(OWNER, Some("carry-turn"), 5, Some(1_000));
        assert_eq!(
            request.owning_turn_keys,
            vec![
                (OWNER.to_owned(), None),
                (OWNER.to_owned(), Some(expected_turn)),
                (OWNER.to_owned(), Some("open-turn".to_owned())),
            ]
        );
    }

    #[test]
    fn t_mu04_b01_owning_turn_context_initializes_context_before_next_token() {
        let replay = line(
            0,
            &token_json(2, 2),
            EnvelopeKind::TokenCount,
            RecordOwnership::ReplayedAncestor,
        );
        let boundary_start = replay.line.end_offset();
        let boundary = line(
            boundary_start,
            &turn_context_json_with_effort("gpt-5.6-sol", "high"),
            EnvelopeKind::TurnContext,
            RecordOwnership::Owning,
        );
        let boundary_end = boundary.line.end_offset();
        let token_json = token_json(10, 10);
        let token = line(
            boundary_end,
            &token_json,
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let observed = token.line.end_offset();

        let PipelineDisposition::Commit(boundary_commit) = UsagePipeline::process_chunk(
            plan(PlanAction::AwaitOwningMeta, 0, observed),
            [replay, boundary, token],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![8; 32]),
            false,
            1,
        )
        .unwrap() else {
            panic!("expected ownership boundary commit");
        };

        assert!(boundary_commit.patch.events.is_empty());
        assert_eq!(boundary_commit.complete_line_count, 2);
        assert_eq!(boundary_commit.replayed_prefix_lines, 1);
        assert_eq!(
            boundary_commit
                .updated_state
                .processor_state
                .active_model
                .as_deref(),
            Some("gpt-5.6-sol")
        );
        assert_eq!(
            boundary_commit
                .updated_state
                .processor_state
                .active_reasoning_effort
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            boundary_commit.updated_state.active_model_offset,
            Some(boundary_start)
        );
        assert_eq!(
            boundary_commit.updated_state.active_reasoning_effort_offset,
            Some(boundary_start)
        );

        let mut resumed = plan(PlanAction::ResumeOwningLive, boundary_end, observed);
        resumed.state = Some(boundary_commit.updated_state);
        let token = line(
            boundary_end,
            &token_json,
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let PipelineDisposition::Commit(token_commit) = UsagePipeline::process_chunk(
            resumed,
            [token],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![9; 32]),
            false,
            2,
        )
        .unwrap() else {
            panic!("expected token commit");
        };

        assert_eq!(token_commit.patch.events.len(), 1);
        assert_eq!(token_commit.patch.events[0].model, "gpt-5.6-sol");
        assert_eq!(
            token_commit.patch.events[0].reasoning_effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn t_mu04_b01_ownership_boundaries_preserve_empty_session_meta_and_missing_effort() {
        let session_meta = line(
            0,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{OWNER}"}}}}"#),
            EnvelopeKind::SessionMeta,
            RecordOwnership::Owning,
        );
        let session_end = session_meta.line.end_offset();
        let PipelineDisposition::Commit(session_commit) = UsagePipeline::process_chunk(
            plan(PlanAction::AwaitOwningMeta, 0, session_end),
            [session_meta],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![8; 32]),
            false,
            1,
        )
        .unwrap() else {
            panic!("expected session boundary commit");
        };
        assert!(session_commit.patch.events.is_empty());
        assert_eq!(
            session_commit.updated_state.processor_state,
            UsageSourceState::default()
        );
        assert_eq!(session_commit.updated_state.active_model_offset, None);
        assert_eq!(
            session_commit.updated_state.active_reasoning_effort_offset,
            None
        );

        let missing_effort = line(
            0,
            &turn_context_json_without_effort("gpt-5.6-terra"),
            EnvelopeKind::TurnContext,
            RecordOwnership::Owning,
        );
        let missing_end = missing_effort.line.end_offset();
        let PipelineDisposition::Commit(missing_commit) = UsagePipeline::process_chunk(
            plan(PlanAction::AwaitOwningMeta, 0, missing_end),
            [missing_effort],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![8; 32]),
            false,
            1,
        )
        .unwrap() else {
            panic!("expected turn context boundary commit");
        };
        assert_eq!(
            missing_commit
                .updated_state
                .processor_state
                .active_model
                .as_deref(),
            Some("gpt-5.6-terra")
        );
        assert_eq!(
            missing_commit
                .updated_state
                .processor_state
                .active_reasoning_effort,
            None
        );
        assert_eq!(missing_commit.updated_state.active_model_offset, Some(0));
        assert_eq!(
            missing_commit.updated_state.active_reasoning_effort_offset,
            None
        );
    }

    #[test]
    fn t_mu04_b02_token_before_owning_model_remains_unresolved_without_inference() {
        let session_meta = line(
            0,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{OWNER}"}}}}"#),
            EnvelopeKind::SessionMeta,
            RecordOwnership::Owning,
        );
        let session_end = session_meta.line.end_offset();

        let PipelineDisposition::Commit(session_commit) = UsagePipeline::process_chunk(
            plan(PlanAction::AwaitOwningMeta, 0, session_end),
            [session_meta],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![8; 32]),
            false,
            1,
        )
        .unwrap() else {
            panic!("expected session ownership boundary commit");
        };
        assert!(session_commit.patch.events.is_empty());

        let first_token = line(
            session_end,
            &token_json(10, 10),
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let model_start = first_token.line.end_offset();
        let model = line(
            model_start,
            &turn_context_json_with_effort("gpt-5.6-luna", "low"),
            EnvelopeKind::TurnContext,
            RecordOwnership::Owning,
        );
        let second_token = line(
            model.line.end_offset(),
            &token_json(15, 5),
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let observed = second_token.line.end_offset();
        let mut resumed = plan(PlanAction::ResumeOwningLive, session_end, observed);
        resumed.state = Some(session_commit.updated_state);

        let PipelineDisposition::Commit(commit) = UsagePipeline::process_chunk(
            resumed,
            [first_token, model, second_token],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![9; 32]),
            false,
            2,
        )
        .unwrap() else {
            panic!("expected resumed usage commit");
        };

        assert_eq!(commit.patch.events.len(), 2);
        assert_eq!(commit.patch.events[0].model, "unknown");
        assert_eq!(commit.patch.events[0].reasoning_effort, None);
        assert_eq!(commit.patch.events[1].model, "gpt-5.6-luna");
        assert_eq!(
            commit.patch.events[1].reasoning_effort.as_deref(),
            Some("low")
        );
        assert_eq!(
            commit.updated_state.processor_state.active_model.as_deref(),
            Some("gpt-5.6-luna")
        );
    }

    #[test]
    fn owning_token_and_ignored_records_form_storage_ready_usage_only_commit() {
        let first_json = token_json(10, 10);
        let first = line(
            10,
            &first_json,
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let second = line(
            first.line.end_offset(),
            r#"{"type":"event_msg","payload":{"type":"rate_limits","body":"BODY_SENTINEL"}}"#,
            EnvelopeKind::Ignored,
            RecordOwnership::Owning,
        );
        let observed = second.line.end_offset();
        let result = UsagePipeline::process_chunk(
            plan(PlanAction::ResumeOwningLive, 10, observed),
            [first, second],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![8; 32]),
            false,
            2_000,
        )
        .unwrap();
        let PipelineDisposition::Commit(commit) = result else {
            panic!("expected commit")
        };
        assert_eq!(
            (
                commit.patch.events.len(),
                commit.patch.occurrences.len(),
                commit.evidence_write_count,
                commit.write_unit_count
            ),
            (1, 1, 2, 4)
        );
        assert_eq!(commit.complete_line_count, 2);
        assert_eq!(commit.last_complete_offset, observed);
        assert_eq!(commit.expected_checkpoint.committed_offset, 10);
        assert_eq!(commit.updated_state.resolved_through_offset, observed);
    }

    #[test]
    fn replay_prefix_stops_at_owning_boundary_and_nonzero_replay_or_foreign_rebuilds() {
        let replay = line(
            0,
            &token_json(2, 2),
            EnvelopeKind::TokenCount,
            RecordOwnership::ReplayedAncestor,
        );
        let boundary_start = replay.line.end_offset();
        let boundary = line(
            boundary_start,
            &format!(r#"{{"type":"session_meta","payload":{{"id":"{OWNER}"}}}}"#),
            EnvelopeKind::SessionMeta,
            RecordOwnership::Owning,
        );
        let after = line(
            boundary.line.end_offset(),
            &token_json(5, 5),
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let observed = after.line.end_offset();
        let result = UsagePipeline::process_chunk(
            plan(PlanAction::AwaitOwningMeta, 0, observed),
            [replay, boundary, after],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![8; 32]),
            false,
            1,
        )
        .unwrap();
        let PipelineDisposition::Commit(commit) = result else {
            panic!("expected boundary commit")
        };
        assert_eq!(
            commit.last_complete_offset,
            commit.replayed_prefix_bytes
                + (commit.source_bytes_consumed - commit.replayed_prefix_bytes)
        );
        assert_eq!(
            (
                commit.replayed_prefix_lines,
                commit.complete_line_count,
                commit.write_unit_count
            ),
            (1, 2, 0)
        );
        assert!(!commit.fixed_view_exhausted);

        let late = line(
            10,
            &token_json(3, 3),
            EnvelopeKind::TokenCount,
            RecordOwnership::ReplayedAncestor,
        );
        let observed = late.line.end_offset();
        assert_eq!(
            UsagePipeline::process_chunk(
                plan(PlanAction::ResumeOwningLive, 10, observed),
                [late],
                FixedViewTail {
                    exhausted: true,
                    status: TailStatus::None,
                    half_line_start: None
                },
                Some(vec![1; 32]),
                false,
                1,
            )
            .unwrap(),
            PipelineDisposition::NeedsRebuild
        );
        assert_eq!(
            UsagePipeline::process_chunk(
                plan(PlanAction::ResumeOwningLive, 10, 10),
                std::iter::empty::<ClassifiedUsageLine>(),
                FixedViewTail {
                    exhausted: true,
                    status: TailStatus::None,
                    half_line_start: None
                },
                Some(vec![1; 32]),
                true,
                1,
            )
            .unwrap(),
            PipelineDisposition::NeedsRebuild
        );
    }

    #[test]
    fn exclusive_large_and_oversized_batches_preserve_contract_without_fake_candidates() {
        let start = 10u64;
        let legal_len = 6 * 1024 * 1024usize;
        let mut legal_bytes = vec![b'x'; legal_len - 1];
        legal_bytes.push(b'\n');
        let legal_line = CompleteUsageLine::new(start, legal_bytes).unwrap();
        let legal_end = legal_line.end_offset();
        let legal_item = ClassifiedUsageItem::Line(ClassifiedUsageLine {
            line: legal_line,
            classification: RecordClassification {
                start_offset: start,
                end_offset: legal_end,
                envelope: EnvelopeKind::Malformed,
                ownership: RecordOwnership::Owning,
                response_ownership_mismatch: false,
            },
            decoded: OnceCell::new(),
        });

        let PipelineDisposition::Commit(legal) = UsagePipeline::process_chunk(
            plan(PlanAction::ResumeOwningLive, start, legal_end),
            [legal_item],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![9; 32]),
            false,
            1,
        )
        .unwrap() else {
            panic!("expected legal exclusive commit");
        };
        assert!(legal.source_bytes_consumed > MAX_BATCH_BYTES);
        assert!(legal.source_bytes_consumed <= MAX_LEGAL_LINE_BYTES);
        assert_eq!(legal.complete_line_count, 1);
        assert_eq!(legal.write_unit_count, 0);
        assert!(legal.patch.events.is_empty());
        assert!(legal.patch.occurrences.is_empty());
        assert!(matches!(
            legal.updated_state.processor_state.chain_state,
            crate::codex::ingestion::usage_processor::ChainState::Interrupted(GapKind::Malformed)
        ));

        let oversized_end = start + MAX_LEGAL_LINE_BYTES + 100;
        let oversized_item = ClassifiedUsageItem::Oversized(ClassifiedOversizedUsageLine {
            start_offset: start,
            end_offset: oversized_end,
            classification: RecordClassification {
                start_offset: start,
                end_offset: oversized_end,
                envelope: EnvelopeKind::Malformed,
                ownership: RecordOwnership::Owning,
                response_ownership_mismatch: false,
            },
        });
        let PipelineDisposition::Commit(oversized) = UsagePipeline::process_chunk(
            plan(PlanAction::ResumeOwningLive, start, oversized_end),
            [oversized_item],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![10; 32]),
            false,
            2,
        )
        .unwrap() else {
            panic!("expected oversized-only commit");
        };
        assert!(oversized.source_bytes_consumed > MAX_LEGAL_LINE_BYTES);
        assert_eq!(oversized.complete_line_count, 1);
        assert_eq!(oversized.write_unit_count, 0);
        assert!(oversized.patch.events.is_empty());
        assert!(oversized.patch.occurrences.is_empty());
        assert!(matches!(
            oversized.updated_state.processor_state.chain_state,
            crate::codex::ingestion::usage_processor::ChainState::Interrupted(GapKind::Oversized)
        ));

        let mut seed_processor = UsageProcessor::new(
            UsageContext {
                source_file_id: 9,
                file_generation: 2,
                owning_thread_id: OWNER.to_owned(),
                root_session_id: ROOT.to_owned(),
            },
            UsageSourceState::default(),
            ReconciliationContext::default(),
        );
        assert_eq!(
            seed_processor.try_process_record(
                UsageRecord::TurnStarted {
                    ownership: Ownership::Owning {
                        thread_id: OWNER.to_owned(),
                    },
                    turn_id: Some("open-turn".to_owned()),
                    timestamp_ms: Some(1),
                    start_offset: 0,
                },
                MAX_BATCH_WRITE_UNITS,
            ),
            RecordApplyOutcome::Applied
        );
        seed_processor.observe_consumed_offset(start);
        let mut open_turn_plan = plan(PlanAction::ResumeOwningLive, start, oversized_end);
        open_turn_plan
            .state
            .as_mut()
            .unwrap()
            .processor_state = seed_processor.finish().updated_state;
        let open_turn_item = ClassifiedOversizedUsageLine {
            start_offset: start,
            end_offset: oversized_end,
            classification: RecordClassification {
                start_offset: start,
                end_offset: oversized_end,
                envelope: EnvelopeKind::Malformed,
                ownership: RecordOwnership::Owning,
                response_ownership_mismatch: false,
            },
        };
        let PipelineDisposition::Commit(gap_turn) = UsagePipeline::process_chunk(
            open_turn_plan,
            [open_turn_item],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![11; 32]),
            false,
            3,
        )
        .unwrap() else {
            panic!("expected oversized Gap Turn commit");
        };
        assert_eq!(
            (
                gap_turn.canonical_event_count,
                gap_turn.occurrence_count,
                gap_turn.evidence_write_count,
                gap_turn.write_unit_count,
            ),
            (0, 0, 1, 1)
        );
        assert!(gap_turn.patch.events.is_empty());
        assert!(gap_turn.patch.occurrences.is_empty());
        assert_eq!(gap_turn.patch.turn_upserts.len(), 1);
        assert_eq!(
            gap_turn.updated_state.processor_state.chain_state,
            crate::codex::ingestion::usage_processor::ChainState::Interrupted(
                GapKind::Oversized
            )
        );
        let turn = &gap_turn.patch.turn_upserts[0];
        assert_eq!(
            turn.status,
            crate::codex::ingestion::usage_processor::PersistedTurnStatus::Open
        );
        assert!(turn.state.blocks.parser_gap);
        assert_eq!(turn.quality_status, "partial");
        assert_eq!(turn.state_through_offset, oversized_end);
    }

    #[test]
    fn resumed_state_and_checkpoint_are_usage_local_across_chunk_restart() {
        let first = line(
            10,
            &token_json(10, 10),
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let first_end = first.line.end_offset();
        let PipelineDisposition::Commit(first_commit) = UsagePipeline::process_chunk(
            plan(PlanAction::ResumeOwningLive, 10, first_end),
            [first],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![2; 32]),
            false,
            1,
        )
        .unwrap() else {
            panic!("expected first commit")
        };

        let second = line(
            first_end,
            &token_json(15, 5),
            EnvelopeKind::TokenCount,
            RecordOwnership::Owning,
        );
        let second_end = second.line.end_offset();
        let mut resumed = plan(PlanAction::ResumeOwningLive, first_end, second_end);
        resumed.checkpoint.guard_hash = Some(vec![2; 32]);
        resumed.state = Some(first_commit.updated_state);
        let PipelineDisposition::Commit(second_commit) = UsagePipeline::process_chunk(
            resumed,
            [second],
            FixedViewTail {
                exhausted: true,
                status: TailStatus::None,
                half_line_start: None,
            },
            Some(vec![3; 32]),
            false,
            2,
        )
        .unwrap() else {
            panic!("expected resumed commit")
        };
        assert_eq!(second_commit.patch.events.len(), 1);
        assert_eq!(second_commit.patch.events[0].usage.input_tokens, 5);
        assert_eq!(
            second_commit.expected_checkpoint.committed_offset,
            first_end
        );
        assert_eq!(
            second_commit.updated_state.resolved_through_offset,
            second_end
        );
    }
}
