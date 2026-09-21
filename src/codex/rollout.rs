//! Safe rollout envelope parsing and per-record ownership classification.
//!
//! The scanner owns file discovery and complete-line framing. This module
//! accepts already framed records, inspects only metadata whitelist fields,
//! and never retains raw JSON, message bodies, prompts, or tool payloads.

use std::path::{Component, Path, PathBuf};

use chrono::DateTime;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::DiagnosticSeverity;

/// Current durable metadata safe-fact parser version.
///
/// This is the only production authority for the metadata parser version;
/// persisted checkpoints and facts carry the version they were produced with.
pub const METADATA_PARSER_VERSION: i64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeKind {
    SessionMeta,
    TurnContext,
    TokenCount,
    Lifecycle,
    Ignored,
    Unknown,
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordOwnership {
    Owning,
    ReplayedAncestor,
    UnknownOwnership,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeState {
    AwaitOwningMeta,
    ReplayedAncestor { owning_thread_id: String },
    OwningLive { owning_thread_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinalContinuation {
    ReplayedAncestor { owning_thread_id: String },
    OwningLive { owning_thread_id: String },
    Unstable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnershipConfidence {
    Confirmed,
    Unresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwningCandidateConfidence {
    Candidate,
    Confirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CwdProvenance {
    SessionMeta,
    TurnContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentHintProvenance {
    SessionMetaParent,
    SubagentSource,
    ForkedFromId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRoleProvenance {
    SubagentSource,
    SessionMetaRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentPathProvenance {
    SessionMeta,
    ThreadSpawn,
}

impl AgentPathProvenance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionMeta => "session_meta",
            Self::ThreadSpawn => "thread_spawn",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<P> {
    pub value: String,
    pub provenance: P,
    pub record_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipBoundary {
    pub replay_start_offset: Option<u64>,
    pub owning_records_start_offset: Option<u64>,
    pub confidence: OwnershipConfidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutThreadFact {
    pub source_file_id: i64,
    pub owning_thread_id: String,
    pub cwd: Option<Candidate<CwdProvenance>>,
    pub created_at_ms: Option<i64>,
    pub latest_context_model: Option<String>,
    pub latest_context_turn_id: Option<String>,
    pub latest_context_at_ms: Option<i64>,
    pub latest_context_record_offset: Option<u64>,
    pub parent_thread_id_hint: Option<Candidate<ParentHintProvenance>>,
    pub agent_role_hint: Option<Candidate<AgentRoleProvenance>>,
    pub agent_path: Option<Candidate<AgentPathProvenance>>,
    pub ownership_boundary: OwnershipBoundary,
    pub has_conflict: bool,
    pub relationship_conflict: bool,
}

impl RolloutThreadFact {
    fn empty(source_file_id: i64, owning_thread_id: String) -> Self {
        Self {
            source_file_id,
            owning_thread_id,
            cwd: None,
            created_at_ms: None,
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            latest_context_record_offset: None,
            parent_thread_id_hint: None,
            agent_role_hint: None,
            agent_path: None,
            ownership_boundary: OwnershipBoundary {
                replay_start_offset: None,
                owning_records_start_offset: None,
                confidence: OwnershipConfidence::Confirmed,
            },
            has_conflict: false,
            relationship_conflict: false,
        }
    }

    /// Convert a parser fact to the durable Spec 01 safe-fact shape.  The
    /// scanner/storage seam supplies every commit-context value explicitly; this
    /// adapter never guesses a generation, parser version, offset, or clock.
    pub fn to_safe_fact(
        &self,
        file_generation: i64,
        metadata_parser_version: i64,
        resolved_through_offset: u64,
        updated_at_ms: i64,
        continuation: &FinalContinuation,
    ) -> Result<crate::codex::domain::RolloutMetadataFact, crate::domain::DomainError> {
        use crate::codex::domain::{
            AgentPathProvenance as DomainAgentPathProvenance,
            AgentRoleProvenance as DomainAgentRoleProvenance,
            ContinuationState as DomainContinuationState, CwdProvenance as DomainCwdProvenance,
            FactQualityStatus as DomainFactQualityStatus,
            OwnershipConfidence as DomainOwnershipConfidence,
            ParentHintProvenance as DomainParentHintProvenance, RolloutMetadataFact,
        };

        let offset = |value: u64, field: &'static str| {
            i64::try_from(value).map_err(|_| crate::domain::DomainError::InvalidValue {
                field,
                reason: "does not fit in i64".to_owned(),
            })
        };
        let continuation_state = match continuation {
            FinalContinuation::ReplayedAncestor { owning_thread_id }
                if owning_thread_id == &self.owning_thread_id =>
            {
                DomainContinuationState::ReplayedAncestor
            }
            FinalContinuation::OwningLive { owning_thread_id }
                if owning_thread_id == &self.owning_thread_id =>
            {
                DomainContinuationState::OwningLive
            }
            FinalContinuation::ReplayedAncestor { .. } | FinalContinuation::OwningLive { .. } => {
                return Err(crate::domain::DomainError::InvariantViolation {
                    invariant: "continuation owning thread must match rollout fact",
                });
            }
            FinalContinuation::Unstable => DomainContinuationState::Unstable,
        };
        let ownership_confidence = if matches!(
            continuation_state,
            DomainContinuationState::ReplayedAncestor | DomainContinuationState::OwningLive
        ) {
            DomainOwnershipConfidence::Confirmed
        } else {
            DomainOwnershipConfidence::Unresolved
        };
        let fact_quality_status = if self.has_conflict || self.relationship_conflict {
            DomainFactQualityStatus::Conflict
        } else if continuation_state == DomainContinuationState::Unstable {
            DomainFactQualityStatus::Partial
        } else {
            DomainFactQualityStatus::Complete
        };
        let cwd = self.cwd.as_ref().map(|candidate| {
            (
                candidate.value.clone(),
                match candidate.provenance {
                    CwdProvenance::SessionMeta => DomainCwdProvenance::SessionMeta,
                    CwdProvenance::TurnContext => DomainCwdProvenance::TurnContext,
                },
                candidate.record_offset,
            )
        });
        let parent = self.parent_thread_id_hint.as_ref().map(|candidate| {
            (
                candidate.value.clone(),
                match candidate.provenance {
                    ParentHintProvenance::SessionMetaParent => {
                        DomainParentHintProvenance::SessionMetaParent
                    }
                    ParentHintProvenance::SubagentSource => {
                        DomainParentHintProvenance::SubagentSource
                    }
                    ParentHintProvenance::ForkedFromId => DomainParentHintProvenance::ForkedFromId,
                },
                candidate.record_offset,
            )
        });
        let role = self.agent_role_hint.as_ref().map(|candidate| {
            (
                candidate.value.clone(),
                match candidate.provenance {
                    AgentRoleProvenance::SubagentSource => {
                        DomainAgentRoleProvenance::SubagentSource
                    }
                    AgentRoleProvenance::SessionMetaRole => {
                        DomainAgentRoleProvenance::SessionMetaRole
                    }
                },
                candidate.record_offset,
            )
        });
        let agent_path = self.agent_path.as_ref().map(|candidate| {
            (
                candidate.value.clone(),
                match candidate.provenance {
                    AgentPathProvenance::SessionMeta => DomainAgentPathProvenance::SessionMeta,
                    AgentPathProvenance::ThreadSpawn => DomainAgentPathProvenance::ThreadSpawn,
                },
                candidate.record_offset,
            )
        });
        let fact = RolloutMetadataFact {
            source_file_id: self.source_file_id,
            file_generation,
            metadata_parser_version,
            resolved_through_offset: offset(resolved_through_offset, "resolved_through_offset")?,
            owning_thread_id: self.owning_thread_id.clone(),
            continuation_state,
            cwd: cwd.as_ref().map(|value| value.0.clone()),
            cwd_provenance: cwd.as_ref().map(|value| value.1),
            cwd_record_offset: cwd
                .as_ref()
                .map(|value| offset(value.2, "cwd_record_offset"))
                .transpose()?,
            created_at_ms: self.created_at_ms,
            latest_context_model: self.latest_context_model.clone(),
            latest_context_turn_id: self.latest_context_turn_id.clone(),
            latest_context_at_ms: self.latest_context_at_ms,
            parent_thread_id_hint: parent.as_ref().map(|value| value.0.clone()),
            parent_hint_provenance: parent.as_ref().map(|value| value.1),
            parent_hint_record_offset: parent
                .as_ref()
                .map(|value| offset(value.2, "parent_hint_record_offset"))
                .transpose()?,
            agent_role_hint: role.as_ref().map(|value| value.0.clone()),
            agent_role_provenance: role.as_ref().map(|value| value.1),
            agent_role_record_offset: role
                .as_ref()
                .map(|value| offset(value.2, "agent_role_record_offset"))
                .transpose()?,
            agent_path: agent_path.as_ref().map(|value| value.0.clone()),
            agent_path_provenance: agent_path.as_ref().map(|value| value.1),
            agent_path_record_offset: agent_path
                .as_ref()
                .map(|value| offset(value.2, "agent_path_record_offset"))
                .transpose()?,
            replay_start_offset: self
                .ownership_boundary
                .replay_start_offset
                .map(|value| offset(value, "replay_start_offset"))
                .transpose()?,
            owning_records_start_offset: self
                .ownership_boundary
                .owning_records_start_offset
                .map(|value| offset(value, "owning_records_start_offset"))
                .transpose()?,
            ownership_confidence,
            fact_quality_status,
            relationship_conflict: self.relationship_conflict,
            updated_at_ms,
        };
        fact.validate()?;
        Ok(fact)
    }

    /// Rehydrate the resolver-facing fact from a storage-matched safe fact.
    /// This is used only for `Skip`: generation/parser/offset/binding matching
    /// has already happened in `load_metadata_scan_state` upstream.
    pub fn from_safe_fact(
        fact: &crate::codex::domain::RolloutMetadataFact,
    ) -> Result<Self, crate::domain::DomainError> {
        use crate::codex::domain::{
            AgentPathProvenance as DomainAgentPathProvenance,
            AgentRoleProvenance as DomainAgentRoleProvenance, CwdProvenance as DomainCwdProvenance,
            OwnershipConfidence as DomainOwnershipConfidence,
            ParentHintProvenance as DomainParentHintProvenance,
        };

        fact.validate()?;
        let offset = |value: i64, field: &'static str| {
            u64::try_from(value).map_err(|_| crate::domain::DomainError::InvalidValue {
                field,
                reason: "must be non-negative".to_owned(),
            })
        };
        let cwd = fact
            .cwd
            .as_ref()
            .zip(fact.cwd_provenance)
            .zip(fact.cwd_record_offset)
            .map(|((value, provenance), record_offset)| {
                Ok(Candidate {
                    value: value.clone(),
                    provenance: match provenance {
                        DomainCwdProvenance::SessionMeta => CwdProvenance::SessionMeta,
                        DomainCwdProvenance::TurnContext => CwdProvenance::TurnContext,
                    },
                    record_offset: offset(record_offset, "cwd_record_offset")?,
                })
            })
            .transpose()?;
        let parent_thread_id_hint = fact
            .parent_thread_id_hint
            .as_ref()
            .zip(fact.parent_hint_provenance)
            .zip(fact.parent_hint_record_offset)
            .map(|((value, provenance), record_offset)| {
                Ok(Candidate {
                    value: value.clone(),
                    provenance: match provenance {
                        DomainParentHintProvenance::SessionMetaParent => {
                            ParentHintProvenance::SessionMetaParent
                        }
                        DomainParentHintProvenance::SubagentSource => {
                            ParentHintProvenance::SubagentSource
                        }
                        DomainParentHintProvenance::ForkedFromId => {
                            ParentHintProvenance::ForkedFromId
                        }
                    },
                    record_offset: offset(record_offset, "parent_hint_record_offset")?,
                })
            })
            .transpose()?;
        let agent_role_hint = fact
            .agent_role_hint
            .as_ref()
            .zip(fact.agent_role_provenance)
            .zip(fact.agent_role_record_offset)
            .map(|((value, provenance), record_offset)| {
                Ok(Candidate {
                    value: value.clone(),
                    provenance: match provenance {
                        DomainAgentRoleProvenance::SubagentSource => {
                            AgentRoleProvenance::SubagentSource
                        }
                        DomainAgentRoleProvenance::SessionMetaRole => {
                            AgentRoleProvenance::SessionMetaRole
                        }
                    },
                    record_offset: offset(record_offset, "agent_role_record_offset")?,
                })
            })
            .transpose()?;
        let agent_path = fact
            .agent_path
            .as_ref()
            .zip(fact.agent_path_provenance)
            .zip(fact.agent_path_record_offset)
            .map(|((value, provenance), record_offset)| {
                Ok(Candidate {
                    value: value.clone(),
                    provenance: match provenance {
                        DomainAgentPathProvenance::SessionMeta => AgentPathProvenance::SessionMeta,
                        DomainAgentPathProvenance::ThreadSpawn => AgentPathProvenance::ThreadSpawn,
                    },
                    record_offset: offset(record_offset, "agent_path_record_offset")?,
                })
            })
            .transpose()?;
        // The durable v0.1 safe-fact schema stores the resolved seam but not
        // the model record's exact byte offset.  A restored model necessarily
        // came from a record before that seam, so retain that ordering fact as
        // the last byte before the seam.  This lets an equal-timestamp model
        // in the next non-zero chunk participate in deterministic conflict and
        // replacement handling instead of being silently ignored.
        let latest_context_record_offset = fact
            .latest_context_model
            .as_ref()
            .map(|_| {
                offset(fact.resolved_through_offset, "resolved_through_offset")
                    .map(|resolved| resolved.saturating_sub(1))
            })
            .transpose()?;
        let value = Self {
            source_file_id: fact.source_file_id,
            owning_thread_id: fact.owning_thread_id.clone(),
            cwd,
            created_at_ms: fact.created_at_ms,
            latest_context_model: fact.latest_context_model.clone(),
            latest_context_turn_id: fact.latest_context_turn_id.clone(),
            latest_context_at_ms: fact.latest_context_at_ms,
            latest_context_record_offset,
            parent_thread_id_hint,
            agent_role_hint,
            agent_path,
            ownership_boundary: OwnershipBoundary {
                replay_start_offset: fact
                    .replay_start_offset
                    .map(|value| offset(value, "replay_start_offset"))
                    .transpose()?,
                owning_records_start_offset: fact
                    .owning_records_start_offset
                    .map(|value| offset(value, "owning_records_start_offset"))
                    .transpose()?,
                confidence: match fact.ownership_confidence {
                    DomainOwnershipConfidence::Confirmed => OwnershipConfidence::Confirmed,
                    DomainOwnershipConfidence::Unresolved => OwnershipConfidence::Unresolved,
                },
            },
            has_conflict: matches!(
                fact.fact_quality_status,
                crate::codex::domain::FactQualityStatus::Conflict
            ),
            relationship_conflict: fact.relationship_conflict,
        };
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwningThreadCandidate {
    pub thread_id: String,
    pub confidence: OwningCandidateConfidence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwningThreadCandidates {
    /// Highest-priority candidate from `state_5.threads.rollout_path`.
    pub state_rollout: Option<OwningThreadCandidate>,
    /// Second-priority candidate extracted from a verified rollout filename.
    pub filename: Option<OwningThreadCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutParseContext {
    pub source_file_id: i64,
    pub chunk_start_offset: u64,
    pub candidates: OwningThreadCandidates,
    pub resume_state: ResumeState,
    /// Required for a non-zero resume so incremental fields can be merged
    /// without reconstructing them from the normalized Thread row.
    pub existing_fact: Option<RolloutThreadFact>,
}

/// A JSONL record already proven complete by the scanner.
///
/// Raw bytes are private and this type deliberately has no `Debug`
/// implementation, preventing accidental diagnostic logging of record bodies.
pub struct CompleteRolloutLine {
    start_offset: u64,
    end_offset: u64,
    bytes: Vec<u8>,
}

impl CompleteRolloutLine {
    pub fn new(start_offset: u64, bytes_with_newline: Vec<u8>) -> Option<Self> {
        if !bytes_with_newline.ends_with(b"\n") {
            return None;
        }
        let length = u64::try_from(bytes_with_newline.len()).ok()?;
        Some(Self {
            start_offset,
            end_offset: start_offset.checked_add(length)?,
            bytes: bytes_with_newline,
        })
    }

    pub const fn start_offset(&self) -> u64 {
        self.start_offset
    }

    pub const fn end_offset(&self) -> u64 {
        self.end_offset
    }

    fn json_bytes(&self) -> &[u8] {
        let without_newline = &self.bytes[..self.bytes.len() - 1];
        without_newline
            .strip_suffix(b"\r")
            .unwrap_or(without_newline)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordClassification {
    pub start_offset: u64,
    pub end_offset: u64,
    pub envelope: EnvelopeKind,
    pub ownership: RecordOwnership,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipRange {
    pub start_offset: u64,
    pub end_offset: u64,
    pub ownership: RecordOwnership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticCode {
    MalformedJson,
    MalformedEnvelope,
    UnknownEnvelope,
    InvalidAllowedField,
    OwningCandidateConflict,
    ForeignSessionMeta,
    UnresolvedReplayBoundary,
    InvalidResumeState,
    NonContiguousOffsets,
    CandidateConflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutDiagnostic {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub source_file_id: i64,
    pub source_start_offset: Option<u64>,
    pub thread_id: Option<String>,
    pub field: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutParseResult {
    pub fact: Option<RolloutThreadFact>,
    pub records: Vec<RecordClassification>,
    pub ownership_ranges: Vec<OwnershipRange>,
    pub diagnostics: Vec<RolloutDiagnostic>,
    pub diagnostic_count: u64,
    pub malformed_record_count: u64,
    pub final_continuation: FinalContinuation,
    pub needs_rebuild: bool,
    pub last_processed_offset: u64,
}

pub struct RolloutMetadataParser;

/// Incremental metadata parser used by the scanner's fixed-view reader.  A
/// caller pushes one complete line at a time and retains only parser-owned
/// safe state; rollout bytes are not buffered as a chunk.
pub(crate) struct RolloutOwnershipClassifier {
    parser: Parser,
}

/// Shared Spec02/Spec04 ownership state machine. Metadata extraction and the
/// usage consumer deliberately drive this same implementation so replay/owning
/// boundaries cannot diverge between consumers.
pub(crate) type RolloutChunkParser = RolloutOwnershipClassifier;

impl RolloutMetadataParser {
    pub(crate) fn start_chunk(context: RolloutParseContext) -> RolloutChunkParser {
        RolloutChunkParser {
            parser: Parser::new_streaming(context),
        }
    }

    pub fn parse_chunk<I>(context: RolloutParseContext, lines: I) -> RolloutParseResult
    where
        I: IntoIterator<Item = CompleteRolloutLine>,
    {
        let mut parser = RolloutChunkParser {
            parser: Parser::new(context),
        };
        for line in lines {
            parser.push(line);
        }
        parser.finish()
    }
}

impl RolloutOwnershipClassifier {
    pub(crate) fn push(&mut self, line: CompleteRolloutLine) {
        self.parser.push_line(line);
    }

    /// Push one complete line while retaining only that line's ownership
    /// classification. This is the bounded hand-off used by the usage
    /// consumer: the classifier state survives arbitrarily long replay
    /// prefixes without accumulating raw records or an ownership vector.
    pub(crate) fn push_classified(
        &mut self,
        line: CompleteRolloutLine,
    ) -> Option<RecordClassification> {
        self.parser.last_classification = None;
        self.parser.push_line(line);
        self.parser.last_classification.take()
    }

    /// Advance classifier state across a complete line whose body was
    /// intentionally discarded by the bounded scanner (for example an
    /// oversized record). The envelope cannot be inspected, but ownership is
    /// still determined from the already-established classifier state.
    pub(crate) fn push_opaque_classified(
        &mut self,
        start_offset: u64,
        end_offset: u64,
    ) -> RecordClassification {
        if start_offset != self.parser.last_processed_offset || end_offset <= start_offset {
            self.parser.needs_rebuild = true;
            self.parser.diagnostic(
                DiagnosticCode::NonContiguousOffsets,
                DiagnosticSeverity::Conflict,
                Some(start_offset),
                self.parser.owning_thread_id.clone(),
                None,
            );
        }
        let classification = RecordClassification {
            start_offset,
            end_offset,
            envelope: EnvelopeKind::Unknown,
            ownership: self.parser.default_ownership(),
        };
        self.parser.last_classification = Some(classification.clone());
        self.parser.last_processed_offset = end_offset;
        classification
    }

    pub(crate) fn finish(self) -> RolloutParseResult {
        self.parser.finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MachineState {
    AwaitOwningMeta,
    OwningBootstrap,
    ReplayedAncestor,
    OwningLive,
}

struct Parser {
    context: RolloutParseContext,
    owning_thread_id: Option<String>,
    owning_confirmed: bool,
    machine: MachineState,
    resumed_nonzero: bool,
    fact: Option<RolloutThreadFact>,
    records: Vec<RecordClassification>,
    ranges: Vec<OwnershipRange>,
    diagnostics: Vec<RolloutDiagnostic>,
    diagnostic_count: u64,
    malformed_record_count: u64,
    retain_details: bool,
    last_classification: Option<RecordClassification>,
    needs_rebuild: bool,
    last_processed_offset: u64,
}

impl Parser {
    fn new(context: RolloutParseContext) -> Self {
        Self::with_detail_retention(context, true)
    }

    fn new_streaming(context: RolloutParseContext) -> Self {
        Self::with_detail_retention(context, false)
    }

    fn with_detail_retention(context: RolloutParseContext, retain_details: bool) -> Self {
        let mut parser = Self {
            last_processed_offset: context.chunk_start_offset,
            context,
            owning_thread_id: None,
            owning_confirmed: false,
            machine: MachineState::AwaitOwningMeta,
            resumed_nonzero: false,
            fact: None,
            records: Vec::new(),
            ranges: Vec::new(),
            diagnostics: Vec::new(),
            diagnostic_count: 0,
            malformed_record_count: 0,
            retain_details,
            last_classification: None,
            needs_rebuild: false,
        };
        parser.initialize();
        parser
    }

    fn initialize(&mut self) {
        let state_candidate =
            valid_external_candidate(self.context.candidates.state_rollout.as_ref());
        let filename_candidate =
            valid_external_candidate(self.context.candidates.filename.as_ref());

        if let (Some((state_id, _)), Some((filename_id, _))) =
            (&state_candidate, &filename_candidate)
            && state_id != filename_id
        {
            self.needs_rebuild = true;
            self.diagnostic(
                DiagnosticCode::OwningCandidateConflict,
                DiagnosticSeverity::Conflict,
                None,
                Some(state_id.clone()),
                Some("owning_thread_id"),
            );
        }
        let external = state_candidate.or(filename_candidate);

        if self.context.chunk_start_offset > 0 {
            self.resumed_nonzero = true;
            let (owning_thread_id, resume_machine) = match &self.context.resume_state {
                ResumeState::ReplayedAncestor { owning_thread_id } => {
                    (owning_thread_id, MachineState::ReplayedAncestor)
                }
                ResumeState::OwningLive { owning_thread_id } => {
                    (owning_thread_id, MachineState::OwningLive)
                }
                ResumeState::AwaitOwningMeta => {
                    self.needs_rebuild = true;
                    self.diagnostic(
                        DiagnosticCode::InvalidResumeState,
                        DiagnosticSeverity::Conflict,
                        None,
                        external.as_ref().map(|(thread_id, _)| thread_id.clone()),
                        Some("resume_state"),
                    );
                    return;
                }
            };
            let Some(resume_id) = valid_uuid_string(Some(owning_thread_id)) else {
                self.needs_rebuild = true;
                self.diagnostic(
                    DiagnosticCode::InvalidResumeState,
                    DiagnosticSeverity::Conflict,
                    None,
                    None,
                    Some("owning_thread_id"),
                );
                return;
            };
            if external
                .as_ref()
                .is_some_and(|(value, _)| value != &resume_id)
            {
                self.needs_rebuild = true;
                self.diagnostic(
                    DiagnosticCode::OwningCandidateConflict,
                    DiagnosticSeverity::Conflict,
                    None,
                    Some(resume_id.clone()),
                    Some("owning_thread_id"),
                );
            }
            match self.context.existing_fact.clone() {
                Some(fact) if fact.owning_thread_id == resume_id => self.fact = Some(fact),
                _ => {
                    self.needs_rebuild = true;
                    self.diagnostic(
                        DiagnosticCode::InvalidResumeState,
                        DiagnosticSeverity::Conflict,
                        None,
                        Some(resume_id.clone()),
                        Some("existing_fact"),
                    );
                }
            }
            self.owning_thread_id = Some(resume_id);
            self.owning_confirmed = true;
            self.machine = resume_machine;
            return;
        }

        if let Some((owning_thread_id, confidence)) = external {
            self.fact = Some(RolloutThreadFact::empty(
                self.context.source_file_id,
                owning_thread_id.clone(),
            ));
            self.owning_thread_id = Some(owning_thread_id);
            self.owning_confirmed = confidence == OwningCandidateConfidence::Confirmed;
            self.machine = MachineState::OwningBootstrap;
        }
    }

    fn push_line(&mut self, line: CompleteRolloutLine) {
        if line.start_offset != self.last_processed_offset {
            self.needs_rebuild = true;
            self.diagnostic(
                DiagnosticCode::NonContiguousOffsets,
                DiagnosticSeverity::Conflict,
                Some(line.start_offset),
                self.owning_thread_id.clone(),
                None,
            );
        }
        self.parse_line(&line);
        self.last_processed_offset = line.end_offset;
    }

    fn finish(mut self) -> RolloutParseResult {
        let final_continuation = if self.needs_rebuild || !self.owning_confirmed {
            FinalContinuation::Unstable
        } else {
            match (&self.owning_thread_id, self.machine) {
                (Some(owning_thread_id), MachineState::ReplayedAncestor) => {
                    FinalContinuation::ReplayedAncestor {
                        owning_thread_id: owning_thread_id.clone(),
                    }
                }
                (
                    Some(owning_thread_id),
                    MachineState::OwningBootstrap | MachineState::OwningLive,
                ) => FinalContinuation::OwningLive {
                    owning_thread_id: owning_thread_id.clone(),
                },
                _ => FinalContinuation::Unstable,
            }
        };
        if let Some(fact) = self.fact.as_mut() {
            fact.ownership_boundary.confidence = if matches!(
                final_continuation,
                FinalContinuation::ReplayedAncestor { .. } | FinalContinuation::OwningLive { .. }
            ) {
                OwnershipConfidence::Confirmed
            } else {
                OwnershipConfidence::Unresolved
            };
        }
        RolloutParseResult {
            fact: if self.needs_rebuild { None } else { self.fact },
            records: if self.retain_details {
                self.records
            } else {
                Vec::new()
            },
            ownership_ranges: if self.retain_details {
                self.ranges
            } else {
                Vec::new()
            },
            diagnostics: if self.retain_details {
                self.diagnostics
            } else {
                Vec::new()
            },
            diagnostic_count: self.diagnostic_count,
            malformed_record_count: self.malformed_record_count,
            final_continuation,
            needs_rebuild: self.needs_rebuild,
            last_processed_offset: self.last_processed_offset,
        }
    }

    fn parse_line(&mut self, line: &CompleteRolloutLine) {
        let parsed = serde_json::from_slice::<Value>(line.json_bytes());
        let Ok(value) = parsed else {
            self.record(line, EnvelopeKind::Malformed, self.default_ownership());
            self.diagnostic(
                DiagnosticCode::MalformedJson,
                DiagnosticSeverity::Warning,
                Some(line.start_offset),
                self.owning_thread_id.clone(),
                None,
            );
            return;
        };
        let Some(object) = value.as_object() else {
            self.record(line, EnvelopeKind::Malformed, self.default_ownership());
            self.diagnostic(
                DiagnosticCode::MalformedEnvelope,
                DiagnosticSeverity::Warning,
                Some(line.start_offset),
                self.owning_thread_id.clone(),
                None,
            );
            return;
        };
        match classify_envelope(object) {
            EnvelopeKind::SessionMeta => self.parse_session_meta(line, object),
            EnvelopeKind::TurnContext => self.parse_turn_context(line, object),
            envelope => {
                let ownership = self.default_ownership();
                self.record(line, envelope, ownership);
                if envelope == EnvelopeKind::Unknown {
                    self.diagnostic(
                        DiagnosticCode::UnknownEnvelope,
                        DiagnosticSeverity::Warning,
                        Some(line.start_offset),
                        self.owning_thread_id.clone(),
                        None,
                    );
                }
            }
        }
    }

    fn parse_session_meta(&mut self, line: &CompleteRolloutLine, object: &Map<String, Value>) {
        let allowed = SessionMetaAllowed::parse(object);
        let Some(session_id) = allowed.thread_id.clone() else {
            self.record(
                line,
                EnvelopeKind::SessionMeta,
                RecordOwnership::UnknownOwnership,
            );
            self.diagnostic(
                DiagnosticCode::InvalidAllowedField,
                DiagnosticSeverity::Warning,
                Some(line.start_offset),
                self.owning_thread_id.clone(),
                Some("id"),
            );
            return;
        };

        if self.owning_thread_id.is_none() {
            self.owning_thread_id = Some(session_id.clone());
            self.fact = Some(RolloutThreadFact::empty(
                self.context.source_file_id,
                session_id.clone(),
            ));
            self.owning_confirmed = true;
            self.machine = MachineState::OwningBootstrap;
        }

        if self.owning_thread_id.as_deref() == Some(session_id.as_str()) {
            let resumed_from_replay = self.machine == MachineState::ReplayedAncestor;
            self.record(line, EnvelopeKind::SessionMeta, RecordOwnership::Owning);
            if resumed_from_replay {
                self.machine = MachineState::OwningLive;
            }
            if let Some(fact) = self.fact.as_mut() {
                if resumed_from_replay
                    || (fact
                        .ownership_boundary
                        .owning_records_start_offset
                        .is_none()
                        && fact.ownership_boundary.replay_start_offset.is_none())
                {
                    fact.ownership_boundary.owning_records_start_offset = Some(line.start_offset);
                }
                apply_session_meta(
                    fact,
                    allowed,
                    line.start_offset,
                    &mut self.diagnostics,
                    self.context.source_file_id,
                );
            }
            return;
        }

        let was_replayed = self.machine == MachineState::ReplayedAncestor;
        self.record(
            line,
            EnvelopeKind::SessionMeta,
            RecordOwnership::ReplayedAncestor,
        );
        if let Some(fact) = self.fact.as_mut() {
            fact.ownership_boundary
                .replay_start_offset
                .get_or_insert(line.start_offset);
        }
        self.machine = MachineState::ReplayedAncestor;
        self.diagnostic(
            DiagnosticCode::ForeignSessionMeta,
            DiagnosticSeverity::Warning,
            Some(line.start_offset),
            self.owning_thread_id.clone(),
            Some("id"),
        );
        if self.resumed_nonzero && !was_replayed {
            self.needs_rebuild = true;
        }
    }

    fn parse_turn_context(&mut self, line: &CompleteRolloutLine, object: &Map<String, Value>) {
        let allowed = TurnContextAllowed::parse(object);
        let ownership = match self.machine {
            MachineState::AwaitOwningMeta => RecordOwnership::UnknownOwnership,
            MachineState::OwningBootstrap | MachineState::OwningLive => RecordOwnership::Owning,
            MachineState::ReplayedAncestor => {
                if allowed.turn_id.as_deref().is_some_and(|turn_id| {
                    self.owning_thread_id
                        .as_deref()
                        .is_some_and(|owning_id| uuid7_is_not_earlier(turn_id, owning_id))
                }) {
                    self.machine = MachineState::OwningLive;
                    if let Some(fact) = self.fact.as_mut() {
                        fact.ownership_boundary.owning_records_start_offset =
                            Some(line.start_offset);
                    }
                    RecordOwnership::Owning
                } else {
                    RecordOwnership::ReplayedAncestor
                }
            }
        };
        self.record(line, EnvelopeKind::TurnContext, ownership);
        if ownership == RecordOwnership::Owning
            && allowed.structurally_valid
            && let Some(fact) = self.fact.as_mut()
        {
            apply_turn_context(
                fact,
                allowed,
                line.start_offset,
                &mut self.diagnostics,
                self.context.source_file_id,
            );
        }
    }

    fn default_ownership(&self) -> RecordOwnership {
        match self.machine {
            MachineState::AwaitOwningMeta => RecordOwnership::UnknownOwnership,
            MachineState::ReplayedAncestor => RecordOwnership::ReplayedAncestor,
            MachineState::OwningBootstrap | MachineState::OwningLive => RecordOwnership::Owning,
        }
    }

    fn record(
        &mut self,
        line: &CompleteRolloutLine,
        envelope: EnvelopeKind,
        ownership: RecordOwnership,
    ) {
        let classification = RecordClassification {
            start_offset: line.start_offset,
            end_offset: line.end_offset,
            envelope,
            ownership,
        };
        self.last_classification = Some(classification.clone());
        if self.retain_details {
            self.records.push(classification);
        }
        if !self.retain_details {
            return;
        }
        match self.ranges.last_mut() {
            Some(range)
                if range.ownership == ownership && range.end_offset == line.start_offset =>
            {
                range.end_offset = line.end_offset;
            }
            _ => self.ranges.push(OwnershipRange {
                start_offset: line.start_offset,
                end_offset: line.end_offset,
                ownership,
            }),
        }
    }

    fn diagnostic(
        &mut self,
        code: DiagnosticCode,
        severity: DiagnosticSeverity,
        offset: Option<u64>,
        thread_id: Option<String>,
        field: Option<&'static str>,
    ) {
        self.diagnostic_count = self.diagnostic_count.saturating_add(1);
        if matches!(
            code,
            DiagnosticCode::MalformedJson | DiagnosticCode::MalformedEnvelope
        ) {
            self.malformed_record_count = self.malformed_record_count.saturating_add(1);
        }
        if !self.retain_details {
            return;
        }
        self.diagnostics.push(RolloutDiagnostic {
            code,
            severity,
            source_file_id: self.context.source_file_id,
            source_start_offset: offset,
            thread_id,
            field,
        });
    }
}

#[derive(Default)]
struct SessionMetaAllowed {
    thread_id: Option<String>,
    created_at_ms: Option<i64>,
    cwd: Option<String>,
    agent_role: Option<String>,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    subagent_parent_thread_id: Option<String>,
    agent_path: Option<String>,
    thread_spawn_agent_path: Option<String>,
    has_subagent_source: bool,
}

impl SessionMetaAllowed {
    fn parse(object: &Map<String, Value>) -> Self {
        let payload = object.get("payload").and_then(Value::as_object);
        let Some(payload) = payload else {
            return Self::default();
        };
        let payload_timestamp = payload.get("timestamp").and_then(parse_timestamp_ms);
        let outer_timestamp = object.get("timestamp").and_then(parse_timestamp_ms);
        let source = payload.get("source").and_then(Value::as_object);
        let subagent = source
            .and_then(|source| source.get("subagent"))
            .and_then(Value::as_object);
        let thread_spawn = subagent
            .and_then(|subagent| subagent.get("thread_spawn"))
            .and_then(Value::as_object);
        Self {
            thread_id: payload
                .get("id")
                .and_then(Value::as_str)
                .and_then(|value| valid_uuid_string(Some(value))),
            created_at_ms: payload_timestamp.or(outer_timestamp),
            cwd: payload
                .get("cwd")
                .and_then(Value::as_str)
                .and_then(normalize_absolute_path),
            agent_role: payload
                .get("agent_role")
                .and_then(Value::as_str)
                .and_then(non_empty),
            parent_thread_id: payload
                .get("parent_thread_id")
                .and_then(Value::as_str)
                .and_then(|value| valid_uuid_string(Some(value))),
            forked_from_id: payload
                .get("forked_from_id")
                .and_then(Value::as_str)
                .and_then(|value| valid_uuid_string(Some(value))),
            subagent_parent_thread_id: thread_spawn
                .and_then(|spawn| spawn.get("parent_thread_id"))
                .and_then(Value::as_str)
                .and_then(|value| valid_uuid_string(Some(value))),
            agent_path: payload
                .get("agent_path")
                .and_then(Value::as_str)
                .and_then(normalize_agent_path),
            thread_spawn_agent_path: thread_spawn
                .and_then(|spawn| spawn.get("agent_path"))
                .and_then(Value::as_str)
                .and_then(normalize_agent_path),
            has_subagent_source: subagent.is_some(),
        }
    }
}

#[derive(Default)]
struct TurnContextAllowed {
    turn_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    timestamp_ms: Option<i64>,
    structurally_valid: bool,
}

impl TurnContextAllowed {
    fn parse(object: &Map<String, Value>) -> Self {
        let Some(payload) = object.get("payload").and_then(Value::as_object) else {
            return Self::default();
        };
        let turn_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok().map(|uuid| uuid.to_string()));
        Self {
            structurally_valid: turn_id.is_some(),
            turn_id,
            cwd: payload
                .get("cwd")
                .and_then(Value::as_str)
                .and_then(normalize_absolute_path),
            model: payload
                .get("model")
                .and_then(Value::as_str)
                .and_then(non_empty),
            timestamp_ms: object.get("timestamp").and_then(parse_timestamp_ms),
        }
    }
}

fn classify_envelope(object: &Map<String, Value>) -> EnvelopeKind {
    let Some(record_type) = object.get("type").and_then(Value::as_str) else {
        return EnvelopeKind::Malformed;
    };
    match record_type {
        "session_meta" => EnvelopeKind::SessionMeta,
        "turn_context" => EnvelopeKind::TurnContext,
        "token_count" => EnvelopeKind::TokenCount,
        "lifecycle" | "turn_started" | "turn_completed" | "turn_aborted" => EnvelopeKind::Lifecycle,
        "response_item" | "compacted" | "ghost_snapshot" => EnvelopeKind::Ignored,
        "event_msg" => match object
            .get("payload")
            .and_then(Value::as_object)
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
        {
            Some("token_count") => EnvelopeKind::TokenCount,
            Some(
                "task_started" | "task_complete" | "turn_started" | "turn_complete"
                | "turn_aborted" | "session_configured",
            ) => EnvelopeKind::Lifecycle,
            Some(
                "user_message" | "agent_message" | "agent_reasoning" | "raw_response_item"
                | "context_compacted",
            ) => EnvelopeKind::Ignored,
            _ => EnvelopeKind::Unknown,
        },
        _ => EnvelopeKind::Unknown,
    }
}

fn apply_session_meta(
    fact: &mut RolloutThreadFact,
    allowed: SessionMetaAllowed,
    offset: u64,
    diagnostics: &mut Vec<RolloutDiagnostic>,
    source_file_id: i64,
) {
    if let Some(created_at_ms) = allowed.created_at_ms {
        fact.created_at_ms = Some(
            fact.created_at_ms
                .map_or(created_at_ms, |existing| existing.min(created_at_ms)),
        );
    }
    if let Some(cwd) = allowed.cwd
        && merge_candidate(
            &mut fact.cwd,
            Candidate {
                value: cwd,
                provenance: CwdProvenance::SessionMeta,
                record_offset: offset,
            },
            |provenance| match provenance {
                CwdProvenance::SessionMeta => 2,
                CwdProvenance::TurnContext => 1,
            },
        )
    {
        mark_candidate_conflict(fact, diagnostics, source_file_id, offset, "cwd");
    }

    if let Some(parent) = allowed.parent_thread_id
        && merge_parent_candidate(
            &mut fact.parent_thread_id_hint,
            Candidate {
                value: parent,
                provenance: ParentHintProvenance::SessionMetaParent,
                record_offset: offset,
            },
        )
    {
        mark_relationship_conflict(
            fact,
            diagnostics,
            source_file_id,
            offset,
            "parent_thread_id_hint",
        );
    }

    if let Some(parent) = allowed.subagent_parent_thread_id
        && merge_parent_candidate(
            &mut fact.parent_thread_id_hint,
            Candidate {
                value: parent,
                provenance: ParentHintProvenance::SubagentSource,
                record_offset: offset,
            },
        )
    {
        mark_relationship_conflict(
            fact,
            diagnostics,
            source_file_id,
            offset,
            "parent_thread_id_hint",
        );
    }

    if (allowed.has_subagent_source || allowed.agent_role.as_deref() == Some("subagent"))
        && let Some(parent) = allowed.forked_from_id
        && merge_parent_candidate(
            &mut fact.parent_thread_id_hint,
            Candidate {
                value: parent,
                provenance: ParentHintProvenance::ForkedFromId,
                record_offset: offset,
            },
        )
    {
        mark_relationship_conflict(
            fact,
            diagnostics,
            source_file_id,
            offset,
            "parent_thread_id_hint",
        );
    }

    if allowed.has_subagent_source {
        if merge_candidate(
            &mut fact.agent_role_hint,
            Candidate {
                value: "subagent".to_owned(),
                provenance: AgentRoleProvenance::SubagentSource,
                record_offset: offset,
            },
            |provenance| match provenance {
                AgentRoleProvenance::SubagentSource => 2,
                AgentRoleProvenance::SessionMetaRole => 1,
            },
        ) {
            mark_relationship_conflict(
                fact,
                diagnostics,
                source_file_id,
                offset,
                "agent_role_hint",
            );
        }
    } else if let Some(agent_role) = allowed.agent_role
        && merge_candidate(
            &mut fact.agent_role_hint,
            Candidate {
                value: agent_role,
                provenance: AgentRoleProvenance::SessionMetaRole,
                record_offset: offset,
            },
            |provenance| match provenance {
                AgentRoleProvenance::SubagentSource => 2,
                AgentRoleProvenance::SessionMetaRole => 1,
            },
        )
    {
        mark_relationship_conflict(fact, diagnostics, source_file_id, offset, "agent_role_hint");
    }

    if let Some(agent_path) = allowed.agent_path
        && merge_agent_path_candidate(
            &mut fact.agent_path,
            Candidate {
                value: agent_path,
                provenance: AgentPathProvenance::SessionMeta,
                record_offset: offset,
            },
        )
    {
        mark_candidate_conflict(fact, diagnostics, source_file_id, offset, "agent_path");
    }
    if let Some(agent_path) = allowed.thread_spawn_agent_path
        && merge_agent_path_candidate(
            &mut fact.agent_path,
            Candidate {
                value: agent_path,
                provenance: AgentPathProvenance::ThreadSpawn,
                record_offset: offset,
            },
        )
    {
        mark_candidate_conflict(fact, diagnostics, source_file_id, offset, "agent_path");
    }
}

fn apply_turn_context(
    fact: &mut RolloutThreadFact,
    allowed: TurnContextAllowed,
    offset: u64,
    diagnostics: &mut Vec<RolloutDiagnostic>,
    source_file_id: i64,
) {
    if let Some(cwd) = allowed.cwd
        && merge_candidate(
            &mut fact.cwd,
            Candidate {
                value: cwd,
                provenance: CwdProvenance::TurnContext,
                record_offset: offset,
            },
            |provenance| match provenance {
                CwdProvenance::SessionMeta => 2,
                CwdProvenance::TurnContext => 1,
            },
        )
    {
        mark_candidate_conflict(fact, diagnostics, source_file_id, offset, "cwd");
    }
    let Some(model) = allowed.model else {
        return;
    };
    let incoming_order = latest_context_order_ms(allowed.turn_id.as_deref(), allowed.timestamp_ms);
    let existing_order = latest_context_order_ms(
        fact.latest_context_turn_id.as_deref(),
        fact.latest_context_at_ms,
    );
    let same_turn = fact.latest_context_turn_id.as_deref() == allowed.turn_id.as_deref()
        && allowed.turn_id.is_some();
    if same_turn && fact.latest_context_model.as_deref() != Some(model.as_str()) {
        fact.has_conflict = true;
        diagnostics.push(safe_diagnostic(
            DiagnosticCode::CandidateConflict,
            DiagnosticSeverity::Conflict,
            source_file_id,
            offset,
            Some(fact.owning_thread_id.clone()),
            "model",
        ));
    }
    let replace = match (incoming_order, existing_order) {
        (Some(incoming), Some(existing)) if incoming != existing => incoming > existing,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        _ => fact
            .latest_context_record_offset
            .is_none_or(|existing_offset| offset > existing_offset),
    };
    if replace {
        fact.latest_context_model = Some(model);
        fact.latest_context_turn_id = allowed.turn_id;
        fact.latest_context_at_ms = allowed.timestamp_ms;
        fact.latest_context_record_offset = Some(offset);
    }
}

/// Select the timestamp used to order the latest model observation. UUIDv7
/// turn ids carry a stable event timestamp; legacy/non-v7 ids fall back to
/// the envelope timestamp retained alongside the model.
pub(crate) fn latest_context_order_ms(
    turn_id: Option<&str>,
    envelope_timestamp_ms: Option<i64>,
) -> Option<i64> {
    turn_id
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|uuid| uuid.get_version_num() == 7)
        .map(|uuid| uuid7_timestamp_ms(&uuid) as i64)
        .or(envelope_timestamp_ms)
}

fn merge_candidate<P: Copy + Eq>(
    current: &mut Option<Candidate<P>>,
    incoming: Candidate<P>,
    priority: impl Fn(P) -> u8,
) -> bool {
    let Some(existing) = current.as_ref() else {
        *current = Some(incoming);
        return false;
    };
    let existing_priority = priority(existing.provenance);
    let incoming_priority = priority(incoming.provenance);
    if incoming_priority > existing_priority {
        *current = Some(incoming);
        return false;
    }
    if incoming_priority == existing_priority && existing.value != incoming.value {
        return true;
    }
    false
}

fn parent_priority(provenance: ParentHintProvenance) -> u8 {
    match provenance {
        ParentHintProvenance::SessionMetaParent => 3,
        ParentHintProvenance::SubagentSource => 2,
        ParentHintProvenance::ForkedFromId => 1,
    }
}

/// Merge parent candidates without changing the generic cwd/model merge
/// semantics. A differing value is always a conflict, even when the incoming
/// candidate loses by priority.
fn merge_parent_candidate(
    current: &mut Option<Candidate<ParentHintProvenance>>,
    incoming: Candidate<ParentHintProvenance>,
) -> bool {
    let Some(existing) = current.as_ref() else {
        *current = Some(incoming);
        return false;
    };
    let values_differ = existing.value != incoming.value;
    let existing_priority = parent_priority(existing.provenance);
    let incoming_priority = parent_priority(incoming.provenance);
    if !values_differ {
        if incoming_priority > existing_priority {
            *current = Some(incoming);
        }
        return false;
    }
    if incoming_priority > existing_priority {
        *current = Some(incoming);
    }
    true
}

fn merge_agent_path_candidate(
    current: &mut Option<Candidate<AgentPathProvenance>>,
    incoming: Candidate<AgentPathProvenance>,
) -> bool {
    let Some(existing) = current.as_ref() else {
        *current = Some(incoming);
        return false;
    };
    let values_differ = existing.value != incoming.value;
    let existing_priority = agent_path_priority(existing.provenance);
    let incoming_priority = agent_path_priority(incoming.provenance);
    if !values_differ {
        if incoming_priority > existing_priority {
            *current = Some(incoming);
        }
        return false;
    }
    if incoming_priority > existing_priority {
        *current = Some(incoming);
    }
    true
}

fn agent_path_priority(provenance: AgentPathProvenance) -> u8 {
    match provenance {
        AgentPathProvenance::SessionMeta => 2,
        AgentPathProvenance::ThreadSpawn => 1,
    }
}

fn mark_candidate_conflict(
    fact: &mut RolloutThreadFact,
    diagnostics: &mut Vec<RolloutDiagnostic>,
    source_file_id: i64,
    offset: u64,
    field: &'static str,
) {
    fact.has_conflict = true;
    diagnostics.push(safe_diagnostic(
        DiagnosticCode::CandidateConflict,
        DiagnosticSeverity::Conflict,
        source_file_id,
        offset,
        Some(fact.owning_thread_id.clone()),
        field,
    ));
}

fn mark_relationship_conflict(
    fact: &mut RolloutThreadFact,
    diagnostics: &mut Vec<RolloutDiagnostic>,
    source_file_id: i64,
    offset: u64,
    field: &'static str,
) {
    fact.relationship_conflict = true;
    mark_candidate_conflict(fact, diagnostics, source_file_id, offset, field);
}

fn safe_diagnostic(
    code: DiagnosticCode,
    severity: DiagnosticSeverity,
    source_file_id: i64,
    offset: u64,
    thread_id: Option<String>,
    field: &'static str,
) -> RolloutDiagnostic {
    RolloutDiagnostic {
        code,
        severity,
        source_file_id,
        source_start_offset: Some(offset),
        thread_id,
        field: Some(field),
    }
}

fn parse_timestamp_ms(value: &Value) -> Option<i64> {
    match value {
        Value::String(value) => DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|timestamp| timestamp.timestamp_millis()),
        Value::Number(value) => value.as_i64().filter(|value| *value >= 0),
        _ => None,
    }
}

fn valid_uuid_string(value: Option<&str>) -> Option<String> {
    Uuid::parse_str(value?).ok().map(|uuid| uuid.to_string())
}

fn valid_external_candidate(
    candidate: Option<&OwningThreadCandidate>,
) -> Option<(String, OwningCandidateConfidence)> {
    let candidate = candidate?;
    Some((
        valid_uuid_string(Some(&candidate.thread_id))?,
        candidate.confidence,
    ))
}

fn uuid7_is_not_earlier(turn_id: &str, owning_thread_id: &str) -> bool {
    let Ok(turn_id) = Uuid::parse_str(turn_id) else {
        return false;
    };
    let Ok(owning_thread_id) = Uuid::parse_str(owning_thread_id) else {
        return false;
    };
    if turn_id.get_version_num() != 7 || owning_thread_id.get_version_num() != 7 {
        return false;
    }
    uuid7_timestamp_ms(&turn_id) >= uuid7_timestamp_ms(&owning_thread_id)
}

fn uuid7_timestamp_ms(uuid: &Uuid) -> u64 {
    uuid.as_bytes()[..6]
        .iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte))
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn normalize_absolute_path(value: &str) -> Option<String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized.to_str().map(str::to_owned)
}

/// Normalize an agent task path lexically.  This value is metadata, not a
/// filesystem reference: no canonicalization or existence check is allowed.
pub(crate) fn normalize_agent_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return None;
    }
    if !raw.starts_with('/') || raw.starts_with("//") {
        return None;
    }

    let mut components = Vec::new();
    for component in raw[1..].split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            component => components.push(component),
        }
    }
    let normalized = format!("/{}", components.join("/"));
    if normalized == "/root" {
        return None;
    }
    if !normalized.starts_with("/root/") {
        return None;
    }
    let final_component = Path::new(&normalized).file_name()?.to_str()?;
    (!final_component.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical_path(components: &[&str]) -> String {
        let mut path = std::env::temp_dir();
        for component in components {
            path.push(component);
        }
        path.to_string_lossy().into_owned()
    }

    fn json_path(value: &str) -> String {
        serde_json::to_string(value).unwrap()
    }

    fn uuid7(timestamp_ms: u64, suffix: u8) -> String {
        let mut bytes = [0_u8; 16];
        for (index, byte) in bytes[..6].iter_mut().enumerate() {
            *byte = ((timestamp_ms >> (8 * (5 - index))) & 0xff) as u8;
        }
        bytes[6] = 0x70;
        bytes[8] = 0x80;
        bytes[15] = suffix;
        Uuid::from_bytes(bytes).to_string()
    }

    fn lines(values: &[String], start: u64) -> Vec<CompleteRolloutLine> {
        let mut offset = start;
        values
            .iter()
            .map(|value| {
                let bytes = format!("{value}\n").into_bytes();
                let line = CompleteRolloutLine::new(offset, bytes).unwrap();
                offset = line.end_offset();
                line
            })
            .collect()
    }

    fn context(
        chunk_start_offset: u64,
        owning_thread_id: &str,
        resume_state: ResumeState,
        existing_fact: Option<RolloutThreadFact>,
    ) -> RolloutParseContext {
        RolloutParseContext {
            source_file_id: 7,
            chunk_start_offset,
            candidates: OwningThreadCandidates {
                state_rollout: None,
                filename: Some(OwningThreadCandidate {
                    thread_id: owning_thread_id.to_owned(),
                    confidence: OwningCandidateConfidence::Confirmed,
                }),
            },
            resume_state,
            existing_fact,
        }
    }

    #[test]
    fn parses_main_rollout_whitelist_and_envelopes() {
        let owning = uuid7(2_000, 1);
        let turn = uuid7(2_100, 2);
        let session_cwd = json_path(&physical_path(&["rollout", "work", ".", "mini"]));
        let turn_cwd = json_path(&physical_path(&["rollout", "fallback"]));
        let records = vec![
            format!(
                r#"{{"timestamp":"2026-08-08T00:00:00Z","type":"session_meta","payload":{{"id":"{owning}","timestamp":"2026-08-08T00:00:00Z","cwd":{session_cwd},"agent_role":"main","base_instructions":"never retain"}}}}"#
            ),
            format!(
                r#"{{"timestamp":"2026-08-08T00:00:01Z","type":"turn_context","payload":{{"turn_id":"{turn}","cwd":{turn_cwd},"model":"gpt-main","timezone":"UTC","other":"ignored"}}}}"#
            ),
            r#"{"type":"event_msg","payload":{"type":"token_count","total":999}}"#.to_owned(),
            r#"{"type":"response_item","payload":{"type":"message","content":"ignored"}}"#
                .to_owned(),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        assert!(!result.needs_rebuild);
        assert_eq!(
            result
                .records
                .iter()
                .map(|record| record.envelope)
                .collect::<Vec<_>>(),
            vec![
                EnvelopeKind::SessionMeta,
                EnvelopeKind::TurnContext,
                EnvelopeKind::TokenCount,
                EnvelopeKind::Ignored
            ]
        );
        assert!(
            result
                .records
                .iter()
                .all(|record| record.ownership == RecordOwnership::Owning)
        );
        let fact = result.fact.unwrap();
        assert_eq!(fact.owning_thread_id, owning);
        assert_eq!(
            fact.cwd.unwrap().value,
            physical_path(&["rollout", "work", "mini"])
        );
        assert_eq!(fact.latest_context_model.as_deref(), Some("gpt-main"));
        assert_eq!(fact.agent_role_hint.unwrap().value, "main");
        assert!(matches!(
            result.final_continuation,
            FinalContinuation::OwningLive { .. }
        ));
        assert_eq!(
            result.last_processed_offset,
            result.records.last().unwrap().end_offset
        );
    }

    #[test]
    fn guardian_direct_parent_and_other_source_are_safe_facts() {
        let parent = uuid7(1_000, 1);
        let guardian = uuid7(2_000, 2);
        let records = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": guardian,
                    "parent_thread_id": parent,
                    "source": {"subagent": {"other": "guardian"}}
                }
            })
            .to_string(),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &guardian, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        let fact = result.fact.unwrap();
        let parent_candidate = fact.parent_thread_id_hint.as_ref().unwrap();
        assert_eq!(parent_candidate.value, parent);
        assert_eq!(
            parent_candidate.provenance,
            ParentHintProvenance::SessionMetaParent
        );
        let role = fact.agent_role_hint.as_ref().unwrap();
        assert_eq!(role.value, "subagent");
        assert_eq!(role.provenance, AgentRoleProvenance::SubagentSource);
        assert!(!fact.has_conflict);
        assert!(!fact.relationship_conflict);
        let safe = fact
            .to_safe_fact(
                1,
                METADATA_PARSER_VERSION,
                result.last_processed_offset,
                10,
                &result.final_continuation,
            )
            .unwrap();
        assert_eq!(
            safe.parent_hint_provenance,
            Some(crate::codex::domain::ParentHintProvenance::SessionMetaParent)
        );
        assert_eq!(
            RolloutThreadFact::from_safe_fact(&safe)
                .unwrap()
                .parent_thread_id_hint
                .unwrap()
                .provenance,
            ParentHintProvenance::SessionMetaParent
        );
    }

    #[test]
    fn direct_and_nested_same_parent_is_consistent() {
        let parent = uuid7(1_000, 1);
        let child = uuid7(2_000, 2);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": child,
                "parent_thread_id": parent,
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": parent}}}
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &child, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let fact = result.fact.unwrap();
        let candidate = fact.parent_thread_id_hint.unwrap();
        assert_eq!(candidate.value, parent);
        assert_eq!(
            candidate.provenance,
            ParentHintProvenance::SessionMetaParent
        );
        assert!(!fact.has_conflict);
    }

    #[test]
    fn direct_and_nested_conflict_keeps_direct_winner_and_marks_conflict() {
        let direct = uuid7(1_000, 1);
        let nested = uuid7(1_100, 2);
        let child = uuid7(2_000, 3);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": child,
                "parent_thread_id": direct,
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": nested}}}
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &child, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let fact = result.fact.unwrap();
        assert_eq!(
            fact.parent_thread_id_hint.unwrap().provenance,
            ParentHintProvenance::SessionMetaParent
        );
        assert!(fact.has_conflict);
        assert!(fact.relationship_conflict);
    }

    #[test]
    fn direct_and_fork_conflict_keeps_direct_winner_and_marks_conflict() {
        let direct = uuid7(1_000, 1);
        let forked = uuid7(1_100, 2);
        let child = uuid7(2_000, 3);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": child,
                "parent_thread_id": direct,
                "forked_from_id": forked,
                "source": {"subagent": {"other": "guardian"}}
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &child, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let fact = result.fact.unwrap();
        assert_eq!(
            fact.parent_thread_id_hint.unwrap().provenance,
            ParentHintProvenance::SessionMetaParent
        );
        assert!(fact.has_conflict);
        assert!(fact.relationship_conflict);
    }

    #[test]
    fn parent_candidates_use_priority_but_preserve_every_conflict() {
        let direct = uuid7(1_000, 1);
        let nested = uuid7(1_100, 2);
        let forked = uuid7(1_200, 3);
        let guardian = uuid7(2_000, 4);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": guardian,
                "parent_thread_id": direct,
                "forked_from_id": forked,
                "source": {
                    "subagent": {
                        "thread_spawn": {"parent_thread_id": nested},
                        "other": "guardian"
                    }
                }
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &guardian, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let fact = result.fact.unwrap();
        let parent = fact.parent_thread_id_hint.unwrap();
        assert_eq!(parent.value, direct);
        assert_eq!(parent.provenance, ParentHintProvenance::SessionMetaParent);
        assert!(fact.has_conflict);
        assert!(fact.relationship_conflict);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.field == Some("parent_thread_id_hint"))
                .count(),
            2
        );
    }

    #[test]
    fn repeated_direct_parent_conflict_keeps_first_trusted_value() {
        let first = uuid7(1_000, 1);
        let second = uuid7(1_100, 2);
        let guardian = uuid7(2_000, 3);
        let records = vec![
            serde_json::json!({
                "type": "session_meta",
                "payload": {"id": guardian, "parent_thread_id": first}
            })
            .to_string(),
            serde_json::json!({
                "type": "session_meta",
                "payload": {"id": guardian, "parent_thread_id": second}
            })
            .to_string(),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &guardian, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        let fact = result.fact.unwrap();
        assert_eq!(fact.parent_thread_id_hint.unwrap().value, first);
        assert!(fact.has_conflict);
        assert!(fact.relationship_conflict);
    }

    #[test]
    fn subagent_replay_switches_only_at_uuid7_owning_turn_boundary() {
        let parent = uuid7(1_000, 1);
        let child = uuid7(2_000, 2);
        let parent_turn = uuid7(1_500, 3);
        let child_turn = uuid7(2_100, 4);
        let child_cwd = physical_path(&["rollout", "child"]);
        let parent_cwd = json_path(&physical_path(&["rollout", "parent"]));
        let parent_turn_cwd = json_path(&physical_path(&["rollout", "parent-turn"]));
        let child_turn_cwd = json_path(&physical_path(&["rollout", "child-turn"]));
        let child_cwd_json = json_path(&child_cwd);
        let records = vec![
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{child}","cwd":{child_cwd_json},"source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}","depth":1}}}}}}}}}}"#
            ),
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{parent}","cwd":{parent_cwd}}}}}"#
            ),
            format!(
                r#"{{"type":"turn_context","payload":{{"turn_id":"{parent_turn}","cwd":{parent_turn_cwd},"model":"parent-model"}}}}"#
            ),
            r#"{"type":"event_msg","payload":{"type":"token_count","total":111}}"#.to_owned(),
            format!(
                r#"{{"type":"turn_context","payload":{{"turn_id":"{child_turn}","cwd":{child_turn_cwd},"model":"child-model"}}}}"#
            ),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &child, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        assert_eq!(
            result
                .records
                .iter()
                .map(|record| record.ownership)
                .collect::<Vec<_>>(),
            vec![
                RecordOwnership::Owning,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::Owning,
            ]
        );
        let fact = result.fact.unwrap();
        assert_eq!(fact.cwd.unwrap().value, child_cwd);
        assert_eq!(fact.latest_context_model.as_deref(), Some("child-model"));
        assert_eq!(fact.parent_thread_id_hint.unwrap().value, parent);
        assert_eq!(
            fact.ownership_boundary.replay_start_offset,
            Some(result.records[1].start_offset)
        );
        assert_eq!(
            fact.ownership_boundary.owning_records_start_offset,
            Some(result.records[4].start_offset)
        );
        assert_eq!(
            fact.ownership_boundary.confidence,
            OwnershipConfidence::Confirmed
        );
        assert_eq!(result.ownership_ranges.len(), 3);
        assert_eq!(
            result
                .ownership_ranges
                .iter()
                .map(|range| range.ownership)
                .collect::<Vec<_>>(),
            vec![
                RecordOwnership::Owning,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::Owning,
            ]
        );
        assert_eq!(
            result.ownership_ranges[1].start_offset,
            result.records[1].start_offset
        );
        assert_eq!(
            result.ownership_ranges[1].end_offset,
            result.records[3].end_offset
        );
        assert!(matches!(
            result.final_continuation,
            FinalContinuation::OwningLive { .. }
        ));
    }

    #[test]
    fn eof_in_replayed_ancestor_is_stable_resumable_continuation() {
        let parent = uuid7(1_000, 1);
        let child = uuid7(2_000, 2);
        let parent_turn = uuid7(1_500, 3);
        let records = vec![
            format!(r#"{{"type":"session_meta","payload":{{"id":"{child}"}}}}"#),
            format!(r#"{{"type":"session_meta","payload":{{"id":"{parent}"}}}}"#),
            format!(
                r#"{{"type":"turn_context","payload":{{"turn_id":"{parent_turn}","model":"parent-model"}}}}"#
            ),
            r#"{"type":"turn_context","payload":{"model":"missing-turn-id"}}"#.to_owned(),
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#.to_owned(),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &child, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        assert!(!result.needs_rebuild);
        assert_eq!(
            result.final_continuation,
            FinalContinuation::ReplayedAncestor {
                owning_thread_id: child.clone(),
            }
        );
        assert_eq!(
            result
                .records
                .iter()
                .map(|record| record.ownership)
                .collect::<Vec<_>>(),
            vec![
                RecordOwnership::Owning,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::ReplayedAncestor,
                RecordOwnership::ReplayedAncestor,
            ]
        );
        assert_eq!(result.ownership_ranges.len(), 2);
        let fact = result.fact.unwrap();
        assert_eq!(
            fact.ownership_boundary.confidence,
            OwnershipConfidence::Confirmed
        );
        assert_eq!(fact.latest_context_model, None);
        let safe = fact
            .to_safe_fact(
                1,
                METADATA_PARSER_VERSION,
                result.last_processed_offset,
                10,
                &result.final_continuation,
            )
            .unwrap();
        assert_eq!(
            safe.continuation_state,
            crate::codex::domain::ContinuationState::ReplayedAncestor
        );
        assert_eq!(
            safe.ownership_confidence,
            crate::codex::domain::OwnershipConfidence::Confirmed
        );
    }

    #[test]
    fn nonzero_replayed_ancestor_resume_stays_replay_until_owning_boundary() {
        let child = uuid7(2_000, 2);
        let parent_turn = uuid7(1_500, 3);
        let child_turn = uuid7(2_100, 4);
        let mut existing = RolloutThreadFact::empty(7, child.clone());
        existing.ownership_boundary.replay_start_offset = Some(20);
        let replay_records = vec![
            format!(
                r#"{{"type":"turn_context","payload":{{"turn_id":"{parent_turn}","model":"parent"}}}}"#
            ),
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#.to_owned(),
        ];
        let replay = RolloutMetadataParser::parse_chunk(
            context(
                100,
                &child,
                ResumeState::ReplayedAncestor {
                    owning_thread_id: child.clone(),
                },
                Some(existing),
            ),
            lines(&replay_records, 100),
        );
        assert!(!replay.needs_rebuild);
        assert_eq!(
            replay.final_continuation,
            FinalContinuation::ReplayedAncestor {
                owning_thread_id: child.clone()
            }
        );
        assert!(
            replay
                .records
                .iter()
                .all(|record| record.ownership == RecordOwnership::ReplayedAncestor)
        );
        let resume_offset = replay.last_processed_offset;
        let owning_record = format!(
            r#"{{"type":"turn_context","payload":{{"turn_id":"{child_turn}","model":"child"}}}}"#
        );
        let owning = RolloutMetadataParser::parse_chunk(
            context(
                resume_offset,
                &child,
                ResumeState::ReplayedAncestor {
                    owning_thread_id: child.clone(),
                },
                replay.fact,
            ),
            lines(&[owning_record], resume_offset),
        );
        assert!(!owning.needs_rebuild);
        assert_eq!(
            owning.final_continuation,
            FinalContinuation::OwningLive {
                owning_thread_id: child
            }
        );
        assert_eq!(owning.records[0].ownership, RecordOwnership::Owning);
    }

    #[test]
    fn normal_nonzero_owning_live_resume_restores_model_ordering_seam() {
        let owning = uuid7(2_000, 1);
        let mut existing = RolloutThreadFact::empty(7, owning.clone());
        existing.latest_context_model = Some("old-model".to_owned());
        existing.latest_context_at_ms = Some(1_000);
        existing.latest_context_record_offset = Some(20);
        let safe = existing
            .to_safe_fact(
                1,
                1,
                100,
                1_000,
                &FinalContinuation::OwningLive {
                    owning_thread_id: owning.clone(),
                },
            )
            .unwrap();
        let restored = RolloutThreadFact::from_safe_fact(&safe).unwrap();
        assert_eq!(restored.latest_context_record_offset, Some(99));
        let turn = uuid7(2_100, 2);
        let records = vec![format!(
            r#"{{"timestamp":"1970-01-01T00:00:01Z","type":"turn_context","payload":{{"turn_id":"{turn}","model":"new-model"}}}}"#
        )];
        let result = RolloutMetadataParser::parse_chunk(
            context(
                100,
                &owning,
                ResumeState::OwningLive {
                    owning_thread_id: owning.clone(),
                },
                Some(restored),
            ),
            lines(&records, 100),
        );

        assert!(!result.needs_rebuild);
        assert_eq!(
            result.final_continuation,
            FinalContinuation::OwningLive {
                owning_thread_id: owning,
            }
        );
        assert_eq!(result.records[0].ownership, RecordOwnership::Owning);
        let fact = result.fact.unwrap();
        assert_eq!(fact.latest_context_model.as_deref(), Some("new-model"));
        assert_eq!(fact.latest_context_turn_id.as_deref(), Some(turn.as_str()));
        assert_eq!(fact.latest_context_record_offset, Some(100));
        assert!(!fact.has_conflict);
    }

    #[test]
    fn conflicting_state_path_and_filename_candidates_require_rebuild() {
        let state_id = uuid7(2_000, 1);
        let filename_id = uuid7(2_000, 2);
        let records = vec![format!(
            r#"{{"type":"session_meta","payload":{{"id":"{state_id}"}}}}"#
        )];
        let result = RolloutMetadataParser::parse_chunk(
            RolloutParseContext {
                source_file_id: 7,
                chunk_start_offset: 0,
                candidates: OwningThreadCandidates {
                    state_rollout: Some(OwningThreadCandidate {
                        thread_id: state_id,
                        confidence: OwningCandidateConfidence::Confirmed,
                    }),
                    filename: Some(OwningThreadCandidate {
                        thread_id: filename_id,
                        confidence: OwningCandidateConfidence::Confirmed,
                    }),
                },
                resume_state: ResumeState::AwaitOwningMeta,
                existing_fact: None,
            },
            lines(&records, 0),
        );

        assert!(result.needs_rebuild);
        assert!(result.fact.is_none());
        assert_eq!(result.final_continuation, FinalContinuation::Unstable);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::OwningCandidateConflict)
        );
    }

    #[test]
    fn missing_owning_meta_and_external_id_keeps_records_unknown() {
        let turn = uuid7(2_100, 1);
        let records = vec![
            format!(
                r#"{{"type":"turn_context","payload":{{"turn_id":"{turn}","model":"unowned-model"}}}}"#
            ),
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#.to_owned(),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            RolloutParseContext {
                source_file_id: 7,
                chunk_start_offset: 0,
                candidates: OwningThreadCandidates::default(),
                resume_state: ResumeState::AwaitOwningMeta,
                existing_fact: None,
            },
            lines(&records, 0),
        );

        assert_eq!(result.fact, None);
        assert_eq!(result.final_continuation, FinalContinuation::Unstable);
        assert!(
            result
                .records
                .iter()
                .all(|record| record.ownership == RecordOwnership::UnknownOwnership)
        );
        assert_eq!(
            result.ownership_ranges,
            vec![OwnershipRange {
                start_offset: result.records[0].start_offset,
                end_offset: result.records[1].end_offset,
                ownership: RecordOwnership::UnknownOwnership,
            }]
        );
    }

    #[test]
    fn multiple_turn_models_choose_latest_turn_and_equal_time_later_offset_without_conflict() {
        let owning = uuid7(2_000, 1);
        let turns = [uuid7(2_100, 2), uuid7(2_200, 3), uuid7(2_300, 4)];
        let records = vec![
            format!(r#"{{"type":"session_meta","payload":{{"id":"{owning}"}}}}"#),
            format!(
                r#"{{"timestamp":"1970-01-01T00:00:01Z","type":"turn_context","payload":{{"turn_id":"{}","model":"old"}}}}"#,
                turns[0]
            ),
            format!(
                r#"{{"timestamp":"1970-01-01T00:00:02Z","type":"turn_context","payload":{{"turn_id":"{}","model":"newer"}}}}"#,
                turns[1]
            ),
            format!(
                r#"{{"timestamp":"1970-01-01T00:00:02Z","type":"turn_context","payload":{{"turn_id":"{}","model":"same-time-later"}}}}"#,
                turns[2]
            ),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );

        let fact = result.fact.unwrap();
        assert_eq!(
            fact.latest_context_model.as_deref(),
            Some("same-time-later")
        );
        assert_eq!(fact.latest_context_at_ms, Some(2_000));
        assert_eq!(
            fact.latest_context_record_offset,
            Some(result.records[3].start_offset)
        );
        assert!(!fact.has_conflict);
        assert!(!result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::CandidateConflict
                && diagnostic.field == Some("model")
        }));
    }

    #[test]
    fn same_turn_model_conflict_is_quality_only_and_later_offset_wins() {
        let owning = uuid7(2_000, 1);
        let turn = uuid7(2_100, 2);
        let records = vec![
            format!(r#"{{"type":"session_meta","payload":{{"id":"{owning}"}}}}"#),
            format!(
                r#"{{"timestamp":"1970-01-01T00:00:02Z","type":"turn_context","payload":{{"turn_id":"{turn}","model":"gpt-5.6-luna"}}}}"#
            ),
            format!(
                r#"{{"timestamp":"1970-01-01T00:00:02Z","type":"turn_context","payload":{{"turn_id":"{turn}","model":"gpt-5.6-sol"}}}}"#
            ),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        let fact = result.fact.unwrap();
        assert_eq!(fact.latest_context_model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(fact.latest_context_turn_id.as_deref(), Some(turn.as_str()));
        assert!(fact.has_conflict);
        assert!(!fact.relationship_conflict);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::CandidateConflict
                && diagnostic.field == Some("model")
        }));
    }

    #[test]
    fn nonzero_resume_with_late_foreign_meta_requires_rebuild() {
        let parent = uuid7(1_000, 1);
        let child = uuid7(2_000, 2);
        let existing = RolloutThreadFact::empty(7, child.clone());
        let records = vec![format!(
            r#"{{"type":"session_meta","payload":{{"id":"{parent}","cwd":"/private-parent"}}}}"#
        )];
        let result = RolloutMetadataParser::parse_chunk(
            context(
                100,
                &child,
                ResumeState::OwningLive {
                    owning_thread_id: child.clone(),
                },
                Some(existing),
            ),
            lines(&records, 100),
        );
        assert!(result.needs_rebuild);
        assert!(result.fact.is_none());
        assert_eq!(result.final_continuation, FinalContinuation::Unstable);
        assert_eq!(
            result.records[0].ownership,
            RecordOwnership::ReplayedAncestor
        );
    }

    #[test]
    fn ignored_unknown_and_malformed_records_never_leak_body_sentinel() {
        let owning = uuid7(2_000, 1);
        let sentinel = "PROMPT_TOOL_OUTPUT_PRIVATE_SENTINEL";
        let records = vec![
            format!(
                r#"{{"type":"response_item","payload":{{"type":"message","content":"{sentinel}"}}}}"#
            ),
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"user_message","message":"{sentinel}"}}}}"#
            ),
            format!(r#"{{"type":"future_record","payload":{{"body":"{sentinel}"}}}}"#),
            format!(r#"{{"type":"broken","payload":"{sentinel}""#),
        ];
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&records, 0),
        );
        assert_eq!(
            result
                .records
                .iter()
                .map(|record| record.envelope)
                .collect::<Vec<_>>(),
            vec![
                EnvelopeKind::Ignored,
                EnvelopeKind::Ignored,
                EnvelopeKind::Unknown,
                EnvelopeKind::Malformed,
            ]
        );
        assert!(!format!("{result:?}").contains(sentinel));
    }

    #[test]
    fn normalize_agent_path_is_lexical_root_namespace_only() {
        assert_eq!(
            normalize_agent_path("  /root/group/./gate/../task  ").as_deref(),
            Some("/root/group/task")
        );
        assert_eq!(normalize_agent_path("/root"), None);
        assert_eq!(normalize_agent_path("/root/"), None);
        assert_eq!(normalize_agent_path("/rooted/task"), None);
        assert_eq!(normalize_agent_path("/tmp/task"), None);
        assert_eq!(normalize_agent_path("root/task"), None);
        assert_eq!(normalize_agent_path("//root/task"), None);
        assert_eq!(normalize_agent_path(r"C:\root\task"), None);
        assert_eq!(normalize_agent_path("C:/root/task"), None);
        assert_eq!(normalize_agent_path("/root/task\0"), None);
        assert_eq!(normalize_agent_path("/root/task/\u{1f}"), None);
        assert_eq!(normalize_agent_path("/root/../../task"), None);
    }

    #[test]
    fn session_meta_agent_path_uses_priority_and_marks_conflict() {
        let owning = uuid7(2_000, 1);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": owning,
                "agent_path": "/root/high_priority",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "agent_path": "/root/lower_priority"
                        }
                    }
                }
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let fact = result.fact.unwrap();
        let candidate = fact.agent_path.unwrap();
        assert_eq!(candidate.value, "/root/high_priority");
        assert_eq!(candidate.provenance, AgentPathProvenance::SessionMeta);
        assert!(fact.has_conflict);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DiagnosticCode::CandidateConflict
                && diagnostic.field == Some("agent_path")
        }));
    }

    #[test]
    fn thread_spawn_agent_path_is_retained_with_thread_spawn_provenance() {
        let owning = uuid7(2_000, 1);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": owning,
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "agent_path": "/root/thread_spawn_task"
                        }
                    }
                }
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let candidate = result.fact.unwrap().agent_path.unwrap();
        assert_eq!(candidate.value, "/root/thread_spawn_task");
        assert_eq!(candidate.provenance, AgentPathProvenance::ThreadSpawn);
    }

    #[test]
    fn agent_path_safe_fact_conversion_preserves_provenance_and_offset() {
        let owning = uuid7(2_000, 1);
        let record = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": owning,
                "agent_path": "/root/persisted_task"
            }
        })
        .to_string();
        let result = RolloutMetadataParser::parse_chunk(
            context(0, &owning, ResumeState::AwaitOwningMeta, None),
            lines(&[record], 0),
        );
        let fact = result.fact.unwrap();
        let record_offset = fact.agent_path.as_ref().unwrap().record_offset;
        let safe = fact
            .to_safe_fact(
                1,
                METADATA_PARSER_VERSION,
                result.last_processed_offset,
                10,
                &result.final_continuation,
            )
            .unwrap();
        assert_eq!(safe.agent_path.as_deref(), Some("/root/persisted_task"));
        assert_eq!(
            safe.agent_path_provenance,
            Some(crate::codex::domain::AgentPathProvenance::SessionMeta)
        );
        assert_eq!(safe.agent_path_record_offset, Some(record_offset as i64));
        let restored = RolloutThreadFact::from_safe_fact(&safe).unwrap();
        let restored_path = restored.agent_path.unwrap();
        assert_eq!(restored_path.value, "/root/persisted_task");
        assert_eq!(restored_path.provenance, AgentPathProvenance::SessionMeta);
        assert_eq!(restored_path.record_offset, record_offset);
    }
}
