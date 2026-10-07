//! Pure usage ingestion state machine.
//!
//! This module deliberately has no scanner or storage dependencies. Its input
//! is the normalized, ownership-classified record stream; its output is a set
//! of deterministic event/occurrence proposals plus restartable source/Turn
//! state. SQL commit, epoch management, aggregation, and rollout decoding live
//! at later integration seams.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Deserialize, Serialize};

pub use crate::codex::usage::UsageValue;
use crate::codex::usage::{
    CodexOperation, CompactionEvidence, EvidenceKind, ResponseUsageEvidence,
};
pub use crate::usage::normalized::NormalizedTokenUsage;
use crate::{domain::DomainError, usage::event::EventKind};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    Malformed,
    Oversized,
    Ownership,
    Parser,
    RequiredInvalid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageRecord {
    ResponseUsage {
        ownership: Ownership,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        evidence: ResponseUsageEvidence,
    },
    Compacted {
        ownership: Ownership,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        evidence: CompactionEvidence,
    },
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
        start_offset: u64,
        end_offset: u64,
    },
}

impl UsageRecord {
    fn ownership(&self) -> &Ownership {
        match self {
            Self::ResponseUsage { ownership, .. }
            | Self::Compacted { ownership, .. }
            | Self::TurnContext { ownership, .. }
            | Self::TurnStarted { ownership, .. }
            | Self::TokenCount { ownership, .. }
            | Self::TurnEnded { ownership, .. }
            | Self::Gap { ownership, .. } => ownership,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheWriteSum {
    Known(i64),
    Unknown,
    Indeterminate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UsageSum {
    usage: NormalizedTokenUsage,
    cache_write: CacheWriteSum,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalUsageProposal {
    pub event_id: String,
    pub kind: EventKind,
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub turn_key: Option<String>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub usage: NormalizedTokenUsage,
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
    ReconciliationPatchTooLarge,
    ResponseUsageConflict,
    ResponseOwnershipMismatch,
    CompactionIdentityMismatch,
    LegacyCoverageAmbiguous,
    ThreadUsageMismatch,
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
pub struct UsageSourceState {
    pub chain_state: ChainState,
    pub previous_total: Option<NormalizedTokenUsage>,
    pub previous_total_offset: Option<u64>,
    pub active_model: Option<String>,
    pub active_reasoning_effort: Option<String>,
    pub open_turn: Option<TurnState>,
    pub reconciliation_carry: ReconciliationCarry,
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
            reconciliation_carry: ReconciliationCarry::default(),
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyReconciliationWindow {
    pub version: u8,
    pub previous_total: UsageValue,
    pub current_total: UsageValue,
    pub last_usage: UsageValue,
    pub explicit_response_ids: Vec<String>,
    pub legacy_covered_response_ids: Vec<String>,
    pub proposal_event_ids: Vec<String>,
    pub turn_accounted_before: NormalizedTokenUsage,
    pub chain_state: ChainState,
    pub closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarryError {
    Invalid,
    UnsupportedVersion,
}

impl LegacyReconciliationWindow {
    pub const VERSION: u8 = 1;

    /// Compact JSON with deduplicated, sorted ID lists.
    pub fn to_json(&self) -> Result<String, CarryError> {
        if self.version != Self::VERSION {
            return Err(CarryError::UnsupportedVersion);
        }
        validate_usage_value(&self.previous_total)?;
        validate_usage_value(&self.current_total)?;
        validate_usage_value(&self.last_usage)?;
        self.turn_accounted_before
            .validate()
            .map_err(|_| CarryError::Invalid)?;
        if self
            .explicit_response_ids
            .iter()
            .chain(&self.legacy_covered_response_ids)
            .any(|id| !valid_identity(id))
            || self.proposal_event_ids.iter().any(|id| !valid_event_id(id))
        {
            return Err(CarryError::Invalid);
        }
        let mut value = self.clone();
        sort_ids(&mut value.explicit_response_ids);
        sort_ids(&mut value.legacy_covered_response_ids);
        sort_ids(&mut value.proposal_event_ids);
        serde_json::to_string(&value).map_err(|_| CarryError::Invalid)
    }

    pub fn from_json(json: &str) -> Result<Self, CarryError> {
        let value: Self = serde_json::from_str(json).map_err(|_| CarryError::Invalid)?;
        if value.version != Self::VERSION {
            return Err(CarryError::UnsupportedVersion);
        }
        value.to_json()?;
        let mut canonical = value.clone();
        sort_ids(&mut canonical.explicit_response_ids);
        sort_ids(&mut canonical.legacy_covered_response_ids);
        sort_ids(&mut canonical.proposal_event_ids);
        if canonical.explicit_response_ids != value.explicit_response_ids
            || canonical.legacy_covered_response_ids != value.legacy_covered_response_ids
            || canonical.proposal_event_ids != value.proposal_event_ids
        {
            return Err(CarryError::Invalid);
        }
        Ok(value)
    }
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn valid_event_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_usage_value(value: &UsageValue) -> Result<(), CarryError> {
    if let UsageValue::Valid(usage) = value {
        usage.validate().map_err(|_| CarryError::Invalid)?;
    }
    Ok(())
}

fn validate_response_evidence(evidence: &ResponseUsageEvidence) -> Result<(), CarryError> {
    if !valid_identity(&evidence.response_id)
        || evidence
            .thread_id
            .as_deref()
            .is_some_and(|value| !valid_identity(value))
        || evidence
            .session_id
            .as_deref()
            .is_some_and(|value| !valid_identity(value))
        || evidence
            .turn_id
            .as_deref()
            .is_some_and(|value| !valid_identity(value))
    {
        return Err(CarryError::Invalid);
    }
    validate_usage_value(&evidence.usage)?;
    validate_usage_value(&evidence.thread_token_usage)
}

fn validate_pending_evidence(entry: &PendingUsageEvidence) -> Result<(), CarryError> {
    if entry
        .model
        .as_deref()
        .is_some_and(|value| !valid_identity(value))
        || entry
            .reasoning_effort
            .as_deref()
            .is_some_and(|value| !valid_identity(value))
    {
        return Err(CarryError::Invalid);
    }
    let valid_offsets = |start: u64, end: u64| start < end && end <= i64::MAX as u64;
    match &entry.record {
        PendingEvidenceRecord::ResponseUsage {
            start_offset,
            end_offset,
            evidence,
            ..
        } => {
            if !valid_offsets(*start_offset, *end_offset) {
                return Err(CarryError::Invalid);
            }
            validate_response_evidence(evidence)
        }
        PendingEvidenceRecord::Compacted {
            start_offset,
            end_offset,
            evidence,
            ..
        } => {
            if !valid_offsets(*start_offset, *end_offset)
                || evidence
                    .compaction_response_id
                    .as_deref()
                    .is_some_and(|value| !valid_identity(value))
            {
                return Err(CarryError::Invalid);
            }
            if let Some(latest) = &evidence.latest_token_usage_record {
                validate_response_evidence(latest)?;
            }
            Ok(())
        }
    }
}

fn sort_ids(ids: &mut Vec<String>) {
    ids.sort();
    ids.dedup();
}

/// Unfinished owning evidence kept in the source carry. Ownership is implied
/// by the source's owning thread; no content body is stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PendingEvidenceRecord {
    ResponseUsage {
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        evidence: ResponseUsageEvidence,
    },
    Compacted {
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        evidence: CompactionEvidence,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingUsageEvidence {
    pub record: PendingEvidenceRecord,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationCarry {
    pub version: u8,
    pub open_window_start_offset: Option<u64>,
    pub pending_response_ids: Vec<String>,
    pub modern_counter_domain: Option<(String, Option<String>)>,
    pub modern_counter_total: Option<NormalizedTokenUsage>,
    pub pending_evidence: Vec<PendingUsageEvidence>,
}

impl Default for ReconciliationCarry {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            open_window_start_offset: None,
            pending_response_ids: Vec::new(),
            modern_counter_domain: None,
            modern_counter_total: None,
            pending_evidence: Vec::new(),
        }
    }
}

impl ReconciliationCarry {
    pub const VERSION: u8 = 1;

    /// Compact JSON in field order with deduplicated, sorted response IDs.
    pub fn to_json(&self) -> Result<String, CarryError> {
        if self.version != Self::VERSION {
            return Err(CarryError::UnsupportedVersion);
        }
        if self
            .open_window_start_offset
            .is_some_and(|offset| offset > i64::MAX as u64)
            || self
                .pending_response_ids
                .iter()
                .any(|id| !valid_identity(id))
            || self
                .modern_counter_domain
                .as_ref()
                .is_some_and(|(thread, session)| {
                    !valid_identity(thread)
                        || session
                            .as_deref()
                            .is_some_and(|value| !valid_identity(value))
                })
        {
            return Err(CarryError::Invalid);
        }
        if let Some(total) = &self.modern_counter_total {
            total.validate().map_err(|_| CarryError::Invalid)?;
        }
        for entry in &self.pending_evidence {
            validate_pending_evidence(entry)?;
        }
        let mut value = self.clone();
        sort_ids(&mut value.pending_response_ids);
        value
            .pending_evidence
            .sort_by_key(|entry| match &entry.record {
                PendingEvidenceRecord::ResponseUsage {
                    start_offset,
                    end_offset,
                    ..
                }
                | PendingEvidenceRecord::Compacted {
                    start_offset,
                    end_offset,
                    ..
                } => (*start_offset, *end_offset),
            });
        serde_json::to_string(&value).map_err(|_| CarryError::Invalid)
    }

    pub fn from_json(json: &str) -> Result<Self, CarryError> {
        let value: Self = serde_json::from_str(json).map_err(|_| CarryError::Invalid)?;
        if value.version != Self::VERSION {
            return Err(CarryError::UnsupportedVersion);
        }
        let canonical = value.to_json()?;
        let supplied = serde_json::to_string(&value).map_err(|_| CarryError::Invalid)?;
        if canonical != supplied {
            return Err(CarryError::Invalid);
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResponseKey {
    pub owning_thread_id: String,
    pub response_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WindowKey {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub start_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageEventFact {
    pub event_id: String,
    pub owning_thread_id: String,
    pub response_id: Option<String>,
    pub evidence_kind: EvidenceKind,
    pub operation: CodexOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseBinding {
    pub proposal: CanonicalUsageProposal,
    pub fact: UsageEventFact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationWindowMetadata {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
    pub source_end_offset: u64,
    pub owning_thread_id: String,
    pub turn_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowProposalBinding {
    pub proposal: CanonicalUsageProposal,
    pub fact: UsageEventFact,
    pub occurrences: Vec<Occurrence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerUnknownReason {
    UsageMissing,
    IdentityMissing,
    UsageInvalid,
    TimeMissing,
    ModelUnresolved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionMarkerWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
    pub source_end_offset: u64,
    pub owning_thread_id: String,
    pub root_session_id: String,
    pub occurred_at_ms: Option<i64>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub response_id: Option<String>,
    pub resolved_event_id: Option<String>,
    pub unknown_reason: Option<MarkerUnknownReason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyWindowWrite {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
    pub source_end_offset: u64,
    pub owning_thread_id: String,
    pub turn_key: Option<String>,
    pub state: LegacyReconciliationWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsagePrivateRowKey {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub source_start_offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageEventHoldReason {
    Replay,
    Carry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageEventHold {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub event_id: String,
    pub hold_reason: UsageEventHoldReason,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsageEventHoldKey {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub event_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconciliationRequest {
    pub response_keys: Vec<ResponseKey>,
    pub owning_turn_keys: Vec<(String, Option<String>)>,
}

impl ReconciliationRequest {
    /// Builds a request with deduplicated, stably ordered keys.
    pub fn new(
        mut response_keys: Vec<ResponseKey>,
        mut owning_turn_keys: Vec<(String, Option<String>)>,
    ) -> Self {
        response_keys.sort();
        response_keys.dedup();
        owning_turn_keys.sort();
        owning_turn_keys.dedup();
        Self {
            response_keys,
            owning_turn_keys,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PersistedTurnKey {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub turn_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedTurnStatus {
    Open,
    Completed,
    Aborted,
    Failed,
}

impl From<TurnEndStatus> for PersistedTurnStatus {
    fn from(status: TurnEndStatus) -> Self {
        match status {
            TurnEndStatus::Completed => Self::Completed,
            TurnEndStatus::Aborted => Self::Aborted,
            TurnEndStatus::Failed => Self::Failed,
        }
    }
}

/// Complete persisted `codex_turns` business columns for one Turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedTurnSnapshot {
    pub key: PersistedTurnKey,
    pub owning_thread_id: String,
    pub state: TurnState,
    pub status: PersistedTurnStatus,
    pub ended_at_ms: Option<i64>,
    pub end_offset: Option<u64>,
    pub quality_status: String,
    pub state_through_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffectedTurn {
    pub snapshot: PersistedTurnSnapshot,
    pub compensation_events: Vec<CanonicalUsageProposal>,
    pub compensation_occurrences: Vec<Occurrence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRewrite {
    pub expected: PersistedTurnSnapshot,
    pub replacement: PersistedTurnSnapshot,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconciliationContext {
    pub bindings: BTreeMap<ResponseKey, ResponseBinding>,
    pub windows: BTreeMap<WindowKey, LegacyReconciliationWindow>,
    pub window_metadata: BTreeMap<WindowKey, ReconciliationWindowMetadata>,
    pub window_proposals: BTreeMap<WindowKey, Vec<WindowProposalBinding>>,
    pub response_occurrences: BTreeMap<ResponseKey, Vec<Occurrence>>,
    pub closure_response_keys: BTreeSet<ResponseKey>,
    pub markers: Vec<CompactionMarkerWrite>,
    pub affected_turns: BTreeMap<PersistedTurnKey, AffectedTurn>,
    pub expected_fingerprint: Vec<u8>,
    pub request: ReconciliationRequest,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconciliationPatch {
    pub delete_event_ids: Vec<String>,
    pub delete_markers: Vec<UsagePrivateRowKey>,
    pub delete_windows: Vec<UsagePrivateRowKey>,
    pub delete_holds: Vec<UsageEventHoldKey>,
    pub events: Vec<CanonicalUsageProposal>,
    pub occurrences: Vec<Occurrence>,
    pub facts: Vec<UsageEventFact>,
    pub marker_updates: Vec<CompactionMarkerWrite>,
    pub window_updates: Vec<LegacyWindowWrite>,
    pub hold_updates: Vec<UsageEventHold>,
    pub turn_upserts: Vec<PersistedTurnSnapshot>,
    pub turn_rewrites: Vec<TurnRewrite>,
    pub anomalies: Vec<Anomaly>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatchCounts {
    pub canonical_event_count: u64,
    pub occurrence_count: u64,
    pub evidence_write_count: u64,
    pub write_unit_count: u64,
}

impl ReconciliationPatch {
    pub fn counts(&self) -> Option<PatchCounts> {
        let patch = self.folded();
        let canonical_event_count = u64::try_from(patch.events.len()).ok()?;
        let occurrence_count = u64::try_from(patch.occurrences.len()).ok()?;
        let evidence_write_count = u64::try_from(patch.facts.len())
            .ok()?
            .checked_add(u64::try_from(patch.marker_updates.len()).ok()?)?
            .checked_add(u64::try_from(patch.window_updates.len()).ok()?)?
            .checked_add(u64::try_from(patch.hold_updates.len()).ok()?)?
            .checked_add(u64::try_from(patch.turn_upserts.len()).ok()?)?
            .checked_add(u64::try_from(patch.turn_rewrites.len()).ok()?)?;
        let write_unit_count = canonical_event_count
            .checked_add(occurrence_count)?
            .checked_add(evidence_write_count)?
            .checked_add(u64::try_from(patch.delete_event_ids.len()).ok()?)?
            .checked_add(u64::try_from(patch.delete_markers.len()).ok()?)?
            .checked_add(u64::try_from(patch.delete_windows.len()).ok()?)?
            .checked_add(u64::try_from(patch.delete_holds.len()).ok()?)?;
        Some(PatchCounts {
            canonical_event_count,
            occurrence_count,
            evidence_write_count,
            write_unit_count,
        })
    }

    pub(crate) fn fold(&mut self) {
        self.events = fold_rows(std::mem::take(&mut self.events), |row| row.event_id.clone());
        self.occurrences = fold_rows(std::mem::take(&mut self.occurrences), |row| {
            (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
            )
        });
        self.facts = fold_rows(std::mem::take(&mut self.facts), |row| row.event_id.clone());
        self.marker_updates = fold_rows(std::mem::take(&mut self.marker_updates), |row| {
            (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
            )
        });
        self.window_updates = fold_rows(std::mem::take(&mut self.window_updates), |row| {
            (
                row.source_file_id,
                row.file_generation,
                row.source_start_offset,
            )
        });
        self.hold_updates = fold_rows(std::mem::take(&mut self.hold_updates), |row| {
            (
                row.source_file_id,
                row.file_generation,
                row.event_id.clone(),
            )
        });
        self.turn_upserts = fold_rows(std::mem::take(&mut self.turn_upserts), |row| {
            row.key.clone()
        });
        let mut rewrites: Vec<TurnRewrite> = Vec::with_capacity(self.turn_rewrites.len());
        let mut rewrite_positions: BTreeMap<PersistedTurnKey, usize> = BTreeMap::new();
        for rewrite in std::mem::take(&mut self.turn_rewrites) {
            let key = rewrite.replacement.key.clone();
            if let Some(index) = rewrite_positions.get(&key).copied() {
                rewrites[index].replacement = rewrite.replacement;
            } else {
                rewrite_positions.insert(key, rewrites.len());
                rewrites.push(rewrite);
            }
        }
        self.turn_rewrites = rewrites;
        self.turn_upserts.retain(|row| {
            !self
                .turn_rewrites
                .iter()
                .any(|rewrite| rewrite.replacement.key == row.key)
        });

        self.delete_event_ids = fold_rows(std::mem::take(&mut self.delete_event_ids), Clone::clone);
        self.delete_markers = fold_rows(std::mem::take(&mut self.delete_markers), |row| *row);
        self.delete_markers.retain(|key| {
            !self.marker_updates.iter().any(|row| {
                row.source_file_id == key.source_file_id
                    && row.file_generation == key.file_generation
                    && row.source_start_offset == key.source_start_offset
            })
        });
        self.delete_windows = fold_rows(std::mem::take(&mut self.delete_windows), |row| *row);
        self.delete_windows.retain(|key| {
            !self.window_updates.iter().any(|row| {
                row.source_file_id == key.source_file_id
                    && row.file_generation == key.file_generation
                    && row.source_start_offset == key.source_start_offset
            })
        });
        self.delete_holds = fold_rows(std::mem::take(&mut self.delete_holds), Clone::clone);
        self.delete_holds.retain(|key| {
            !self.hold_updates.iter().any(|row| {
                row.source_file_id == key.source_file_id
                    && row.file_generation == key.file_generation
                    && row.event_id == key.event_id
            })
        });
    }

    fn folded(&self) -> Self {
        let mut patch = self.clone();
        patch.fold();
        patch
    }
}

fn fold_rows<T, K: Ord>(rows: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut folded = Vec::with_capacity(rows.len());
    let mut positions = BTreeMap::new();
    for row in rows {
        let row_key = key(&row);
        if let Some(index) = positions.get(&row_key).copied() {
            folded[index] = row;
        } else {
            positions.insert(row_key, folded.len());
            folded.push(row);
        }
    }
    folded
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessResult {
    pub patch: ReconciliationPatch,
    pub updated_state: UsageSourceState,
    pub needs_rebuild: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordApplyOutcome {
    Applied,
    BudgetExceeded,
}

struct RecordCheckpoint {
    state: UsageSourceState,
    patch: ReconciliationPatch,
    event_ids: BTreeSet<String>,
    through_offset: u64,
    reconciliation: ReconciliationContext,
    active_window_key: Option<WindowKey>,
    materialized_delete_event_ids: BTreeSet<String>,
    forward_turn_end_ranges: BTreeMap<PersistedTurnKey, (u64, u64)>,
    window_record_ranges: BTreeMap<WindowKey, (u64, u64)>,
    late_response_windows: BTreeMap<ResponseKey, BTreeSet<WindowKey>>,
}

pub struct UsageProcessor {
    context: UsageContext,
    state: UsageSourceState,
    patch: ReconciliationPatch,
    event_ids: BTreeSet<String>,
    through_offset: u64,
    needs_rebuild: bool,
    original_state: UsageSourceState,
    reconciliation_baseline: ReconciliationContext,
    reconciliation: ReconciliationContext,
    active_window_key: Option<WindowKey>,
    materialized_delete_event_ids: BTreeSet<String>,
    forward_turn_end_ranges: BTreeMap<PersistedTurnKey, (u64, u64)>,
    window_record_ranges: BTreeMap<WindowKey, (u64, u64)>,
    late_response_windows: BTreeMap<ResponseKey, BTreeSet<WindowKey>>,
}

impl UsageProcessor {
    pub fn new(
        context: UsageContext,
        source_state: UsageSourceState,
        reconciliation: ReconciliationContext,
    ) -> Self {
        let through_offset = source_state
            .open_turn
            .as_ref()
            .and_then(|turn| {
                let key = PersistedTurnKey {
                    source_file_id: context.source_file_id,
                    file_generation: context.file_generation,
                    turn_key: turn.turn_key.clone(),
                };
                reconciliation
                    .affected_turns
                    .get(&key)
                    .map(|affected| affected.snapshot.state_through_offset)
            })
            .unwrap_or(0);
        Self {
            context,
            original_state: source_state.clone(),
            state: source_state,
            patch: ReconciliationPatch::default(),
            event_ids: BTreeSet::new(),
            through_offset,
            needs_rebuild: false,
            reconciliation_baseline: reconciliation.clone(),
            reconciliation,
            active_window_key: None,
            materialized_delete_event_ids: BTreeSet::new(),
            forward_turn_end_ranges: BTreeMap::new(),
            window_record_ranges: BTreeMap::new(),
            late_response_windows: BTreeMap::new(),
        }
    }

    pub fn needs_rebuild(&self) -> bool {
        self.needs_rebuild
    }

    pub fn state(&self) -> &UsageSourceState {
        &self.state
    }

    pub fn patch_write_units(&self) -> Option<u64> {
        self.patch.counts().map(|counts| counts.write_unit_count)
    }

    /// Write units the patch would commit now, including the pending upsert of
    /// the still-open Turn.
    pub fn write_units(&self) -> Option<u64> {
        let pending_open_turn = self.state.open_turn.as_ref().is_some_and(|turn| {
            let key = PersistedTurnKey {
                source_file_id: self.context.source_file_id,
                file_generation: self.context.file_generation,
                turn_key: turn.turn_key.clone(),
            };
            !self
                .patch
                .turn_upserts
                .iter()
                .any(|snapshot| snapshot.key == key)
                && !self
                    .patch
                    .turn_rewrites
                    .iter()
                    .any(|rewrite| rewrite.replacement.key == key)
        });
        self.patch
            .counts()?
            .write_unit_count
            .checked_add(if pending_open_turn { 1 } else { 0 })
    }

    /// Applies one record unless it needs more than `remaining_write_units`
    /// additional write units; then all state is left untouched.
    pub fn try_process_record(
        &mut self,
        record: UsageRecord,
        remaining_write_units: u64,
    ) -> RecordApplyOutcome {
        if self.needs_rebuild {
            return RecordApplyOutcome::Applied;
        }
        match record.ownership() {
            Ownership::ReplayedAncestor => return RecordApplyOutcome::Applied,
            Ownership::UnknownOwnership => {
                self.needs_rebuild = true;
                return RecordApplyOutcome::Applied;
            }
            Ownership::Owning { thread_id } if thread_id != &self.context.owning_thread_id => {
                self.needs_rebuild = true;
                return RecordApplyOutcome::Applied;
            }
            Ownership::Owning { .. } => {}
        }
        let Some(before) = self.write_units() else {
            self.accounting_overflow();
            return RecordApplyOutcome::Applied;
        };
        let checkpoint = RecordCheckpoint {
            state: self.state.clone(),
            patch: self.patch.clone(),
            event_ids: self.event_ids.clone(),
            through_offset: self.through_offset,
            reconciliation: self.reconciliation.clone(),
            active_window_key: self.active_window_key,
            materialized_delete_event_ids: self.materialized_delete_event_ids.clone(),
            forward_turn_end_ranges: self.forward_turn_end_ranges.clone(),
            window_record_ranges: self.window_record_ranges.clone(),
            late_response_windows: self.late_response_windows.clone(),
        };
        let start_offset = record_start_offset(&record);
        if self.apply(record).is_err() {
            self.restore(checkpoint);
            self.anomaly(AnomalyCode::ArithmeticOverflow, start_offset);
            self.block_required();
            return RecordApplyOutcome::Applied;
        }
        if self.needs_rebuild {
            return RecordApplyOutcome::Applied;
        }
        self.materialize_reconciliation_patch();
        let Some(after) = self.write_units() else {
            self.restore(checkpoint);
            self.accounting_overflow();
            return RecordApplyOutcome::Applied;
        };
        let additional = if after > before {
            let Some(additional) = after.checked_sub(before) else {
                self.restore(checkpoint);
                self.accounting_overflow();
                return RecordApplyOutcome::Applied;
            };
            additional
        } else {
            0
        };
        if additional > remaining_write_units {
            self.restore(checkpoint);
            return RecordApplyOutcome::BudgetExceeded;
        }
        RecordApplyOutcome::Applied
    }

    pub fn finish(mut self) -> ProcessResult {
        if self.needs_rebuild {
            let anomalies = std::mem::take(&mut self.patch.anomalies);
            return ProcessResult {
                patch: ReconciliationPatch {
                    anomalies,
                    ..ReconciliationPatch::default()
                },
                updated_state: self.original_state,
                needs_rebuild: true,
            };
        }
        if let Some(turn) = self.state.open_turn.clone() {
            let snapshot = self.snapshot(&turn, PersistedTurnStatus::Open, None, None);
            self.upsert_turn(snapshot);
        }
        self.materialize_reconciliation_patch();
        self.patch.fold();
        if self.write_units().is_none() {
            self.accounting_overflow();
            let anomalies = std::mem::take(&mut self.patch.anomalies);
            return ProcessResult {
                patch: ReconciliationPatch {
                    anomalies,
                    ..ReconciliationPatch::default()
                },
                updated_state: self.original_state,
                needs_rebuild: true,
            };
        }
        ProcessResult {
            patch: self.patch,
            updated_state: self.state,
            needs_rebuild: false,
        }
    }

    fn restore(&mut self, checkpoint: RecordCheckpoint) {
        self.state = checkpoint.state;
        self.patch = checkpoint.patch;
        self.event_ids = checkpoint.event_ids;
        self.through_offset = checkpoint.through_offset;
        self.reconciliation = checkpoint.reconciliation;
        self.active_window_key = checkpoint.active_window_key;
        self.materialized_delete_event_ids = checkpoint.materialized_delete_event_ids;
        self.forward_turn_end_ranges = checkpoint.forward_turn_end_ranges;
        self.window_record_ranges = checkpoint.window_record_ranges;
        self.late_response_windows = checkpoint.late_response_windows;
    }

    fn snapshot(
        &self,
        turn: &TurnState,
        status: PersistedTurnStatus,
        ended_at_ms: Option<i64>,
        end_offset: Option<u64>,
    ) -> PersistedTurnSnapshot {
        let quality_status = if turn.blocks.allowed() && !turn.unresolved_model_seen {
            "complete"
        } else {
            "partial"
        };
        PersistedTurnSnapshot {
            key: PersistedTurnKey {
                source_file_id: self.context.source_file_id,
                file_generation: self.context.file_generation,
                turn_key: turn.turn_key.clone(),
            },
            owning_thread_id: self.context.owning_thread_id.clone(),
            state: turn.clone(),
            status,
            ended_at_ms,
            end_offset,
            quality_status: quality_status.to_owned(),
            state_through_offset: self.through_offset,
        }
    }

    fn upsert_turn(&mut self, snapshot: PersistedTurnSnapshot) {
        match self
            .patch
            .turn_upserts
            .iter()
            .position(|existing| existing.key == snapshot.key)
        {
            Some(index) => {
                self.patch.turn_upserts[index] = snapshot;
            }
            None => {
                self.patch.turn_upserts.push(snapshot);
            }
        }
    }

    fn materialize_reconciliation_patch(&mut self) {
        let baseline = reconciliation_references(&self.reconciliation_baseline);
        let current = reconciliation_references(&self.reconciliation);
        let baseline_compensations = compensation_references(&self.reconciliation_baseline);
        let current_compensations = compensation_references(&self.reconciliation);
        let mut baseline_events = baseline
            .keys()
            .chain(baseline_compensations.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let current_events = current
            .keys()
            .chain(current_compensations.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        baseline_events.retain(|event_id| !current_events.contains(event_id));
        let delete_event_ids = baseline_events;

        let mut events = current
            .iter()
            .filter_map(|(event_id, (proposal, _))| {
                (baseline.get(event_id).map(|(old, _)| old) != Some(proposal))
                    .then(|| proposal.clone())
            })
            .collect::<Vec<_>>();
        events.extend(
            current_compensations
                .iter()
                .filter_map(|(event_id, proposal)| {
                    (baseline_compensations.get(event_id) != Some(proposal))
                        .then(|| proposal.clone())
                }),
        );
        let mut facts = current
            .iter()
            .filter_map(|(event_id, (_, fact))| {
                (baseline.get(event_id).map(|(_, old)| old) != Some(fact)).then(|| fact.clone())
            })
            .collect::<Vec<_>>();
        facts.extend(
            current_compensations
                .iter()
                .filter_map(|(event_id, proposal)| {
                    (baseline_compensations.get(event_id) != Some(proposal))
                        .then(|| compensation_fact(proposal))
                }),
        );

        // Forward Turn closes are not part of the preloaded durable context.
        // Keep their staged compensation rows while deriving existing Turn
        // compensation writes from the original context difference.
        let direct_event_ids = self
            .patch
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.source_file_id == self.context.source_file_id
                    && occurrence.file_generation == self.context.file_generation
            })
            .map(|occurrence| occurrence.event_id.clone())
            .collect::<BTreeSet<_>>();
        let mut direct_events = self
            .patch
            .events
            .iter()
            .filter(|event| {
                event.kind == EventKind::TurnCompensation
                    && direct_event_ids.contains(&event.event_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let direct_event_ids = direct_events
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<BTreeSet<_>>();
        let mut direct_occurrences = self
            .patch
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.source_file_id == self.context.source_file_id
                    && occurrence.file_generation == self.context.file_generation
                    && direct_event_ids.contains(&occurrence.event_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut direct_facts = self
            .patch
            .facts
            .iter()
            .filter(|fact| {
                direct_event_ids.contains(&fact.event_id)
                    && self.patch.events.iter().any(|event| {
                        event.event_id == fact.event_id && event.kind == EventKind::TurnCompensation
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let baseline_occurrences = reconciliation_occurrences(&self.reconciliation_baseline);
        let current_occurrences = reconciliation_occurrences(&self.reconciliation);
        let mut occurrences = Vec::new();
        for (key, occurrence) in &current_occurrences {
            if baseline_occurrences.get(key) != Some(occurrence) {
                occurrences.push(occurrence.clone());
            }
        }

        let baseline_markers = marker_map(&self.reconciliation_baseline.markers);
        let current_markers = marker_map(&self.reconciliation.markers);
        let marker_updates = current_markers
            .iter()
            .filter_map(|(key, marker)| {
                (baseline_markers.get(key) != Some(marker)).then(|| marker.clone())
            })
            .collect::<Vec<_>>();
        let delete_markers = baseline_markers
            .keys()
            .filter(|key| !current_markers.contains_key(key))
            .map(
                |(source_file_id, file_generation, source_start_offset)| UsagePrivateRowKey {
                    source_file_id: *source_file_id,
                    file_generation: *file_generation,
                    source_start_offset: *source_start_offset,
                },
            )
            .collect::<Vec<_>>();

        let baseline_windows = &self.reconciliation_baseline.windows;
        let current_windows = &self.reconciliation.windows;
        let window_updates = current_windows
            .iter()
            .filter_map(|(key, state)| {
                if baseline_windows.get(key) == Some(state) {
                    return None;
                }
                let metadata = self.reconciliation.window_metadata.get(key)?;
                Some(LegacyWindowWrite {
                    source_file_id: metadata.source_file_id,
                    file_generation: metadata.file_generation,
                    source_start_offset: metadata.source_start_offset,
                    source_end_offset: metadata.source_end_offset,
                    owning_thread_id: metadata.owning_thread_id.clone(),
                    turn_key: metadata.turn_key.clone(),
                    state: state.clone(),
                })
            })
            .collect::<Vec<_>>();
        let delete_windows = baseline_windows
            .keys()
            .filter(|key| !current_windows.contains_key(key))
            .map(|key| UsagePrivateRowKey {
                source_file_id: key.source_file_id,
                file_generation: key.file_generation,
                source_start_offset: key.start_offset,
            })
            .collect::<Vec<_>>();

        self.patch
            .delete_event_ids
            .retain(|event_id| !self.materialized_delete_event_ids.contains(event_id));
        self.materialized_delete_event_ids = delete_event_ids.clone();
        self.patch
            .delete_event_ids
            .extend(delete_event_ids.into_iter());

        direct_events.extend(events);
        direct_occurrences.extend(occurrences);
        direct_facts.extend(facts);
        self.patch.events = direct_events;
        self.patch.occurrences = direct_occurrences;
        self.patch.facts = direct_facts;
        self.patch.marker_updates = marker_updates;
        self.patch.delete_markers = delete_markers;
        self.patch.window_updates = window_updates;
        self.patch.delete_windows = delete_windows;
        self.patch.turn_rewrites = self
            .reconciliation
            .affected_turns
            .iter()
            .filter_map(|(key, affected)| {
                let expected = self
                    .reconciliation_baseline
                    .affected_turns
                    .get(key)?
                    .snapshot
                    .clone();
                (expected.status != PersistedTurnStatus::Open && expected != affected.snapshot)
                    .then(|| TurnRewrite {
                        expected,
                        replacement: affected.snapshot.clone(),
                    })
            })
            .collect();
    }

    fn observe_offset(&mut self, offset: u64) {
        self.through_offset = self.through_offset.max(offset);
    }

    pub fn observe_consumed_offset(&mut self, end_offset: u64) {
        self.observe_offset(end_offset);
    }

    fn accounting_overflow(&mut self) {
        self.anomaly(AnomalyCode::ArithmeticOverflow, None);
        self.block_required();
        self.needs_rebuild = true;
    }

    fn apply(&mut self, record: UsageRecord) -> Result<(), ProcessorError> {
        match record {
            UsageRecord::ResponseUsage {
                timestamp_ms,
                start_offset,
                end_offset,
                evidence,
                ..
            } => {
                let pending_record = PendingEvidenceRecord::ResponseUsage {
                    timestamp_ms,
                    start_offset,
                    end_offset,
                    evidence: evidence.clone(),
                };
                self.response_usage(
                    timestamp_ms,
                    start_offset,
                    end_offset,
                    evidence,
                    CodexOperation::Response,
                    pending_record,
                )?;
            }
            UsageRecord::Compacted {
                timestamp_ms,
                start_offset,
                end_offset,
                evidence,
                ..
            } => self.compacted(timestamp_ms, start_offset, end_offset, evidence)?,
            UsageRecord::TurnContext {
                model,
                reasoning_effort,
                ..
            } => {
                self.state.active_model = model.filter(|model| !model.trim().is_empty());
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
            } => {
                self.observe_offset(start_offset);
                self.start_turn(turn_id, timestamp_ms, start_offset);
            }
            UsageRecord::TokenCount {
                timestamp_ms,
                start_offset,
                end_offset,
                total,
                last,
                ..
            } => {
                self.observe_offset(end_offset);
                self.token_count(timestamp_ms, start_offset, end_offset, total, last)?;
            }
            UsageRecord::TurnEnded {
                turn_id,
                timestamp_ms,
                start_offset,
                end_offset,
                status,
                ..
            } => {
                self.observe_offset(end_offset);
                self.end_turn(turn_id, timestamp_ms, start_offset, end_offset, status)?;
            }
            UsageRecord::Gap {
                kind, end_offset, ..
            } => {
                self.observe_offset(end_offset);
                self.gap(kind);
            }
        }
        Ok(())
    }

    fn response_usage(
        &mut self,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        evidence: ResponseUsageEvidence,
        operation: CodexOperation,
        pending_record: PendingEvidenceRecord,
    ) -> Result<(), ProcessorError> {
        self.observe_offset(end_offset);
        if evidence
            .thread_id
            .as_deref()
            .is_some_and(|thread_id| thread_id != self.context.owning_thread_id)
        {
            self.fatal_anomaly(AnomalyCode::ResponseOwnershipMismatch, start_offset);
            return Ok(());
        }
        if !valid_identity(&evidence.response_id) {
            self.fatal_anomaly(AnomalyCode::ResponseOwnershipMismatch, start_offset);
            return Ok(());
        }
        let key = ResponseKey {
            owning_thread_id: self.context.owning_thread_id.clone(),
            response_id: evidence.response_id.clone(),
        };
        let incoming_usage = match &evidence.usage {
            UsageValue::Valid(usage) => Some(usage.clone()),
            UsageValue::Missing | UsageValue::Invalid => None,
        };
        let pending_usage = self
            .state
            .reconciliation_carry
            .pending_evidence
            .iter()
            .find(|pending| pending_response_id(pending) == Some(evidence.response_id.as_str()))
            .and_then(|pending| pending_usage_value(pending).cloned());
        if let (Some(usage), Some(UsageValue::Valid(pending))) =
            (incoming_usage.as_ref(), pending_usage.as_ref())
            && usage != pending
        {
            self.fatal_anomaly(AnomalyCode::ResponseUsageConflict, start_offset);
            return Ok(());
        }
        if let (Some(usage), Some(existing)) = (
            incoming_usage.as_ref(),
            self.reconciliation.bindings.get(&key),
        ) && existing.proposal.usage != *usage
        {
            self.fatal_anomaly(AnomalyCode::ResponseUsageConflict, start_offset);
            return Ok(());
        }

        let existing = self.reconciliation.bindings.get(&key).cloned();
        let has_pending_identity = pending_usage.is_some();
        let duplicate = existing.is_some() || has_pending_identity;
        self.validate_thread_counter(&evidence, duplicate, start_offset);

        let Some(usage) = incoming_usage else {
            if operation == CodexOperation::Response {
                self.anomaly(AnomalyCode::RequiredTotalInvalid, Some(start_offset));
                self.gap(GapKind::RequiredInvalid);
            }
            return Ok(());
        };
        if existing.is_none() && has_pending_identity {
            // Keep the first unresolved record's physical range and context.
            // A later TurnContext must never supply attribution for it.
            return Ok(());
        }

        if let Some(binding) = existing.as_ref()
            && (binding.fact.owning_thread_id != self.context.owning_thread_id
                || binding.fact.response_id.as_deref() != Some(evidence.response_id.as_str())
                || binding.fact.evidence_kind != EvidenceKind::Explicit)
        {
            self.fatal_anomaly(AnomalyCode::ResponseOwnershipMismatch, start_offset);
            return Ok(());
        }

        let model = existing
            .as_ref()
            .map(|binding| binding.proposal.model.clone())
            .or_else(|| self.state.active_model.clone());
        let occurred_at_ms = existing
            .as_ref()
            .map(|binding| binding.proposal.occurred_at_ms)
            .or(timestamp_ms);
        if existing.is_none() && (occurred_at_ms.is_none() || model.is_none()) {
            self.store_pending_response(
                pending_record,
                model,
                self.state.active_reasoning_effort.clone(),
            );
            return Ok(());
        }

        let turn_key = existing
            .as_ref()
            .and_then(|binding| binding.proposal.turn_key.clone())
            .or_else(|| {
                evidence.turn_id.as_deref().map(|turn_id| {
                    turn_key_for(
                        &self.context.owning_thread_id,
                        Some(turn_id),
                        start_offset,
                        occurred_at_ms,
                    )
                })
            })
            .or_else(|| {
                self.state
                    .open_turn
                    .as_ref()
                    .map(|turn| turn.turn_key.clone())
            });
        let turn_key = match turn_key {
            Some(turn_key) => Some(turn_key),
            None => self.infer_turn_key_for_response(start_offset)?,
        };
        let response_id = evidence.response_id.clone();
        let (proposal, mut fact) = if let Some(existing) = existing {
            // The first durable evidence owns the canonical time and model.
            let mut fact = existing.fact.clone();
            if existing.fact.operation == CodexOperation::Compaction
                || operation == CodexOperation::Compaction
                || self.has_compaction_marker(&key)
            {
                fact.operation = CodexOperation::Compaction;
            } else {
                fact.operation = existing.fact.operation;
            }
            (existing.proposal, fact)
        } else {
            let Some(model) = model else {
                self.store_pending_response(
                    pending_record,
                    None,
                    self.state.active_reasoning_effort.clone(),
                );
                return Ok(());
            };
            let Some(occurred_at_ms) = occurred_at_ms else {
                self.store_pending_response(
                    pending_record,
                    Some(model),
                    self.state.active_reasoning_effort.clone(),
                );
                return Ok(());
            };
            let mut proposal = CanonicalUsageProposal {
                event_id: response_event_id(&self.context.owning_thread_id, &response_id),
                kind: EventKind::Normal,
                occurred_at_ms,
                thread_id: self.context.owning_thread_id.clone(),
                root_session_id: self.context.root_session_id.clone(),
                turn_key: turn_key.clone(),
                model,
                reasoning_effort: self.state.active_reasoning_effort.clone(),
                usage,
            };
            // Explicit response identity is independent of operation
            // classification; every explicit canonical is normal usage.
            proposal.kind = EventKind::Normal;
            let mut fact = UsageEventFact {
                event_id: proposal.event_id.clone(),
                owning_thread_id: self.context.owning_thread_id.clone(),
                response_id: Some(response_id.clone()),
                evidence_kind: EvidenceKind::Explicit,
                operation,
            };
            if self.has_compaction_marker(&key) {
                fact.operation = CodexOperation::Compaction;
            }
            (proposal, fact)
        };
        fact.event_id = proposal.event_id.clone();
        let occurrence = Occurrence {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: start_offset,
            source_end_offset: end_offset,
            event_id: proposal.event_id.clone(),
        };
        self.reconciliation
            .bindings
            .insert(key.clone(), ResponseBinding { proposal, fact });
        let occurrences = self
            .reconciliation
            .response_occurrences
            .entry(key.clone())
            .or_default();
        if !occurrences
            .iter()
            .any(|existing| occurrence_key(existing) == occurrence_key(&occurrence))
        {
            occurrences.push(occurrence);
        }
        self.reconciliation
            .closure_response_keys
            .insert(key.clone());
        self.state
            .reconciliation_carry
            .pending_response_ids
            .retain(|id| id != &response_id);
        self.state
            .reconciliation_carry
            .pending_evidence
            .retain(|pending| pending_response_id(pending) != Some(response_id.as_str()));
        self.attach_response_to_window(
            &key,
            turn_key.as_deref(),
            evidence.turn_id.as_deref(),
            start_offset,
        )?;
        if self.needs_rebuild {
            return Ok(());
        }
        self.resolve_markers(&key);
        if let (Some(turn), Some(turn_key)) = (&mut self.state.open_turn, turn_key.as_deref())
            && turn.turn_key == turn_key
        {
            let binding = self.reconciliation.bindings.get(&key).unwrap();
            observe_turn_model(turn, &binding.proposal.model);
            observe_turn_reasoning_effort(turn, binding.proposal.reasoning_effort.as_deref());
        }
        self.reconcile_affected_turn(turn_key.as_deref(), start_offset)?;
        Ok(())
    }

    fn compacted(
        &mut self,
        timestamp_ms: Option<i64>,
        start_offset: u64,
        end_offset: u64,
        evidence: CompactionEvidence,
    ) -> Result<(), ProcessorError> {
        self.observe_offset(end_offset);
        let embedded_id = evidence
            .latest_token_usage_record
            .as_ref()
            .map(|latest| latest.response_id.as_str());
        if let (Some(marker_id), Some(embedded_id)) =
            (evidence.compaction_response_id.as_deref(), embedded_id)
            && marker_id != embedded_id
        {
            self.fatal_anomaly(AnomalyCode::CompactionIdentityMismatch, start_offset);
            return Ok(());
        }
        let response_id = evidence
            .compaction_response_id
            .clone()
            .or_else(|| embedded_id.map(str::to_owned));
        if response_id.as_deref().is_some_and(|id| !valid_identity(id)) {
            self.fatal_anomaly(AnomalyCode::CompactionIdentityMismatch, start_offset);
            return Ok(());
        }
        if let Some(latest) = evidence.latest_token_usage_record.clone() {
            if latest
                .thread_id
                .as_deref()
                .is_some_and(|thread_id| thread_id != self.context.owning_thread_id)
            {
                self.fatal_anomaly(AnomalyCode::ResponseOwnershipMismatch, start_offset);
                return Ok(());
            }
            if matches!(&latest.usage, UsageValue::Valid(_)) {
                self.response_usage(
                    timestamp_ms,
                    start_offset,
                    end_offset,
                    latest,
                    CodexOperation::Compaction,
                    PendingEvidenceRecord::Compacted {
                        timestamp_ms,
                        start_offset,
                        end_offset,
                        evidence: evidence.clone(),
                    },
                )?;
                if self.needs_rebuild {
                    return Ok(());
                }
            }
        }

        let key = response_id.as_ref().map(|response_id| ResponseKey {
            owning_thread_id: self.context.owning_thread_id.clone(),
            response_id: response_id.clone(),
        });
        if let Some(key) = key.as_ref()
            && self.reconciliation.bindings.contains_key(key)
        {
            self.promote_compaction(key);
        }
        let marker_unknown = if response_id.is_none() {
            Some(MarkerUnknownReason::IdentityMissing)
        } else if key
            .as_ref()
            .and_then(|key| self.reconciliation.bindings.get(key))
            .is_some()
        {
            None
        } else {
            match evidence
                .latest_token_usage_record
                .as_ref()
                .map(|latest| &latest.usage)
            {
                None | Some(UsageValue::Missing) => Some(MarkerUnknownReason::UsageMissing),
                Some(UsageValue::Invalid) => Some(MarkerUnknownReason::UsageInvalid),
                Some(UsageValue::Valid(_)) if timestamp_ms.is_none() => {
                    Some(MarkerUnknownReason::TimeMissing)
                }
                Some(UsageValue::Valid(_)) if self.state.active_model.is_none() => {
                    Some(MarkerUnknownReason::ModelUnresolved)
                }
                Some(UsageValue::Valid(_)) => Some(MarkerUnknownReason::UsageMissing),
            }
        };
        let marker = CompactionMarkerWrite {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: start_offset,
            source_end_offset: end_offset,
            owning_thread_id: self.context.owning_thread_id.clone(),
            root_session_id: self.context.root_session_id.clone(),
            occurred_at_ms: timestamp_ms,
            model: self.state.active_model.clone(),
            reasoning_effort: self.state.active_reasoning_effort.clone(),
            response_id,
            resolved_event_id: key.as_ref().and_then(|key| {
                self.reconciliation
                    .bindings
                    .get(key)
                    .filter(|binding| binding.fact.operation == CodexOperation::Compaction)
                    .map(|binding| binding.proposal.event_id.clone())
            }),
            unknown_reason: marker_unknown,
        };
        self.upsert_marker(marker);
        if let Some(key) = key {
            if self.reconciliation.bindings.contains_key(&key) {
                self.promote_compaction(&key);
                self.resolve_markers(&key);
                let turn_key = self
                    .reconciliation
                    .bindings
                    .get(&key)
                    .and_then(|binding| binding.proposal.turn_key.clone());
                self.reconcile_affected_turn(turn_key.as_deref(), start_offset)?;
            }
        }
        Ok(())
    }

    fn validate_thread_counter(
        &mut self,
        evidence: &ResponseUsageEvidence,
        duplicate: bool,
        offset: u64,
    ) {
        let domain = (
            evidence
                .thread_id
                .clone()
                .unwrap_or_else(|| self.context.owning_thread_id.clone()),
            evidence.session_id.clone(),
        );
        let same_domain = self
            .state
            .reconciliation_carry
            .modern_counter_domain
            .as_ref()
            == Some(&domain);
        match &evidence.thread_token_usage {
            UsageValue::Missing => {
                if !same_domain {
                    self.state.reconciliation_carry.modern_counter_domain = Some(domain);
                    self.state.reconciliation_carry.modern_counter_total = None;
                }
            }
            UsageValue::Invalid => {
                self.anomaly(AnomalyCode::ThreadUsageMismatch, Some(offset));
                self.state.reconciliation_carry.modern_counter_domain = Some(domain);
                self.state.reconciliation_carry.modern_counter_total = None;
            }
            UsageValue::Valid(current) => {
                let previous = same_domain
                    .then(|| self.state.reconciliation_carry.modern_counter_total.clone())
                    .flatten();
                if let Some(previous) = previous {
                    if duplicate && current == &previous {
                        return;
                    }
                    let required_reset = required_decreased_from(current, &previous);
                    let cache_reset = cache_decreased_from(current, &previous);
                    if required_reset || cache_reset {
                        if !duplicate {
                            self.anomaly(AnomalyCode::ThreadUsageMismatch, Some(offset));
                        }
                    } else if let UsageValue::Valid(usage) = &evidence.usage {
                        match processor_checked_sub(current, &previous) {
                            Ok(delta) if delta == *usage => {}
                            _ => {
                                self.anomaly(AnomalyCode::ThreadUsageMismatch, Some(offset));
                                self.state.reconciliation_carry.modern_counter_domain =
                                    Some(domain);
                                self.state.reconciliation_carry.modern_counter_total = None;
                                return;
                            }
                        }
                    } else {
                        self.anomaly(AnomalyCode::ThreadUsageMismatch, Some(offset));
                        self.state.reconciliation_carry.modern_counter_domain = Some(domain);
                        self.state.reconciliation_carry.modern_counter_total = None;
                        return;
                    }
                } else if let UsageValue::Valid(usage) = &evidence.usage
                    && processor_checked_sub(current, usage).is_err()
                {
                    self.anomaly(AnomalyCode::ThreadUsageMismatch, Some(offset));
                    self.state.reconciliation_carry.modern_counter_domain = Some(domain);
                    self.state.reconciliation_carry.modern_counter_total = None;
                    return;
                }
                self.state.reconciliation_carry.modern_counter_domain = Some(domain);
                self.state.reconciliation_carry.modern_counter_total = Some(current.clone());
            }
        }
    }

    fn store_pending_response(
        &mut self,
        record: PendingEvidenceRecord,
        model: Option<String>,
        reasoning_effort: Option<String>,
    ) {
        let Some(response_id) = pending_record_response_id(&record).map(str::to_owned) else {
            return;
        };
        let start_offset = match &record {
            PendingEvidenceRecord::ResponseUsage { start_offset, .. }
            | PendingEvidenceRecord::Compacted { start_offset, .. } => *start_offset,
        };
        let carry = &mut self.state.reconciliation_carry;
        if !carry
            .pending_evidence
            .iter()
            .any(|pending| pending_response_id(pending) == Some(response_id.as_str()))
        {
            carry.pending_evidence.push(PendingUsageEvidence {
                record,
                model,
                reasoning_effort,
            });
        }
        if !carry
            .pending_response_ids
            .iter()
            .any(|id| id == &response_id)
        {
            carry.pending_response_ids.push(response_id);
            carry.pending_response_ids.sort();
        }
        self.ensure_open_window(start_offset);
    }

    fn close_pending_evidence(&mut self, turn_id: Option<&str>, boundary_offset: u64) {
        let mut closed_ids = BTreeSet::new();
        let mut keep = Vec::new();
        for pending in std::mem::take(&mut self.state.reconciliation_carry.pending_evidence) {
            let (pending_turn, start_offset, timestamp_missing) = match &pending.record {
                PendingEvidenceRecord::ResponseUsage {
                    start_offset,
                    timestamp_ms,
                    evidence,
                    ..
                } => (
                    evidence.turn_id.as_deref(),
                    *start_offset,
                    timestamp_ms.is_none(),
                ),
                PendingEvidenceRecord::Compacted {
                    start_offset,
                    timestamp_ms,
                    evidence,
                    ..
                } => (
                    evidence
                        .latest_token_usage_record
                        .as_ref()
                        .and_then(|latest| latest.turn_id.as_deref()),
                    *start_offset,
                    timestamp_ms.is_none(),
                ),
            };
            let closes = start_offset < boundary_offset
                && match turn_id {
                    Some(expected) => pending_turn.is_none_or(|actual| actual == expected),
                    None => true,
                };
            if closes {
                if timestamp_missing {
                    self.anomaly(AnomalyCode::UsageTimeMissing, Some(start_offset));
                } else {
                    self.anomaly(AnomalyCode::LegacyCoverageAmbiguous, Some(start_offset));
                }
                match pending.record {
                    PendingEvidenceRecord::ResponseUsage { evidence, .. } => {
                        closed_ids.insert(evidence.response_id);
                    }
                    PendingEvidenceRecord::Compacted { evidence, .. } => {
                        if let Some(latest) = evidence.latest_token_usage_record {
                            closed_ids.insert(latest.response_id);
                        }
                    }
                }
            } else {
                keep.push(pending);
            }
        }
        self.state.reconciliation_carry.pending_evidence = keep;
        self.state
            .reconciliation_carry
            .pending_response_ids
            .retain(|id| !closed_ids.contains(id));
    }

    fn has_compaction_marker(&self, key: &ResponseKey) -> bool {
        self.reconciliation.markers.iter().any(|marker| {
            marker.owning_thread_id == key.owning_thread_id
                && marker.response_id.as_deref() == Some(key.response_id.as_str())
        })
    }

    fn upsert_marker(&mut self, marker: CompactionMarkerWrite) {
        if let Some(existing) = self.reconciliation.markers.iter_mut().find(|existing| {
            existing.source_file_id == marker.source_file_id
                && existing.file_generation == marker.file_generation
                && existing.source_start_offset == marker.source_start_offset
        }) {
            *existing = marker;
        } else {
            self.reconciliation.markers.push(marker);
        }
        self.reconciliation.markers.sort_by_key(|marker| {
            (
                marker.source_file_id,
                marker.file_generation,
                marker.source_start_offset,
            )
        });
    }

    fn promote_compaction(&mut self, key: &ResponseKey) {
        if let Some(binding) = self.reconciliation.bindings.get_mut(key) {
            binding.fact.operation = CodexOperation::Compaction;
        }
    }

    fn resolve_markers(&mut self, key: &ResponseKey) {
        let Some(binding) = self.reconciliation.bindings.get(key) else {
            return;
        };
        if binding.fact.operation != CodexOperation::Compaction {
            return;
        }
        let event_id = binding.proposal.event_id.clone();
        for marker in &mut self.reconciliation.markers {
            if marker.owning_thread_id == key.owning_thread_id
                && marker.response_id.as_deref() == Some(key.response_id.as_str())
            {
                marker.resolved_event_id = Some(event_id.clone());
                marker.unknown_reason = None;
            }
        }
    }

    fn ensure_open_window(&mut self, start_offset: u64) -> WindowKey {
        let start = *self
            .state
            .reconciliation_carry
            .open_window_start_offset
            .get_or_insert(start_offset);
        self.active_window_key = Some(WindowKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            start_offset: start,
        });
        WindowKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            start_offset: start,
        }
    }

    fn infer_turn_key_for_response(
        &mut self,
        response_offset: u64,
    ) -> Result<Option<String>, ProcessorError> {
        let exact = self
            .reconciliation
            .window_metadata
            .values()
            .filter(|metadata| {
                metadata.owning_thread_id == self.context.owning_thread_id
                    && metadata.source_file_id == self.context.source_file_id
                    && metadata.file_generation == self.context.file_generation
                    && metadata.source_end_offset == response_offset
            })
            .collect::<Vec<_>>();
        let matching = if exact.is_empty() {
            self.reconciliation
                .window_metadata
                .values()
                .filter(|metadata| {
                    metadata.owning_thread_id == self.context.owning_thread_id
                        && metadata.source_file_id == self.context.source_file_id
                        && metadata.file_generation == self.context.file_generation
                        && response_offset >= metadata.source_start_offset
                        && response_offset < metadata.source_end_offset
                })
                .collect::<Vec<_>>()
        } else {
            exact
        };
        match matching.as_slice() {
            [] => Ok(None),
            [metadata] => Ok(metadata.turn_key.clone()),
            _ => {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, response_offset);
                Ok(None)
            }
        }
    }

    fn attach_response_to_window(
        &mut self,
        key: &ResponseKey,
        turn_key: Option<&str>,
        response_turn_id: Option<&str>,
        response_offset: u64,
    ) -> Result<(), ProcessorError> {
        let mut exact_windows = Vec::new();
        let mut contained_windows = Vec::new();
        let mut after_boundary_new_call = false;
        for (window_key, metadata) in &self.reconciliation.window_metadata {
            if metadata.source_file_id != self.context.source_file_id
                || metadata.file_generation != self.context.file_generation
                || metadata.owning_thread_id != key.owning_thread_id
                || metadata.turn_key.as_deref() != turn_key
            {
                continue;
            }
            if response_offset >= metadata.source_end_offset {
                let durable = self
                    .reconciliation_baseline
                    .windows
                    .get(window_key)
                    .is_some_and(|window| window.explicit_response_ids.contains(&key.response_id));
                let adjacent = self.response_adjacent_to_legacy(
                    *window_key,
                    &key.response_id,
                    self.legacy_record_range(*window_key),
                );
                let after_boundary_is_new_call =
                    adjacent && self.after_boundary_is_proven_new_call(*window_key, key)?;
                after_boundary_new_call |= after_boundary_is_new_call;
                if durable || (adjacent && !after_boundary_is_new_call) {
                    exact_windows.push(*window_key);
                }
            } else if response_offset >= metadata.source_start_offset
                && response_offset < metadata.source_end_offset
            {
                contained_windows.push(*window_key);
            }
        }
        let has_closed_durable_turn = turn_key.is_some_and(|turn_key| {
            let persisted_turn_key = PersistedTurnKey {
                source_file_id: self.context.source_file_id,
                file_generation: self.context.file_generation,
                turn_key: turn_key.to_owned(),
            };
            self.reconciliation_baseline
                .affected_turns
                .get(&persisted_turn_key)
                .is_some_and(|affected| affected.snapshot.status != PersistedTurnStatus::Open)
        });
        let matching_windows = if has_closed_durable_turn {
            Vec::new()
        } else if exact_windows.is_empty() {
            contained_windows
        } else {
            exact_windows
        };
        if matching_windows.len() > 1 {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, response_offset);
            return Ok(());
        }
        let durable_windows = self
            .reconciliation
            .windows
            .iter()
            .filter_map(|(window_key, window)| {
                let metadata = self.reconciliation.window_metadata.get(window_key)?;
                (window.explicit_response_ids.contains(&key.response_id)
                    && metadata.source_file_id == self.context.source_file_id
                    && metadata.file_generation == self.context.file_generation
                    && metadata.owning_thread_id == key.owning_thread_id
                    && metadata.turn_key.as_deref() == turn_key)
                    .then_some(*window_key)
            })
            .collect::<Vec<_>>();
        if durable_windows.len() > 1 {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, response_offset);
            return Ok(());
        }
        if matching_windows
            .first()
            .zip(durable_windows.first())
            .is_some_and(|(physical, durable)| physical != durable)
        {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, response_offset);
            return Ok(());
        }
        let window_key = if let Some(window_key) = matching_windows
            .first()
            .copied()
            .or_else(|| durable_windows.first().copied())
        {
            window_key
        } else if let Some(turn_key) = turn_key {
            if !has_closed_durable_turn
                && self
                    .state
                    .open_turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_key == turn_key)
            {
                if after_boundary_new_call {
                    self.state.reconciliation_carry.open_window_start_offset =
                        Some(response_offset);
                }
                self.ensure_open_window(response_offset)
            } else {
                let persisted_turn_key = PersistedTurnKey {
                    source_file_id: self.context.source_file_id,
                    file_generation: self.context.file_generation,
                    turn_key: turn_key.to_owned(),
                };
                let has_durable_turn = self
                    .reconciliation_baseline
                    .affected_turns
                    .contains_key(&persisted_turn_key);
                let has_forward_closed_turn = self
                    .forward_turn_end_ranges
                    .contains_key(&persisted_turn_key);
                let has_prior_closed_window =
                    self.reconciliation
                        .window_metadata
                        .iter()
                        .any(|(window_key, metadata)| {
                            metadata.source_file_id == self.context.source_file_id
                                && metadata.file_generation == self.context.file_generation
                                && metadata.owning_thread_id == key.owning_thread_id
                                && metadata.turn_key.as_deref() == Some(turn_key)
                                && self
                                    .reconciliation
                                    .windows
                                    .get(window_key)
                                    .is_some_and(|window| window.closed)
                        });
                if self.state.open_turn.is_none()
                    && response_turn_id == Some(turn_key)
                    && !has_durable_turn
                    && !has_forward_closed_turn
                    && (!has_prior_closed_window || after_boundary_new_call)
                    && (self
                        .state
                        .reconciliation_carry
                        .open_window_start_offset
                        .is_none()
                        || after_boundary_new_call)
                {
                    if after_boundary_new_call {
                        self.state.reconciliation_carry.open_window_start_offset =
                            Some(response_offset);
                    }
                    self.ensure_open_window(response_offset)
                } else {
                    match self.closed_late_response_windows(key, turn_key, response_offset) {
                        Ok(window_keys) => {
                            let Some(window_key) = window_keys.iter().copied().find(|window_key| {
                                window_key.source_file_id == self.context.source_file_id
                                    && window_key.file_generation == self.context.file_generation
                            }) else {
                                self.fatal_anomaly(
                                    AnomalyCode::LegacyCoverageAmbiguous,
                                    response_offset,
                                );
                                return Ok(());
                            };
                            self.late_response_windows
                                .insert(key.clone(), window_keys.into_iter().collect());
                            window_key
                        }
                        Err(()) => {
                            self.fatal_anomaly(
                                AnomalyCode::LegacyCoverageAmbiguous,
                                response_offset,
                            );
                            return Ok(());
                        }
                    }
                }
            }
        } else {
            self.ensure_open_window(response_offset)
        };
        let windows = self
            .late_response_windows
            .get(key)
            .cloned()
            .unwrap_or_else(|| BTreeSet::from([window_key]));
        let mut reconciled = false;
        for window_key in windows {
            if !self.reconciliation.windows.contains_key(&window_key) {
                if self
                    .late_response_windows
                    .get(key)
                    .is_some_and(|keys| keys.contains(&window_key))
                {
                    self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, response_offset);
                    return Ok(());
                }
                continue;
            }
            let explicit_ids = &mut self
                .reconciliation
                .windows
                .get_mut(&window_key)
                .unwrap()
                .explicit_response_ids;
            if !explicit_ids.contains(&key.response_id) {
                explicit_ids.push(key.response_id.clone());
                explicit_ids.sort();
            }
            self.reconcile_window(window_key, response_offset)?;
            if self.needs_rebuild {
                return Ok(());
            }
            if self
                .late_response_windows
                .get(key)
                .is_some_and(|keys| keys.contains(&window_key))
                && !self
                    .reconciliation
                    .windows
                    .get(&window_key)
                    .is_some_and(|window| {
                        window
                            .legacy_covered_response_ids
                            .contains(&key.response_id)
                    })
            {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, response_offset);
                return Ok(());
            }
            reconciled = true;
        }
        if !reconciled
            && !self
                .state
                .reconciliation_carry
                .pending_response_ids
                .contains(&key.response_id)
        {
            self.state
                .reconciliation_carry
                .pending_response_ids
                .push(key.response_id.clone());
            self.state.reconciliation_carry.pending_response_ids.sort();
        }
        Ok(())
    }

    fn closed_late_response_window(
        &self,
        response_key: &ResponseKey,
        turn_key: &str,
        response_offset: u64,
    ) -> Result<WindowKey, ()> {
        let persisted_key = PersistedTurnKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            turn_key: turn_key.to_owned(),
        };
        let (_, compensation_occurrences) = self.compensation_closure_for_turn(&persisted_key)?;
        self.closed_late_response_window_for_turn(
            &persisted_key,
            response_key,
            Some(response_offset),
            &compensation_occurrences,
        )
    }

    fn closed_late_response_window_for_turn(
        &self,
        persisted_key: &PersistedTurnKey,
        response_key: &ResponseKey,
        response_offset: Option<u64>,
        compensation_occurrences: &[Occurrence],
    ) -> Result<WindowKey, ()> {
        let affected = self
            .reconciliation_baseline
            .affected_turns
            .get(persisted_key)
            .ok_or(())?;
        let snapshot = &affected.snapshot;
        if snapshot.status == PersistedTurnStatus::Open
            || snapshot.owning_thread_id != response_key.owning_thread_id
            || snapshot.state.raw_turn_id.as_deref() != Some(persisted_key.turn_key.as_str())
        {
            return Err(());
        }
        let turn_end_offset = snapshot.end_offset.ok_or(())?;
        if response_offset.is_some_and(|offset| offset < turn_end_offset)
            || (persisted_key.source_file_id == self.context.source_file_id
                && persisted_key.file_generation == self.context.file_generation
                && self
                    .state
                    .open_turn
                    .as_ref()
                    .is_some_and(|turn| turn.turn_key == persisted_key.turn_key))
        {
            return Err(());
        }
        let local_compensations = compensation_occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.source_file_id == persisted_key.source_file_id
                    && occurrence.file_generation == persisted_key.file_generation
            })
            .collect::<Vec<_>>();
        let cutoff = match local_compensations.as_slice() {
            [occurrence] if occurrence.source_end_offset == turn_end_offset => {
                occurrence.source_start_offset
            }
            [] => turn_end_offset,
            _ => return Err(()),
        };

        let mut candidates = Vec::new();
        for (window_key, metadata) in &self.reconciliation_baseline.window_metadata {
            if metadata.source_file_id != persisted_key.source_file_id
                || metadata.file_generation != persisted_key.file_generation
                || metadata.owning_thread_id != snapshot.owning_thread_id
                || metadata.turn_key.as_deref() != Some(persisted_key.turn_key.as_str())
            {
                continue;
            }
            let Some(window) = self.reconciliation_baseline.windows.get(window_key) else {
                return Err(());
            };
            let proposals = self
                .reconciliation_baseline
                .window_proposals
                .get(window_key)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut proposal_ids = proposals
                .iter()
                .map(|proposal| proposal.proposal.event_id.clone())
                .collect::<Vec<_>>();
            proposal_ids.sort();
            let mut window_ids = window.proposal_event_ids.clone();
            window_ids.sort();
            if !window.closed
                || metadata.source_end_offset > cutoff
                || proposal_ids != window_ids
                || proposals.iter().any(|proposal| {
                    proposal.fact.evidence_kind != EvidenceKind::Legacy
                        || proposal.occurrences.len() != 1
                        || proposal.occurrences[0].event_id != proposal.proposal.event_id
                        || proposal.occurrences[0].source_file_id != persisted_key.source_file_id
                        || proposal.occurrences[0].file_generation != persisted_key.file_generation
                        || proposal.occurrences[0].source_start_offset
                            < metadata.source_start_offset
                        || proposal.occurrences[0].source_end_offset > metadata.source_end_offset
                })
            {
                return Err(());
            }
            candidates.push((*window_key, metadata.source_end_offset));
        }
        let latest_end = candidates.iter().map(|(_, end)| *end).max().ok_or(())?;
        let mut latest = candidates
            .into_iter()
            .filter_map(|(window_key, end)| (end == latest_end).then_some(window_key))
            .collect::<Vec<_>>();
        if latest.len() != 1 {
            return Err(());
        }
        latest.pop().ok_or(())
    }

    fn closed_late_response_windows(
        &self,
        response_key: &ResponseKey,
        turn_key: &str,
        response_offset: u64,
    ) -> Result<Vec<WindowKey>, ()> {
        let local_key = PersistedTurnKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            turn_key: turn_key.to_owned(),
        };
        let binding = self.reconciliation.bindings.get(response_key).ok_or(())?;
        if binding.proposal.turn_key.as_deref() != Some(turn_key)
            || binding.fact.owning_thread_id != response_key.owning_thread_id
            || binding.fact.response_id.as_deref() != Some(response_key.response_id.as_str())
        {
            return Err(());
        }
        if self.forward_turn_end_ranges.contains_key(&local_key) {
            return self.forward_closed_late_response_windows(
                response_key,
                &local_key,
                response_offset,
            );
        }
        let local = self.closed_late_response_window(response_key, turn_key, response_offset)?;
        let (compensation_events, compensation_occurrences) =
            self.compensation_closure_for_turn(&local_key)?;
        if compensation_events.is_empty() || compensation_occurrences.is_empty() {
            return Err(());
        }
        let current_occurrences = self
            .reconciliation
            .response_occurrences
            .get(response_key)
            .ok_or(())?
            .iter()
            .filter(|occurrence| {
                occurrence.source_file_id == self.context.source_file_id
                    && occurrence.file_generation == self.context.file_generation
                    && occurrence.source_start_offset == response_offset
                    && occurrence.event_id == binding.proposal.event_id
            })
            .count();
        if current_occurrences != 1 {
            return Err(());
        }

        let mut window_keys = BTreeSet::new();
        let mut checked_turns = BTreeSet::new();
        for occurrence in &compensation_occurrences {
            let persisted_key = PersistedTurnKey {
                source_file_id: occurrence.source_file_id,
                file_generation: occurrence.file_generation,
                turn_key: turn_key.to_owned(),
            };
            if !checked_turns.insert(persisted_key.clone()) {
                return Err(());
            }
            let response_offset_for_turn = (persisted_key.source_file_id
                == self.context.source_file_id
                && persisted_key.file_generation == self.context.file_generation)
                .then_some(response_offset);
            let window_key = self.closed_late_response_window_for_turn(
                &persisted_key,
                response_key,
                response_offset_for_turn,
                &compensation_occurrences,
            )?;
            window_keys.insert(window_key);
        }
        if !window_keys.contains(&local) {
            return Err(());
        }
        Ok(window_keys.into_iter().collect())
    }

    fn forward_closed_late_response_windows(
        &self,
        response_key: &ResponseKey,
        local_key: &PersistedTurnKey,
        response_offset: u64,
    ) -> Result<Vec<WindowKey>, ()> {
        let (turn_end_start, turn_end_offset) = self
            .forward_turn_end_ranges
            .get(local_key)
            .copied()
            .ok_or(())?;
        let current_turn = self
            .patch
            .turn_upserts
            .iter()
            .find(|snapshot| snapshot.key == *local_key)
            .ok_or(())?;
        if current_turn.status == PersistedTurnStatus::Open
            || current_turn.owning_thread_id != response_key.owning_thread_id
            || current_turn.end_offset != Some(turn_end_offset)
            || response_offset < turn_end_offset
        {
            return Err(());
        }

        let mut local_candidates = self
            .reconciliation
            .window_metadata
            .iter()
            .filter_map(|(key, metadata)| {
                (metadata.source_file_id == local_key.source_file_id
                    && metadata.file_generation == local_key.file_generation
                    && metadata.owning_thread_id == current_turn.owning_thread_id
                    && metadata.turn_key.as_deref() == Some(local_key.turn_key.as_str())
                    && metadata.source_end_offset <= turn_end_start)
                    .then_some((*key, metadata))
            })
            .filter_map(|(key, metadata)| {
                let window = self.reconciliation.windows.get(&key)?;
                let proposals = self
                    .reconciliation
                    .window_proposals
                    .get(&key)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let mut proposal_ids = proposals
                    .iter()
                    .map(|proposal| proposal.proposal.event_id.clone())
                    .collect::<Vec<_>>();
                proposal_ids.sort();
                let mut window_ids = window.proposal_event_ids.clone();
                window_ids.sort();
                (window.closed
                    && proposal_ids == window_ids
                    && proposals.iter().all(|proposal| {
                        proposal.fact.evidence_kind == EvidenceKind::Legacy
                            && proposal.occurrences.len() == 1
                            && proposal.occurrences[0].event_id == proposal.proposal.event_id
                            && proposal.occurrences[0].source_file_id == local_key.source_file_id
                            && proposal.occurrences[0].file_generation == local_key.file_generation
                            && proposal.occurrences[0].source_start_offset
                                >= metadata.source_start_offset
                            && proposal.occurrences[0].source_end_offset
                                <= metadata.source_end_offset
                    }))
                .then_some((key, metadata.source_end_offset))
            })
            .collect::<Vec<_>>();
        let latest_end = local_candidates
            .iter()
            .map(|(_, end)| *end)
            .max()
            .ok_or(())?;
        local_candidates.retain(|(_, end)| *end == latest_end);
        if local_candidates.len() != 1 {
            return Err(());
        }
        let mut window_keys = BTreeSet::from([local_candidates[0].0]);

        let sibling_turns = self
            .reconciliation_baseline
            .affected_turns
            .keys()
            .filter(|key| {
                key.turn_key == local_key.turn_key
                    && (key.source_file_id != local_key.source_file_id
                        || key.file_generation != local_key.file_generation)
                    && self
                        .reconciliation_baseline
                        .affected_turns
                        .get(*key)
                        .is_some_and(|affected| {
                            affected.snapshot.owning_thread_id == response_key.owning_thread_id
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        for sibling_key in sibling_turns {
            let (_, compensation_occurrences) = self.compensation_closure_for_turn(&sibling_key)?;
            let window_key = self.closed_late_response_window_for_turn(
                &sibling_key,
                response_key,
                None,
                &compensation_occurrences,
            )?;
            window_keys.insert(window_key);
        }

        let current_occurrences = self
            .reconciliation
            .response_occurrences
            .get(response_key)
            .ok_or(())?
            .iter()
            .filter(|occurrence| {
                occurrence.source_file_id == self.context.source_file_id
                    && occurrence.file_generation == self.context.file_generation
                    && occurrence.source_start_offset == response_offset
                    && occurrence.event_id
                        == self.reconciliation.bindings[response_key].proposal.event_id
            })
            .count();
        if current_occurrences != 1 {
            return Err(());
        }
        Ok(window_keys.into_iter().collect())
    }

    fn compensation_closure_for_turn(
        &self,
        target: &PersistedTurnKey,
    ) -> Result<(Vec<CanonicalUsageProposal>, Vec<Occurrence>), ()> {
        let mut affected = self
            .reconciliation_baseline
            .affected_turns
            .iter()
            .filter(|(key, value)| {
                key.turn_key == target.turn_key
                    && value.snapshot.owning_thread_id == self.context.owning_thread_id
            })
            .map(|(key, value)| (key, value))
            .collect::<Vec<_>>();
        affected.sort_by(|(left, _), (right, _)| left.cmp(right));
        let Some((_, first)) = affected.first().copied() else {
            return Err(());
        };
        let mut events = first.compensation_events.clone();
        events.sort_by(|left, right| left.event_id.cmp(&right.event_id));
        let mut occurrences = first.compensation_occurrences.clone();
        occurrences.sort_by_key(|occurrence| {
            (
                occurrence.source_file_id,
                occurrence.file_generation,
                occurrence.source_start_offset,
                occurrence.source_end_offset,
                occurrence.event_id.clone(),
            )
        });
        let event_ids = events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<BTreeSet<_>>();
        if events.iter().any(|event| {
            event.kind != EventKind::TurnCompensation
                || event.thread_id != self.context.owning_thread_id
                || event.turn_key.as_deref() != Some(target.turn_key.as_str())
        }) || event_ids.len() != events.len()
            || affected.iter().any(|(key, value)| {
                value.snapshot.key != **key
                    || value.compensation_events.len() != events.len()
                    || value.compensation_occurrences.len() != occurrences.len()
                    || {
                        let mut copy = value.compensation_events.clone();
                        copy.sort_by(|left, right| left.event_id.cmp(&right.event_id));
                        copy != events
                    }
                    || {
                        let mut copy = value.compensation_occurrences.clone();
                        copy.sort_by_key(|occurrence| {
                            (
                                occurrence.source_file_id,
                                occurrence.file_generation,
                                occurrence.source_start_offset,
                                occurrence.source_end_offset,
                                occurrence.event_id.clone(),
                            )
                        });
                        copy != occurrences
                    }
            })
        {
            return Err(());
        }
        let mut occurrence_keys = BTreeSet::new();
        let mut referenced_events = BTreeSet::new();
        for occurrence in &occurrences {
            if !event_ids.contains(occurrence.event_id.as_str())
                || occurrence.source_start_offset >= occurrence.source_end_offset
                || !occurrence_keys.insert(occurrence_key(occurrence))
            {
                return Err(());
            }
            let source_turns = affected
                .iter()
                .filter(|(key, _)| {
                    key.source_file_id == occurrence.source_file_id
                        && key.file_generation == occurrence.file_generation
                })
                .map(|(_, value)| value)
                .collect::<Vec<_>>();
            if source_turns.len() != 1
                || source_turns[0].snapshot.status == PersistedTurnStatus::Open
                || source_turns[0].snapshot.end_offset != Some(occurrence.source_end_offset)
            {
                return Err(());
            }
            referenced_events.insert(occurrence.event_id.as_str());
        }
        if referenced_events.len() != events.len() {
            return Err(());
        }
        let Some((_, target_turn)) = affected.iter().find(|(key, _)| *key == target) else {
            return Err(());
        };
        if events.is_empty() && occurrences.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        if target_turn.snapshot.status == PersistedTurnStatus::Open {
            return Err(());
        }
        Ok((events, occurrences))
    }

    fn start_turn(&mut self, turn_id: Option<String>, timestamp_ms: Option<i64>, offset: u64) {
        self.close_pending_evidence(None, offset);
        if let Some(mut old) = self.state.open_turn.take() {
            old.blocks.required_invalid = true;
            self.anomaly(AnomalyCode::TurnReplaced, Some(offset));
            let snapshot = self.snapshot(
                &old,
                PersistedTurnStatus::Aborted,
                timestamp_ms,
                Some(offset),
            );
            self.upsert_turn(snapshot);
        }
        let key = turn_key_for(
            &self.context.owning_thread_id,
            turn_id.as_deref(),
            offset,
            timestamp_ms,
        );
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
            if let UsageValue::Valid(current) = &total {
                self.set_baseline(current.clone(), end_offset);
            }
            return Ok(());
        }

        let previous = self.state.previous_total.clone();
        let window_start = self
            .state
            .reconciliation_carry
            .open_window_start_offset
            .or(self.state.previous_total_offset)
            .unwrap_or(start_offset);
        let turn_key = if let Some(turn) = self.state.open_turn.as_ref() {
            Some(turn.turn_key.clone())
        } else {
            let pending_turn_keys = self
                .state
                .reconciliation_carry
                .pending_response_ids
                .iter()
                .filter_map(|response_id| {
                    let response_key = ResponseKey {
                        owning_thread_id: self.context.owning_thread_id.clone(),
                        response_id: response_id.clone(),
                    };
                    let binding = self.reconciliation.bindings.get(&response_key)?;
                    let same_window_start = self
                        .reconciliation
                        .response_occurrences
                        .get(&response_key)?
                        .iter()
                        .any(|occurrence| {
                            occurrence.source_file_id == self.context.source_file_id
                                && occurrence.file_generation == self.context.file_generation
                                && occurrence.source_start_offset == window_start
                        });
                    (same_window_start
                        && binding.fact.evidence_kind == EvidenceKind::Explicit
                        && binding.fact.owning_thread_id == self.context.owning_thread_id
                        && binding.fact.response_id.as_deref() == Some(response_id.as_str()))
                    .then(|| binding.proposal.turn_key.clone())
                    .flatten()
                })
                .collect::<BTreeSet<_>>();
            match pending_turn_keys.len() {
                0 => None,
                1 => pending_turn_keys.into_iter().next(),
                _ => {
                    self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, start_offset);
                    return Ok(());
                }
            }
        };
        let accounted_before = self
            .state
            .open_turn
            .as_ref()
            .map(|turn| turn.accounted.clone())
            .unwrap_or_else(NormalizedTokenUsage::zero);
        let prior_chain = self.state.chain_state;
        let window_key = WindowKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            start_offset: window_start,
        };
        let metadata = ReconciliationWindowMetadata {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: window_start,
            source_end_offset: end_offset,
            owning_thread_id: self.context.owning_thread_id.clone(),
            turn_key: turn_key.clone(),
        };
        if self
            .reconciliation
            .window_metadata
            .get(&window_key)
            .is_some_and(|existing| existing != &metadata)
        {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, start_offset);
            return Ok(());
        }
        let expected_previous = previous
            .clone()
            .map(UsageValue::Valid)
            .unwrap_or(UsageValue::Missing);
        let mut window = self
            .reconciliation
            .windows
            .get(&window_key)
            .cloned()
            .unwrap_or_else(|| LegacyReconciliationWindow {
                version: LegacyReconciliationWindow::VERSION,
                previous_total: expected_previous.clone(),
                current_total: total.clone(),
                last_usage: last.clone(),
                explicit_response_ids: Vec::new(),
                legacy_covered_response_ids: Vec::new(),
                proposal_event_ids: Vec::new(),
                turn_accounted_before: accounted_before.clone(),
                chain_state: prior_chain,
                closed: true,
            });
        if window.previous_total != expected_previous
            || window.current_total != total
            || window.last_usage != last
        {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, start_offset);
            return Ok(());
        }
        window.turn_accounted_before = accounted_before;
        window.chain_state = prior_chain;
        window.closed = true;
        self.reconciliation.windows.insert(window_key, window);
        self.reconciliation
            .window_metadata
            .insert(window_key, metadata);
        self.window_record_ranges
            .insert(window_key, (start_offset, end_offset));
        self.active_window_key = Some(window_key);
        self.collect_window_responses(window_key);

        let current = match total {
            UsageValue::Valid(value) => value,
            UsageValue::Missing | UsageValue::Invalid => {
                self.anomaly(AnomalyCode::RequiredTotalInvalid, Some(start_offset));
                self.gap(GapKind::RequiredInvalid);
                self.finish_legacy_window(window_key, start_offset)?;
                self.state.reconciliation_carry.open_window_start_offset = Some(end_offset);
                return Ok(());
            }
        };

        if timestamp_ms.is_none() {
            self.anomaly(AnomalyCode::UsageTimeMissing, Some(start_offset));
            if last == UsageValue::Invalid {
                self.anomaly(AnomalyCode::LastUsageInvalid, Some(start_offset));
            }
            if let Some(turn) = &mut self.state.open_turn {
                turn.blocks.time_missing = true;
                turn.last_total = Some(current.clone());
            }
            self.set_baseline(current, end_offset);
            self.finish_legacy_window(window_key, start_offset)?;
            self.state.reconciliation_carry.open_window_start_offset = Some(end_offset);
            return Ok(());
        }

        if matches!(self.state.chain_state, ChainState::Interrupted(_)) {
            self.set_baseline(current, end_offset);
            self.finish_legacy_window(window_key, start_offset)?;
            self.state.reconciliation_carry.open_window_start_offset = Some(end_offset);
            return Ok(());
        }

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
                if !usage_is_zero(&usage) {
                    self.emit_candidate(CandidateInput {
                        kind: EventKind::Normal,
                        occurred_at_ms: timestamp_ms.unwrap(),
                        start_offset,
                        end_offset,
                        usage,
                        previous_total: previous.clone(),
                        current_total: current.clone(),
                    })?;
                }
            } else if last == UsageValue::Invalid {
                self.anomaly(AnomalyCode::LastUsageInvalid, Some(start_offset));
            }
            self.set_baseline(current, end_offset);
            self.finish_legacy_window(window_key, start_offset)?;
            self.state.reconciliation_carry.open_window_start_offset = Some(end_offset);
            return Ok(());
        }

        if previous.as_ref() == Some(&current) {
            // A duplicate cumulative snapshot emits no candidate, but the
            // trusted boundary itself is newer and must become the durable
            // baseline/Turn end snapshot for restart and compensation checks.
            self.set_baseline(current, end_offset);
            self.finish_legacy_window(window_key, start_offset)?;
            self.state.reconciliation_carry.open_window_start_offset = Some(end_offset);
            return Ok(());
        }

        match last {
            UsageValue::Valid(usage) => {
                if !usage_is_zero(&usage) {
                    self.emit_candidate(CandidateInput {
                        kind: EventKind::Normal,
                        occurred_at_ms: timestamp_ms.unwrap(),
                        start_offset,
                        end_offset,
                        usage,
                        previous_total: previous.clone(),
                        current_total: current.clone(),
                    })?;
                }
            }
            UsageValue::Missing => {
                if let Some(old) = &previous {
                    let usage = processor_checked_sub(&current, old)?;
                    if !usage_is_zero(&usage) {
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
            }
            UsageValue::Invalid => {
                self.anomaly(AnomalyCode::LastUsageInvalid, Some(start_offset));
            }
        }
        self.set_baseline(current, end_offset);
        self.finish_legacy_window(window_key, start_offset)?;
        self.state.reconciliation_carry.open_window_start_offset = Some(end_offset);
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
        let mut event = CanonicalUsageProposal {
            event_id: String::new(),
            kind,
            occurred_at_ms,
            thread_id: self.context.owning_thread_id.clone(),
            root_session_id: self.context.root_session_id.clone(),
            turn_key,
            model: model.clone(),
            reasoning_effort: reasoning_effort.clone(),
            usage: usage.clone(),
        };
        event.event_id = event_id(&event, previous_total.as_ref(), &current_total);
        if let Some(turn) = &mut self.state.open_turn {
            observe_turn_model(turn, &model);
            observe_turn_reasoning_effort(turn, reasoning_effort.as_deref());
            turn.last_total = Some(current_total);
        }
        let key = match self.active_window_key {
            Some(key) => key,
            None => self.ensure_open_window(start_offset),
        };
        let occurrence = Occurrence {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: start_offset,
            source_end_offset: end_offset,
            event_id: event.event_id.clone(),
        };
        let fact = UsageEventFact {
            event_id: event.event_id.clone(),
            owning_thread_id: self.context.owning_thread_id.clone(),
            response_id: None,
            evidence_kind: EvidenceKind::Legacy,
            operation: CodexOperation::Response,
        };
        let proposals = self.reconciliation.window_proposals.entry(key).or_default();
        if let Some(existing) = proposals
            .iter_mut()
            .find(|existing| existing.proposal.event_id == event.event_id)
        {
            existing.proposal = event;
            existing.fact = fact;
            if !existing
                .occurrences
                .iter()
                .any(|row| occurrence_key(row) == occurrence_key(&occurrence))
            {
                existing.occurrences.push(occurrence);
            }
        } else {
            proposals.push(WindowProposalBinding {
                proposal: event,
                fact,
                occurrences: vec![occurrence],
            });
        }
        Ok(())
    }

    fn collect_window_responses(&mut self, key: WindowKey) {
        let Some(metadata) = self.reconciliation.window_metadata.get(&key) else {
            return;
        };
        let metadata = metadata.clone();
        let bindings = self
            .reconciliation
            .bindings
            .iter()
            .map(|(key, binding)| (key.clone(), binding.clone()))
            .collect::<Vec<_>>();
        let mut response_ids = Vec::new();
        for (response_key, binding) in bindings {
            if response_key.owning_thread_id != metadata.owning_thread_id
                || binding.proposal.turn_key.as_deref() != metadata.turn_key.as_deref()
            {
                continue;
            }
            let occurrences = self
                .reconciliation
                .response_occurrences
                .get(&response_key)
                .into_iter()
                .flatten()
                .filter(|occurrence| {
                    occurrence.source_file_id == metadata.source_file_id
                        && occurrence.file_generation == metadata.file_generation
                })
                .cloned()
                .collect::<Vec<_>>();
            if occurrences.is_empty() {
                continue;
            }

            let assigned = self
                .reconciliation
                .windows
                .iter()
                .filter_map(|(window_key, window)| {
                    (window
                        .explicit_response_ids
                        .contains(&response_key.response_id)
                        && window_key.source_file_id == metadata.source_file_id
                        && window_key.file_generation == metadata.file_generation)
                        .then_some(*window_key)
                })
                .collect::<Vec<_>>();
            let selected = match assigned.as_slice() {
                [assigned] => Some(*assigned),
                [] => {
                    let candidates = self
                        .reconciliation
                        .window_metadata
                        .iter()
                        .filter_map(|(candidate_key, candidate)| {
                            (candidate.source_file_id == metadata.source_file_id
                                && candidate.file_generation == metadata.file_generation
                                && candidate.owning_thread_id == metadata.owning_thread_id
                                && candidate.turn_key == metadata.turn_key
                                && self.response_occurs_in_window(&response_key, candidate))
                            .then_some(*candidate_key)
                        })
                        .collect::<Vec<_>>();
                    let adjacent = candidates
                        .iter()
                        .copied()
                        .filter(|candidate| {
                            self.response_adjacent_to_legacy(
                                *candidate,
                                &response_key.response_id,
                                self.legacy_record_range(*candidate),
                            )
                        })
                        .collect::<Vec<_>>();
                    match adjacent.as_slice() {
                        [adjacent] => Some(*adjacent),
                        [] => match candidates.as_slice() {
                            [candidate] => Some(*candidate),
                            [] => None,
                            _ => {
                                self.fatal_anomaly(
                                    AnomalyCode::LegacyCoverageAmbiguous,
                                    occurrences[0].source_start_offset,
                                );
                                None
                            }
                        },
                        _ => {
                            self.fatal_anomaly(
                                AnomalyCode::LegacyCoverageAmbiguous,
                                occurrences[0].source_start_offset,
                            );
                            None
                        }
                    }
                }
                _ => {
                    self.fatal_anomaly(
                        AnomalyCode::LegacyCoverageAmbiguous,
                        occurrences[0].source_start_offset,
                    );
                    None
                }
            };
            if selected == Some(key) {
                response_ids.push(response_key.response_id);
            }
        }
        if let Some(window) = self.reconciliation.windows.get_mut(&key) {
            window.explicit_response_ids.extend(response_ids);
            sort_ids(&mut window.explicit_response_ids);
        }
    }

    fn finish_legacy_window(
        &mut self,
        key: WindowKey,
        source_offset: u64,
    ) -> Result<(), ProcessorError> {
        self.reconcile_window(key, source_offset)?;
        let turn_key = self
            .reconciliation
            .window_metadata
            .get(&key)
            .and_then(|metadata| metadata.turn_key.clone());
        if let Some(turn_key) = turn_key.as_deref() {
            self.reconcile_affected_turn(Some(turn_key), source_offset)?;
        }
        Ok(())
    }

    fn reconcile_window(
        &mut self,
        key: WindowKey,
        source_offset: u64,
    ) -> Result<(), ProcessorError> {
        let Some(mut window) = self.reconciliation.windows.get(&key).cloned() else {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
            return Ok(());
        };
        let Some(metadata) = self.reconciliation.window_metadata.get(&key).cloned() else {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
            return Ok(());
        };
        let response_ids = window
            .explicit_response_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let previously_covered = window
            .legacy_covered_response_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if previously_covered.iter().any(|response_id| {
            if !response_ids.contains(response_id) {
                return true;
            }
            let response_key = ResponseKey {
                owning_thread_id: metadata.owning_thread_id.clone(),
                response_id: response_id.clone(),
            };
            self.reconciliation
                .bindings
                .get(&response_key)
                .is_none_or(|binding| {
                    binding.fact.evidence_kind != EvidenceKind::Explicit
                        || binding.fact.owning_thread_id != metadata.owning_thread_id
                        || binding.fact.response_id.as_deref() != Some(response_id.as_str())
                        || binding.proposal.turn_key.as_deref() != metadata.turn_key.as_deref()
                        || (!self.response_occurs_in_window(&response_key, &metadata)
                            && !self
                                .late_response_windows
                                .get(&response_key)
                                .is_some_and(|keys| keys.contains(&key)))
                })
        }) {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
            return Ok(());
        }
        let mut explicit = Vec::new();
        for response_id in response_ids {
            let response_key = ResponseKey {
                owning_thread_id: metadata.owning_thread_id.clone(),
                response_id: response_id.clone(),
            };
            let Some(binding) = self.reconciliation.bindings.get(&response_key) else {
                continue;
            };
            if binding.fact.evidence_kind != EvidenceKind::Explicit
                || binding.proposal.turn_key.as_deref() != metadata.turn_key.as_deref()
                || (!self.response_occurs_in_window(&response_key, &metadata)
                    && !self
                        .late_response_windows
                        .get(&response_key)
                        .is_some_and(|keys| keys.contains(&key)))
            {
                continue;
            }
            explicit.push((response_id, binding.clone()));
        }
        explicit.sort_by(|left, right| left.0.cmp(&right.0));

        let delta = match (
            &window.previous_total,
            &window.current_total,
            window.chain_state,
        ) {
            (UsageValue::Valid(previous), UsageValue::Valid(current), ChainState::Continuous)
                if !required_decreased_from(current, previous)
                    && !cache_decreased_from(current, previous) =>
            {
                match processor_checked_sub(current, previous) {
                    Ok(delta) => Some(delta),
                    Err(ProcessorError::NegativeDifference)
                    | Err(ProcessorError::CacheWriteNegativeDifference) => None,
                    Err(error) => return Err(error),
                }
            }
            _ => None,
        };
        let last = match &window.last_usage {
            UsageValue::Valid(value) => Some(value),
            UsageValue::Missing | UsageValue::Invalid => None,
        };
        let legacy_range = self.legacy_record_range(key);
        let mut covered = previously_covered.clone();
        let mut remove_legacy = false;
        let mut coverage_complete = false;
        let mut ambiguous = false;
        let zero_context_window =
            delta.as_ref().is_some_and(usage_is_zero) && last.is_none_or(usage_is_zero);

        if !zero_context_window && let Some(last_usage) = last {
            let unbound_explicit = explicit
                .iter()
                .filter(|(response_id, _)| !previously_covered.contains(response_id))
                .cloned()
                .collect::<Vec<_>>();
            let adjacent_normals = unbound_explicit
                .iter()
                .filter(|(response_id, binding)| {
                    binding.fact.operation == CodexOperation::Response
                        && self.response_adjacent_to_legacy(key, response_id, legacy_range)
                })
                .collect::<Vec<_>>();
            let matching_normal_ids = adjacent_normals
                .iter()
                .filter(|(_, binding)| {
                    usage_coverage_equal(&binding.proposal.usage, last_usage) == Some(true)
                })
                .map(|(response_id, _)| response_id.clone())
                .collect::<Vec<_>>();
            let uncertain_normal = adjacent_normals.iter().any(|(_, binding)| {
                usage_coverage_equal(&binding.proposal.usage, last_usage).is_none()
            });
            let adjacent_compactions = unbound_explicit
                .iter()
                .filter(|(response_id, binding)| {
                    binding.fact.operation == CodexOperation::Compaction
                        && self.response_adjacent_to_legacy(key, response_id, legacy_range)
                })
                .collect::<Vec<_>>();
            let matching_compaction_ids = adjacent_compactions
                .iter()
                .filter(|(_, binding)| {
                    usage_coverage_equal(&binding.proposal.usage, last_usage) == Some(true)
                })
                .map(|(response_id, _)| (*response_id).clone())
                .collect::<Vec<_>>();
            let uncertain_compaction = adjacent_compactions.iter().any(|(_, binding)| {
                usage_coverage_equal(&binding.proposal.usage, last_usage).is_none()
            });
            let coincident_nonadjacent = unbound_explicit.iter().any(|(response_id, binding)| {
                binding.fact.operation == CodexOperation::Response
                    && !self.response_adjacent_to_legacy(key, response_id, legacy_range)
                    && usage_coverage_equal(&binding.proposal.usage, last_usage) != Some(false)
            });
            let includes_last = delta
                .as_ref()
                .is_some_and(|delta| usage_coverage_equal(delta, last_usage) == Some(true));
            let unique_compaction_pair = if matching_compaction_ids.len() == 1
                && includes_last
                && unbound_explicit.len() == 1
                && matching_normal_ids.is_empty()
            {
                matching_compaction_ids.first().cloned()
            } else {
                None
            };
            if uncertain_normal
                || uncertain_compaction
                || matching_normal_ids.len() > 1
                || coincident_nonadjacent
                || (includes_last
                    && !matching_compaction_ids.is_empty()
                    && unique_compaction_pair.is_none())
            {
                ambiguous = true;
            } else if let Some(delta) = delta.as_ref() {
                let paired_id = matching_normal_ids
                    .first()
                    .cloned()
                    .or_else(|| unique_compaction_pair.clone());
                let other_explicit = unbound_explicit
                    .iter()
                    .filter(|(id, _)| paired_id.as_ref() != Some(id))
                    .collect::<Vec<_>>();
                let other_sum = sum_usage(
                    &other_explicit
                        .iter()
                        .map(|(_, binding)| &binding.proposal.usage)
                        .collect::<Vec<_>>(),
                )?;
                let residual_and_other = sum_usage_with(&other_sum, last_usage)?;
                let includes_last_and_other =
                    usage_sum_coverage_equal(&residual_and_other, delta) == Some(true);
                let other_are_compactions = other_explicit
                    .iter()
                    .all(|(_, binding)| binding.fact.operation == CodexOperation::Compaction);

                if let Some(response_id) = matching_normal_ids.first() {
                    if !usage_is_zero(last_usage)
                        && !self.unique_legacy_proposal_matches(key, last_usage)
                    {
                        ambiguous = true;
                    } else {
                        covered.insert(response_id.clone());
                        remove_legacy = true;
                    }
                }
                if let Some(response_id) = unique_compaction_pair.as_ref() {
                    if !usage_is_zero(last_usage)
                        && !self.unique_legacy_proposal_matches(key, last_usage)
                    {
                        ambiguous = true;
                    } else {
                        covered.insert(response_id.clone());
                        remove_legacy = true;
                    }
                }

                if !ambiguous && !other_explicit.is_empty() {
                    if includes_last && other_are_compactions {
                        // D == L: Compactions remain actual-only.
                    } else if includes_last_and_other {
                        covered.extend(other_explicit.iter().map(|(id, _)| (*id).clone()));
                        if paired_id.is_none()
                            && !usage_is_zero(last_usage)
                            && !self.unique_legacy_proposal_matches(key, last_usage)
                            && !(other_are_compactions
                                && self.unique_legacy_proposal_matches(key, &other_sum.usage))
                        {
                            ambiguous = true;
                        }
                    } else {
                        ambiguous = true;
                    }
                } else if !ambiguous
                    && paired_id.is_none()
                    && unbound_explicit
                        .iter()
                        .any(|(_, binding)| binding.fact.operation == CodexOperation::Response)
                {
                    // A normal response that cannot uniquely replace L must
                    // not be left beside a guessed legacy proposal.
                    ambiguous = true;
                }
            } else {
                if !matching_compaction_ids.is_empty() || uncertain_compaction {
                    ambiguous = true;
                }
                if let Some(response_id) = matching_normal_ids.first() {
                    if !usage_is_zero(last_usage)
                        && !self.unique_legacy_proposal_matches(key, last_usage)
                    {
                        ambiguous = true;
                    } else {
                        covered.insert(response_id.clone());
                        remove_legacy = true;
                    }
                }
            }
        } else if !zero_context_window
            && let Some(delta) = delta.as_ref()
            && !explicit.is_empty()
        {
            let explicit_sum = sum_usage(
                &explicit
                    .iter()
                    .map(|(_, binding)| &binding.proposal.usage)
                    .collect::<Vec<_>>(),
            )?;
            match usage_sum_coverage_equal(&explicit_sum, delta) {
                Some(true) => {
                    covered.extend(explicit.iter().map(|(id, _)| id.clone()));
                    coverage_complete = true;
                }
                Some(false) | None => match subtract_usage_sum(delta, &explicit_sum) {
                    Ok(Some(residual)) if usage_is_zero(&residual) => {
                        covered.extend(explicit.iter().map(|(id, _)| id.clone()));
                        coverage_complete = true;
                    }
                    Ok(Some(residual)) => {
                        if self.replace_legacy_residual(
                            key,
                            &window,
                            delta,
                            &explicit_sum,
                            &residual,
                        )? {
                            covered.extend(explicit.iter().map(|(id, _)| id.clone()));
                        } else {
                            ambiguous = true;
                        }
                    }
                    Ok(None) | Err(_) => ambiguous = true,
                },
            }
        }

        if !ambiguous
            && !zero_context_window
            && last.is_some()
            && let Some(delta) = delta.as_ref()
            && !covered.is_empty()
        {
            let covered_usages = covered
                .iter()
                .map(|response_id| {
                    self.reconciliation
                        .bindings
                        .get(&ResponseKey {
                            owning_thread_id: metadata.owning_thread_id.clone(),
                            response_id: response_id.clone(),
                        })
                        .map(|binding| &binding.proposal.usage)
                })
                .collect::<Option<Vec<_>>>();
            if let Some(covered_usages) = covered_usages {
                let covered_sum = sum_usage(&covered_usages)?;
                match subtract_usage_sum(delta, &covered_sum) {
                    Ok(Some(residual)) if usage_is_zero(&residual) => {
                        remove_legacy = true;
                        coverage_complete = true;
                    }
                    Ok(Some(residual)) => {
                        if self.replace_legacy_residual(
                            key,
                            &window,
                            delta,
                            &covered_sum,
                            &residual,
                        )? {
                            remove_legacy = false;
                        } else {
                            ambiguous = true;
                        }
                    }
                    Ok(None) | Err(_) => ambiguous = true,
                }
            } else {
                ambiguous = true;
            }
        }

        if ambiguous {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
            return Ok(());
        }
        let proposals = self.reconciliation.window_proposals.entry(key).or_default();
        if remove_legacy || coverage_complete {
            proposals.clear();
        }
        window.legacy_covered_response_ids = covered.into_iter().collect();
        window.proposal_event_ids = proposals
            .iter()
            .map(|binding| binding.proposal.event_id.clone())
            .collect();
        sort_ids(&mut window.legacy_covered_response_ids);
        sort_ids(&mut window.proposal_event_ids);
        self.reconciliation.windows.insert(key, window.clone());

        if let Some(turn_key) = metadata.turn_key.as_deref() {
            if metadata.source_file_id == self.context.source_file_id
                && metadata.file_generation == self.context.file_generation
            {
                self.recompute_current_turn_accounted(turn_key, source_offset)?;
            }
            if window.last_usage == UsageValue::Invalid
                && !coverage_complete
                && !remove_legacy
                && !zero_context_window
            {
                self.block_turn_required(&PersistedTurnKey {
                    source_file_id: metadata.source_file_id,
                    file_generation: metadata.file_generation,
                    turn_key: turn_key.to_owned(),
                });
            }
        }
        Ok(())
    }

    fn legacy_record_range(&self, key: WindowKey) -> Option<(u64, u64)> {
        if let Some(range) = self.window_record_ranges.get(&key) {
            return Some(*range);
        }
        let ranges = self
            .reconciliation
            .window_proposals
            .get(&key)?
            .iter()
            .flat_map(|binding| &binding.occurrences)
            .map(|occurrence| (occurrence.source_start_offset, occurrence.source_end_offset))
            .collect::<BTreeSet<_>>();
        (ranges.len() == 1).then(|| *ranges.iter().next().unwrap())
    }

    fn response_adjacent_to_legacy(
        &self,
        window_key: WindowKey,
        response_id: &str,
        legacy_range: Option<(u64, u64)>,
    ) -> bool {
        let Some((legacy_start, legacy_end)) = legacy_range else {
            return false;
        };
        let Some(metadata) = self.reconciliation.window_metadata.get(&window_key) else {
            return false;
        };
        let Some(window) = self.reconciliation.windows.get(&window_key) else {
            return false;
        };
        if metadata.source_file_id != window_key.source_file_id
            || metadata.file_generation != window_key.file_generation
            || metadata.owning_thread_id != self.context.owning_thread_id
            || window.chain_state != ChainState::Continuous
        {
            return false;
        }
        let response_key = ResponseKey {
            owning_thread_id: self.context.owning_thread_id.clone(),
            response_id: response_id.to_owned(),
        };
        let Some(binding) = self.reconciliation.bindings.get(&response_key) else {
            return false;
        };
        if binding.proposal.turn_key != metadata.turn_key {
            return false;
        }
        self.reconciliation
            .response_occurrences
            .get(&response_key)
            .into_iter()
            .flatten()
            .any(|occurrence| {
                if occurrence.source_file_id != window_key.source_file_id
                    || occurrence.file_generation != window_key.file_generation
                {
                    return false;
                }
                let gap = if occurrence.source_end_offset <= legacy_start {
                    (occurrence.source_end_offset, legacy_start)
                } else if occurrence.source_start_offset >= legacy_end {
                    (legacy_end, occurrence.source_start_offset)
                } else {
                    return false;
                };
                if gap.0 == gap.1 {
                    return true;
                }
                if gap.0 > gap.1
                    || (occurrence.source_start_offset >= legacy_end
                        && self.state.chain_state != ChainState::Continuous)
                {
                    return false;
                }
                let overlaps_gap = |start: u64, end: u64| start < gap.1 && gap.0 < end;
                let other_response = self
                    .reconciliation
                    .response_occurrences
                    .iter()
                    .filter(|(key, _)| {
                        key.owning_thread_id == response_key.owning_thread_id
                            && key.response_id != response_key.response_id
                    })
                    .flat_map(|(_, occurrences)| occurrences)
                    .any(|other| {
                        other.source_file_id == window_key.source_file_id
                            && other.file_generation == window_key.file_generation
                            && overlaps_gap(other.source_start_offset, other.source_end_offset)
                    });
                let other_legacy = self
                    .reconciliation
                    .window_metadata
                    .iter()
                    .filter(|(key, _)| **key != window_key)
                    .any(|(_, other)| {
                        other.source_file_id == window_key.source_file_id
                            && other.file_generation == window_key.file_generation
                            && overlaps_gap(other.source_start_offset, other.source_end_offset)
                    });
                let other_proposal = self
                    .reconciliation
                    .window_proposals
                    .values()
                    .flatten()
                    .flat_map(|proposal| &proposal.occurrences)
                    .any(|other| {
                        other.source_file_id == window_key.source_file_id
                            && other.file_generation == window_key.file_generation
                            && other.event_id != binding.proposal.event_id
                            && overlaps_gap(other.source_start_offset, other.source_end_offset)
                    });
                let other_marker = self.reconciliation.markers.iter().any(|marker| {
                    marker.source_file_id == window_key.source_file_id
                        && marker.file_generation == window_key.file_generation
                        && marker.response_id.as_deref() != Some(response_id)
                        && gap.0 < marker.source_start_offset
                        && marker.source_start_offset < gap.1
                });
                !(other_response || other_legacy || other_proposal || other_marker)
            })
    }

    fn after_boundary_is_proven_new_call(
        &self,
        window_key: WindowKey,
        response_key: &ResponseKey,
    ) -> Result<bool, ProcessorError> {
        let Some(window) = self.reconciliation.windows.get(&window_key) else {
            return Ok(false);
        };
        if window
            .explicit_response_ids
            .contains(&response_key.response_id)
        {
            return Ok(false);
        }
        let Some(binding) = self.reconciliation.bindings.get(response_key) else {
            return Ok(false);
        };
        let delta = match (
            &window.previous_total,
            &window.current_total,
            window.chain_state,
        ) {
            (UsageValue::Valid(previous), UsageValue::Valid(current), ChainState::Continuous)
                if !required_decreased_from(current, previous)
                    && !cache_decreased_from(current, previous) =>
            {
                match processor_checked_sub(current, previous) {
                    Ok(delta) => Some(delta),
                    Err(ProcessorError::NegativeDifference)
                    | Err(ProcessorError::CacheWriteNegativeDifference) => None,
                    Err(error) => return Err(error),
                }
            }
            _ => None,
        };
        let covered_usages = window
            .legacy_covered_response_ids
            .iter()
            .map(|response_id| {
                self.reconciliation
                    .bindings
                    .get(&ResponseKey {
                        owning_thread_id: response_key.owning_thread_id.clone(),
                        response_id: response_id.clone(),
                    })
                    .map(|binding| &binding.proposal.usage)
            })
            .collect::<Option<Vec<_>>>();
        let Some(covered_usages) = covered_usages else {
            return Ok(false);
        };
        let covered_sum = sum_usage(&covered_usages)?;
        let noncovered_explicit_ids_are_compactions = window
            .explicit_response_ids
            .iter()
            .filter(|id| !window.legacy_covered_response_ids.contains(*id))
            .all(|response_id| {
                let key = ResponseKey {
                    owning_thread_id: response_key.owning_thread_id.clone(),
                    response_id: response_id.clone(),
                };
                self.reconciliation
                    .bindings
                    .get(&key)
                    .is_some_and(|binding| {
                        binding.fact.evidence_kind == EvidenceKind::Explicit
                            && binding.fact.operation == CodexOperation::Compaction
                            && binding.fact.owning_thread_id == response_key.owning_thread_id
                            && binding.fact.response_id.as_deref() == Some(response_id.as_str())
                            && self
                                .reconciliation
                                .window_metadata
                                .get(&window_key)
                                .is_some_and(|metadata| {
                                    binding.proposal.turn_key == metadata.turn_key
                                })
                    })
            });
        let sole_covered_last = window.legacy_covered_response_ids.len() == 1
            && window
                .legacy_covered_response_ids
                .iter()
                .all(|id| window.explicit_response_ids.contains(id))
            && window.proposal_event_ids.is_empty()
            && self
                .reconciliation
                .window_proposals
                .get(&window_key)
                .is_none_or(Vec::is_empty)
            && matches!(
                (covered_usages.first(), &window.last_usage),
                (Some(covered), UsageValue::Valid(last))
                    if usage_coverage_equal(covered, last) == Some(true)
            );
        let first_anchor_covered = delta.is_none()
            && matches!(window.previous_total, UsageValue::Missing)
            && matches!(window.current_total, UsageValue::Valid(_))
            && sole_covered_last;
        let reset_boundary = matches!(
            (
                &window.previous_total,
                &window.current_total,
                window.chain_state
            ),
            (UsageValue::Valid(previous), UsageValue::Valid(current), ChainState::Continuous)
                if required_decreased_from(current, previous)
                    || cache_decreased_from(current, previous)
        );
        let reset_anchor_covered = reset_boundary
            && sole_covered_last
            && noncovered_explicit_ids_are_compactions
            && matches!(
                &window.last_usage,
                UsageValue::Valid(last)
                    if usage_coverage_equal(&binding.proposal.usage, last) == Some(false)
            );
        if let Some(delta) = delta {
            if usage_is_zero(&delta)
                && window.legacy_covered_response_ids.is_empty()
                && noncovered_explicit_ids_are_compactions
                && window.proposal_event_ids.is_empty()
                && self
                    .reconciliation
                    .window_proposals
                    .get(&window_key)
                    .is_none_or(Vec::is_empty)
                && matches!(
                    &window.last_usage,
                    UsageValue::Missing | UsageValue::Invalid
                )
            {
                return Ok(true);
            }
            if usage_is_zero(&delta)
                && window.legacy_covered_response_ids.is_empty()
                && noncovered_explicit_ids_are_compactions
                && window.proposal_event_ids.is_empty()
                && self
                    .reconciliation
                    .window_proposals
                    .get(&window_key)
                    .is_none_or(Vec::is_empty)
                && let UsageValue::Valid(last) = &window.last_usage
                && usage_coverage_equal(&binding.proposal.usage, last) == Some(false)
            {
                return Ok(true);
            }
            if !window.legacy_covered_response_ids.is_empty()
                && usage_sum_coverage_equal(&covered_sum, &delta) == Some(true)
            {
                return Ok(true);
            }
            if window.legacy_covered_response_ids.is_empty()
                && let UsageValue::Valid(last) = &window.last_usage
                && usage_coverage_equal(&delta, last) == Some(true)
                && usage_coverage_equal(&binding.proposal.usage, last) == Some(false)
                && self.unique_legacy_proposal_matches(window_key, last)
            {
                return Ok(true);
            }
        } else if first_anchor_covered || reset_anchor_covered {
            return Ok(true);
        }
        Ok(false)
    }

    fn unique_legacy_proposal_matches(&self, key: WindowKey, usage: &NormalizedTokenUsage) -> bool {
        let Some(proposals) = self.reconciliation.window_proposals.get(&key) else {
            return false;
        };
        proposals.len() == 1
            && proposals[0].fact.evidence_kind == EvidenceKind::Legacy
            && proposals[0].fact.response_id.is_none()
            && matches!(
                proposals[0].proposal.kind,
                EventKind::Normal | EventKind::Recovered
            )
            && usage_coverage_equal(&proposals[0].proposal.usage, usage) == Some(true)
            && proposals[0].occurrences.len() == 1
            && proposals[0].occurrences[0].source_file_id == key.source_file_id
            && proposals[0].occurrences[0].file_generation == key.file_generation
    }

    fn replace_legacy_residual(
        &mut self,
        key: WindowKey,
        window: &LegacyReconciliationWindow,
        delta: &NormalizedTokenUsage,
        covered: &UsageSum,
        residual: &NormalizedTokenUsage,
    ) -> Result<bool, ProcessorError> {
        let Some(proposals) = self.reconciliation.window_proposals.get(&key) else {
            return Ok(false);
        };
        if proposals.len() != 1
            || proposals[0].fact.evidence_kind != EvidenceKind::Legacy
            || proposals[0].fact.response_id.is_some()
            || !matches!(
                proposals[0].proposal.kind,
                EventKind::Normal | EventKind::Recovered
            )
            || proposals[0].occurrences.len() != 1
            || proposals[0].occurrences[0].source_file_id != key.source_file_id
            || proposals[0].occurrences[0].file_generation != key.file_generation
        {
            return Ok(false);
        }
        let Some(previous) = (match &window.previous_total {
            UsageValue::Valid(previous) => Some(previous.clone()),
            UsageValue::Missing | UsageValue::Invalid => None,
        }) else {
            return Ok(false);
        };
        let UsageValue::Valid(current) = &window.current_total else {
            return Ok(false);
        };
        let proof = sum_usage_with(covered, residual)?;
        if usage_sum_coverage_equal(&proof, delta) != Some(true) {
            return Ok(false);
        }
        let mut binding = proposals[0].clone();
        if usage_coverage_equal(&binding.proposal.usage, residual) != Some(true) {
            binding.proposal.kind = EventKind::Recovered;
        }
        binding.proposal.usage = residual.clone();
        binding.proposal.event_id = event_id(&binding.proposal, Some(&previous), current);
        binding.fact.event_id = binding.proposal.event_id.clone();
        for occurrence in &mut binding.occurrences {
            occurrence.event_id = binding.proposal.event_id.clone();
        }
        let proposals = self
            .reconciliation
            .window_proposals
            .get_mut(&key)
            .expect("proposal was checked above");
        proposals.clear();
        if !usage_is_zero(residual) {
            proposals.push(binding);
        }
        Ok(true)
    }

    fn response_occurs_in_window(
        &self,
        key: &ResponseKey,
        metadata: &ReconciliationWindowMetadata,
    ) -> bool {
        let window_key = WindowKey {
            source_file_id: metadata.source_file_id,
            file_generation: metadata.file_generation,
            start_offset: metadata.source_start_offset,
        };
        let durably_assigned = self
            .reconciliation_baseline
            .windows
            .get(&window_key)
            .is_some_and(|window| window.explicit_response_ids.contains(&key.response_id));
        self.reconciliation
            .response_occurrences
            .get(key)
            .into_iter()
            .flatten()
            .any(|occurrence| {
                occurrence.source_file_id == metadata.source_file_id
                    && occurrence.file_generation == metadata.file_generation
                    && (durably_assigned
                        || (occurrence.source_start_offset >= metadata.source_start_offset
                            && occurrence.source_start_offset < metadata.source_end_offset)
                        || (self.response_adjacent_to_legacy(
                            window_key,
                            &key.response_id,
                            self.legacy_record_range(window_key),
                        ) && !self
                            .after_boundary_is_proven_new_call(window_key, key)
                            .unwrap_or(false)))
            })
    }

    fn recompute_current_turn_accounted(
        &mut self,
        turn_key: &str,
        source_offset: u64,
    ) -> Result<(), ProcessorError> {
        if !self
            .state
            .open_turn
            .as_ref()
            .is_some_and(|turn| turn.turn_key == turn_key)
        {
            return Ok(());
        }
        let key = PersistedTurnKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            turn_key: turn_key.to_owned(),
        };
        let (accounted, count) = match self.accounted_for_turn(&key) {
            Ok(value) => value,
            Err(error) => {
                self.fatal_anomaly(AnomalyCode::ArithmeticOverflow, source_offset);
                return Err(error);
            }
        };
        if let Some(current) = &mut self.state.open_turn
            && current.turn_key == turn_key
        {
            current.accounted = accounted;
            current.accounted_candidate_count = count;
        }
        Ok(())
    }

    fn accounted_for_turn(
        &mut self,
        turn_key: &PersistedTurnKey,
    ) -> Result<(NormalizedTokenUsage, u64), ProcessorError> {
        let windows = self
            .reconciliation
            .window_metadata
            .iter()
            .filter_map(|(key, metadata)| {
                (metadata.source_file_id == turn_key.source_file_id
                    && metadata.file_generation == turn_key.file_generation
                    && metadata.owning_thread_id == self.context.owning_thread_id
                    && metadata.turn_key.as_deref() == Some(turn_key.turn_key.as_str()))
                .then_some(*key)
            })
            .collect::<Vec<_>>();
        let mut seen_responses = BTreeSet::new();
        let mut usages = Vec::new();
        let mut count = 0u64;
        for key in windows {
            let Some(legacy_covered_response_ids) = self
                .reconciliation
                .windows
                .get(&key)
                .map(|window| window.legacy_covered_response_ids.clone())
            else {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, key.start_offset);
                continue;
            };
            for response_id in legacy_covered_response_ids {
                if !seen_responses.insert(response_id.clone()) {
                    self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, key.start_offset);
                    continue;
                }
                let response_key = ResponseKey {
                    owning_thread_id: self.context.owning_thread_id.clone(),
                    response_id,
                };
                let Some(binding) = self.reconciliation.bindings.get(&response_key) else {
                    self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, key.start_offset);
                    continue;
                };
                usages.push(binding.proposal.usage.clone());
                count = count
                    .checked_add(1)
                    .ok_or(ProcessorError::ArithmeticOverflow)?;
            }
            for proposal in self
                .reconciliation
                .window_proposals
                .get(&key)
                .into_iter()
                .flatten()
                .map(|binding| &binding.proposal)
            {
                usages.push(proposal.usage.clone());
                count = count
                    .checked_add(1)
                    .ok_or(ProcessorError::ArithmeticOverflow)?;
            }
        }
        let accounted = if usages.is_empty() {
            NormalizedTokenUsage::zero()
        } else {
            let mut total = usages.remove(0);
            for usage in usages {
                total = processor_checked_add(&total, &usage)?;
            }
            total
        };
        Ok((accounted, count))
    }

    fn block_turn_required(&mut self, turn_key: &PersistedTurnKey) {
        if let Some(turn) = &mut self.state.open_turn
            && self.context.source_file_id == turn_key.source_file_id
            && self.context.file_generation == turn_key.file_generation
            && turn.turn_key == turn_key.turn_key
        {
            turn.blocks.required_invalid = true;
        }
    }

    fn has_uncovered_compaction(&self, turn_key: &PersistedTurnKey) -> bool {
        self.reconciliation
            .window_metadata
            .iter()
            .any(|(key, metadata)| {
                if metadata.source_file_id != turn_key.source_file_id
                    || metadata.file_generation != turn_key.file_generation
                    || metadata.owning_thread_id != self.context.owning_thread_id
                    || metadata.turn_key.as_deref() != Some(turn_key.turn_key.as_str())
                {
                    return false;
                }
                let Some(window) = self.reconciliation.windows.get(key) else {
                    return true;
                };
                window.explicit_response_ids.iter().any(|response_id| {
                    if window.legacy_covered_response_ids.contains(response_id) {
                        return false;
                    }
                    let response_key = ResponseKey {
                        owning_thread_id: metadata.owning_thread_id.clone(),
                        response_id: response_id.clone(),
                    };
                    self.reconciliation
                        .bindings
                        .get(&response_key)
                        .is_some_and(|binding| binding.fact.operation == CodexOperation::Compaction)
                })
            })
    }

    fn reconcile_affected_turn(
        &mut self,
        turn_key: Option<&str>,
        source_offset: u64,
    ) -> Result<(), ProcessorError> {
        let Some(turn_key) = turn_key else {
            return Ok(());
        };
        self.recompute_current_turn_accounted(turn_key, source_offset)?;

        let affected_keys = self
            .reconciliation
            .affected_turns
            .iter()
            .filter_map(|(key, affected)| {
                (affected.snapshot.owning_thread_id == self.context.owning_thread_id
                    && key.turn_key == turn_key)
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        for key in affected_keys {
            let Some(expected) = self
                .reconciliation_baseline
                .affected_turns
                .get(&key)
                .map(|affected| affected.snapshot.clone())
            else {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                return Ok(());
            };
            if expected.status == PersistedTurnStatus::Open {
                continue;
            }
            let window_exists = self
                .reconciliation
                .window_metadata
                .values()
                .any(|metadata| {
                    metadata.source_file_id == key.source_file_id
                        && metadata.file_generation == key.file_generation
                        && metadata.owning_thread_id == expected.owning_thread_id
                        && metadata.turn_key.as_deref() == Some(key.turn_key.as_str())
                });
            if !window_exists && expected.state.accounted_candidate_count > 0 {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                return Ok(());
            }
            let (accounted, count) = match self.accounted_for_turn(&key) {
                Ok(value) => value,
                Err(error) => {
                    self.fatal_anomaly(AnomalyCode::ArithmeticOverflow, source_offset);
                    return Err(error);
                }
            };
            let mut replacement = expected.clone();
            replacement.state.accounted = accounted;
            replacement.state.accounted_candidate_count = count;
            replacement.quality_status =
                if replacement.state.blocks.allowed() && !replacement.state.unresolved_model_seen {
                    "complete".to_owned()
                } else {
                    "partial".to_owned()
                };

            let (closure_events, closure_occurrences) =
                match self.compensation_closure_for_turn(&key) {
                    Ok(closure) => closure,
                    Err(()) => {
                        self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                        return Ok(());
                    }
                };
            let old_occurrences = closure_occurrences
                .into_iter()
                .filter(|occurrence| {
                    occurrence.source_file_id == key.source_file_id
                        && occurrence.file_generation == key.file_generation
                })
                .collect::<Vec<_>>();
            if old_occurrences.len() > 1 {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                return Ok(());
            }
            let local_event_ids = old_occurrences
                .iter()
                .map(|occurrence| occurrence.event_id.as_str())
                .collect::<BTreeSet<_>>();
            let old_compensation_events = closure_events
                .into_iter()
                .filter(|event| local_event_ids.contains(event.event_id.as_str()))
                .collect::<Vec<_>>();
            if old_compensation_events.len() != local_event_ids.len() {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                return Ok(());
            }
            let replacement_compensation =
                if replacement.state.blocks.allowed() && !self.has_uncovered_compaction(&key) {
                    match (
                        replacement.state.start_total.as_ref(),
                        replacement.state.last_total.as_ref(),
                        replacement.ended_at_ms,
                    ) {
                        (Some(start), Some(end), Some(ended_at)) => self.compensation_proposal(
                            &replacement.state,
                            start,
                            end,
                            ended_at,
                            source_offset,
                        )?,
                        _ => None,
                    }
                } else {
                    None
                };
            let (compensation_events, compensation_occurrences) =
                if let Some(event) = replacement_compensation {
                    if old_occurrences.len() != 1 || old_compensation_events.len() != 1 {
                        self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                        return Ok(());
                    }
                    let occurrences = old_occurrences
                        .into_iter()
                        .map(|mut occurrence| {
                            occurrence.event_id = event.event_id.clone();
                            occurrence
                        })
                        .collect();
                    (vec![event], occurrences)
                } else {
                    (Vec::new(), Vec::new())
                };
            if let Some(current) = self.reconciliation.affected_turns.get_mut(&key) {
                current.snapshot = replacement;
                current.compensation_events = compensation_events;
                current.compensation_occurrences = compensation_occurrences;
            }
        }

        let upsert = self
            .patch
            .turn_upserts
            .iter()
            .find(|snapshot| {
                snapshot.owning_thread_id == self.context.owning_thread_id
                    && snapshot.key.source_file_id == self.context.source_file_id
                    && snapshot.key.file_generation == self.context.file_generation
                    && snapshot.key.turn_key == turn_key
                    && snapshot.status != PersistedTurnStatus::Open
            })
            .cloned();
        if let Some(mut snapshot) = upsert {
            let windows = self
                .reconciliation
                .window_metadata
                .values()
                .any(|metadata| {
                    metadata.source_file_id == snapshot.key.source_file_id
                        && metadata.file_generation == snapshot.key.file_generation
                        && metadata.owning_thread_id == snapshot.owning_thread_id
                        && metadata.turn_key.as_deref() == Some(turn_key)
                });
            if !windows && snapshot.state.accounted_candidate_count > 0 {
                self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
                return Ok(());
            }
            let (accounted, count) = match self.accounted_for_turn(&snapshot.key) {
                Ok(value) => value,
                Err(error) => {
                    self.fatal_anomaly(AnomalyCode::ArithmeticOverflow, source_offset);
                    return Err(error);
                }
            };
            snapshot.state.accounted = accounted;
            snapshot.state.accounted_candidate_count = count;
            snapshot.quality_status =
                if snapshot.state.blocks.allowed() && !snapshot.state.unresolved_model_seen {
                    "complete".to_owned()
                } else {
                    "partial".to_owned()
                };
            self.replace_forward_compensation(&snapshot, source_offset)?;
            self.upsert_turn(snapshot);
        }
        Ok(())
    }

    fn replace_forward_compensation(
        &mut self,
        snapshot: &PersistedTurnSnapshot,
        source_offset: u64,
    ) -> Result<(), ProcessorError> {
        let old_events = self
            .patch
            .events
            .iter()
            .filter(|event| {
                event.kind == EventKind::TurnCompensation
                    && event.turn_key.as_deref() == Some(snapshot.key.turn_key.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let old_ids = old_events
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<BTreeSet<_>>();
        let old_occurrences = self
            .patch
            .occurrences
            .iter()
            .filter(|occurrence| old_ids.contains(&occurrence.event_id))
            .cloned()
            .collect::<Vec<_>>();
        let desired =
            if snapshot.state.blocks.allowed() && !self.has_uncovered_compaction(&snapshot.key) {
                match (
                    snapshot.state.start_total.as_ref(),
                    snapshot.state.last_total.as_ref(),
                    snapshot.ended_at_ms,
                ) {
                    (Some(start), Some(end), Some(ended_at)) => self.compensation_proposal(
                        &snapshot.state,
                        start,
                        end,
                        ended_at,
                        source_offset,
                    )?,
                    _ => None,
                }
            } else {
                None
            };
        if desired
            .as_ref()
            .is_some_and(|desired| old_events.len() == 1 && old_events.first() == Some(desired))
        {
            return Ok(());
        }
        self.patch
            .events
            .retain(|event| !old_ids.contains(&event.event_id));
        self.patch
            .occurrences
            .retain(|occurrence| !old_ids.contains(&occurrence.event_id));
        self.patch
            .facts
            .retain(|fact| !old_ids.contains(&fact.event_id));
        for event_id in &old_ids {
            self.event_ids.remove(event_id);
        }
        let Some(event) = desired else {
            return Ok(());
        };
        let end_range = self
            .forward_turn_end_ranges
            .get(&snapshot.key)
            .copied()
            .or_else(|| {
                old_occurrences.first().map(|occurrence| {
                    (occurrence.source_start_offset, occurrence.source_end_offset)
                })
            });
        let Some((start_offset, end_offset)) = end_range else {
            self.fatal_anomaly(AnomalyCode::LegacyCoverageAmbiguous, source_offset);
            return Ok(());
        };
        self.push_event(event, start_offset, end_offset);
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
        let Some(turn_key) = self
            .state
            .open_turn
            .as_ref()
            .map(|turn| turn.turn_key.clone())
        else {
            return Ok(());
        };
        let Some(turn_snapshot) = self.state.open_turn.as_ref() else {
            return Ok(());
        };
        let raw_turn_id = turn_snapshot.raw_turn_id.clone();
        if turn_id.is_some() && raw_turn_id != turn_id {
            self.anomaly(AnomalyCode::TurnIdMismatch, Some(start_offset));
            return Ok(());
        }
        self.close_pending_evidence(raw_turn_id.as_deref(), start_offset);
        self.forward_turn_end_ranges.insert(
            PersistedTurnKey {
                source_file_id: self.context.source_file_id,
                file_generation: self.context.file_generation,
                turn_key: turn_key.clone(),
            },
            (start_offset, end_offset),
        );
        for (key, metadata) in &self.reconciliation.window_metadata {
            if metadata.source_file_id == self.context.source_file_id
                && metadata.file_generation == self.context.file_generation
                && metadata.owning_thread_id == self.context.owning_thread_id
                && metadata.turn_key.as_deref() == Some(turn_key.as_str())
                && let Some(window) = self.reconciliation.windows.get_mut(key)
            {
                window.closed = true;
            }
        }
        self.recompute_current_turn_accounted(&turn_key, start_offset)?;
        let Some(mut turn) = self.state.open_turn.take() else {
            return Ok(());
        };
        if timestamp_ms.is_none() {
            turn.blocks.time_missing = true;
        }
        if turn.unresolved_model_seen {
            turn.blocks.model_unresolved = true;
        }
        let persisted_turn_key = PersistedTurnKey {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            turn_key: turn_key.clone(),
        };
        if turn.blocks.allowed()
            && !self.has_uncovered_compaction(&persisted_turn_key)
            && let (Some(start), Some(end), Some(ended_at)) = (
                turn.start_total.clone(),
                turn.last_total.clone(),
                timestamp_ms,
            )
        {
            self.compensate(&mut turn, &start, &end, ended_at, start_offset, end_offset)?;
        }
        let snapshot = self.snapshot(&turn, status.into(), timestamp_ms, Some(end_offset));
        self.upsert_turn(snapshot);
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
        if let Some(event) =
            self.compensation_proposal(turn, start, end, occurred_at_ms, start_offset)?
        {
            self.push_event(event, start_offset, end_offset);
        }
        Ok(())
    }

    fn compensation_proposal(
        &mut self,
        turn: &TurnState,
        start: &NormalizedTokenUsage,
        end: &NormalizedTokenUsage,
        occurred_at_ms: i64,
        source_offset: u64,
    ) -> Result<Option<CanonicalUsageProposal>, ProcessorError> {
        let delta = match processor_checked_sub(end, start) {
            Ok(delta) => delta,
            Err(ProcessorError::NegativeDifference) => {
                self.anomaly_for_turn(
                    AnomalyCode::TotalChainReset,
                    Some(source_offset),
                    Some(&turn.turn_key),
                );
                return Ok(None);
            }
            Err(ProcessorError::CacheWriteNegativeDifference) => {
                self.anomaly_for_turn(
                    AnomalyCode::TurnCacheWriteDeltaNegative,
                    Some(source_offset),
                    Some(&turn.turn_key),
                );
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let missing = match processor_checked_sub(&delta, &turn.accounted) {
            Ok(missing) => missing,
            Err(ProcessorError::NegativeDifference) => {
                self.anomaly_for_turn(
                    AnomalyCode::TurnAccountedExceedsTotal,
                    Some(source_offset),
                    Some(&turn.turn_key),
                );
                return Ok(None);
            }
            Err(ProcessorError::CacheWriteNegativeDifference) => {
                self.anomaly_for_turn(
                    AnomalyCode::TurnAccountedExceedsTotal,
                    Some(source_offset),
                    Some(&turn.turn_key),
                );
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if usage_is_zero(&missing) {
            return Ok(None);
        }
        let model = match &turn.model_state {
            TurnModelState::Single(model) => model.clone(),
            TurnModelState::Mixed => "unknown".to_owned(),
            TurnModelState::None => return Ok(None),
        };
        let reasoning_effort = if turn.unresolved_reasoning_effort_seen {
            None
        } else {
            match &turn.reasoning_effort_state {
                TurnReasoningEffortState::Single(effort) => Some(effort.clone()),
                TurnReasoningEffortState::None | TurnReasoningEffortState::Mixed => None,
            }
        };
        let mut event = CanonicalUsageProposal {
            event_id: String::new(),
            kind: EventKind::TurnCompensation,
            occurred_at_ms,
            thread_id: self.context.owning_thread_id.clone(),
            root_session_id: self.context.root_session_id.clone(),
            turn_key: Some(turn.turn_key.clone()),
            model,
            reasoning_effort,
            usage: missing.clone(),
        };
        event.event_id = event_id(&event, Some(start), end);
        Ok(Some(event))
    }

    fn push_event(&mut self, event: CanonicalUsageProposal, start_offset: u64, end_offset: u64) {
        self.patch.occurrences.push(Occurrence {
            source_file_id: self.context.source_file_id,
            file_generation: self.context.file_generation,
            source_start_offset: start_offset,
            source_end_offset: end_offset,
            event_id: event.event_id.clone(),
        });
        if self.event_ids.insert(event.event_id.clone()) {
            self.patch.facts.push(compensation_fact(&event));
            self.patch.events.push(event);
        }
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
        self.patch.anomalies.push(Anomaly {
            code,
            source_start_offset: offset,
            turn_key: self
                .state
                .open_turn
                .as_ref()
                .map(|turn| turn.turn_key.clone()),
        });
    }

    fn anomaly_for_turn(&mut self, code: AnomalyCode, offset: Option<u64>, turn_key: Option<&str>) {
        self.patch.anomalies.push(Anomaly {
            code,
            source_start_offset: offset,
            turn_key: turn_key.map(str::to_owned).or_else(|| {
                self.state
                    .open_turn
                    .as_ref()
                    .map(|turn| turn.turn_key.clone())
            }),
        });
    }

    fn fatal_anomaly(&mut self, code: AnomalyCode, offset: u64) {
        self.anomaly(code, Some(offset));
        self.needs_rebuild = true;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalDecision {
    Insert,
    Duplicate,
    Conflict,
}

pub fn compare_canonical(
    existing: Option<&CanonicalUsageProposal>,
    incoming: &CanonicalUsageProposal,
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

fn record_start_offset(record: &UsageRecord) -> Option<u64> {
    match record {
        UsageRecord::ResponseUsage { start_offset, .. }
        | UsageRecord::Compacted { start_offset, .. }
        | UsageRecord::TokenCount { start_offset, .. }
        | UsageRecord::TurnEnded { start_offset, .. }
        | UsageRecord::Gap { start_offset, .. } => Some(*start_offset),
        UsageRecord::TurnStarted { start_offset, .. } => Some(*start_offset),
        UsageRecord::TurnContext { .. } => None,
    }
}

fn response_event_id(owning_thread_id: &str, response_id: &str) -> String {
    let mut encoder = Encoder::new(b"codex-response-v6");
    encoder.text(owning_thread_id);
    encoder.text(response_id);
    encoder.finish()
}

fn compensation_fact(proposal: &CanonicalUsageProposal) -> UsageEventFact {
    UsageEventFact {
        event_id: proposal.event_id.clone(),
        owning_thread_id: proposal.thread_id.clone(),
        response_id: None,
        evidence_kind: EvidenceKind::Legacy,
        operation: CodexOperation::Response,
    }
}

fn pending_record_response_id(record: &PendingEvidenceRecord) -> Option<&str> {
    match record {
        PendingEvidenceRecord::ResponseUsage { evidence, .. } => Some(&evidence.response_id),
        PendingEvidenceRecord::Compacted { evidence, .. } => evidence
            .latest_token_usage_record
            .as_ref()
            .map(|latest| latest.response_id.as_str())
            .or(evidence.compaction_response_id.as_deref()),
    }
}

fn pending_response_id(pending: &PendingUsageEvidence) -> Option<&str> {
    pending_record_response_id(&pending.record)
}

fn pending_usage_value(pending: &PendingUsageEvidence) -> Option<&UsageValue> {
    match &pending.record {
        PendingEvidenceRecord::ResponseUsage { evidence, .. } => Some(&evidence.usage),
        PendingEvidenceRecord::Compacted { evidence, .. } => evidence
            .latest_token_usage_record
            .as_ref()
            .map(|latest| &latest.usage),
    }
}

fn reconciliation_references(
    context: &ReconciliationContext,
) -> BTreeMap<String, (CanonicalUsageProposal, UsageEventFact)> {
    let mut references = BTreeMap::new();
    for binding in context.bindings.values() {
        references.insert(
            binding.proposal.event_id.clone(),
            (binding.proposal.clone(), binding.fact.clone()),
        );
    }
    for binding in context.window_proposals.values().flatten() {
        references.insert(
            binding.proposal.event_id.clone(),
            (binding.proposal.clone(), binding.fact.clone()),
        );
    }
    references
}

fn compensation_references(
    context: &ReconciliationContext,
) -> BTreeMap<String, CanonicalUsageProposal> {
    let mut references = BTreeMap::new();
    for affected in context.affected_turns.values() {
        for proposal in &affected.compensation_events {
            references.insert(proposal.event_id.clone(), proposal.clone());
        }
    }
    references
}

type OccurrenceKey = (i64, i64, u64);

fn occurrence_key(occurrence: &Occurrence) -> OccurrenceKey {
    (
        occurrence.source_file_id,
        occurrence.file_generation,
        occurrence.source_start_offset,
    )
}

fn reconciliation_occurrences(
    context: &ReconciliationContext,
) -> BTreeMap<OccurrenceKey, Occurrence> {
    let mut occurrences = BTreeMap::new();
    for occurrence in context
        .response_occurrences
        .values()
        .flatten()
        .chain(
            context
                .window_proposals
                .values()
                .flatten()
                .flat_map(|binding| binding.occurrences.iter()),
        )
        .chain(
            context
                .affected_turns
                .values()
                .flat_map(|affected| affected.compensation_occurrences.iter()),
        )
    {
        occurrences.insert(occurrence_key(occurrence), occurrence.clone());
    }
    occurrences
}

type MarkerKey = (i64, i64, u64);

fn marker_map(markers: &[CompactionMarkerWrite]) -> BTreeMap<MarkerKey, CompactionMarkerWrite> {
    markers
        .iter()
        .cloned()
        .map(|marker| {
            (
                (
                    marker.source_file_id,
                    marker.file_generation,
                    marker.source_start_offset,
                ),
                marker,
            )
        })
        .collect()
}

fn sum_usage(usages: &[&NormalizedTokenUsage]) -> Result<UsageSum, ProcessorError> {
    let mut input_tokens = 0i64;
    let mut cached_tokens = 0i64;
    let mut output_tokens = 0i64;
    let mut reasoning_tokens = 0i64;
    let mut total_tokens = 0i64;
    let mut cache_write_total = 0i64;
    let mut has_known_cache_write = false;
    let mut has_unknown_cache_write = false;
    for usage in usages {
        input_tokens = input_tokens
            .checked_add(usage.input_tokens)
            .ok_or(ProcessorError::ArithmeticOverflow)?;
        cached_tokens = cached_tokens
            .checked_add(usage.cached_tokens)
            .ok_or(ProcessorError::ArithmeticOverflow)?;
        output_tokens = output_tokens
            .checked_add(usage.output_tokens)
            .ok_or(ProcessorError::ArithmeticOverflow)?;
        reasoning_tokens = reasoning_tokens
            .checked_add(usage.reasoning_tokens)
            .ok_or(ProcessorError::ArithmeticOverflow)?;
        total_tokens = total_tokens
            .checked_add(usage.total_tokens)
            .ok_or(ProcessorError::ArithmeticOverflow)?;
        match usage.cache_write_tokens {
            Some(value) => {
                has_known_cache_write = true;
                cache_write_total = cache_write_total
                    .checked_add(value)
                    .ok_or(ProcessorError::ArithmeticOverflow)?;
            }
            None => has_unknown_cache_write = true,
        }
    }
    let cache_write = match (has_known_cache_write, has_unknown_cache_write) {
        (true, true) => CacheWriteSum::Indeterminate,
        (true, false) => CacheWriteSum::Known(cache_write_total),
        (false, true) => CacheWriteSum::Unknown,
        (false, false) => CacheWriteSum::Known(0),
    };
    let cache_write_tokens = match cache_write {
        CacheWriteSum::Known(value) => Some(value),
        CacheWriteSum::Unknown | CacheWriteSum::Indeterminate => None,
    };
    let usage = NormalizedTokenUsage::new(
        input_tokens,
        cached_tokens,
        cache_write_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
    )
    .map_err(ProcessorError::from)?;
    Ok(UsageSum { usage, cache_write })
}

fn sum_usage_with(
    sum: &UsageSum,
    extra: &NormalizedTokenUsage,
) -> Result<UsageSum, ProcessorError> {
    let add = |left: i64, right: i64| {
        left.checked_add(right)
            .ok_or(ProcessorError::ArithmeticOverflow)
    };
    let cache_write = match (sum.cache_write, extra.cache_write_tokens) {
        (CacheWriteSum::Known(left), Some(right)) => CacheWriteSum::Known(add(left, right)?),
        (CacheWriteSum::Unknown, None) => CacheWriteSum::Unknown,
        (CacheWriteSum::Indeterminate, _) => CacheWriteSum::Indeterminate,
        (CacheWriteSum::Known(_), None) | (CacheWriteSum::Unknown, Some(_)) => {
            CacheWriteSum::Indeterminate
        }
    };
    let usage = NormalizedTokenUsage::new(
        add(sum.usage.input_tokens, extra.input_tokens)?,
        add(sum.usage.cached_tokens, extra.cached_tokens)?,
        match cache_write {
            CacheWriteSum::Known(value) => Some(value),
            CacheWriteSum::Unknown | CacheWriteSum::Indeterminate => None,
        },
        add(sum.usage.output_tokens, extra.output_tokens)?,
        add(sum.usage.reasoning_tokens, extra.reasoning_tokens)?,
        add(sum.usage.total_tokens, extra.total_tokens)?,
    )
    .map_err(ProcessorError::from)?;
    Ok(UsageSum { usage, cache_write })
}

/// Returns `None` when cache-write knownness prevents a complete coverage proof.
fn usage_coverage_equal(left: &NormalizedTokenUsage, right: &NormalizedTokenUsage) -> Option<bool> {
    if left.input_tokens != right.input_tokens
        || left.cached_tokens != right.cached_tokens
        || left.output_tokens != right.output_tokens
        || left.reasoning_tokens != right.reasoning_tokens
        || left.total_tokens != right.total_tokens
    {
        return Some(false);
    }
    match (left.cache_write_tokens, right.cache_write_tokens) {
        (Some(left), Some(right)) => Some(left == right),
        (None, None) => Some(true),
        _ => None,
    }
}

fn usage_sum_coverage_equal(sum: &UsageSum, right: &NormalizedTokenUsage) -> Option<bool> {
    if sum.usage.input_tokens != right.input_tokens
        || sum.usage.cached_tokens != right.cached_tokens
        || sum.usage.output_tokens != right.output_tokens
        || sum.usage.reasoning_tokens != right.reasoning_tokens
        || sum.usage.total_tokens != right.total_tokens
    {
        return Some(false);
    }
    match (sum.cache_write, right.cache_write_tokens) {
        (CacheWriteSum::Known(left), Some(right)) => Some(left == right),
        (CacheWriteSum::Unknown, None) => Some(true),
        (CacheWriteSum::Indeterminate, _) | (CacheWriteSum::Known(_), None) => None,
        (CacheWriteSum::Unknown, Some(_)) => None,
    }
}

fn subtract_usage_sum(
    total: &NormalizedTokenUsage,
    covered: &UsageSum,
) -> Result<Option<NormalizedTokenUsage>, ProcessorError> {
    let subtract = |left: i64, right: i64| {
        left.checked_sub(right)
            .filter(|value| *value >= 0)
            .ok_or(ProcessorError::NegativeDifference)
    };
    let cache_write_tokens = match (total.cache_write_tokens, covered.cache_write) {
        (Some(total), CacheWriteSum::Known(covered)) => Some(subtract(total, covered)?),
        (None, CacheWriteSum::Unknown) => None,
        (Some(_), CacheWriteSum::Unknown)
        | (None, CacheWriteSum::Known(_))
        | (_, CacheWriteSum::Indeterminate) => return Ok(None),
    };
    Ok(Some(
        NormalizedTokenUsage::new(
            subtract(total.input_tokens, covered.usage.input_tokens)?,
            subtract(total.cached_tokens, covered.usage.cached_tokens)?,
            cache_write_tokens,
            subtract(total.output_tokens, covered.usage.output_tokens)?,
            subtract(total.reasoning_tokens, covered.usage.reasoning_tokens)?,
            subtract(total.total_tokens, covered.usage.total_tokens)?,
        )
        .map_err(ProcessorError::from)?,
    ))
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

fn usage_is_zero(value: &NormalizedTokenUsage) -> bool {
    required_is_zero(value)
        && value.total_tokens == 0
        && value.cache_write_tokens.is_none_or(|tokens| tokens == 0)
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

/// The single Turn key rule shared by the processor and storage requests.
pub fn turn_key_for(
    thread_id: &str,
    turn_id: Option<&str>,
    start_offset: u64,
    timestamp_ms: Option<i64>,
) -> String {
    if let Some(turn_id) = turn_id {
        return turn_id.to_owned();
    }
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

fn event_id(
    event: &CanonicalUsageProposal,
    previous_total: Option<&NormalizedTokenUsage>,
    current_total: &NormalizedTokenUsage,
) -> String {
    let mut encoder = Encoder::new(b"usage-event-v2");
    encoder.text(&event.thread_id);
    encoder.optional_text(event.turn_key.as_deref());
    encoder.byte(match event.kind {
        EventKind::Normal => 0,
        EventKind::Recovered => 1,
        EventKind::TurnCompensation => 2,
    });
    encoder.i64(event.occurred_at_ms);
    encoder.optional_fingerprint(previous_total);
    encoder.fingerprint(current_total);
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

    struct ClosedTurn {
        turn: TurnState,
        status: TurnEndStatus,
    }

    struct TestResult {
        events: Vec<CanonicalUsageProposal>,
        occurrences: Vec<Occurrence>,
        anomalies: Vec<Anomaly>,
        closed_turns: Vec<ClosedTurn>,
        updated_state: UsageSourceState,
        patch: ReconciliationPatch,
    }

    struct TestProcessor(UsageProcessor);

    fn event_at_offset(result: &TestResult, offset: u64) -> &CanonicalUsageProposal {
        let occurrence = result
            .occurrences
            .iter()
            .find(|occurrence| occurrence.source_start_offset == offset)
            .expect("the source record has a canonical occurrence");
        result
            .events
            .iter()
            .find(|event| event.event_id == occurrence.event_id)
            .expect("the occurrence resolves to its canonical event")
    }

    impl TestProcessor {
        fn new(context: UsageContext, state: Option<UsageSourceState>) -> Self {
            Self::with_reconciliation(context, state, ReconciliationContext::default())
        }

        fn with_reconciliation(
            context: UsageContext,
            state: Option<UsageSourceState>,
            reconciliation: ReconciliationContext,
        ) -> Self {
            Self(UsageProcessor::new(
                context,
                state.unwrap_or_default(),
                reconciliation,
            ))
        }

        fn run(mut self, records: impl IntoIterator<Item = UsageRecord>) -> ProcessResult {
            for record in records {
                assert_eq!(
                    self.0.try_process_record(record, u64::MAX),
                    RecordApplyOutcome::Applied
                );
            }
            self.0.finish()
        }

        fn process(self, records: impl IntoIterator<Item = UsageRecord>) -> TestResult {
            let result = self.run(records);
            assert!(!result.needs_rebuild);
            let closed_turns = result
                .patch
                .turn_upserts
                .iter()
                .filter_map(|snapshot| {
                    let status = match snapshot.status {
                        PersistedTurnStatus::Open => return None,
                        PersistedTurnStatus::Completed => TurnEndStatus::Completed,
                        PersistedTurnStatus::Aborted => TurnEndStatus::Aborted,
                        PersistedTurnStatus::Failed => TurnEndStatus::Failed,
                    };
                    Some(ClosedTurn {
                        turn: snapshot.state.clone(),
                        status,
                    })
                })
                .collect();
            TestResult {
                events: result.patch.events.clone(),
                occurrences: result.patch.occurrences.clone(),
                anomalies: result.patch.anomalies.clone(),
                closed_turns,
                updated_state: result.updated_state,
                patch: result.patch,
            }
        }
    }

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

    fn response_evidence(
        response_id: &str,
        turn_id: Option<&str>,
        usage: NormalizedTokenUsage,
    ) -> ResponseUsageEvidence {
        ResponseUsageEvidence {
            response_id: response_id.to_owned(),
            thread_id: Some("thread".to_owned()),
            session_id: Some("session".to_owned()),
            turn_id: turn_id.map(str::to_owned),
            usage: UsageValue::Valid(usage),
            thread_token_usage: UsageValue::Missing,
        }
    }

    fn response_record(
        timestamp_ms: Option<i64>,
        start_offset: u64,
        response_id: &str,
        turn_id: Option<&str>,
        usage: NormalizedTokenUsage,
    ) -> UsageRecord {
        UsageRecord::ResponseUsage {
            ownership: owning(),
            timestamp_ms,
            start_offset,
            end_offset: start_offset + 10,
            evidence: response_evidence(response_id, turn_id, usage),
        }
    }

    fn compacted_record(
        timestamp_ms: Option<i64>,
        start_offset: u64,
        response_id: &str,
        turn_id: Option<&str>,
        usage: NormalizedTokenUsage,
    ) -> UsageRecord {
        UsageRecord::Compacted {
            ownership: owning(),
            timestamp_ms,
            start_offset,
            end_offset: start_offset + 10,
            evidence: CompactionEvidence {
                compaction_response_id: Some(response_id.to_owned()),
                latest_token_usage_record: Some(response_evidence(response_id, turn_id, usage)),
            },
        }
    }

    #[test]
    fn compaction_storage_patch_fold_keeps_canonical_replace_and_first_turn_cas() {
        let snapshot = |candidate_count| PersistedTurnSnapshot {
            key: PersistedTurnKey {
                source_file_id: 1,
                file_generation: 1,
                turn_key: "turn".to_owned(),
            },
            owning_thread_id: "thread".to_owned(),
            state: TurnState {
                turn_key: "turn".to_owned(),
                raw_turn_id: Some("raw-turn".to_owned()),
                started_at_ms: Some(1),
                start_offset: 0,
                start_total: None,
                last_total: None,
                accounted: known(10, 2, 1, 4, 1),
                accounted_candidate_count: candidate_count,
                model_state: TurnModelState::Single("model".to_owned()),
                unresolved_model_seen: false,
                reasoning_effort_state: TurnReasoningEffortState::None,
                unresolved_reasoning_effort_seen: false,
                blocks: CompensationBlocks::default(),
            },
            status: PersistedTurnStatus::Completed,
            ended_at_ms: Some(20),
            end_offset: Some(20),
            quality_status: "complete".to_owned(),
            state_through_offset: 20,
        };
        let event_id = "a".repeat(64);
        let mut patch = ReconciliationPatch {
            delete_event_ids: vec![event_id.clone()],
            events: vec![CanonicalUsageProposal {
                event_id,
                kind: EventKind::Normal,
                occurred_at_ms: 10,
                thread_id: "thread".to_owned(),
                root_session_id: "thread".to_owned(),
                turn_key: Some("turn".to_owned()),
                model: "model".to_owned(),
                reasoning_effort: None,
                usage: known(1, 0, 0, 1, 0),
            }],
            turn_rewrites: vec![
                TurnRewrite {
                    expected: snapshot(1),
                    replacement: snapshot(2),
                },
                TurnRewrite {
                    expected: snapshot(99),
                    replacement: snapshot(3),
                },
            ],
            ..ReconciliationPatch::default()
        };
        patch.fold();

        assert_eq!(patch.delete_event_ids, vec!["a".repeat(64)]);
        assert_eq!(patch.turn_rewrites.len(), 1);
        assert_eq!(patch.turn_rewrites[0].expected, snapshot(1));
        assert_eq!(patch.turn_rewrites[0].replacement, snapshot(3));
        assert_eq!(
            patch.counts(),
            Some(PatchCounts {
                canonical_event_count: 1,
                occurrence_count: 0,
                evidence_write_count: 1,
                write_unit_count: 3,
            })
        );
    }

    #[test]
    fn subagent_pre_context_snapshot_only_baselines_and_post_context_counts_delta() {
        let mut subagent = context(1);
        subagent.root_session_id = "root".to_owned();
        let baseline = known(10, 2, 1, 4, 1);
        let current = known(15, 3, 2, 6, 2);
        let result = TestProcessor::new(subagent, None).process(vec![
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
        assert_eq!(event.usage, known(5, 1, 1, 2, 1));
        assert!(result.anomalies.is_empty());
    }

    #[test]
    fn main_pre_context_snapshot_remains_unknown() {
        let baseline = known(10, 2, 1, 4, 1);
        let current = known(15, 3, 2, 6, 2);
        let result = TestProcessor::new(context(1), None).process(vec![
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
        let first = TestProcessor::new(context(1), None).process(records.clone());
        assert_eq!(first.events.len(), 3, "duplicate total ignores last");
        assert!(
            first
                .events
                .iter()
                .all(|event| event.kind == EventKind::Normal)
        );
        let first_response = event_at_offset(&first, 10);
        let third_response = event_at_offset(&first, 30);
        let fourth_response = event_at_offset(&first, 40);
        assert_eq!(first_response.usage, last);
        assert_eq!(third_response.usage, known(4, 0, 0, 2, 0));

        let archive = TestProcessor::new(context(2), None).process(records);
        let archive_first_response = event_at_offset(&archive, 10);
        let first_occurrence = first
            .occurrences
            .iter()
            .find(|occurrence| occurrence.event_id == first_response.event_id)
            .expect("the first response has its source occurrence");
        let archive_occurrence = archive
            .occurrences
            .iter()
            .find(|occurrence| occurrence.event_id == archive_first_response.event_id)
            .expect("the replay has a source occurrence for the same response");
        assert_eq!(archive_first_response.event_id, first_response.event_id);
        assert_ne!(archive_occurrence, first_occurrence);
        assert_eq!(
            compare_canonical(Some(first_response), archive_first_response),
            CanonicalDecision::Duplicate
        );
        assert_eq!(
            compare_occurrence(None, archive_occurrence),
            CanonicalDecision::Insert
        );
        assert_ne!(first_response.event_id, third_response.event_id);
        assert_eq!(first_response.usage, fourth_response.usage);
        assert_ne!(
            first_response.event_id, fourth_response.event_id,
            "equal request vectors at different time/total anchors stay distinct"
        );

        let mut conflicting_event = first_response.clone();
        conflicting_event.usage.output_tokens += 1;
        assert_eq!(
            compare_canonical(Some(first_response), &conflicting_event),
            CanonicalDecision::Conflict
        );
        let mut conflicting_occurrence = first_occurrence.clone();
        conflicting_occurrence.source_end_offset += 1;
        assert_eq!(
            compare_occurrence(Some(first_occurrence), &conflicting_occurrence),
            CanonicalDecision::Conflict
        );
        assert_eq!(
            compare_occurrence(Some(first_occurrence), first_occurrence),
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
        let recovered = TestProcessor::new(context(1), Some(baseline.clone())).process(vec![
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

        let no_previous = TestProcessor::new(context(1), None).process(vec![token(
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

        let unknown_cache = TestProcessor::new(
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

        let interrupted = TestProcessor::new(
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

        let reset = TestProcessor::new(context(1), Some(baseline.clone())).process(vec![token(
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
            TestProcessor::new(context(1), Some(cache_decrease_state)).process(vec![token(
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
        let first = TestProcessor::new(context(1), None).process(records.clone());
        let copy = TestProcessor::new(context(2), None).process(records);
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
        let initial = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(5),
                ..UsageSourceState::default()
            }),
        )
        .process(records);
        let persisted = initial.updated_state.clone();
        let mut persisted_reconciliation = ReconciliationContext::default();
        for window in &initial.patch.window_updates {
            let key = WindowKey {
                source_file_id: window.source_file_id,
                file_generation: window.file_generation,
                start_offset: window.source_start_offset,
            };
            persisted_reconciliation
                .windows
                .insert(key, window.state.clone());
            persisted_reconciliation.window_metadata.insert(
                key,
                ReconciliationWindowMetadata {
                    source_file_id: window.source_file_id,
                    file_generation: window.file_generation,
                    source_start_offset: window.source_start_offset,
                    source_end_offset: window.source_end_offset,
                    owning_thread_id: window.owning_thread_id.clone(),
                    turn_key: window.turn_key.clone(),
                },
            );
            let proposals = window
                .state
                .proposal_event_ids
                .iter()
                .map(|event_id| {
                    let proposal = initial
                        .patch
                        .events
                        .iter()
                        .find(|proposal| &proposal.event_id == event_id)
                        .expect("the persisted window references its canonical proposal")
                        .clone();
                    let fact = initial
                        .patch
                        .facts
                        .iter()
                        .find(|fact| &fact.event_id == event_id)
                        .expect("the persisted proposal retains its legacy evidence fact")
                        .clone();
                    let occurrences = initial
                        .patch
                        .occurrences
                        .iter()
                        .filter(|occurrence| &occurrence.event_id == event_id)
                        .cloned()
                        .collect();
                    WindowProposalBinding {
                        proposal,
                        fact,
                        occurrences,
                    }
                })
                .collect();
            persisted_reconciliation
                .window_proposals
                .insert(key, proposals);
        }
        assert_eq!(
            persisted
                .open_turn
                .as_ref()
                .unwrap()
                .accounted_candidate_count,
            1
        );
        assert_eq!(persisted.active_reasoning_effort.as_deref(), Some("high"));

        let completed = TestProcessor::with_reconciliation(
            context(1),
            Some(persisted),
            persisted_reconciliation,
        )
        .process(vec![
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
        let recovered = completed
            .events
            .iter()
            .find(|event| event.kind == EventKind::Recovered)
            .expect("missing legacy last is recovered from the counter delta");
        assert_eq!(recovered.usage, known(4, 1, 1, 2, 1));
        assert_eq!(recovered.reasoning_effort.as_deref(), Some("high"));
        let compensation = completed
            .events
            .iter()
            .find(|event| event.kind == EventKind::TurnCompensation)
            .expect("the legacy turn residual is compensated exactly once");
        assert_eq!(compensation.model, "model-a");
        assert_eq!(compensation.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(compensation.usage, known(2, 0, 0, 1, 0));
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
            let result =
                TestProcessor::new(context(1), Some(state)).process(vec![UsageRecord::TurnEnded {
                    ownership: owning(),
                    turn_id: Some("turn".to_owned()),
                    timestamp_ms: Some(100),
                    start_offset: 30,
                    end_offset: 40,
                    status,
                }]);
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
            let result = TestProcessor::new(
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
        let result = TestProcessor::new(
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
        let result = close_existing_turn(excessive, TurnEndStatus::Completed);
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
        let primary = TestProcessor::new(context(1), Some(copy_state.clone()))
            .process(duplicate_records.clone());
        let archive = TestProcessor::new(context(2), Some(copy_state)).process(duplicate_records);
        assert_eq!(primary.events.len(), 1);
        assert_eq!(archive.events.len(), 1);
        assert_eq!(primary.events[0].event_id, archive.events[0].event_id);
        assert_eq!(primary.closed_turns[0].turn.accounted_candidate_count, 1);
        assert_eq!(archive.closed_turns[0].turn.accounted_candidate_count, 1);

        let gap_then_turn = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(5),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::Gap {
                ownership: owning(),
                kind: GapKind::Parser,
                start_offset: 5,
                end_offset: 10,
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

        let after_new_baseline = TestProcessor::new(context(1), Some(gap_then_turn.updated_state))
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
        let high = TestProcessor::new(context(1), None)
            .process(records)
            .events
            .pop()
            .expect("canonical event");
        let mut medium = high.clone();
        medium.reasoning_effort = Some("medium".to_owned());
        let current_total = known(10, 2, 1, 4, 1);
        medium.event_id = event_id(&medium, None, &current_total);

        assert_eq!(
            crate::codex::normalization::canonical_algorithm_for(
                crate::codex::normalization::USAGE_PARSER_VERSION,
            ),
            Some(6)
        );
        assert_eq!(crate::codex::normalization::USAGE_PARSER_VERSION, 12);
        assert_eq!(
            crate::codex::normalization::USAGE_CANONICAL_ALGORITHM_VERSION,
            6
        );
        assert_eq!(
            high.event_id,
            event_id(&high, None, &current_total),
            "replay is stable"
        );
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
            TestProcessor::new(
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

    #[test]
    fn compaction_legacy_covered_keeps_only_the_legacy_residual() {
        let baseline = known(100, 10, 5, 40, 3);
        let legacy = known(10, 1, 2, 5, 1);
        let compact = known(20, 3, 4, 8, 2);
        let delta = processor_checked_add(&legacy, &compact).unwrap();
        let current = processor_checked_add(&baseline, &delta).unwrap();
        let second_usage = known(5, 1, 1, 3, 1);
        let total_after_two = processor_checked_add(&compact, &second_usage).unwrap();
        let first_owning_sequence = TestProcessor::new(context(1), None).process(vec![
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            response_record(Some(100), 20, "forward-r1", Some("turn"), compact.clone()),
            token(110, 30, compact.clone(), UsageValue::Valid(compact.clone())),
            response_record(
                Some(120),
                40,
                "forward-r2",
                Some("turn"),
                second_usage.clone(),
            ),
            token(
                130,
                50,
                total_after_two,
                UsageValue::Valid(second_usage.clone()),
            ),
        ]);
        assert_eq!(first_owning_sequence.events.len(), 2);
        for (response_id, usage, offset) in [
            ("forward-r1", &compact, 20),
            ("forward-r2", &second_usage, 40),
        ] {
            let event = first_owning_sequence
                .events
                .iter()
                .find(|event| event.event_id == response_event_id("thread", response_id))
                .expect("each modern response remains one canonical event");
            assert_eq!(&event.usage, usage);
            assert_eq!(event.turn_key.as_deref(), Some("turn"));
            assert!(first_owning_sequence.occurrences.iter().any(|occurrence| {
                occurrence.source_start_offset == offset
                    && occurrence.event_id == response_event_id("thread", response_id)
            }));
        }
        assert_eq!(first_owning_sequence.patch.window_updates.len(), 2);
        for (start_offset, response_id) in [(20, "forward-r1"), (40, "forward-r2")] {
            let window = first_owning_sequence
                .patch
                .window_updates
                .iter()
                .find(|window| window.source_start_offset == start_offset)
                .expect("each response and matching counter form a physical window");
            assert_eq!(window.turn_key.as_deref(), Some("turn"));
            assert_eq!(window.state.explicit_response_ids, vec![response_id]);
        }
        assert!(first_owning_sequence.patch.turn_upserts.is_empty());
        assert!(first_owning_sequence.patch.turn_rewrites.is_empty());

        let result = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: Some("high".to_owned()),
            },
            token(100, 20, current.clone(), UsageValue::Valid(legacy.clone())),
            compacted_record(Some(110), 30, "compact-r", Some("turn"), compact.clone()),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(120),
                start_offset: 40,
                end_offset: 50,
                status: TurnEndStatus::Completed,
            },
        ]);

        assert_eq!(result.events.len(), 2);
        assert!(result.events.iter().any(|event| {
            event.kind == EventKind::Normal
                && event.usage == compact
                && event.event_id == response_event_id("thread", "compact-r")
        }));
        assert!(
            result
                .patch
                .facts
                .iter()
                .any(|fact| fact.response_id.as_deref() == Some("compact-r")
                    && fact.operation == CodexOperation::Compaction)
        );
        let turn = &result.closed_turns[0].turn;
        assert_eq!(
            turn.accounted,
            processor_checked_add(&legacy, &compact).unwrap()
        );
        assert_eq!(turn.accounted_candidate_count, 2);
        assert!(
            result
                .events
                .iter()
                .all(|event| event.kind != EventKind::TurnCompensation)
        );

        let same_call_total = processor_checked_add(&baseline, &legacy).unwrap();
        let same_call = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("same-call-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(
                100,
                20,
                same_call_total.clone(),
                UsageValue::Valid(legacy.clone()),
            ),
            compacted_record(
                Some(110),
                30,
                "same-call-c",
                Some("same-call-turn"),
                legacy.clone(),
            ),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("same-call-turn".to_owned()),
                timestamp_ms: Some(120),
                start_offset: 40,
                end_offset: 50,
                status: TurnEndStatus::Completed,
            },
        ]);
        assert_eq!(same_call.events.len(), 1);
        assert_eq!(
            same_call.events[0].event_id,
            response_event_id("thread", "same-call-c")
        );
        assert_eq!(same_call.closed_turns[0].turn.accounted, legacy);
        assert_eq!(same_call.closed_turns[0].turn.accounted_candidate_count, 1);

        let next_call_result = TestProcessor::new(context(1), None).process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("next-call-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            response_record(
                Some(100),
                20,
                "first-r",
                Some("next-call-turn"),
                legacy.clone(),
            ),
            token(110, 30, baseline.clone(), UsageValue::Valid(legacy.clone())),
            response_record(
                Some(120),
                40,
                "next-r",
                Some("next-call-turn"),
                compact.clone(),
            ),
        ]);
        assert!(
            next_call_result
                .anomalies
                .iter()
                .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
        );
        assert!(
            next_call_result
                .events
                .iter()
                .any(|event| { event.event_id == response_event_id("thread", "next-r") })
        );
        assert_eq!(
            next_call_result
                .updated_state
                .reconciliation_carry
                .open_window_start_offset,
            Some(40)
        );
        assert!(
            next_call_result
                .updated_state
                .reconciliation_carry
                .pending_response_ids
                .contains(&"next-r".to_owned())
        );
        assert!(next_call_result.patch.window_updates.iter().any(|window| {
            window.state.legacy_covered_response_ids == vec!["first-r"]
                && window.state.proposal_event_ids.is_empty()
        }));
        assert_eq!(
            next_call_result.updated_state.open_turn.unwrap().accounted,
            legacy
        );

        let adjacent_new_call = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("legacy-only-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(100, 20, same_call_total, UsageValue::Valid(legacy.clone())),
            response_record(
                Some(110),
                30,
                "next-r",
                Some("legacy-only-turn"),
                compact.clone(),
            ),
        ]);
        assert!(
            adjacent_new_call
                .anomalies
                .iter()
                .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
        );
        assert_eq!(
            adjacent_new_call
                .updated_state
                .reconciliation_carry
                .open_window_start_offset,
            Some(30)
        );
        assert!(
            adjacent_new_call
                .updated_state
                .reconciliation_carry
                .pending_response_ids
                .contains(&"next-r".to_owned())
        );
        assert!(adjacent_new_call.patch.window_updates.iter().any(|window| {
            window.state.legacy_covered_response_ids.is_empty()
                && window.state.proposal_event_ids.len() == 1
                && !window
                    .state
                    .explicit_response_ids
                    .contains(&"next-r".to_owned())
        }));

        let zero_delta_new_call = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("zero-delta-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(100, 20, baseline.clone(), UsageValue::Valid(legacy.clone())),
            response_record(
                Some(110),
                30,
                "zero-delta-next-r",
                Some("zero-delta-turn"),
                compact.clone(),
            ),
        ]);
        assert!(
            zero_delta_new_call
                .anomalies
                .iter()
                .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
        );
        assert_eq!(
            zero_delta_new_call
                .updated_state
                .reconciliation_carry
                .open_window_start_offset,
            Some(30)
        );
        assert!(
            zero_delta_new_call
                .updated_state
                .reconciliation_carry
                .pending_response_ids
                .contains(&"zero-delta-next-r".to_owned())
        );

        for (response_id, last_usage) in [
            ("zero-delta-missing-r", UsageValue::Missing),
            ("zero-delta-invalid-r", UsageValue::Invalid),
        ] {
            let result = TestProcessor::new(
                context(1),
                Some(UsageSourceState {
                    previous_total: Some(baseline.clone()),
                    previous_total_offset: Some(10),
                    ..UsageSourceState::default()
                }),
            )
            .process(vec![
                UsageRecord::TurnStarted {
                    ownership: owning(),
                    turn_id: Some("zero-delta-unproven-turn".to_owned()),
                    timestamp_ms: Some(90),
                    start_offset: 11,
                },
                UsageRecord::TurnContext {
                    ownership: owning(),
                    model: Some("model-a".to_owned()),
                    reasoning_effort: None,
                },
                token(100, 20, baseline.clone(), last_usage),
                response_record(
                    Some(110),
                    30,
                    response_id,
                    Some("zero-delta-unproven-turn"),
                    compact.clone(),
                ),
            ]);

            assert!(
                result
                    .anomalies
                    .iter()
                    .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
            );
            assert_eq!(
                result
                    .updated_state
                    .reconciliation_carry
                    .open_window_start_offset,
                Some(30)
            );
            assert!(
                result
                    .updated_state
                    .reconciliation_carry
                    .pending_response_ids
                    .contains(&response_id.to_owned())
            );
            assert!(result.patch.window_updates.iter().all(|window| {
                !window
                    .state
                    .explicit_response_ids
                    .contains(&response_id.to_owned())
            }));
        }

        let actual_only_compaction = known(20, 3, 4, 8, 2);
        let boundary_cases = [
            (
                "resp_fx_52248826137f0b4724730588",
                known(34_410_875, 33_881_088, 0, 48_386, 18_976),
                known(31_449, 17_024, 0, 153, 46),
            ),
            (
                "resp_fx_1e475020ba9d0fc38f627f20",
                known(193_988, 0, 0, 2_674, 1_603),
                known(35_439, 18_048, 0, 1_219, 398),
            ),
            (
                "resp_fx_844268d5313c642b942bf46e",
                known(19_844_210, 19_231_360, 0, 55_458, 24_962),
                known(38_385, 18_048, 0, 171, 36),
            ),
        ];
        for (index, (response_id, baseline, usage)) in boundary_cases.into_iter().enumerate() {
            let turn_id = format!("zero-estimate-turn-{index}");
            let compaction_id = format!("actual-only-compaction-{index}");
            let total = processor_checked_add(&baseline, &usage).unwrap();
            let result = TestProcessor::new(
                context(1),
                Some(UsageSourceState {
                    previous_total: Some(baseline.clone()),
                    previous_total_offset: Some(10),
                    ..UsageSourceState::default()
                }),
            )
            .process(vec![
                UsageRecord::TurnStarted {
                    ownership: owning(),
                    turn_id: Some(turn_id.clone()),
                    timestamp_ms: Some(90),
                    start_offset: 11,
                },
                UsageRecord::TurnContext {
                    ownership: owning(),
                    model: Some("model-a".to_owned()),
                    reasoning_effort: None,
                },
                response_record(
                    Some(100),
                    20,
                    &compaction_id,
                    Some(&turn_id),
                    actual_only_compaction.clone(),
                ),
                compacted_record(
                    Some(101),
                    50,
                    &compaction_id,
                    Some(&turn_id),
                    actual_only_compaction.clone(),
                ),
                token(110, 70, baseline, UsageValue::Invalid),
                response_record(Some(120), 100, response_id, Some(&turn_id), usage.clone()),
                token(130, 200, total, UsageValue::Valid(usage.clone())),
            ]);

            assert!(
                result
                    .anomalies
                    .iter()
                    .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
            );
            assert_eq!(result.events.len(), 2);
            assert!(
                result
                    .events
                    .iter()
                    .any(|event| { event.event_id == response_event_id("thread", &compaction_id) })
            );
            assert!(
                result
                    .events
                    .iter()
                    .any(|event| event.event_id == response_event_id("thread", response_id))
            );
            assert_eq!(
                result
                    .occurrences
                    .iter()
                    .filter(|occurrence| {
                        occurrence.event_id == response_event_id("thread", &compaction_id)
                    })
                    .count(),
                2
            );
            let turn = result.updated_state.open_turn.as_ref().unwrap();
            assert_eq!(turn.accounted, usage);
            assert_eq!(turn.accounted_candidate_count, 1);

            let estimate_window = result
                .patch
                .window_updates
                .iter()
                .find(|window| window.source_start_offset == 20)
                .expect("zero-estimate window");
            assert_eq!(
                estimate_window.state.explicit_response_ids,
                vec![compaction_id.clone()]
            );
            assert!(estimate_window.state.legacy_covered_response_ids.is_empty());
            assert!(estimate_window.state.proposal_event_ids.is_empty());
            let matching_window = result
                .patch
                .window_updates
                .iter()
                .find(|window| window.source_start_offset == 100)
                .expect("matching counter window");
            assert_eq!(
                matching_window.state.explicit_response_ids,
                vec![response_id.to_owned()]
            );
            assert_eq!(
                matching_window.state.legacy_covered_response_ids,
                vec![response_id.to_owned()]
            );
            assert!(matching_window.state.proposal_event_ids.is_empty());
        }

        let reset_previous = known(23_916_820, 23_080_192, 0, 64_872, 27_756);
        let reset_anchor = known(101_793, 98_944, 0, 539, 153);
        let after_reset_call = known(103_888, 101_632, 0, 307, 46);
        let after_reset_total = processor_checked_add(&reset_anchor, &after_reset_call).unwrap();
        let reset_turn = "reset-boundary-turn";
        let reset_anchor_id = "resp_fx_a755a8acd3917d1e4a381282";
        let after_reset_id = "resp_fx_f11199d5170c3928c7f68534";
        let reset_boundary = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(reset_previous),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some(reset_turn.to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            response_record(
                Some(100),
                20,
                reset_anchor_id,
                Some(reset_turn),
                reset_anchor.clone(),
            ),
            token(
                110,
                30,
                reset_anchor.clone(),
                UsageValue::Valid(reset_anchor.clone()),
            ),
            response_record(
                Some(120),
                100,
                after_reset_id,
                Some(reset_turn),
                after_reset_call.clone(),
            ),
            token(
                130,
                200,
                after_reset_total,
                UsageValue::Valid(after_reset_call.clone()),
            ),
        ]);

        assert!(
            reset_boundary
                .anomalies
                .iter()
                .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
        );
        assert_eq!(reset_boundary.events.len(), 2);
        assert!(
            reset_boundary
                .events
                .iter()
                .any(|event| { event.event_id == response_event_id("thread", reset_anchor_id) })
        );
        assert!(
            reset_boundary
                .events
                .iter()
                .any(|event| { event.event_id == response_event_id("thread", after_reset_id) })
        );
        let reset_turn_state = reset_boundary.updated_state.open_turn.as_ref().unwrap();
        assert_eq!(
            reset_turn_state.accounted,
            processor_checked_add(&reset_anchor, &after_reset_call).unwrap()
        );
        assert_eq!(reset_turn_state.accounted_candidate_count, 2);
        assert!(reset_boundary.patch.window_updates.iter().any(|window| {
            window.source_start_offset == 20
                && window.state.legacy_covered_response_ids == vec![reset_anchor_id]
                && window.state.proposal_event_ids.is_empty()
        }));
        assert!(reset_boundary.patch.window_updates.iter().any(|window| {
            window.source_start_offset == 100
                && window.state.legacy_covered_response_ids == vec![after_reset_id]
                && window.state.proposal_event_ids.is_empty()
        }));
    }

    #[test]
    fn compaction_legacy_covered_residual_decomposition_replaces_the_full_delta() {
        let baseline = known(100, 10, 5, 40, 3);
        let legacy = known(10, 1, 2, 5, 1);
        let compact = known(20, 3, 4, 8, 2);
        let delta = processor_checked_add(&legacy, &compact).unwrap();
        let current = processor_checked_add(&baseline, &delta).unwrap();
        let result = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(100, 20, current, UsageValue::Missing),
            compacted_record(Some(110), 30, "compact-r", None, compact.clone()),
        ]);

        let legacy_proposals = result
            .events
            .iter()
            .filter(|event| event.kind == EventKind::Recovered)
            .collect::<Vec<_>>();
        assert_eq!(legacy_proposals.len(), 1);
        assert_eq!(legacy_proposals[0].usage, legacy);
        assert!(result.events.iter().any(|event| event.event_id
            == response_event_id("thread", "compact-r")
            && event.usage == compact));
        assert!(
            result
                .patch
                .window_updates
                .iter()
                .all(|window| window.state.proposal_event_ids
                    == vec![legacy_proposals[0].event_id.clone()])
        );
    }

    #[test]
    fn compaction_late_evidence_reconciles_legacy_then_marker() {
        let baseline = known(100, 10, 5, 40, 3);
        let legacy = known(10, 1, 2, 5, 1);
        let compact = known(20, 3, 4, 8, 2);
        let delta = processor_checked_add(&legacy, &compact).unwrap();
        let current = processor_checked_add(&baseline, &delta).unwrap();
        let result = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(100, 20, current.clone(), UsageValue::Valid(legacy.clone())),
            response_record(Some(110), 30, "compact-r", Some("turn"), compact.clone()),
            compacted_record(Some(111), 40, "compact-r", Some("turn"), compact.clone()),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(120),
                start_offset: 50,
                end_offset: 60,
                status: TurnEndStatus::Completed,
            },
        ]);

        assert_eq!(result.events.len(), 2);
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| event.event_id == response_event_id("thread", "compact-r"))
                .count(),
            1
        );
        assert_eq!(
            result
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.event_id == response_event_id("thread", "compact-r"))
                .count(),
            2
        );
        assert!(
            result
                .patch
                .facts
                .iter()
                .any(|fact| fact.response_id.as_deref() == Some("compact-r")
                    && fact.operation == CodexOperation::Compaction)
        );
        assert_eq!(result.closed_turns[0].turn.accounted_candidate_count, 2);
        assert_eq!(
            result.closed_turns[0].turn.accounted,
            processor_checked_add(&legacy, &compact).unwrap()
        );
        assert!(
            result
                .events
                .iter()
                .all(|event| event.kind != EventKind::TurnCompensation)
        );

        let ignored_interval = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("ignored-interval-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            response_record(
                Some(100),
                20,
                "ignored-interval-r",
                Some("ignored-interval-turn"),
                legacy.clone(),
            ),
            token(
                110,
                40,
                processor_checked_add(&baseline, &legacy).unwrap(),
                UsageValue::Valid(legacy.clone()),
            ),
        ]);
        assert!(
            ignored_interval
                .anomalies
                .iter()
                .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
        );
        assert_eq!(ignored_interval.events.len(), 1);
        assert_eq!(
            ignored_interval.events[0].event_id,
            response_event_id("thread", "ignored-interval-r")
        );
        assert!(ignored_interval.patch.window_updates.iter().any(|window| {
            window.state.legacy_covered_response_ids == vec!["ignored-interval-r"]
                && window.state.proposal_event_ids.is_empty()
        }));

        let explicit_then_counter = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("explicit-first-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            response_record(
                Some(100),
                20,
                "legacy-r",
                Some("explicit-first-turn"),
                legacy.clone(),
            ),
            token(110, 30, current, UsageValue::Valid(legacy.clone())),
            compacted_record(
                Some(120),
                40,
                "late-c",
                Some("explicit-first-turn"),
                compact.clone(),
            ),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("explicit-first-turn".to_owned()),
                timestamp_ms: Some(130),
                start_offset: 50,
                end_offset: 60,
                status: TurnEndStatus::Completed,
            },
        ]);
        assert!(
            explicit_then_counter
                .anomalies
                .iter()
                .all(|anomaly| anomaly.code != AnomalyCode::LegacyCoverageAmbiguous)
        );
        assert_eq!(explicit_then_counter.events.len(), 2);
        assert!(explicit_then_counter.events.iter().any(|event| {
            event.event_id == response_event_id("thread", "legacy-r") && event.usage == legacy
        }));
        assert!(explicit_then_counter.events.iter().any(|event| {
            event.event_id == response_event_id("thread", "late-c") && event.usage == compact
        }));
        assert_eq!(
            explicit_then_counter.closed_turns[0].turn.accounted,
            processor_checked_add(&legacy, &compact).unwrap()
        );
        assert_eq!(
            explicit_then_counter.closed_turns[0]
                .turn
                .accounted_candidate_count,
            2
        );
        assert!(
            explicit_then_counter
                .patch
                .window_updates
                .iter()
                .any(|window| {
                    window
                        .state
                        .legacy_covered_response_ids
                        .contains(&"legacy-r".to_owned())
                        && window
                            .state
                            .legacy_covered_response_ids
                            .contains(&"late-c".to_owned())
                        && window.state.proposal_event_ids.is_empty()
                })
        );
    }

    #[test]
    fn compaction_identity_conflict_checks_unbound_pending_usage() {
        let original = known(8, 2, 1, 3, 1);
        let first = TestProcessor::new(context(1), None).process(vec![compacted_record(
            Some(100),
            20,
            "pending-r",
            Some("turn"),
            original.clone(),
        )]);
        let pending = first
            .updated_state
            .reconciliation_carry
            .pending_evidence
            .first()
            .expect("valid usage without a model remains pending");
        assert_eq!(pending.model, None);
        assert_eq!(pending.reasoning_effort, None);
        match &pending.record {
            PendingEvidenceRecord::Compacted {
                timestamp_ms,
                start_offset,
                end_offset,
                evidence,
            } => {
                assert_eq!(*timestamp_ms, Some(100));
                assert_eq!(*start_offset, 20);
                assert_eq!(*end_offset, 30);
                assert_eq!(
                    evidence
                        .latest_token_usage_record
                        .as_ref()
                        .unwrap()
                        .response_id,
                    "pending-r"
                );
            }
            PendingEvidenceRecord::ResponseUsage { .. } => {
                panic!("Compacted evidence must retain its original variant")
            }
        }

        let mut conflicting = original.clone();
        conflicting.output_tokens += 1;
        conflicting.total_tokens += 1;
        let result = TestProcessor::new(context(1), Some(first.updated_state.clone())).run(vec![
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("later-model".to_owned()),
                reasoning_effort: Some("high".to_owned()),
            },
            response_record(Some(120), 30, "pending-r", Some("turn"), conflicting),
        ]);
        assert!(result.needs_rebuild);
        assert_eq!(result.patch.events.len(), 0);
        assert_eq!(result.patch.anomalies.len(), 1);
        assert_eq!(
            result.patch.anomalies[0].code,
            AnomalyCode::ResponseUsageConflict
        );
        assert_eq!(result.patch.anomalies[0].source_start_offset, Some(30));
        assert_eq!(
            result
                .updated_state
                .reconciliation_carry
                .pending_evidence
                .len(),
            1
        );

        let process_for_owner = |owner: &str| {
            let mut usage_context = context(1);
            usage_context.owning_thread_id = owner.to_owned();
            usage_context.root_session_id = "root".to_owned();
            let ownership = || Ownership::Owning {
                thread_id: owner.to_owned(),
            };
            let mut evidence = response_evidence("shared-owner-response", None, original.clone());
            evidence.thread_id = Some(owner.to_owned());
            TestProcessor::new(usage_context, None).process(vec![
                UsageRecord::TurnContext {
                    ownership: ownership(),
                    model: Some("model-a".to_owned()),
                    reasoning_effort: None,
                },
                UsageRecord::ResponseUsage {
                    ownership: ownership(),
                    timestamp_ms: Some(130),
                    start_offset: 40,
                    end_offset: 50,
                    evidence,
                },
            ])
        };
        let owner_a = process_for_owner("owner-a");
        let owner_b = process_for_owner("owner-b");
        assert_eq!(owner_a.events.len(), 1);
        assert_eq!(owner_b.events.len(), 1);
        assert_eq!(owner_a.events[0].usage, owner_b.events[0].usage);
        assert_ne!(owner_a.events[0].event_id, owner_b.events[0].event_id);
        assert_eq!(
            owner_a.events[0].event_id,
            response_event_id("owner-a", "shared-owner-response")
        );
        assert_eq!(
            owner_b.events[0].event_id,
            response_event_id("owner-b", "shared-owner-response")
        );
    }

    #[test]
    fn compaction_late_evidence_pending_closes_at_turn_end_and_never_borrows_next_turn() {
        let usage = known(8, 2, 1, 3, 1);
        let pending_without_turn =
            TestProcessor::new(context(1), None).process(vec![compacted_record(
                Some(100),
                20,
                "unscoped-r",
                None,
                usage.clone(),
            )]);
        let next_turn = TestProcessor::new(context(1), Some(pending_without_turn.updated_state))
            .process(vec![
                UsageRecord::TurnStarted {
                    ownership: owning(),
                    turn_id: Some("next-turn".to_owned()),
                    timestamp_ms: Some(110),
                    start_offset: 30,
                },
                UsageRecord::TurnContext {
                    ownership: owning(),
                    model: Some("next-model".to_owned()),
                    reasoning_effort: Some("low".to_owned()),
                },
            ]);
        assert!(next_turn.events.is_empty());
        assert!(
            next_turn
                .updated_state
                .reconciliation_carry
                .pending_evidence
                .is_empty()
        );
        assert!(
            next_turn
                .updated_state
                .reconciliation_carry
                .pending_response_ids
                .is_empty()
        );
        assert!(next_turn.anomalies.iter().any(|anomaly| {
            anomaly.code == AnomalyCode::LegacyCoverageAmbiguous
                && anomaly.source_start_offset == Some(20)
        }));

        let pending_in_turn = TestProcessor::new(context(1), None).process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("owned-turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 10,
            },
            compacted_record(Some(100), 20, "owned-r", Some("owned-turn"), usage),
        ]);
        let ended =
            TestProcessor::new(context(1), Some(pending_in_turn.updated_state)).process(vec![
                UsageRecord::TurnEnded {
                    ownership: owning(),
                    turn_id: Some("owned-turn".to_owned()),
                    timestamp_ms: Some(110),
                    start_offset: 30,
                    end_offset: 40,
                    status: TurnEndStatus::Completed,
                },
            ]);
        assert!(
            ended
                .updated_state
                .reconciliation_carry
                .pending_evidence
                .is_empty()
        );
        assert!(ended.anomalies.iter().any(|anomaly| {
            anomaly.code == AnomalyCode::LegacyCoverageAmbiguous
                && anomaly.source_start_offset == Some(20)
        }));
    }

    #[test]
    fn compaction_coverage_ambiguous_rejects_nonunique_and_non_decomposable_proofs() {
        let baseline = known(100, 10, 5, 40, 3);
        let legacy = known(10, 1, 2, 5, 1);
        let current = processor_checked_add(&baseline, &legacy).unwrap();
        let ambiguous_candidates = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .run(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            response_record(Some(95), 30, "same-a", Some("turn"), legacy.clone()),
            response_record(Some(96), 40, "same-b", Some("turn"), legacy.clone()),
            token(100, 50, current, UsageValue::Valid(legacy.clone())),
        ]);
        assert!(ambiguous_candidates.needs_rebuild);
        assert_eq!(ambiguous_candidates.patch.events.len(), 0);
        assert!(
            ambiguous_candidates
                .patch
                .anomalies
                .iter()
                .any(|anomaly| { anomaly.code == AnomalyCode::LegacyCoverageAmbiguous })
        );

        let compact = known(20, 3, 4, 8, 2);
        let invalid_delta = known(35, 5, 7, 14, 3);
        let invalid_current = processor_checked_add(&baseline, &invalid_delta).unwrap();
        let non_decomposable = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline.clone()),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .run(vec![
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(100, 20, invalid_current, UsageValue::Valid(legacy)),
            compacted_record(Some(110), 30, "unmatched-c", None, compact),
        ]);
        assert!(non_decomposable.needs_rebuild);
        assert_eq!(non_decomposable.patch.events.len(), 0);
        assert!(
            non_decomposable
                .patch
                .anomalies
                .iter()
                .any(|anomaly| { anomaly.code == AnomalyCode::LegacyCoverageAmbiguous })
        );

        let compact = known(20, 3, 4, 8, 2);
        for last in [UsageValue::Missing, UsageValue::Invalid] {
            let duplicate_snapshot = TestProcessor::new(
                context(1),
                Some(UsageSourceState {
                    previous_total: Some(baseline.clone()),
                    previous_total_offset: Some(10),
                    ..UsageSourceState::default()
                }),
            )
            .run(vec![
                UsageRecord::TurnStarted {
                    ownership: owning(),
                    turn_id: Some("zero-delta-turn".to_owned()),
                    timestamp_ms: Some(90),
                    start_offset: 11,
                },
                UsageRecord::TurnContext {
                    ownership: owning(),
                    model: Some("model-a".to_owned()),
                    reasoning_effort: None,
                },
                compacted_record(
                    Some(95),
                    15,
                    "zero-delta-compaction",
                    Some("zero-delta-turn"),
                    compact.clone(),
                ),
                token(100, 30, baseline.clone(), last),
            ]);

            assert!(!duplicate_snapshot.needs_rebuild);
            assert_eq!(duplicate_snapshot.patch.events.len(), 1);
            assert_eq!(duplicate_snapshot.patch.events[0].usage, compact);
            assert_eq!(
                duplicate_snapshot
                    .patch
                    .facts
                    .iter()
                    .filter(|fact| {
                        fact.response_id.as_deref() == Some("zero-delta-compaction")
                            && fact.operation == CodexOperation::Compaction
                    })
                    .count(),
                1
            );
            assert!(
                duplicate_snapshot
                    .patch
                    .anomalies
                    .iter()
                    .all(|anomaly| { anomaly.code != AnomalyCode::LegacyCoverageAmbiguous })
            );
            let window = duplicate_snapshot
                .patch
                .window_updates
                .iter()
                .find(|update| update.state.current_total == UsageValue::Valid(baseline.clone()))
                .unwrap();
            assert!(window.state.legacy_covered_response_ids.is_empty());
            assert!(window.state.proposal_event_ids.is_empty());
            let turn = duplicate_snapshot.updated_state.open_turn.as_ref().unwrap();
            assert_eq!(turn.accounted, NormalizedTokenUsage::zero());
            assert_eq!(turn.accounted_candidate_count, 0);
        }
    }

    #[test]
    fn compaction_cache_write_proof_preserves_unknown_and_mixed_knownness() {
        let unknown_value = unknown(10, 2, 4, 1);
        let same_unknown = unknown(10, 2, 4, 1);
        let known_value = known(10, 2, 3, 4, 1);
        let same_known = known(10, 2, 3, 4, 1);
        let changed_known = known(10, 2, 4, 4, 1);
        assert_eq!(
            usage_coverage_equal(&unknown_value, &same_unknown),
            Some(true)
        );
        assert_eq!(usage_coverage_equal(&known_value, &same_known), Some(true));
        assert_eq!(
            usage_coverage_equal(&known_value, &changed_known),
            Some(false)
        );
        assert_eq!(usage_coverage_equal(&unknown_value, &known_value), None);

        let sum_unknown = sum_usage(&[&unknown_value, &same_unknown]).unwrap();
        let unknown_total = unknown(20, 4, 8, 2);
        assert_eq!(sum_unknown.cache_write, CacheWriteSum::Unknown);
        assert_eq!(
            usage_sum_coverage_equal(&sum_unknown, &unknown_total),
            Some(true)
        );
        let sum_mixed = sum_usage(&[&known_value, &unknown_value]).unwrap();
        assert_eq!(sum_mixed.cache_write, CacheWriteSum::Indeterminate);
        assert_eq!(
            usage_sum_coverage_equal(&sum_mixed, &unknown(20, 4, 8, 2)),
            None
        );
        assert!(
            subtract_usage_sum(&unknown_total, &sum_mixed)
                .unwrap()
                .is_none()
        );

        let baseline = known(100, 10, 2, 40, 3);
        let legacy = known(10, 1, 2, 5, 1);
        let compact = unknown(20, 3, 8, 2);
        let delta = processor_checked_add(&legacy, &compact).unwrap();
        let current = processor_checked_add(&baseline, &delta).unwrap();
        let result = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .run(vec![
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: None,
            },
            token(100, 20, current, UsageValue::Valid(legacy)),
            compacted_record(Some(110), 30, "mixed-c", None, compact),
        ]);
        assert!(result.needs_rebuild);
        assert!(
            result
                .patch
                .anomalies
                .iter()
                .any(|anomaly| { anomaly.code == AnomalyCode::LegacyCoverageAmbiguous })
        );
        assert!(result.patch.events.is_empty());
    }

    #[test]
    fn compaction_closed_turn_late_rewrite_isolated_by_complete_persisted_turn_key() {
        let mut reconciliation = ReconciliationContext::default();
        let usage = known(7, 2, 1, 3, 1);
        let proposal = CanonicalUsageProposal {
            event_id: response_event_id("thread", "shared-response"),
            kind: EventKind::Normal,
            occurred_at_ms: 100,
            thread_id: "thread".to_owned(),
            root_session_id: "thread".to_owned(),
            turn_key: Some("same-raw-turn".to_owned()),
            model: "model-a".to_owned(),
            reasoning_effort: None,
            usage: usage.clone(),
        };
        let response_key = ResponseKey {
            owning_thread_id: "thread".to_owned(),
            response_id: "shared-response".to_owned(),
        };
        reconciliation.bindings.insert(
            response_key.clone(),
            ResponseBinding {
                proposal: proposal.clone(),
                fact: UsageEventFact {
                    event_id: proposal.event_id.clone(),
                    owning_thread_id: "thread".to_owned(),
                    response_id: Some("shared-response".to_owned()),
                    evidence_kind: EvidenceKind::Explicit,
                    operation: CodexOperation::Response,
                },
            },
        );
        for source_file_id in [1, 2] {
            let key = WindowKey {
                source_file_id,
                file_generation: 1,
                start_offset: 10,
            };
            reconciliation.windows.insert(
                key,
                LegacyReconciliationWindow {
                    version: LegacyReconciliationWindow::VERSION,
                    previous_total: UsageValue::Missing,
                    current_total: UsageValue::Valid(usage.clone()),
                    last_usage: UsageValue::Missing,
                    explicit_response_ids: vec!["shared-response".to_owned()],
                    legacy_covered_response_ids: vec!["shared-response".to_owned()],
                    proposal_event_ids: Vec::new(),
                    turn_accounted_before: NormalizedTokenUsage::zero(),
                    chain_state: ChainState::Continuous,
                    closed: true,
                },
            );
            reconciliation.window_metadata.insert(
                key,
                ReconciliationWindowMetadata {
                    source_file_id,
                    file_generation: 1,
                    source_start_offset: 10,
                    source_end_offset: 20,
                    owning_thread_id: "thread".to_owned(),
                    turn_key: Some("same-raw-turn".to_owned()),
                },
            );
        }

        let mut processor =
            UsageProcessor::new(context(1), UsageSourceState::default(), reconciliation);
        for source_file_id in [1, 2] {
            let (accounted, count) = processor
                .accounted_for_turn(&PersistedTurnKey {
                    source_file_id,
                    file_generation: 1,
                    turn_key: "same-raw-turn".to_owned(),
                })
                .unwrap();
            assert_eq!(accounted, usage);
            assert_eq!(count, 1);
        }
        assert!(!processor.needs_rebuild);
    }

    #[test]
    fn compaction_closed_turn_late_rewrite_uses_durable_old_closure() {
        let baseline = known(100, 10, 5, 40, 3);
        let legacy = known(10, 1, 2, 5, 1);
        let compact = known(20, 3, 4, 8, 2);
        let delta = processor_checked_add(&legacy, &compact).unwrap();
        let current = processor_checked_add(&baseline, &delta).unwrap();
        let closed = TestProcessor::new(
            context(1),
            Some(UsageSourceState {
                previous_total: Some(baseline),
                previous_total_offset: Some(10),
                ..UsageSourceState::default()
            }),
        )
        .process(vec![
            UsageRecord::TurnStarted {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(90),
                start_offset: 11,
            },
            UsageRecord::TurnContext {
                ownership: owning(),
                model: Some("model-a".to_owned()),
                reasoning_effort: Some("high".to_owned()),
            },
            token(100, 20, current, UsageValue::Valid(legacy.clone())),
            UsageRecord::TurnEnded {
                ownership: owning(),
                turn_id: Some("turn".to_owned()),
                timestamp_ms: Some(120),
                start_offset: 30,
                end_offset: 40,
                status: TurnEndStatus::Completed,
            },
        ]);
        let old_turn = closed
            .patch
            .turn_upserts
            .iter()
            .find(|turn| turn.status == PersistedTurnStatus::Completed)
            .cloned()
            .expect("the first chunk persists the closed Turn");
        let old_compensation = closed
            .patch
            .events
            .iter()
            .find(|event| event.kind == EventKind::TurnCompensation)
            .cloned()
            .expect("the first chunk persists the Turn compensation");
        let old_window = closed
            .patch
            .window_updates
            .iter()
            .find(|window| window.turn_key.as_deref() == Some("turn"))
            .cloned()
            .expect("the first chunk persists the legacy window");
        let window_key = WindowKey {
            source_file_id: old_window.source_file_id,
            file_generation: old_window.file_generation,
            start_offset: old_window.source_start_offset,
        };
        let mut reconciliation = ReconciliationContext::default();
        reconciliation
            .windows
            .insert(window_key, old_window.state.clone());
        reconciliation.window_metadata.insert(
            window_key,
            ReconciliationWindowMetadata {
                source_file_id: old_window.source_file_id,
                file_generation: old_window.file_generation,
                source_start_offset: old_window.source_start_offset,
                source_end_offset: old_window.source_end_offset,
                owning_thread_id: old_window.owning_thread_id.clone(),
                turn_key: old_window.turn_key.clone(),
            },
        );
        let old_legacy_bindings = old_window
            .state
            .proposal_event_ids
            .iter()
            .map(|event_id| {
                let proposal = closed
                    .patch
                    .events
                    .iter()
                    .find(|proposal| &proposal.event_id == event_id)
                    .unwrap()
                    .clone();
                let fact = closed
                    .patch
                    .facts
                    .iter()
                    .find(|fact| &fact.event_id == event_id)
                    .unwrap()
                    .clone();
                let occurrences = closed
                    .patch
                    .occurrences
                    .iter()
                    .filter(|occurrence| &occurrence.event_id == event_id)
                    .cloned()
                    .collect();
                WindowProposalBinding {
                    proposal,
                    fact,
                    occurrences,
                }
            })
            .collect::<Vec<WindowProposalBinding>>();
        reconciliation
            .window_proposals
            .insert(window_key, old_legacy_bindings.clone());
        let mut sibling_turn = old_turn.clone();
        sibling_turn.key.source_file_id = 2;
        let sibling_window_key = WindowKey {
            source_file_id: 2,
            file_generation: old_window.file_generation,
            start_offset: old_window.source_start_offset,
        };
        let mut sibling_window = old_window.clone();
        sibling_window.source_file_id = 2;
        reconciliation
            .windows
            .insert(sibling_window_key, sibling_window.state.clone());
        reconciliation.window_metadata.insert(
            sibling_window_key,
            ReconciliationWindowMetadata {
                source_file_id: 2,
                file_generation: sibling_window.file_generation,
                source_start_offset: sibling_window.source_start_offset,
                source_end_offset: sibling_window.source_end_offset,
                owning_thread_id: sibling_window.owning_thread_id.clone(),
                turn_key: sibling_window.turn_key.clone(),
            },
        );
        let sibling_legacy_bindings = old_legacy_bindings
            .iter()
            .map(|binding| {
                let mut binding = binding.clone();
                for occurrence in &mut binding.occurrences {
                    occurrence.source_file_id = 2;
                }
                binding
            })
            .collect();
        reconciliation
            .window_proposals
            .insert(sibling_window_key, sibling_legacy_bindings);
        let mut sibling_compensation_occurrences = closed
            .patch
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.event_id == old_compensation.event_id)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(sibling_compensation_occurrences.len(), 1);
        sibling_compensation_occurrences[0].source_file_id = 2;
        let complete_compensation_closure = closed
            .patch
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.event_id == old_compensation.event_id)
            .cloned()
            .chain(sibling_compensation_occurrences)
            .collect::<Vec<_>>();
        reconciliation.affected_turns.insert(
            old_turn.key.clone(),
            AffectedTurn {
                snapshot: old_turn.clone(),
                compensation_events: vec![old_compensation.clone()],
                compensation_occurrences: complete_compensation_closure.clone(),
            },
        );
        reconciliation.affected_turns.insert(
            sibling_turn.key.clone(),
            AffectedTurn {
                snapshot: sibling_turn.clone(),
                compensation_events: vec![old_compensation.clone()],
                compensation_occurrences: complete_compensation_closure,
            },
        );

        // The first late response exists only in source 1. Its exact owning
        // Turn key and the shared compensation occurrence closure must prove
        // the source 2 counter window before that Turn can be rewritten.
        let rewritten = TestProcessor::with_reconciliation(
            context(1),
            Some(closed.updated_state.clone()),
            reconciliation.clone(),
        )
        .run(vec![compacted_record(
            Some(130),
            60,
            "late-compact",
            Some("turn"),
            compact.clone(),
        )]);
        assert!(!rewritten.needs_rebuild);
        assert!(
            rewritten
                .patch
                .delete_event_ids
                .contains(&old_compensation.event_id)
        );
        assert!(
            rewritten
                .patch
                .events
                .iter()
                .all(|event| event.kind != EventKind::TurnCompensation)
        );
        assert_eq!(
            rewritten
                .patch
                .events
                .iter()
                .filter(|event| { event.event_id == response_event_id("thread", "late-compact") })
                .count(),
            1
        );
        assert_eq!(rewritten.patch.turn_rewrites.len(), 2);
        let rewrite = rewritten
            .patch
            .turn_rewrites
            .iter()
            .find(|rewrite| rewrite.expected.key == old_turn.key)
            .expect("the observed source Turn is rewritten by full key");
        let sibling_rewrite = rewritten
            .patch
            .turn_rewrites
            .iter()
            .find(|rewrite| rewrite.expected.key == sibling_turn.key)
            .expect("the sibling source Turn is recomputed by full key");
        assert_eq!(rewrite.expected, old_turn);
        assert_eq!(rewrite.replacement.key, rewrite.expected.key);
        assert_eq!(rewrite.replacement.status, rewrite.expected.status);
        assert_eq!(
            rewrite.replacement.ended_at_ms,
            rewrite.expected.ended_at_ms
        );
        assert_eq!(rewrite.replacement.end_offset, rewrite.expected.end_offset);
        assert_eq!(
            rewrite.replacement.state_through_offset,
            rewrite.expected.state_through_offset
        );
        assert_eq!(
            rewrite.replacement.quality_status,
            rewrite.expected.quality_status
        );
        assert_eq!(
            rewrite.replacement.state.turn_key,
            rewrite.expected.state.turn_key
        );
        assert_eq!(
            rewrite.replacement.state.raw_turn_id,
            rewrite.expected.state.raw_turn_id
        );
        assert_eq!(
            rewrite.replacement.state.started_at_ms,
            rewrite.expected.state.started_at_ms
        );
        assert_eq!(
            rewrite.replacement.state.start_offset,
            rewrite.expected.state.start_offset
        );
        assert_eq!(
            rewrite.replacement.state.start_total,
            rewrite.expected.state.start_total
        );
        assert_eq!(
            rewrite.replacement.state.last_total,
            rewrite.expected.state.last_total
        );
        assert_eq!(
            rewrite.replacement.state.model_state,
            rewrite.expected.state.model_state
        );
        assert_eq!(
            rewrite.replacement.state.reasoning_effort_state,
            rewrite.expected.state.reasoning_effort_state
        );
        assert_eq!(
            rewrite.replacement.state.blocks,
            rewrite.expected.state.blocks
        );
        assert_eq!(rewrite.replacement.state.accounted, delta);
        assert_eq!(rewrite.replacement.state.accounted_candidate_count, 2);
        assert_eq!(sibling_rewrite.replacement.key, sibling_turn.key);
        assert_eq!(sibling_rewrite.replacement.state.accounted, delta);
        assert_eq!(
            sibling_rewrite.replacement.state.accounted_candidate_count,
            2
        );
        assert!(rewritten.patch.occurrences.iter().any(|occurrence| {
            occurrence.source_file_id == 1
                && occurrence.source_start_offset == 60
                && occurrence.source_end_offset == 70
                && occurrence.event_id == response_event_id("thread", "late-compact")
        }));
        assert_eq!(rewritten.patch.window_updates.len(), 2);
        assert!(rewritten.patch.window_updates.iter().all(|window| {
            window
                .state
                .explicit_response_ids
                .contains(&"late-compact".to_owned())
                && window
                    .state
                    .legacy_covered_response_ids
                    .contains(&"late-compact".to_owned())
        }));

        let response_key = ResponseKey {
            owning_thread_id: "thread".to_owned(),
            response_id: "late-compact".to_owned(),
        };
        let response_proposal = rewritten
            .patch
            .events
            .iter()
            .find(|event| event.event_id == response_event_id("thread", "late-compact"))
            .cloned()
            .expect("the late response canonical is committed with its coverage proof");
        let response_fact = rewritten
            .patch
            .facts
            .iter()
            .find(|fact| fact.event_id == response_proposal.event_id)
            .cloned()
            .expect("the explicit response binding is committed");
        let response_occurrences = rewritten
            .patch
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.event_id == response_proposal.event_id)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(response_occurrences.len(), 1);

        let mut after_commit = reconciliation.clone();
        for window in &rewritten.patch.window_updates {
            let key = WindowKey {
                source_file_id: window.source_file_id,
                file_generation: window.file_generation,
                start_offset: window.source_start_offset,
            };
            after_commit.windows.insert(key, window.state.clone());
            after_commit.window_metadata.insert(
                key,
                ReconciliationWindowMetadata {
                    source_file_id: window.source_file_id,
                    file_generation: window.file_generation,
                    source_start_offset: window.source_start_offset,
                    source_end_offset: window.source_end_offset,
                    owning_thread_id: window.owning_thread_id.clone(),
                    turn_key: window.turn_key.clone(),
                },
            );
            let proposal_ids = window
                .state
                .proposal_event_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            if let Some(proposals) = after_commit.window_proposals.get_mut(&key) {
                proposals.retain(|binding| proposal_ids.contains(&binding.proposal.event_id));
            }
        }
        after_commit.bindings.insert(
            response_key.clone(),
            ResponseBinding {
                proposal: response_proposal.clone(),
                fact: response_fact,
            },
        );
        after_commit
            .response_occurrences
            .insert(response_key.clone(), response_occurrences.clone());
        after_commit
            .closure_response_keys
            .insert(response_key.clone());
        after_commit.markers = rewritten.patch.marker_updates.clone();
        for rewrite in &rewritten.patch.turn_rewrites {
            let affected = after_commit
                .affected_turns
                .get_mut(&rewrite.expected.key)
                .expect("the complete closure keeps both CAS snapshots");
            affected.snapshot = rewrite.replacement.clone();
            affected.compensation_events.clear();
            affected.compensation_occurrences.clear();
        }
        assert!(after_commit.affected_turns.values().all(|affected| {
            affected.compensation_events.is_empty() && affected.compensation_occurrences.is_empty()
        }));

        let mut resumed =
            UsageProcessor::new(context(1), UsageSourceState::default(), after_commit);
        assert!(resumed.late_response_windows.is_empty());
        let outcome = resumed.try_process_record(
            response_record(Some(140), 80, "late-compact", Some("turn"), compact.clone()),
            u64::MAX,
        );
        assert_eq!(outcome, RecordApplyOutcome::Applied);
        assert!(!resumed.needs_rebuild);
        assert!(resumed.late_response_windows.is_empty());
        for source_file_id in [1, 2] {
            let (accounted, count) = resumed
                .accounted_for_turn(&PersistedTurnKey {
                    source_file_id,
                    file_generation: 1,
                    turn_key: "turn".to_owned(),
                })
                .unwrap();
            assert_eq!(accounted, delta);
            assert_eq!(count, 2);
        }
        assert_eq!(response_occurrences[0].source_start_offset, 60);
        assert_eq!(response_occurrences[0].source_end_offset, 70);
        let resumed = resumed.finish();
        assert!(!resumed.needs_rebuild);
        assert!(resumed.patch.delete_event_ids.is_empty());
        assert!(
            resumed
                .patch
                .events
                .iter()
                .all(|event| event.kind != EventKind::TurnCompensation)
        );
        assert!(resumed.patch.window_updates.is_empty());
        assert!(resumed.patch.turn_rewrites.is_empty());
        assert!(resumed.patch.occurrences.iter().any(|occurrence| {
            occurrence.source_file_id == 1
                && occurrence.source_start_offset == 80
                && occurrence.source_end_offset == 90
                && occurrence.event_id == response_proposal.event_id
        }));

        let mut incomplete_closure = reconciliation.clone();
        incomplete_closure.affected_turns.remove(&sibling_turn.key);
        let incomplete = TestProcessor::with_reconciliation(
            context(1),
            Some(closed.updated_state.clone()),
            incomplete_closure,
        )
        .run(vec![compacted_record(
            Some(130),
            60,
            "late-compact",
            Some("turn"),
            compact.clone(),
        )]);
        assert!(incomplete.needs_rebuild);
        assert!(incomplete.patch.events.is_empty());
        assert!(incomplete.patch.delete_event_ids.is_empty());
        assert!(incomplete.patch.anomalies.iter().any(|anomaly| {
            anomaly.code == AnomalyCode::LegacyCoverageAmbiguous
                && anomaly.source_start_offset == Some(60)
        }));

        let missing_closure =
            TestProcessor::new(context(1), Some(closed.updated_state)).run(vec![compacted_record(
                Some(130),
                60,
                "late-without-closure",
                Some("turn"),
                compact,
            )]);
        assert!(missing_closure.needs_rebuild);
        assert!(missing_closure.patch.events.is_empty());
        assert!(missing_closure.patch.anomalies.iter().any(|anomaly| {
            anomaly.code == AnomalyCode::LegacyCoverageAmbiguous
                && anomaly.source_start_offset == Some(60)
        }));
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

    fn close_existing_turn(turn: TurnState, status: TurnEndStatus) -> TestResult {
        let mut reconciliation = ReconciliationContext::default();
        if turn.accounted_candidate_count > 0 {
            assert_eq!(turn.accounted_candidate_count, 1);
            let start = turn
                .start_total
                .as_ref()
                .expect("accounted candidates have a persisted Turn baseline");
            let end = turn
                .last_total
                .as_ref()
                .expect("accounted candidates have a persisted Turn total");
            let mut proposal = CanonicalUsageProposal {
                event_id: String::new(),
                kind: EventKind::Normal,
                occurred_at_ms: turn
                    .started_at_ms
                    .expect("persisted legacy proposal has a timestamp"),
                thread_id: "thread".to_owned(),
                root_session_id: "thread".to_owned(),
                turn_key: Some(turn.turn_key.clone()),
                model: "model-a".to_owned(),
                reasoning_effort: None,
                usage: turn.accounted.clone(),
            };
            proposal.event_id = event_id(&proposal, Some(start), end);
            let occurrence = Occurrence {
                source_file_id: 1,
                file_generation: 1,
                source_start_offset: turn.start_offset,
                source_end_offset: turn.start_offset + 1,
                event_id: proposal.event_id.clone(),
            };
            let fact = UsageEventFact {
                event_id: proposal.event_id.clone(),
                owning_thread_id: "thread".to_owned(),
                response_id: None,
                evidence_kind: EvidenceKind::Legacy,
                operation: CodexOperation::Response,
            };
            let key = WindowKey {
                source_file_id: 1,
                file_generation: 1,
                start_offset: turn.start_offset,
            };
            reconciliation.windows.insert(
                key,
                LegacyReconciliationWindow {
                    version: LegacyReconciliationWindow::VERSION,
                    previous_total: UsageValue::Valid(start.clone()),
                    current_total: UsageValue::Valid(end.clone()),
                    last_usage: UsageValue::Valid(turn.accounted.clone()),
                    explicit_response_ids: Vec::new(),
                    legacy_covered_response_ids: Vec::new(),
                    proposal_event_ids: vec![proposal.event_id.clone()],
                    turn_accounted_before: NormalizedTokenUsage::zero(),
                    chain_state: ChainState::Continuous,
                    closed: true,
                },
            );
            reconciliation.window_metadata.insert(
                key,
                ReconciliationWindowMetadata {
                    source_file_id: 1,
                    file_generation: 1,
                    source_start_offset: turn.start_offset,
                    source_end_offset: turn.start_offset + 1,
                    owning_thread_id: "thread".to_owned(),
                    turn_key: Some(turn.turn_key.clone()),
                },
            );
            reconciliation.window_proposals.insert(
                key,
                vec![WindowProposalBinding {
                    proposal,
                    fact,
                    occurrences: vec![occurrence],
                }],
            );
        }
        TestProcessor::with_reconciliation(
            context(1),
            Some(UsageSourceState {
                previous_total: turn.last_total.clone(),
                previous_total_offset: Some(20),
                open_turn: Some(turn),
                ..UsageSourceState::default()
            }),
            reconciliation,
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
