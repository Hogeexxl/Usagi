//! Pure usage ingestion state machine.
//!
//! This module deliberately has no scanner or storage dependencies. Its input
//! is the normalized, ownership-classified record stream; its output is a set
//! of deterministic event/occurrence proposals plus restartable source/Turn
//! state. SQL commit, epoch management, aggregation, and rollout decoding live
//! at later integration seams.

use std::fmt;

pub use crate::usage::normalized::NormalizedTokenUsage;
use crate::{domain::DomainError, usage::event::EventKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageValue {
    Missing,
    Invalid,
    Valid(NormalizedTokenUsage),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ownership {
    Owning { thread_id: String },
    ReplayedAncestor,
    UnknownOwnership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEndStatus {
    Completed,
    Aborted,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GapKind {
    Malformed,
    Oversized,
    Ownership,
    Parser,
    RequiredInvalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageRecord {
    TurnContext {
        ownership: Ownership,
        model: Option<String>,
        reasoning_effort: Option<String>,
    },
    TurnStarted {
        ownership: Ownership,
        turn_id: Option<String>,
        timestamp_ms: Option<i64>,
        start_offset: u64,
    },
    TokenCount {
        ownership: Ownership,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        total: UsageValue,
        last: UsageValue,
    },
    TurnEnded {
        ownership: Ownership,
        turn_id: Option<String>,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        status: TurnEndStatus,
    },
    Gap {
        ownership: Ownership,
        kind: GapKind,
    },
}

impl UsageRecord {
    fn ownership(&self) -> &Ownership {
        match self {
            Self::TurnContext { ownership, .. }
            | Self::TurnStarted { ownership, .. }
            | Self::TokenCount { ownership, .. }
            | Self::TurnEnded { ownership, .. }
            | Self::Gap { ownership, .. } => ownership,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainState {
    Continuous,
    Interrupted(GapKind),
}

struct CandidateInput {
    kind: EventKind,
    occurred_at_ms: i64,
    start_offset: u64,
    end_offset: u64,
    usage: NormalizedTokenUsage,
    previous_total: Option<NormalizedTokenUsage>,
    current_total: NormalizedTokenUsage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageEvent {
    pub event_id: String,
    pub kind: EventKind,
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub turn_key: Option<String>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub usage: NormalizedTokenUsage,
    pub previous_total: Option<NormalizedTokenUsage>,
    pub current_total: NormalizedTokenUsage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
    pub source_end_offset: u64,
    pub event_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnomalyCode {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anomaly {
    pub code: AnomalyCode,
    pub source_start_offset: Option<u64>,
    pub turn_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompensationBlocks {
    pub start_missing: bool,
    pub time_missing: bool,
    pub reset: bool,
    pub ownership_gap: bool,
    pub parser_gap: bool,
    pub required_invalid: bool,
    pub model_unresolved: bool,
}

impl CompensationBlocks {
    pub fn allowed(self) -> bool {
        self == Self::default()
    }

    fn observe_gap(&mut self, kind: GapKind) {
        match kind {
            GapKind::Ownership => self.ownership_gap = true,
            GapKind::Parser | GapKind::Malformed | GapKind::Oversized => self.parser_gap = true,
            GapKind::RequiredInvalid => self.required_invalid = true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnModelState {
    None,
    Single(String),
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnReasoningEffortState {
    None,
    Single(String),
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnState {
    pub turn_key: String,
    pub raw_turn_id: Option<String>,
    pub started_at_ms: Option<i64>,
    pub start_offset: u64,
    pub start_total: Option<NormalizedTokenUsage>,
    pub last_total: Option<NormalizedTokenUsage>,
    pub accounted: NormalizedTokenUsage,
    pub accounted_candidate_count: u64,
    pub model_state: TurnModelState,
    pub unresolved_model_seen: bool,
    pub reasoning_effort_state: TurnReasoningEffortState,
    pub unresolved_reasoning_effort_seen: bool,
    pub blocks: CompensationBlocks,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedTurn {
    pub turn: TurnState,
    pub ended_at_ms: Option<i64>,
    pub end_offset: u64,
    pub status: TurnEndStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageSourceState {
    pub chain_state: ChainState,
    pub previous_total: Option<NormalizedTokenUsage>,
    pub previous_total_offset: Option<u64>,
    pub active_model: Option<String>,
    pub active_reasoning_effort: Option<String>,
    pub open_turn: Option<TurnState>,
}

impl Default for UsageSourceState {
    fn default() -> Self {
        Self {
            chain_state: ChainState::Continuous,
            previous_total: None,
            previous_total_offset: None,
            active_model: None,
            active_reasoning_effort: None,
            open_turn: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageContext {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub owning_thread_id: String,
    pub root_session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessResult {
    pub events: Vec<UsageEvent>,
    pub occurrences: Vec<Occurrence>,
    pub anomalies: Vec<Anomaly>,
    pub closed_turns: Vec<ClosedTurn>,
    pub updated_state: UsageSourceState,
    pub needs_rebuild: bool,
}

pub struct UsageProcessor {
    context: UsageContext,
    state: UsageSourceState,
    events: Vec<UsageEvent>,
    occurrences: Vec<Occurrence>,
    anomalies: Vec<Anomaly>,
    closed_turns: Vec<ClosedTurn>,
}

impl UsageProcessor {
    pub fn new(context: UsageContext, existing_state: Option<UsageSourceState>) -> Self {
        Self {
            context,
            state: existing_state.unwrap_or_default(),
            events: Vec::new(),
            occurrences: Vec::new(),
            anomalies: Vec::new(),
            closed_turns: Vec::new(),
        }
    }

    pub fn process(mut self, records: impl IntoIterator<Item = UsageRecord>) -> ProcessResult {
        let original = self.state.clone();
        for record in records {
            match record.ownership() {
                Ownership::ReplayedAncestor => continue,
                Ownership::UnknownOwnership => return rebuild(original),
                Ownership::Owning { thread_id } if thread_id != &self.context.owning_thread_id => {
                    return rebuild(original);
                }
                Ownership::Owning { .. } => {}
            }
            if self.apply(record).is_err() {
                self.anomaly(AnomalyCode::ArithmeticOverflow, None);
                self.block_required();
            }
        }
        ProcessResult {
            events: self.events,
            occurrences: self.occurrences,
            anomalies: self.anomalies,
            closed_turns: self.closed_turns,
            updated_state: self.state,
            needs_rebuild: false,
        }
    }

    fn apply(&mut self, record: UsageRecord) -> Result<(), ProcessorError> {
        match record {
            UsageRecord::TurnContext {
                model,
                reasoning_effort,
                ..
            } => {
                if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
                    self.state.active_model = Some(model);
                }
                // An owning turn_context is the boundary for both context
                // dimensions.  A missing effort explicitly clears the
                // previous turn's value; it is never inherited implicitly.
                self.state.active_reasoning_effort = reasoning_effort;
            }
            UsageRecord::TurnStarted {
                turn_id,
                timestamp_ms,
                start_offset,
                ..
            } => self.start_turn(turn_id, timestamp_ms, start_offset),
            UsageRecord::TokenCount {
                timestamp_ms,
                start_offset,
                end_offset,
                total,
                last,
                ..
            } => self.token_count(timestamp_ms, start_offset, end_offset, total, last)?,
            UsageRecord::TurnEnded {
                turn_id,
                timestamp_ms,
                start_offset,
                end_offset,
                status,
                ..
            } => self.end_turn(turn_id, timestamp_ms, start_offset, end_offset, status)?,
            UsageRecord::Gap { kind, .. } => self.gap(kind),
        }
        Ok(())
    }

    fn start_turn(&mut self, turn_id: Option<String>, timestamp_ms: Option<i64>, offset: u64) {
        if let Some(mut old) = self.state.open_turn.take() {
            old.blocks.required_invalid = true;
            self.anomaly(AnomalyCode::TurnReplaced, Some(offset));
            self.closed_turns.push(ClosedTurn {
                turn: old,
                ended_at_ms: timestamp_ms,
                end_offset: offset,
                status: TurnEndStatus::Aborted,
            });
        }
        let key = turn_id.clone().unwrap_or_else(|| {
            synthetic_turn_key(&self.context.owning_thread_id, offset, timestamp_ms)
        });
        let mut blocks = CompensationBlocks::default();
        if timestamp_ms.is_none() {
            blocks.time_missing = true;
        }
        let start_total = if self.state.chain_state == ChainState::Continuous {
            self.state.previous_total.clone()
        } else {
            if let ChainState::Interrupted(kind) = self.state.chain_state {
                blocks.observe_gap(kind);
            }
            None
        };
        if start_total.is_none() {
            blocks.start_missing = true;
        }
        self.state.open_turn = Some(TurnState {
            turn_key: key,
            raw_turn_id: turn_id,
            started_at_ms: timestamp_ms,
            start_offset: offset,
            start_total,
            last_total: None,
            accounted: NormalizedTokenUsage::zero(),
            accounted_candidate_count: 0,
            model_state: TurnModelState::None,
            unresolved_model_seen: false,
            reasoning_effort_state: TurnReasoningEffortState::None,
            unresolved_reasoning_effort_seen: false,
            blocks,
        });
    }

    fn gap(&mut self, kind: GapKind) {
        self.state.chain_state = ChainState::Interrupted(kind);
        if let Some(turn) = &mut self.state.open_turn {
            turn.blocks.observe_gap(kind);
        }
    }

    fn token_count(
        &mut self,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        total: UsageValue,
        last: UsageValue,
    ) -> Result<(), ProcessorError> {
        // A subagent rollout can begin with cumulative snapshots copied from
        // its parent before its first owning turn_context.  Those snapshots
        // are initialization telemetry, not usage owned by this rollout, but
        // their valid cumulative total remains the baseline for later deltas.
        if self.context.owning_thread_id != self.context.root_session_id
            && self.state.active_model.is_none()
        {
            if let UsageValue::Valid(current) = total {
                self.set_baseline(current, end_offset);
            }
            return Ok(());
        }
        let current = match total {
            UsageValue::Valid(value) => value,
            UsageValue::Missing | UsageValue::Invalid => {
                self.anomaly(AnomalyCode::RequiredTotalInvalid, Some(start_offset));
                self.gap(GapKind::RequiredInvalid);
                return Ok(());
            }
        };

        if timestamp_ms.is_none() {
            self.anomaly(AnomalyCode::UsageTimeMissing, Some(start_offset));
            if last == UsageValue::Invalid {
                self.anomaly(AnomalyCode::LastUsageInvalid, Some(start_offset));
                self.block_required();
            }
            if let Some(turn) = &mut self.state.open_turn {
                turn.blocks.time_missing = true;
                turn.last_total = Some(current.clone());
            }
            self.set_baseline(current, end_offset);
            return Ok(());
        }

        if matches!(self.state.chain_state, ChainState::Interrupted(_)) {
            self.set_baseline(current, end_offset);
            return Ok(());
        }

        let previous = self.state.previous_total.clone();
        let required_reset = previous
            .as_ref()
            .is_some_and(|old| required_decreased_from(&current, old));
        let cache_reset = previous
            .as_ref()
            .is_some_and(|old| cache_decreased_from(&current, old));
        if required_reset || cache_reset {
            if required_reset {
                self.anomaly(AnomalyCode::TotalChainReset, Some(start_offset));
            }
            if cache_reset {
                self.anomaly(AnomalyCode::CacheWriteChainDecrease, Some(start_offset));
            }
            if let Some(turn) = &mut self.state.open_turn {
                turn.blocks.reset = true;
            }
            if let UsageValue::Valid(usage) = last {
                self.emit_candidate(CandidateInput {
                    kind: EventKind::Normal,
                    occurred_at_ms: timestamp_ms.unwrap(),
                    start_offset,
                    end_offset,
                    usage,
                    previous_total: previous.clone(),
                    current_total: current.clone(),
                })?;
            } else if last == UsageValue::Invalid {
                self.anomaly(AnomalyCode::LastUsageInvalid, Some(start_offset));
                self.block_required();
            }
            self.set_baseline(current, end_offset);
            return Ok(());
        }

        if previous.as_ref() == Some(&current) {
            // A duplicate cumulative snapshot emits no candidate, but the
            // trusted boundary itself is newer and must become the durable
            // baseline/Turn end snapshot for restart and compensation checks.
            self.set_baseline(current, end_offset);
            return Ok(());
        }

        match last {
            UsageValue::Valid(usage) => self.emit_candidate(CandidateInput {
                kind: EventKind::Normal,
                occurred_at_ms: timestamp_ms.unwrap(),
                start_offset,
                end_offset,
                usage,
                previous_total: previous.clone(),
                current_total: current.clone(),
            })?,
            UsageValue::Missing => {
                if let Some(old) = &previous {
                    let usage = processor_checked_sub(&current, old)?;
                    self.emit_candidate(CandidateInput {
                        kind: EventKind::Recovered,
                        occurred_at_ms: timestamp_ms.unwrap(),
                        start_offset,
                        end_offset,
                        usage,
                        previous_total: previous.clone(),
                        current_total: current.clone(),
                    })?;
                }
            }
            UsageValue::Invalid => {
                self.anomaly(AnomalyCode::LastUsageInvalid, Some(start_offset));
                self.block_required();
            }
        }
        self.set_baseline(current, end_offset);
        Ok(())
    }

    fn emit_candidate(&mut self, input: CandidateInput) -> Result<(), ProcessorError> {
        let CandidateInput {
            kind,
            occurred_at_ms,
            start_offset,
            end_offset,
            usage,
            previous_total,
            current_total,
        } = input;
        let model = self
            .state
            .active_model
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let reasoning_effort = self.state.active_reasoning_effort.clone();
        let turn_key = self
            .state
            .open_turn
            .as_ref()
            .map(|turn| turn.turn_key.clone());
        let mut event = UsageEvent {
            event_id: String::new(),
            kind,
            occurred_at_ms,
            thread_id: self.context.owning_thread_id.clone(),
            root_session_id: self.context.root_session_id.clone(),
            turn_key,
            model: model.clone(),
            reasoning_effort: reasoning_effort.clone(),
            usage: usage.clone(),
            previous_total,
            current_total,
        };
        event.event_id = event_id(&event);
        if let Some(turn) = &mut self.state.open_turn {
            observe_turn_model(turn, &model);
            observe_turn_reasoning_effort(turn, reasoning_effort.as_deref());
            if add_accounted(turn, &usage).is_err() {
                turn.blocks.required_invalid = true;
                self.anomalies.push(Anomaly {
                    code: AnomalyCode::ArithmeticOverflow,
                    source_start_offset: Some(start_offset),
                    turn_key: Some(turn.turn_key.clone()),
                });
            }
            turn.last_total = Some(event.current_total.clone());
        }
        self.occurrences.push(Occurrence {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: start_offset,
            source_end_offset: end_offset,
            event_id: event.event_id.clone(),
        });
        self.events.push(event);
        Ok(())
    }

    fn end_turn(
        &mut self,
        turn_id: Option<String>,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        status: TurnEndStatus,
    ) -> Result<(), ProcessorError> {
        let Some(mut turn) = self.state.open_turn.take() else {
            return Ok(());
        };
        if turn_id.is_some() && turn.raw_turn_id != turn_id {
            self.anomaly(AnomalyCode::TurnIdMismatch, Some(start_offset));
            self.state.open_turn = Some(turn);
            return Ok(());
        }
        if timestamp_ms.is_none() {
            turn.blocks.time_missing = true;
        }
        if turn.unresolved_model_seen {
            turn.blocks.model_unresolved = true;
        }
        if turn.blocks.allowed()
            && let (Some(start), Some(end), Some(ended_at)) = (
                turn.start_total.clone(),
                turn.last_total.clone(),
                timestamp_ms,
            )
        {
            self.compensate(&mut turn, &start, &end, ended_at, start_offset, end_offset)?;
        }
        self.closed_turns.push(ClosedTurn {
            turn,
            ended_at_ms: timestamp_ms,
            end_offset,
            status,
        });
        Ok(())
    }

    fn compensate(
        &mut self,
        turn: &mut TurnState,
        start: &NormalizedTokenUsage,
        end: &NormalizedTokenUsage,
        occurred_at_ms: i64,
        start_offset: u64,
        end_offset: u64,
    ) -> Result<(), ProcessorError> {
        let delta = match processor_checked_sub(end, start) {
            Ok(delta) => delta,
            Err(ProcessorError::NegativeDifference) => {
                self.anomaly(AnomalyCode::TotalChainReset, Some(start_offset));
                return Ok(());
            }
            Err(ProcessorError::CacheWriteNegativeDifference) => {
                self.anomaly(AnomalyCode::TurnCacheWriteDeltaNegative, Some(start_offset));
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let missing = match processor_checked_sub(&delta, &turn.accounted) {
            Ok(missing) => missing,
            Err(ProcessorError::NegativeDifference) => {
                self.anomaly(AnomalyCode::TurnAccountedExceedsTotal, Some(start_offset));
                return Ok(());
            }
            Err(ProcessorError::CacheWriteNegativeDifference) => {
                self.anomaly(AnomalyCode::TurnAccountedExceedsTotal, Some(start_offset));
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if required_is_zero(&missing) {
            return Ok(());
        }
        let model = match &turn.model_state {
            TurnModelState::Single(model) => model.clone(),
            TurnModelState::Mixed => "unknown".to_owned(),
            TurnModelState::None => return Ok(()),
        };
        let reasoning_effort = if turn.unresolved_reasoning_effort_seen {
            None
        } else {
            match &turn.reasoning_effort_state {
                TurnReasoningEffortState::Single(effort) => Some(effort.clone()),
                TurnReasoningEffortState::None | TurnReasoningEffortState::Mixed => None,
            }
        };
        let previous_total = Some(start.clone());
        let mut event = UsageEvent {
            event_id: String::new(),
            kind: EventKind::TurnCompensation,
            occurred_at_ms,
            thread_id: self.context.owning_thread_id.clone(),
            root_session_id: self.context.root_session_id.clone(),
            turn_key: Some(turn.turn_key.clone()),
            model,
            reasoning_effort,
            usage: missing.clone(),
            previous_total,
            current_total: end.clone(),
        };
        event.event_id = event_id(&event);
        if add_accounted(turn, &missing).is_err() {
            turn.blocks.required_invalid = true;
            self.anomalies.push(Anomaly {
                code: AnomalyCode::ArithmeticOverflow,
                source_start_offset: Some(start_offset),
                turn_key: Some(turn.turn_key.clone()),
            });
            return Ok(());
        }
        self.occurrences.push(Occurrence {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: start_offset,
            source_end_offset: end_offset,
            event_id: event.event_id.clone(),
        });
        self.events.push(event);
        Ok(())
    }

    fn set_baseline(&mut self, total: NormalizedTokenUsage, offset: u64) {
        if let Some(turn) = &mut self.state.open_turn {
            turn.last_total = Some(total.clone());
        }
        self.state.previous_total = Some(total);
        self.state.previous_total_offset = Some(offset);
        self.state.chain_state = ChainState::Continuous;
    }

    fn block_required(&mut self) {
        if let Some(turn) = &mut self.state.open_turn {
            turn.blocks.required_invalid = true;
        }
    }

    fn anomaly(&mut self, code: AnomalyCode, offset: Option<u64>) {
        self.anomalies.push(Anomaly {
            code,
            source_start_offset: offset,
            turn_key: self
                .state
                .open_turn
                .as_ref()
                .map(|turn| turn.turn_key.clone()),
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalDecision {
    Insert,
    Duplicate,
    Conflict,
}

pub fn compare_canonical(
    existing: Option<&UsageEvent>,
    incoming: &UsageEvent,
) -> CanonicalDecision {
    match existing {
        None => CanonicalDecision::Insert,
        Some(existing) if existing == incoming => CanonicalDecision::Duplicate,
        Some(_) => CanonicalDecision::Conflict,
    }
}

pub fn compare_occurrence(
    existing: Option<&Occurrence>,
    incoming: &Occurrence,
) -> CanonicalDecision {
    match existing {
        None => CanonicalDecision::Insert,
        Some(existing)
            if existing.event_id == incoming.event_id
                && existing.source_end_offset == incoming.source_end_offset =>
        {
            CanonicalDecision::Duplicate
        }
        Some(_) => CanonicalDecision::Conflict,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessorError {
    InvalidNormalizedTokenUsage,
    ArithmeticOverflow,
    NegativeDifference,
    CacheWriteNegativeDifference,
}

impl From<DomainError> for ProcessorError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::InvalidValue { reason, .. } if reason.contains("cache-write delta") => {
                ProcessorError::CacheWriteNegativeDifference
            }
            DomainError::InvalidValue { reason, .. }
                if reason.contains("negative delta")
                    || reason.contains("delta must not be negative") =>
            {
                ProcessorError::NegativeDifference
            }
            DomainError::InvalidValue { reason, .. } if reason.contains("overflow") => {
                ProcessorError::ArithmeticOverflow
            }
            _ => ProcessorError::InvalidNormalizedTokenUsage,
        }
    }
}

impl fmt::Display for ProcessorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ProcessorError {}

fn rebuild(state: UsageSourceState) -> ProcessResult {
    ProcessResult {
        events: Vec::new(),
        occurrences: Vec::new(),
        anomalies: Vec::new(),
        closed_turns: Vec::new(),
        updated_state: state,
        needs_rebuild: true,
    }
}

fn processor_checked_add(
    left: &NormalizedTokenUsage,
    right: &NormalizedTokenUsage,
) -> Result<NormalizedTokenUsage, ProcessorError> {
    left.checked_add(right).map_err(ProcessorError::from)
}

fn processor_checked_sub(
    current: &NormalizedTokenUsage,
    previous: &NormalizedTokenUsage,
) -> Result<NormalizedTokenUsage, ProcessorError> {
    current.checked_sub(previous).map_err(ProcessorError::from)
}

fn required_decreased_from(
    current: &NormalizedTokenUsage,
    previous: &NormalizedTokenUsage,
) -> bool {
    current.input_tokens < previous.input_tokens
        || current.cached_tokens < previous.cached_tokens
        || current.output_tokens < previous.output_tokens
        || current.reasoning_tokens < previous.reasoning_tokens
}

fn required_is_zero(value: &NormalizedTokenUsage) -> bool {
    value.input_tokens == 0
        && value.cached_tokens == 0
        && value.output_tokens == 0
        && value.reasoning_tokens == 0
}

fn cache_decreased_from(current: &NormalizedTokenUsage, previous: &NormalizedTokenUsage) -> bool {
    matches!(
        (
            current.cache_write_tokens,
            previous.cache_write_tokens
        ),
        (Some(current), Some(previous)) if current < previous
    )
}

fn observe_turn_model(turn: &mut TurnState, model: &str) {
    if model == "unknown" {
        turn.unresolved_model_seen = true;
        turn.blocks.model_unresolved = true;
        return;
    }
    turn.model_state = match &turn.model_state {
        TurnModelState::None => TurnModelState::Single(model.to_owned()),
        TurnModelState::Single(existing) if existing == model => turn.model_state.clone(),
        TurnModelState::Single(_) | TurnModelState::Mixed => TurnModelState::Mixed,
    };
}

fn observe_turn_reasoning_effort(turn: &mut TurnState, effort: Option<&str>) {
    let Some(effort) = effort else {
        turn.unresolved_reasoning_effort_seen = true;
        return;
    };
    turn.reasoning_effort_state = match &turn.reasoning_effort_state {
        TurnReasoningEffortState::None => TurnReasoningEffortState::Single(effort.to_owned()),
        TurnReasoningEffortState::Single(existing) if existing == effort => {
            turn.reasoning_effort_state.clone()
        }
        TurnReasoningEffortState::Single(_) | TurnReasoningEffortState::Mixed => {
            TurnReasoningEffortState::Mixed
        }
    };
}

fn add_accounted(turn: &mut TurnState, usage: &NormalizedTokenUsage) -> Result<(), ProcessorError> {
    let next_accounted = if turn.accounted_candidate_count == 0 {
        usage.clone()
    } else {
        processor_checked_add(&turn.accounted, usage)?
    };
    let next_count = turn
        .accounted_candidate_count
        .checked_add(1)
        .ok_or(ProcessorError::ArithmeticOverflow)?;
    turn.accounted = next_accounted;
    turn.accounted_candidate_count = next_count;
    Ok(())
}

fn synthetic_turn_key(thread_id: &str, start_offset: u64, timestamp_ms: Option<i64>) -> String {
    let mut encoder = Encoder::new(b"synthetic-turn-v1");
    encoder.text(thread_id);
    encoder.u64(start_offset);
    match timestamp_ms {
        Some(value) => {
            encoder.byte(1);
            encoder.i64(value);
        }
        None => encoder.byte(0),
    }
    encoder.finish()
}

fn event_id(event: &UsageEvent) -> String {
    let mut encoder = Encoder::new(b"usage-event-v2");
    encoder.text(&event.thread_id);
    encoder.optional_text(event.turn_key.as_deref());
    encoder.byte(match event.kind {
        EventKind::Normal => 0,
        EventKind::Recovered => 1,
        EventKind::TurnCompensation => 2,
    });
    encoder.i64(event.occurred_at_ms);
    encoder.optional_fingerprint(event.previous_total.as_ref());
    encoder.fingerprint(&event.current_total);
    encoder.vector(&event.usage);
    encoder.text(&event.model);
    encoder.optional_text(event.reasoning_effort.as_deref());
    encoder.finish()
}

struct Encoder(blake3::Hasher);

impl Encoder {
    fn new(tag: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(tag.len() as u64).to_be_bytes());
        hasher.update(tag);
        Self(hasher)
    }

    fn byte(&mut self, value: u8) {
        self.0.update(&[value]);
    }

    fn u64(&mut self, value: u64) {
        self.0.update(&value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
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

    fn optional_fingerprint(&mut self, value: Option<&NormalizedTokenUsage>) {
        match value {
            Some(value) => {
                self.byte(1);
                self.fingerprint(value);
            }
            None => self.byte(0),
        }
    }

    fn fingerprint(&mut self, value: &NormalizedTokenUsage) {
        self.0
            .update(&crate::codex::normalization::usage_fingerprint(value));
    }

    fn vector(&mut self, value: &NormalizedTokenUsage) {
        self.i64(value.input_tokens);
        self.i64(value.cached_tokens);
        match value.cache_write_tokens {
            Some(cache_write) => {
                self.byte(1);
                self.i64(cache_write);
            }
            None => self.byte(0),
        }
        self.i64(value.output_tokens);
        self.i64(value.reasoning_tokens);
        self.i64(value.total_tokens);
    }

    fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn context(source_file_id: i64) -> UsageContext {
        UsageContext {
            source_file_id,
            file_generation: 1,
            owning_thread_id: "thread".to_owned(),
            root_session_id: "thread".to_owned(),
        }
    }

    fn owning() -> Ownership {
        Ownership::Owning {
            thread_id: "thread".to_owned(),
        }
    }

    fn known(
        input: i64,
        cached: i64,
        write: i64,
        output: i64,
        reasoning: i64,
    ) -> NormalizedTokenUsage {
        NormalizedTokenUsage::new(
            input,
            cached,
            Some(write),
            output,
            reasoning,
            input + output,
        )
        .unwrap()
    }

    fn unknown(input: i64, cached: i64, output: i64, reasoning: i64) -> NormalizedTokenUsage {
        NormalizedTokenUsage::new(input, cached, None, output, reasoning, input + output).unwrap()
    }

    fn token(at: i64, offset: u64, total: NormalizedTokenUsage, last: UsageValue) -> UsageRecord {
        UsageRecord::TokenCount {
            ownership: owning(),
            timestamp_ms: Some(at),
            start_offset: offset,
            end_offset: offset + 10,
            total: UsageValue::Valid(total),
            last,
        }
    }

    #[test]
    fn subagent_pre_context_snapshot_only_baselines_and_post_context_counts_delta() {
        let mut subagent = context(1);
        subagent.root_session_id = "root".to_owned();
        let baseline = known(10, 2, 1, 4, 1);
        let current = known(15, 3, 2, 6, 2);
        let result = UsageProcessor::new(subagent, None).process(vec![
            token(
                100,
                10,
                baseline.clone(),
                UsageValue::Valid(baseline.clone()),
            ),
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("gpt-5.6-luna".to_owned()),
                reasoning_effort: Some("high".to_owned()),
            },
            token(110, 20, current.clone(), UsageValue::Missing),
        ]);

        assert_eq!(result.events.len(), 1);
        let event = &result.events[0];
        assert_eq!(event.model, "gpt-5.6-luna");
        assert_eq!(event.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(event.previous_total, Some(baseline));
        assert_eq!(event.usage, known(5, 1, 1, 2, 1));
        assert_eq!(event.current_total, current);
        assert!(result.anomalies.is_empty());
    }

    #[test]
    fn main_pre_context_snapshot_remains_unknown() {
        let baseline = known(10, 2, 1, 4, 1);
        let current = known(15, 3, 2, 6, 2);
        let result = UsageProcessor::new(context(1), None).process(vec![
            token(100, 10, baseline, UsageValue::Valid(known(10, 2, 1, 4, 1))),
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("gpt-5.6-luna".to_owned()),
                reasoning_effort: Some("low".to_owned()),
            },
            token(110, 20, current, UsageValue::Missing),
        ]);

        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[0].model, "unknown");
        assert_eq!(result.events[0].reasoning_effort, None);
        assert_eq!(result.events[1].model, "gpt-5.6-luna");
        assert_eq!(result.events[1].reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn normal_dedup_and_occurrence_matrix() {
        let total = known(10, 2, 1, 4, 1);
        let last = known(3, 1, 0, 2, 1);
        let records = vec![
            token(100, 10, total.clone(), UsageValue::Valid(last.clone())),
            token(101, 20, total.clone(), UsageValue::Valid(last.clone())),
            token(
                102,
                30,
                known(14, 2, 1, 6, 1),
                UsageValue::Valid(known(4, 0, 0, 2, 0)),
            ),
            token(
                103,
                40,
                known(17, 3, 1, 8, 2),
                UsageValue::Valid(last.clone()),
            ),
        ];
        let first = UsageProcessor::new(context(1), None).process(records.clone());
        assert_eq!(first.events.len(), 3, "duplicate total ignores last");
        assert!(
            first
                .events
                .iter()
                .all(|event| event.kind == EventKind::Normal)
        );
        assert_eq!(first.events[0].usage, last);

        let archive = UsageProcessor::new(context(2), None).process(records);
        assert_eq!(archive.events[0].event_id, first.events[0].event_id);
        assert_ne!(archive.occurrences[0], first.occurrences[0]);
        assert_eq!(
            compare_canonical(Some(&first.events[0]), &archive.events[0]),
            CanonicalDecision::Duplicate
        );
        assert_eq!(
            compare_occurrence(None, &archive.occurrences[0]),
            CanonicalDecision::Insert
        );
        assert_ne!(first.events[0].event_id, first.events[1].event_id);
        assert_eq!(first.events[0].usage, first.events[2].usage);
        assert_ne!(
            first.events[0].event_id, first.events[2].event_id,
            "equal request vectors at different time/total anchors stay distinct"
        );

        let mut conflicting_event = first.events[0].clone();
        conflicting_event.usage.output_tokens += 1;
        assert_eq!(
            compare_canonical(Some(&first.events[0]), &conflicting_event),
            CanonicalDecision::Conflict
        );
        let mut conflicting_occurrence = first.occurrences[0].clone();
        conflicting_occurrence.source_end_offset += 1;
        assert_eq!(
            compare_occurrence(Some(&first.occurrences[0]), &conflicting_occurrence),
            CanonicalDecision::Conflict
        );
        assert_eq!(
            compare_occurrence(Some(&first.occurrences[0]), &first.occurrences[0]),
            CanonicalDecision::Duplicate,
            "retry/race comparison is explicit rather than ignored"
        );
    }

    #[test]
    fn missing_recovery_and_chain_break_matrix() {
        let baseline = UsageSourceState {
            previous_total: Some(known(10, 2, 1, 4, 1)),
            previous_total_offset: Some(10),
            ..UsageSourceState::default()
        };
        let recovered = UsageProcessor::new(context(1), Some(baseline.clone())).process(vec![
            token(100, 10, known(15, 3, 2, 6, 2), UsageValue::Missing),
            token(101, 20, known(15, 3, 2, 6, 2), UsageValue::Missing),
        ]);
        assert_eq!(recovered.events.len(), 1);
        assert_eq!(recovered.events[0].kind, EventKind::Recovered);
        assert_eq!(recovered.events[0].usage, known(5, 1, 1, 2, 1));
        assert_eq!(
            recovered.updated_state.previous_total,
            Some(known(15, 3, 2, 6, 2))
        );

        let no_previous = UsageProcessor::new(context(1), None).process(vec![token(
            100,
            10,
            known(5, 1, 0, 2, 1),
            UsageValue::Missing,
        )]);
        assert!(no_previous.events.is_empty());
        assert_eq!(
            no_previous.updated_state.previous_total,
            Some(known(5, 1, 0, 2, 1))
        );

        let unknown_cache = UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(unknown(10, 2, 4, 1)),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![token(
            100,
            10,
            known(15, 3, 2, 6, 2),
            UsageValue::Missing,
        )]);
        assert_eq!(unknown_cache.events[0].usage.cache_write_tokens, None);
        assert!(
            !unknown_cache
                .anomalies
                .iter()
                .any(|item| { matches!(item.code, AnomalyCode::CacheWriteChainDecrease) })
        );

        let interrupted = UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                chain_state: ChainState::Interrupted(GapKind::Ownership),
                ..baseline.clone()
            }),
        )
        .process(vec![token(
            100,
            10,
            known(15, 3, 2, 6, 2),
            UsageValue::Missing,
        )]);
        assert!(interrupted.events.is_empty());
        assert_eq!(
            interrupted.updated_state.chain_state,
            ChainState::Continuous
        );

        let reset = UsageProcessor::new(context(1), Some(baseline.clone())).process(vec![token(
            100,
            10,
            known(9, 1, 0, 3, 0),
            UsageValue::Missing,
        )]);
        assert!(reset.events.is_empty());
        assert!(
            reset
                .anomalies
                .iter()
                .any(|item| item.code == AnomalyCode::TotalChainReset)
        );
        assert_eq!(
            reset.updated_state.previous_total,
            Some(known(9, 1, 0, 3, 0))
        );

        let mut cache_decrease_state = baseline;
        cache_decrease_state.open_turn = Some(blocked_cases_template(
            cache_decrease_state.previous_total.as_ref().unwrap(),
        ));
        let cache_decrease =
            UsageProcessor::new(context(1), Some(cache_decrease_state)).process(vec![token(
                100,
                10,
                known(12, 2, 0, 5, 1),
                UsageValue::Valid(known(2, 0, 0, 1, 0)),
            )]);
        assert_eq!(cache_decrease.events.len(), 1);
        assert_eq!(cache_decrease.events[0].kind, EventKind::Normal);
        assert!(
            cache_decrease
                .anomalies
                .iter()
                .any(|item| item.code == AnomalyCode::CacheWriteChainDecrease)
        );
        assert!(
            cache_decrease
                .updated_state
                .open_turn
                .as_ref()
                .unwrap()
                .blocks
                .reset
        );
    }

    #[test]
    fn synthetic_turn_key_is_copy_stable_and_missing_time_blocks_compensation() {
        let records = vec![UsageRecord::TurnStarted {
            ownership: owning(),
            turn_id: None,
            timestamp_ms: None,
            start_offset: 77,
        }];
        let first = UsageProcessor::new(context(1), None).process(records.clone());
        let copy = UsageProcessor::new(context(2), None).process(records);
        let first_turn = first.updated_state.open_turn.as_ref().unwrap();
        let copy_turn = copy.updated_state.open_turn.as_ref().unwrap();
        assert_eq!(first_turn.turn_key, copy_turn.turn_key);
        assert!(first_turn.raw_turn_id.is_none());
        assert!(first_turn.started_at_ms.is_none());
        assert!(first_turn.blocks.time_missing);
        assert!(!first_turn.blocks.allowed());
    }

    #[test]
    fn turn_compensation_restart_model_and_block_matrix() {
        let baseline = known(10, 2, 1, 4, 1);
        let records = vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 10,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: Some("high".to_owned()),
            },
            token(
                100,
                20,
                known(14, 3, 1, 6, 1),
                UsageValue::Valid(known(2, 1, 0, 1, 0)),
            ),
        ];
        let initial = UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(5),
                ..UsageSourceState::default()
            }),
        )
        .process(records);
        let persisted = initial.updated_state.clone();
        assert_eq!(
            persisted
                .open_turn
                .as_ref()
                .unwrap()
                .accounted_candidate_count,
            1
        );
        assert_eq!(persisted.active_reasoning_effort.as_deref(), Some("high"));

        let completed = UsageProcessor::new(context(1), Some(persisted)).process(vec![
            token(110, 30, known(18, 4, 2, 8, 2), UsageValue::Missing),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(120),
                start_offset: 40,
                end_offset: 50,
                status: TurnEndStatus::Completed,
            },
        ]);
        assert_eq!(completed.events.len(), 2);
        assert_eq!(completed.events[0].kind, EventKind::Recovered);
        assert_eq!(
            completed.events[0].reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(completed.events[1].kind, EventKind::TurnCompensation);
        assert_eq!(completed.events[1].model, "model-a");
        assert_eq!(
            completed.events[1].reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(completed.events[1].usage, known(2, 0, 0, 1, 0));
        assert_eq!(completed.closed_turns[0].status, TurnEndStatus::Completed);

        let mut exact = blocked_cases_template(&baseline);
        exact.accounted = known(5, 0, 0, 2, 0);
        exact.accounted_candidate_count = 1;
        let exact = close_existing_turn(exact, TurnEndStatus::Completed);
        assert!(
            exact.events.is_empty(),
            "delta equal to accounted needs no compensation"
        );

        let mut negative_cache = blocked_cases_template(&baseline);
        negative_cache.start_total = Some(known(10, 2, 2, 4, 1));
        negative_cache.last_total = Some(known(15, 3, 1, 6, 1));
        let negative_cache = close_existing_turn(negative_cache, TurnEndStatus::Completed);
        assert!(negative_cache.events.is_empty());
        assert!(
            negative_cache
                .anomalies
                .iter()
                .any(|item| { item.code == AnomalyCode::TurnCacheWriteDeltaNegative })
        );

        let mut cache_overaccounted = blocked_cases_template(&baseline);
        cache_overaccounted.start_total = Some(known(10, 2, 0, 4, 1));
        cache_overaccounted.last_total = Some(known(15, 3, 1, 6, 1));
        cache_overaccounted.accounted = known(2, 0, 2, 1, 0);
        cache_overaccounted.accounted_candidate_count = 1;
        let cache_overaccounted =
            close_existing_turn(cache_overaccounted, TurnEndStatus::Completed);
        assert!(cache_overaccounted.events.is_empty());
        assert!(
            cache_overaccounted
                .anomalies
                .iter()
                .any(|item| { item.code == AnomalyCode::TurnAccountedExceedsTotal })
        );

        let mut accumulator = blocked_cases_template(&baseline);
        accumulator.accounted = NormalizedTokenUsage::zero();
        accumulator.accounted_candidate_count = 0;
        add_accounted(&mut accumulator, &known(1, 0, 0, 1, 0)).unwrap();
        assert_eq!(accumulator.accounted.cache_write_tokens, Some(0));
        add_accounted(&mut accumulator, &known(1, 0, 0, 1, 0)).unwrap();
        assert_eq!(accumulator.accounted.cache_write_tokens, Some(0));
        add_accounted(&mut accumulator, &known(2, 0, 1, 1, 0)).unwrap();
        assert_eq!(accumulator.accounted.cache_write_tokens, Some(1));
        add_accounted(&mut accumulator, &unknown(1, 0, 1, 0)).unwrap();
        assert_eq!(accumulator.accounted.cache_write_tokens, None);

        for status in [TurnEndStatus::Aborted, TurnEndStatus::Failed] {
            let state = UsageSourceState {
                previous_total: Some(known(15, 2, 1, 6, 1)),
                previous_total_offset: Some(20),
                open_turn: Some(TurnState {
                    turn_key: "turn".to_owned(),
                    raw_turn_id: Some("turn".to_owned()),
                    started_at_ms: Some(1),
                    start_offset: 1,
                    start_total: Some(baseline.clone()),
                    last_total: Some(known(15, 2, 1, 6, 1)),
                    accounted: NormalizedTokenUsage::zero(),
                    accounted_candidate_count: 0,
                    model_state: TurnModelState::Single("model-a".to_owned()),
                    reasoning_effort_state: TurnReasoningEffortState::None,
                    unresolved_reasoning_effort_seen: false,
                    unresolved_model_seen: false,
                    blocks: CompensationBlocks::default(),
                }),
                ..UsageSourceState::default()
            };
            let result = UsageProcessor::new(context(1), Some(state)).process(vec![
                UsageRecord::TurnEnded {
                    ownership: owning(),
                    turn_id: Some("turn".to_owned()),
                    timestamp_ms: Some(100),
                    start_offset: 30,
                    end_offset: 40,
                    status,
                },
            ]);
            assert_eq!(result.events[0].kind, EventKind::TurnCompensation);
            assert_eq!(result.closed_turns[0].status, status);
        }

        let mut blocked_cases = Vec::new();
        for block in [
            CompensationBlocks {
                start_missing: true,
                ..CompensationBlocks::default()
            },
            CompensationBlocks {
                time_missing: true,
                ..CompensationBlocks::default()
            },
            CompensationBlocks {
                reset: true,
                ..CompensationBlocks::default()
            },
            CompensationBlocks {
                ownership_gap: true,
                ..CompensationBlocks::default()
            },
            CompensationBlocks {
                parser_gap: true,
                ..CompensationBlocks::default()
            },
            CompensationBlocks {
                required_invalid: true,
                ..CompensationBlocks::default()
            },
            CompensationBlocks {
                model_unresolved: true,
                ..CompensationBlocks::default()
            },
        ] {
            blocked_cases.push(TurnState {
                turn_key: "turn".to_owned(),
                raw_turn_id: Some("turn".to_owned()),
                started_at_ms: Some(1),
                start_offset: 1,
                start_total: Some(baseline.clone()),
                last_total: Some(known(15, 2, 1, 6, 1)),
                accounted: NormalizedTokenUsage::zero(),
                accounted_candidate_count: 0,
                model_state: TurnModelState::Single("model-a".to_owned()),
                reasoning_effort_state: TurnReasoningEffortState::None,
                unresolved_reasoning_effort_seen: false,
                unresolved_model_seen: block.model_unresolved,
                blocks: block,
            });
        }
        blocked_cases.push(TurnState {
            model_state: TurnModelState::None,
            blocks: CompensationBlocks::default(),
            ..blocked_cases[0].clone()
        });
        for turn in blocked_cases {
            let result = UsageProcessor::new(
                context(1),
                Some(UsageSourceState {
                    previous_total: turn.last_total.clone(),
                    previous_total_offset: Some(20),
                    open_turn: Some(turn),
                    ..UsageSourceState::default()
                }),
            )
            .process(vec![UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(100),
                start_offset: 30,
                end_offset: 40,
                status: TurnEndStatus::Completed,
            }]);
            assert!(result.events.is_empty());
        }

        let mixed = TurnState {
            model_state: TurnModelState::Mixed,
            blocks: CompensationBlocks::default(),
            ..blocked_cases_template(&baseline)
        };
        let result = UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: mixed.last_total.clone(),
                previous_total_offset: Some(20),
                open_turn: Some(mixed),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![UsageRecord::TurnEnded {
            ownership: owning(),
            turn_id: Some("turn".to_owned()),
            timestamp_ms: Some(100),
            start_offset: 30,
            end_offset: 40,
            status: TurnEndStatus::Completed,
        }]);
        assert_eq!(result.events[0].model, "unknown");

        let mut excessive = blocked_cases_template(&baseline);
        excessive.accounted = known(20, 0, 0, 20, 0);
        excessive.accounted_candidate_count = 1;
        let result = UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: excessive.last_total.clone(),
                previous_total_offset: Some(20),
                open_turn: Some(excessive),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![UsageRecord::TurnEnded {
            ownership: owning(),
            turn_id: Some("turn".to_owned()),
            timestamp_ms: Some(100),
            start_offset: 30,
            end_offset: 40,
            status: TurnEndStatus::Completed,
        }]);
        assert!(result.events.is_empty());
        assert!(
            result
                .anomalies
                .iter()
                .any(|item| item.code == AnomalyCode::TurnAccountedExceedsTotal)
        );

        let exact_request = known(4, 1, 0, 2, 0);
        let duplicate_records = vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("copy-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 10,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(
                100,
                20,
                known(14, 3, 1, 6, 1),
                UsageValue::Valid(exact_request),
            ),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("copy-turn".to_owned()),
                timestamp_ms: Some(110),
                start_offset: 30,
                end_offset: 40,
                status: TurnEndStatus::Completed,
            },
        ];
        let copy_state = UsageSourceState {
            previous_total: Some(baseline.clone()),
            previous_total_offset: Some(5),
            ..UsageSourceState::default()
        };
        let primary = UsageProcessor::new(context(1), Some(copy_state.clone()))
            .process(duplicate_records.clone());
        let archive = UsageProcessor::new(context(2), Some(copy_state)).process(duplicate_records);
        assert_eq!(primary.events.len(), 1);
        assert_eq!(archive.events.len(), 1);
        assert_eq!(primary.events[0].event_id, archive.events[0].event_id);
        assert_eq!(primary.closed_turns[0].turn.accounted_candidate_count, 1);
        assert_eq!(archive.closed_turns[0].turn.accounted_candidate_count, 1);

        let gap_then_turn = UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline),
                previous_total_offset: Some(5),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::Gap {
                ownership: owning(),
                kind: GapKind::Parser,
            },
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("after-gap".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 10,
            },
            token(100, 20, known(15, 3, 1, 6, 1), UsageValue::Missing),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("after-gap".to_owned()),
                timestamp_ms: Some(110),
                start_offset: 30,
                end_offset: 40,
                status: TurnEndStatus::Completed,
            },
        ]);
        assert!(gap_then_turn.events.is_empty());
        assert!(gap_then_turn.closed_turns[0].turn.start_total.is_none());
        assert!(gap_then_turn.closed_turns[0].turn.blocks.parser_gap);

        let after_new_baseline = UsageProcessor::new(context(1), Some(gap_then_turn.updated_state))
            .process(vec![
                UsageRecord::TurnStarted {
                    ownership: owning(),
                    turn_id: Some("clean-turn".to_owned()),
                    timestamp_ms: Some(120),
                    start_offset: 50,
                },
                UsageRecord::TurnContext {
                    ownership: owning(),
                    model: Some("model-a".to_owned()),
                    reasoning_effort: None,
                },
                token(130, 60, known(20, 4, 2, 8, 2), UsageValue::Missing),
                UsageRecord::TurnEnded {
                    ownership: owning(),
                    turn_id: Some("clean-turn".to_owned()),
                    timestamp_ms: Some(140),
                    start_offset: 70,
                    end_offset: 80,
                    status: TurnEndStatus::Completed,
                },
            ]);
        assert_eq!(after_new_baseline.events.len(), 1);
        assert_eq!(after_new_baseline.events[0].kind, EventKind::Recovered);
        assert!(
            after_new_baseline.closed_turns[0]
                .turn
                .start_total
                .is_some()
        );
        assert!(after_new_baseline.closed_turns[0].turn.blocks.allowed());
    }

    #[test]
    fn t_mu03_c03_effort_is_canonical_identity_but_not_derived_cost() {
        let records = vec![
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: Some("high".to_owned()),
            },
            token(
                100,
                10,
                known(10, 2, 1, 4, 1),
                UsageValue::Valid(known(3, 1, 0, 2, 1)),
            ),
        ];
        let high = UsageProcessor::new(context(1), None)
            .process(records)
            .events
            .pop()
            .expect("canonical event");
        let mut medium = high.clone();
        medium.reasoning_effort = Some("medium".to_owned());
        medium.event_id = event_id(&medium);

        assert_eq!(
            crate::codex::normalization::canonical_algorithm_for(
                crate::codex::normalization::USAGE_PARSER_VERSION,
            ),
            Some(5)
        );
        assert_eq!(crate::codex::normalization::USAGE_PARSER_VERSION, 11);
        assert_eq!(
            crate::codex::normalization::USAGE_CANONICAL_ALGORITHM_VERSION,
            5
        );
        assert_eq!(high.event_id, event_id(&high), "replay is stable");
        assert_ne!(
            high.event_id, medium.event_id,
            "effort is canonical context"
        );
        assert_eq!(
            compare_canonical(Some(&high), &high),
            CanonicalDecision::Duplicate
        );
        assert_eq!(
            compare_canonical(Some(&high), &medium),
            CanonicalDecision::Conflict
        );
    }

    #[test]
    fn t_mu03_c04_compensation_protects_effort_ownership_without_changing_tokens() {
        let baseline = known(10, 2, 1, 4, 1);
        let process = |contexts: &[Option<&str>]| {
            let mut records = vec![UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("turn-effort".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 10,
            }];
            let mut total = 10;
            let mut offset = 20;
            for (index, effort) in contexts.iter().enumerate() {
                records.push(UsageRecord::TurnContext {
                    ownership: owning(),
                    model: Some("model-a".to_owned()),
                    reasoning_effort: effort.map(str::to_owned),
                });
                total += 4;
                records.push(token(
                    100 + index as i64,
                    offset,
                    known(
                        total,
                        2 + index as i64,
                        1,
                        4 + (index as i64 + 1) * 4,
                        index as i64 + 2,
                    ),
                    UsageValue::Valid(known(2, 0, 0, 2, 1)),
                ));
                offset += 10;
            }
            records.push(UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("turn-effort".to_owned()),
                timestamp_ms: Some(200),
                start_offset: offset,
                end_offset: offset + 10,
                status: TurnEndStatus::Completed,
            });
            UsageProcessor::new(
                context(1),
                Some(UsageSourceState {
                    previous_total: Some(baseline.clone()),
                    previous_total_offset: Some(5),
                    ..UsageSourceState::default()
                }),
            )
            .process(records)
        };

        let single = process(&[Some("high")]);
        let compensation = single
            .events
            .iter()
            .find(|event| event.kind == EventKind::TurnCompensation)
            .expect("single-effort compensation");
        assert_eq!(compensation.reasoning_effort.as_deref(), Some("high"));

        let mixed = process(&[Some("high"), Some("medium")]);
        let mixed_compensation = mixed
            .events
            .iter()
            .find(|event| event.kind == EventKind::TurnCompensation)
            .expect("mixed-effort compensation");
        assert_eq!(mixed_compensation.reasoning_effort, None);
        assert!(mixed_compensation.usage.input_tokens > 0);

        let unknown = process(&[Some("high"), None]);
        let unknown_compensation = unknown
            .events
            .iter()
            .find(|event| event.kind == EventKind::TurnCompensation)
            .expect("known-plus-unknown compensation");
        assert_eq!(unknown_compensation.reasoning_effort, None);
        assert!(unknown_compensation.usage.input_tokens > 0);
    }

    fn blocked_cases_template(baseline: &NormalizedTokenUsage) -> TurnState {
        TurnState {
            turn_key: "turn".to_owned(),
            raw_turn_id: Some("turn".to_owned()),
            started_at_ms: Some(1),
            start_offset: 1,
            start_total: Some(baseline.clone()),
            last_total: Some(known(15, 2, 1, 6, 1)),
            accounted: NormalizedTokenUsage::zero(),
            accounted_candidate_count: 0,
            model_state: TurnModelState::Single("model-a".to_owned()),
            reasoning_effort_state: TurnReasoningEffortState::None,
            unresolved_reasoning_effort_seen: false,
            unresolved_model_seen: false,
            blocks: CompensationBlocks::default(),
        }
    }

    fn close_existing_turn(turn: TurnState, status: TurnEndStatus) -> ProcessResult {
        UsageProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: turn.last_total.clone(),
                previous_total_offset: Some(20),
                open_turn: Some(turn),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![UsageRecord::TurnEnded {
            ownership: owning(),
            turn_id: Some("turn".to_owned()),
            timestamp_ms: Some(100),
            start_offset: 30,
            end_offset: 40,
            status,
        }])
    }
}
