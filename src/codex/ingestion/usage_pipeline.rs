//! Usage-side orchestration for one fixed rollout-file view.
//!
//! The scanner supplies complete lines together with the ownership decisions
//! made by the metadata parser. This module deliberately owns neither file IO
//! nor SQL transactions: it turns that shared chunk into the single-source DTO
//! consumed by the storage usage commit seam.

use crate::codex::{
    CodexRolloutParser, CompleteUsageLine, EnvelopeKind, LifecycleKind, NormalizedTokenValue,
    OptionalTokenValue, RecordClassification, RecordOwnership, SkillUsageParser, UsageRawRecord,
};

use super::usage_processor::{
    Anomaly, ClosedTurn, GapKind, Occurrence, Ownership, ProcessResult, TurnEndStatus, TurnState,
    UsageContext, UsageEvent, UsageProcessor, UsageRecord, UsageSourceState, UsageValue,
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
pub const MAX_BATCH_CANDIDATES: u64 = 2048;
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
    pub candidate_count: u64,
    pub replayed_prefix_bytes: u64,
    pub replayed_prefix_lines: u64,
    pub fixed_view_exhausted: bool,
    pub tail_status: TailStatus,
    pub tail_start_offset: Option<u64>,
    pub owning_thread_id: String,
    pub root_session_id: String,
    pub events: Vec<UsageEvent>,
    pub occurrences: Vec<Occurrence>,
    pub skill_events: Vec<SkillUsageEvent>,
    pub closed_turns: Vec<ClosedTurn>,
    pub open_turn: Option<TurnState>,
    pub anomalies: Vec<Anomaly>,
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
    AwaitingOwningMeta,
    Skip,
    BlockedRelationship,
    NeedsRebuild,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineError {
    InvalidPlan,
    InvalidTail,
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
        let adapter = CodexRolloutParser;
        let mut state = original_state;
        let mut active_model_offset = plan
            .state
            .as_ref()
            .and_then(|state| state.active_model_offset);
        let mut active_reasoning_effort_offset = plan
            .state
            .as_ref()
            .and_then(|state| state.active_reasoning_effort_offset);
        let mut events = Vec::new();
        let mut occurrences = Vec::new();
        let mut skill_events = Vec::new();
        let mut anomalies = Vec::new();
        let mut closed_turns = Vec::new();
        let mut complete_line_count = 0u64;
        let mut last_complete_offset = plan.start_offset;

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
            collect_skill_events(&item, &state, &context, &mut skill_events);
            let record = match &item {
                ClassifiedUsageItem::Line(value) => adapter.parse_line(&value.line),
                ClassifiedUsageItem::Oversized(_) => UsageRawRecord::OversizedComplete {
                    start_offset: start,
                    end_offset: end,
                },
            };
            let Some(record) = normalized_record(record, &owning_thread_id, start, end) else {
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
            let result =
                UsageProcessor::new(context.clone(), Some(state.clone())).process([record]);
            if result.needs_rebuild {
                return Ok(PipelineDisposition::NeedsRebuild);
            }
            if occurrences.len() + result.occurrences.len() > MAX_BATCH_CANDIDATES as usize {
                break;
            }
            state = result.updated_state;
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
            events.extend(result.events);
            occurrences.extend(result.occurrences);
            anomalies.extend(result.anomalies);
            closed_turns.extend(result.closed_turns);
            if oversized {
                break;
            }
        }

        let result = ProcessResult {
            events,
            occurrences,
            anomalies,
            closed_turns,
            updated_state: state,
            needs_rebuild: false,
        };
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
        )))
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
    let adapter = CodexRolloutParser;
    let mut state = UsageSourceState::default();
    let mut active_model_offset = None;
    let mut active_reasoning_effort_offset = None;
    let mut events = Vec::new();
    let mut occurrences = Vec::new();
    let mut skill_events = Vec::new();
    let mut anomalies = Vec::new();
    let mut closed_turns = Vec::new();
    let mut last = plan.read_start_offset;
    let mut replayed_bytes = plan.replayed_prefix_bytes_before_chunk;
    let mut replayed_lines = plan.replayed_prefix_lines_before_chunk;
    let mut complete_line_count = replayed_lines;
    let mut ownership_established = false;
    let mut continuation_state = SourceContinuationState::OwningLive;

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
        collect_skill_events(&item, &state, &context, &mut skill_events);
        let raw = match &item {
            ClassifiedUsageItem::Line(value) => adapter.parse_line(&value.line),
            ClassifiedUsageItem::Oversized(_) => UsageRawRecord::OversizedComplete {
                start_offset: start,
                end_offset: end,
            },
        };
        if let Some(record) = normalized_record(raw, owning_thread_id, start, end) {
            let context_record = match &record {
                UsageRecord::TurnContext {
                    model,
                    reasoning_effort,
                    ..
                } => Some((model.is_some(), reasoning_effort.is_some())),
                _ => None,
            };
            let processed =
                UsageProcessor::new(context.clone(), Some(state.clone())).process([record]);
            if processed.needs_rebuild
                || occurrences.len() + processed.occurrences.len() > MAX_BATCH_CANDIDATES as usize
            {
                return Ok(PipelineDisposition::NeedsRebuild);
            }
            state = processed.updated_state;
            if let Some((has_model, has_effort)) = context_record {
                if has_model {
                    active_model_offset = Some(start);
                }
                active_reasoning_effort_offset = has_effort.then_some(start);
            }
            events.extend(processed.events);
            occurrences.extend(processed.occurrences);
            anomalies.extend(processed.anomalies);
            closed_turns.extend(processed.closed_turns);
        }
        complete_line_count = complete_line_count.saturating_add(1);
        last = end;

        // Preserve the historical empty ownership-boundary commit for normal
        // sources. The extended path is entered only for metadata-proven replay EOF.
        if !plan.allow_replay_tail {
            return Ok(PipelineDisposition::Commit(commit_dto(
                plan,
                owning_thread_id.to_owned(),
                root_session_id.to_owned(),
                ProcessResult {
                    events,
                    occurrences,
                    anomalies,
                    closed_turns,
                    updated_state: state,
                    needs_rebuild: false,
                },
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
            )));
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
    Ok(PipelineDisposition::Commit(commit_dto(
        plan,
        owning_thread_id.to_owned(),
        root_session_id.to_owned(),
        ProcessResult {
            events,
            occurrences,
            anomalies,
            closed_turns,
            updated_state: state,
            needs_rebuild: false,
        },
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
    )))
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
    let adapter = CodexRolloutParser;
    let mut state = UsageSourceState::default();
    let mut active_model_offset = None;
    let mut active_reasoning_effort_offset = None;
    let mut events = Vec::new();
    let mut occurrences = Vec::new();
    let mut skill_events = Vec::new();
    let mut anomalies = Vec::new();
    let mut closed_turns = Vec::new();
    let mut last = plan.read_start_offset;
    let mut replayed_bytes = plan.replayed_prefix_bytes_before_chunk;
    let mut replayed_lines = plan.replayed_prefix_lines_before_chunk;
    let mut adapter_lines = 0u64;
    let mut adapter_bytes = 0u64;
    let mut ownership_established = false;
    let mut continuation_state = SourceContinuationState::OwningLive;

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
        collect_skill_events(&item, &state, &context, &mut skill_events);
        let raw = match &item {
            ClassifiedUsageItem::Line(value) => adapter.parse_line(&value.line),
            ClassifiedUsageItem::Oversized(_) => UsageRawRecord::OversizedComplete {
                start_offset: start,
                end_offset: end,
            },
        };
        let record = normalized_record(raw, owning_thread_id, start, end);
        if let Some(record) = record {
            let context_record = match &record {
                UsageRecord::TurnContext {
                    model,
                    reasoning_effort,
                    ..
                } => Some((model.is_some(), reasoning_effort.is_some())),
                _ => None,
            };
            let processed =
                UsageProcessor::new(context.clone(), Some(state.clone())).process([record]);
            if processed.needs_rebuild
                || occurrences.len() + processed.occurrences.len() > MAX_BATCH_CANDIDATES as usize
            {
                return Ok(PipelineDisposition::NeedsRebuild);
            }
            state = processed.updated_state;
            if let Some((has_model, has_effort)) = context_record {
                if has_model {
                    active_model_offset = Some(start);
                }
                active_reasoning_effort_offset = has_effort.then_some(start);
            }
            events.extend(processed.events);
            occurrences.extend(processed.occurrences);
            anomalies.extend(processed.anomalies);
            closed_turns.extend(processed.closed_turns);
        }
        adapter_lines += 1;
        adapter_bytes += bytes;
        last = end;
        if oversized && lines.peek().is_some() {
            return Ok(PipelineDisposition::NeedsRebuild);
        }
    }

    if !ownership_established || !tail.exhausted {
        return Ok(PipelineDisposition::NeedsRebuild);
    }
    validate_completed_tail(last, plan.fixed_observed_size, tail)?;
    let result = ProcessResult {
        events,
        occurrences,
        anomalies,
        closed_turns,
        updated_state: state,
        needs_rebuild: false,
    };
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
        tail,
        next_guard_hash,
        committed_at_ms,
        active_model_offset,
        active_reasoning_effort_offset,
        continuation_state,
    )))
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
        }),
        UsageRawRecord::OversizedComplete { .. } => Some(UsageRecord::Gap {
            ownership: ownership(),
            kind: GapKind::Oversized,
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
) -> UsageSourceCommitDto {
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
    UsageSourceCommitDto {
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
        source_bytes_consumed: last_complete_offset - plan.start_offset,
        complete_line_count,
        candidate_count: result.occurrences.len() as u64,
        replayed_prefix_bytes,
        replayed_prefix_lines,
        fixed_view_exhausted: tail.exhausted,
        tail_status: tail.status,
        tail_start_offset: tail.half_line_start,
        owning_thread_id,
        root_session_id,
        events: result.events,
        occurrences: result.occurrences,
        skill_events,
        closed_turns: result.closed_turns,
        open_turn: result.updated_state.open_turn,
        anomalies: result.anomalies,
        updated_state,
        next_guard_hash,
        committed_at_ms,
    }
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

    const OWNER: &str = "01981111-1111-7111-8111-111111111111";
    const ROOT: &str = "01981111-1111-7111-8111-111111111111";

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
        };
        ClassifiedUsageLine {
            line,
            classification,
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

        assert!(boundary_commit.events.is_empty());
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

        assert_eq!(token_commit.events.len(), 1);
        assert_eq!(token_commit.events[0].model, "gpt-5.6-sol");
        assert_eq!(
            token_commit.events[0].reasoning_effort.as_deref(),
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
        assert!(session_commit.events.is_empty());
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
        assert!(session_commit.events.is_empty());

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

        assert_eq!(commit.events.len(), 2);
        assert_eq!(commit.events[0].model, "unknown");
        assert_eq!(commit.events[0].reasoning_effort, None);
        assert_eq!(commit.events[1].model, "gpt-5.6-luna");
        assert_eq!(commit.events[1].reasoning_effort.as_deref(), Some("low"));
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
                commit.events.len(),
                commit.occurrences.len(),
                commit.candidate_count
            ),
            (1, 1, 1)
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
                commit.candidate_count
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
            },
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
        assert_eq!(legal.candidate_count, 0);
        assert!(legal.events.is_empty());
        assert!(legal.occurrences.is_empty());
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
        assert_eq!(oversized.candidate_count, 0);
        assert!(oversized.events.is_empty());
        assert!(oversized.occurrences.is_empty());
        assert!(matches!(
            oversized.updated_state.processor_state.chain_state,
            crate::codex::ingestion::usage_processor::ChainState::Interrupted(GapKind::Oversized)
        ));
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
        assert_eq!(second_commit.events.len(), 1);
        assert_eq!(second_commit.events[0].usage.input_tokens, 5);
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
