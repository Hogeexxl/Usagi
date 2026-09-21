//! Codex source adapters and the metadata resolver.
//!
//! The source modules expose facts rather than raw Codex rows or JSON.  The
//! resolver consumes those facts after the scanner/storage layer has selected
//! matching generations and safe facts.

/// Shared severity for source-adapter diagnostics.  Resolver diagnostics use
/// their own severity because they describe normalized relationship quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Conflict,
    Error,
}

impl DiagnosticSeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Conflict => "conflict",
            Self::Error => "error",
        }
    }
}

/// Availability of one non-rollout source view.  `Partial` means the reader
/// retained complete rows but encountered a trailing half-line; `Unavailable`
/// means the required schema/source could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAvailability {
    Complete,
    Partial,
    Unavailable,
}

impl SourceAvailability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }

    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

pub mod domain;
pub mod global_state;
pub mod metadata;
pub mod normalization;
pub mod quota;
pub mod rollout;
pub mod session_index;
mod skill_usage;
pub mod state_index;
pub mod usage;

mod adapter;
pub(crate) mod analytics;
mod config;
pub(crate) mod ingestion;
pub(crate) mod status;
pub(crate) mod storage;

pub use adapter::{CodexAdapter, CodexSessionErrorSidecar};
pub use config::{CodexConfig, CodexConfigError, CodexConfigResolution, CodexMetadataPaths};

pub use crate::domain::ExistingThreadProjection;

pub use global_state::{
    GlobalStateDiagnostic, GlobalStateReader, GlobalStateSnapshot, GlobalStateStatus,
};

pub use metadata::{
    ExistingThread, MetadataDiagnostic, MetadataDiagnosticCode, MetadataDiagnosticSeverity,
    MetadataSourceKind, ResolutionInput, ResolutionResult, ThreadMetadataResolver,
};
pub use rollout::{
    AgentRoleProvenance, Candidate, CompleteRolloutLine, CwdProvenance, EnvelopeKind,
    FinalContinuation, METADATA_PARSER_VERSION, OwnershipBoundary, OwnershipConfidence,
    OwnershipRange, ParentHintProvenance, RecordClassification, RecordOwnership, ResumeState,
    RolloutDiagnostic, RolloutMetadataParser, RolloutParseContext, RolloutParseResult,
    RolloutThreadFact,
};
pub use session_index::{
    SessionIndexDiagnostic, SessionIndexError, SessionIndexReader, SessionNameFact,
    SessionNameSnapshot, SessionSourceStatus,
};
pub use skill_usage::{SkillUsageEvidence, SkillUsageParser};
pub use state_index::{
    SpawnEdgeFact, SpawnEdgeSource, StateDiagnostic, StateIndexError, StateIndexReader,
    StateSnapshot, StateSourceStatus, StateThreadFact,
};
pub use usage::{
    CodexRolloutParser, CompleteUsageLine, LifecycleKind, LifecycleRecord, NormalizedTokenValue,
    OptionalTokenValue, TokenCountInfo, TokenCountRecord, TokenValueError, TurnContextRecord,
    UsageRawRecord,
};
