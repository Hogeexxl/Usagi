//! Codex physical source-domain values.
//!
//! These DTOs describe Codex rollout files, checkpoints, safe facts, and
//! metadata commit batches. They intentionally live outside the generic
//! canonical domain so source ownership remains explicit.

use std::path::Path;

use crate::domain::{DomainError, ResolvedThreadPatch};

fn invalid(field: &'static str, reason: impl Into<String>) -> DomainError {
    DomainError::InvalidValue {
        field,
        reason: reason.into(),
    }
}

fn non_empty(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        Err(invalid(field, "must not be empty"))
    } else if value.chars().any(char::is_control) {
        Err(invalid(field, "must not contain control characters"))
    } else {
        Ok(())
    }
}

fn non_negative(value: i64, field: &'static str) -> Result<(), DomainError> {
    if value < 0 {
        Err(invalid(field, "must be non-negative"))
    } else {
        Ok(())
    }
}

fn positive(value: i64, field: &'static str) -> Result<(), DomainError> {
    if value <= 0 {
        Err(invalid(field, "must be positive"))
    } else {
        Ok(())
    }
}

fn optional_non_negative(value: Option<i64>, field: &'static str) -> Result<(), DomainError> {
    if let Some(value) = value {
        non_negative(value, field)?;
    }
    Ok(())
}

fn safe_code(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.len() > 64 {
        return Err(invalid(field, "must be at most 64 bytes"));
    }
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_uppercase()) {
        return Err(invalid(field, "must start with an ASCII uppercase letter"));
    }
    if !bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_') {
        return Err(invalid(
            field,
            "must contain only ASCII uppercase letters, digits, or underscores",
        ));
    }
    Ok(())
}

fn absolute_path(value: &str, field: &'static str) -> Result<(), DomainError> {
    non_empty(value, field)?;
    if !Path::new(value).is_absolute() {
        return Err(invalid(field, "must be an absolute path"));
    }
    Ok(())
}

fn validate_optional_code(value: &Option<String>, field: &'static str) -> Result<(), DomainError> {
    if let Some(value) = value {
        safe_code(value, field)?;
    }
    Ok(())
}

macro_rules! impl_string_enum {
    ($type:ty, $field:literal, $( $variant:ident => $value:literal ),+ $(,)?) => {
        impl TryFrom<&str> for $type {
            type Error = DomainError;

            fn try_from(value: &str) -> Result<Self, DomainError> {
                match value {
                    $( $value => Ok(Self::$variant), )+
                    other => Err(invalid($field, format!("unknown value {other:?}"))),
                }
            }
        }
    };
}

#[cfg(not(windows))]
const INTERNAL_VALIDATION_PATH: &str = "/validated/by/storage";
#[cfg(windows)]
const INTERNAL_VALIDATION_PATH: &str = r"C:\validated\by\storage";

/// Physical area in which a rollout is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceArea {
    Sessions,
    ArchivedSessions,
}

impl SourceArea {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::ArchivedSessions => "archived_sessions",
        }
    }
}

impl_string_enum!(SourceArea, "source_area", Sessions => "sessions", ArchivedSessions => "archived_sessions");

/// Whether one rollout directory was completely enumerated for an
/// observation pass.  A complete area is the only evidence that absence means
/// a source became missing; an unavailable area leaves existing source state
/// untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceRegionStatus {
    Complete,
    Unavailable(String),
}

impl SourceRegionStatus {
    pub fn unavailable(error_code: impl Into<String>) -> Result<Self, DomainError> {
        let error_code = error_code.into();
        safe_code(&error_code, "region error code")?;
        Ok(Self::Unavailable(error_code))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if let Self::Unavailable(error_code) = self {
            safe_code(error_code, "region error code")?;
        }
        Ok(())
    }

    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Current observation of a physical rollout file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileStatus {
    Present,
    Missing,
    Replaced,
}

impl FileStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Replaced => "replaced",
        }
    }
}

impl_string_enum!(FileStatus, "file_status", Present => "present", Missing => "missing", Replaced => "replaced");

/// A scan consumer owns its own checkpoint stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConsumerKind {
    Metadata,
    Usage,
}

impl ConsumerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Usage => "usage",
        }
    }
}

impl_string_enum!(ConsumerKind, "consumer_kind", Metadata => "metadata", Usage => "usage");

/// Processing state for an individual consumer checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckpointProcessingStatus {
    Pending,
    Ready,
    RebuildRequired,
    Error,
}

impl CheckpointProcessingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::RebuildRequired => "rebuild_required",
            Self::Error => "error",
        }
    }
}

impl_string_enum!(
    CheckpointProcessingStatus,
    "processing_status",
    Pending => "pending",
    Ready => "ready",
    RebuildRequired => "rebuild_required",
    Error => "error"
);

/// `source_files.file_status` result represented by an observation outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuildDisposition {
    Unchanged,
    MemberAdded,
    CompletionInvalidated,
    CarryResumedPresent,
    Replaced,
}

impl BuildDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::MemberAdded => "member_added",
            Self::CompletionInvalidated => "completion_invalidated",
            Self::CarryResumedPresent => "carry_resumed_present",
            Self::Replaced => "replaced",
        }
    }
}

impl_string_enum!(
    BuildDisposition,
    "build_disposition",
    Unchanged => "unchanged",
    MemberAdded => "member_added",
    CompletionInvalidated => "completion_invalidated",
    CarryResumedPresent => "carry_resumed_present",
    Replaced => "replaced"
);

/// Safe fact continuation state. Confirmed owning identity may be resumed
/// either while still replaying an ancestor or after returning to owning data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContinuationState {
    ReplayedAncestor,
    OwningLive,
    Unstable,
}

impl ContinuationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReplayedAncestor => "replayed_ancestor",
            Self::OwningLive => "owning_live",
            Self::Unstable => "unstable",
        }
    }
}

impl_string_enum!(
    ContinuationState,
    "continuation_state",
    ReplayedAncestor => "replayed_ancestor",
    OwningLive => "owning_live",
    Unstable => "unstable"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnershipConfidence {
    Confirmed,
    Unresolved,
}

impl OwnershipConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unresolved => "unresolved",
        }
    }
}

impl_string_enum!(OwnershipConfidence, "ownership_confidence", Confirmed => "confirmed", Unresolved => "unresolved");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FactQualityStatus {
    Complete,
    Partial,
    Conflict,
}

impl FactQualityStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Conflict => "conflict",
        }
    }
}

impl_string_enum!(
    FactQualityStatus,
    "fact_quality_status",
    Complete => "complete",
    Partial => "partial",
    Conflict => "conflict"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CwdProvenance {
    SessionMeta,
    TurnContext,
}

impl CwdProvenance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionMeta => "session_meta",
            Self::TurnContext => "turn_context",
        }
    }
}

impl_string_enum!(CwdProvenance, "cwd_provenance", SessionMeta => "session_meta", TurnContext => "turn_context");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParentHintProvenance {
    SessionMetaParent,
    SubagentSource,
    ForkedFromId,
}

impl ParentHintProvenance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionMetaParent => "session_meta_parent",
            Self::SubagentSource => "subagent_source",
            Self::ForkedFromId => "forked_from_id",
        }
    }
}

impl_string_enum!(
    ParentHintProvenance,
    "parent_hint_provenance",
    SessionMetaParent => "session_meta_parent",
    SubagentSource => "subagent_source",
    ForkedFromId => "forked_from_id"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentRoleProvenance {
    SessionMetaRole,
    SubagentSource,
}

impl AgentRoleProvenance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionMetaRole => "session_meta_role",
            Self::SubagentSource => "subagent_source",
        }
    }
}

impl_string_enum!(
    AgentRoleProvenance,
    "agent_role_provenance",
    SessionMetaRole => "session_meta_role",
    SubagentSource => "subagent_source"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

impl_string_enum!(
    AgentPathProvenance,
    "agent_path_provenance",
    SessionMeta => "session_meta",
    ThreadSpawn => "thread_spawn"
);

/// Why a persisted source fact cannot safely be used by the resolver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafeFactMismatchReason {
    MissingCheckpoint,
    SourceMissing,
    GenerationMismatch,
    ParserVersionMismatch,
    OffsetMismatch,
    BindingMismatch,
    OwningThreadMismatch,
    ContinuationUnstable,
    InvalidFact,
}

/// Three-state safe-fact result returned by metadata scan-state loading.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "preserve the established public safe-fact ownership shape"
)]
pub enum SafeFactState {
    None,
    Matching(RolloutMetadataFact),
    Stale(SafeFactMismatchReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFileState {
    pub source_file_id: i64,
    pub thread_id: Option<String>,
    pub current_path: String,
    pub source_area: SourceArea,
    pub device_id: i64,
    pub inode: i64,
    pub file_generation: i64,
    pub observed_size: i64,
    pub observed_mtime_ns: i64,
    pub file_status: FileStatus,
    pub last_seen_at_ms: i64,
}

impl SourceFileState {
    #[expect(
        clippy::too_many_arguments,
        reason = "preserve the established public source-state constructor"
    )]
    pub fn new(
        source_file_id: i64,
        thread_id: Option<String>,
        current_path: impl Into<String>,
        source_area: SourceArea,
        device_id: i64,
        inode: i64,
        file_generation: i64,
        observed_size: i64,
        observed_mtime_ns: i64,
        file_status: FileStatus,
        last_seen_at_ms: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            source_file_id,
            thread_id,
            current_path: current_path.into(),
            source_area,
            device_id,
            inode,
            file_generation,
            observed_size,
            observed_mtime_ns,
            file_status,
            last_seen_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "preserve the established public source-state constructor"
    )]
    pub fn try_new(
        source_file_id: i64,
        thread_id: Option<String>,
        current_path: impl Into<String>,
        source_area: SourceArea,
        device_id: i64,
        inode: i64,
        file_generation: i64,
        observed_size: i64,
        observed_mtime_ns: i64,
        file_status: FileStatus,
        last_seen_at_ms: i64,
    ) -> Result<Self, DomainError> {
        Self::new(
            source_file_id,
            thread_id,
            current_path,
            source_area,
            device_id,
            inode,
            file_generation,
            observed_size,
            observed_mtime_ns,
            file_status,
            last_seen_at_ms,
        )
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.source_file_id, "source_file_id")?;
        if let Some(thread_id) = self.thread_id.as_deref() {
            non_empty(thread_id, "thread_id")?;
        }
        absolute_path(&self.current_path, "current_path")?;
        non_negative(self.device_id, "device_id")?;
        non_negative(self.inode, "inode")?;
        positive(self.file_generation, "file_generation")?;
        non_negative(self.observed_size, "observed_size")?;
        non_negative(self.observed_mtime_ns, "observed_mtime_ns")?;
        non_negative(self.last_seen_at_ms, "last_seen_at_ms")?;
        Ok(())
    }
}

/// One file metadata observation supplied by the scanner.  Generation and the
/// MU source id are assigned by storage from the physical identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceObservation {
    pub current_path: String,
    pub source_area: SourceArea,
    pub device_id: i64,
    pub inode: i64,
    pub observed_size: i64,
    pub observed_mtime_ns: i64,
    pub last_seen_at_ms: i64,
}

impl SourceObservation {
    pub fn new(
        current_path: impl Into<String>,
        source_area: SourceArea,
        device_id: i64,
        inode: i64,
        observed_size: i64,
        observed_mtime_ns: i64,
        last_seen_at_ms: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            current_path: current_path.into(),
            source_area,
            device_id,
            inode,
            observed_size,
            observed_mtime_ns,
            last_seen_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        absolute_path(&self.current_path, "current_path")?;
        non_negative(self.device_id, "device_id")?;
        non_negative(self.inode, "inode")?;
        non_negative(self.observed_size, "observed_size")?;
        non_negative(self.observed_mtime_ns, "observed_mtime_ns")?;
        non_negative(self.last_seen_at_ms, "last_seen_at_ms")?;
        Ok(())
    }

    pub fn try_new(
        current_path: impl Into<String>,
        source_area: SourceArea,
        device_id: i64,
        inode: i64,
        observed_size: i64,
        observed_mtime_ns: i64,
        last_seen_at_ms: i64,
    ) -> Result<Self, DomainError> {
        Self::new(
            current_path,
            source_area,
            device_id,
            inode,
            observed_size,
            observed_mtime_ns,
            last_seen_at_ms,
        )
    }
}

/// Scanner input for a physical-source observation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceObservationBatch {
    pub observations: Vec<SourceObservation>,
    pub sessions: SourceRegionStatus,
    pub archived_sessions: SourceRegionStatus,
}

impl SourceObservationBatch {
    pub fn new(
        observations: Vec<SourceObservation>,
        sessions: SourceRegionStatus,
        archived_sessions: SourceRegionStatus,
    ) -> Result<Self, DomainError> {
        let value = Self {
            observations,
            sessions,
            archived_sessions,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.sessions.validate()?;
        self.archived_sessions.validate()?;
        let mut paths = Vec::new();
        let mut identities = Vec::new();
        for observation in &self.observations {
            observation.validate()?;
            if paths.contains(&observation.current_path) {
                return Err(DomainError::DuplicateId {
                    kind: "source path",
                    id: observation.current_path.clone(),
                });
            }
            paths.push(observation.current_path.clone());
            let identity = (observation.device_id, observation.inode);
            if identities.contains(&identity) {
                return Err(DomainError::InvariantViolation {
                    invariant: "one observation batch cannot repeat a physical source identity",
                });
            }
            identities.push(identity);
        }
        Ok(())
    }
}

/// Result for one source observation.  The scanner uses this result rather
/// than re-identifying a source from its path after the write transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceObservationResult {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub created: bool,
    pub moved: bool,
    pub replaced: bool,
    pub rebuild_consumers: Vec<ConsumerKind>,
    pub build_disposition: BuildDisposition,
}

impl SourceObservationResult {
    pub fn new(
        source_file_id: i64,
        file_generation: i64,
        created: bool,
        moved: bool,
        replaced: bool,
        rebuild_consumers: Vec<ConsumerKind>,
        build_disposition: BuildDisposition,
    ) -> Result<Self, DomainError> {
        let value = Self {
            source_file_id,
            file_generation,
            created,
            moved,
            replaced,
            rebuild_consumers,
            build_disposition,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.source_file_id, "source_file_id")?;
        positive(self.file_generation, "file_generation")?;
        let mut seen = Vec::new();
        for consumer in &self.rebuild_consumers {
            if seen.contains(consumer) {
                return Err(DomainError::InvariantViolation {
                    invariant: "rebuild consumers must be unique",
                });
            }
            seen.push(*consumer);
        }
        Ok(())
    }

    pub const fn current_generation(&self) -> i64 {
        self.file_generation
    }

    pub const fn is_new(&self) -> bool {
        self.created
    }

    pub const fn generation_changed(&self) -> bool {
        self.replaced
    }
}

/// Batch result from `record_source_observations`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceOutcome {
    pub results: Vec<SourceObservationResult>,
}

impl SourceOutcome {
    pub fn new(results: Vec<SourceObservationResult>) -> Result<Self, DomainError> {
        let value = Self { results };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let mut seen = Vec::new();
        for result in &self.results {
            result.validate()?;
            if seen.contains(&result.source_file_id) {
                return Err(DomainError::DuplicateId {
                    kind: "source outcome",
                    id: result.source_file_id.to_string(),
                });
            }
            seen.push(result.source_file_id);
        }
        Ok(())
    }

    pub fn try_new(results: Vec<SourceObservationResult>) -> Result<Self, DomainError> {
        Self::new(results)
    }
}

/// Persisted checkpoint for a single source and consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCheckpoint {
    pub source_file_id: i64,
    pub consumer_kind: ConsumerKind,
    pub parser_version: i64,
    pub committed_offset: i64,
    pub guard_hash: Option<Vec<u8>>,
    pub processing_status: CheckpointProcessingStatus,
    pub last_successful_scan_at_ms: Option<i64>,
    pub last_error_code: Option<String>,
}

impl SourceCheckpoint {
    #[expect(
        clippy::too_many_arguments,
        reason = "preserve the established public checkpoint constructor"
    )]
    pub fn new(
        source_file_id: i64,
        consumer_kind: ConsumerKind,
        parser_version: i64,
        committed_offset: i64,
        guard_hash: Option<Vec<u8>>,
        processing_status: CheckpointProcessingStatus,
        last_successful_scan_at_ms: Option<i64>,
        last_error_code: Option<String>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            source_file_id,
            consumer_kind,
            parser_version,
            committed_offset,
            guard_hash,
            processing_status,
            last_successful_scan_at_ms,
            last_error_code,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.source_file_id, "source_file_id")?;
        non_negative(self.parser_version, "parser_version")?;
        non_negative(self.committed_offset, "committed_offset")?;
        optional_non_negative(
            self.last_successful_scan_at_ms,
            "last_successful_scan_at_ms",
        )?;
        if self.committed_offset == 0 && self.guard_hash.is_some() {
            return Err(DomainError::InvariantViolation {
                invariant: "zero checkpoint offset must have a null guard hash",
            });
        }
        if self.committed_offset > 0 && self.guard_hash.as_ref().is_none_or(Vec::is_empty) {
            return Err(DomainError::InvariantViolation {
                invariant: "non-zero checkpoint offset must have a guard hash",
            });
        }
        validate_optional_code(&self.last_error_code, "last_error_code")?;
        Ok(())
    }

    pub fn validate_against(&self, source: &SourceFileState) -> Result<(), DomainError> {
        self.validate()?;
        source.validate()?;
        if self.source_file_id != source.source_file_id {
            return Err(DomainError::InvariantViolation {
                invariant: "checkpoint source id must match source file id",
            });
        }
        if self.committed_offset > source.observed_size {
            return Err(DomainError::InvariantViolation {
                invariant: "checkpoint offset must not exceed observed source size",
            });
        }
        Ok(())
    }
}

/// Read projection used by `MetadataScanState`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataCheckpointState {
    pub source_file_id: i64,
    pub parser_version: i64,
    pub committed_offset: i64,
    pub guard_hash: Option<Vec<u8>>,
    pub processing_status: CheckpointProcessingStatus,
    pub last_successful_scan_at_ms: Option<i64>,
    pub last_error_code: Option<String>,
}

impl MetadataCheckpointState {
    pub fn new(
        source_file_id: i64,
        parser_version: i64,
        committed_offset: i64,
        guard_hash: Option<Vec<u8>>,
        processing_status: CheckpointProcessingStatus,
        last_successful_scan_at_ms: Option<i64>,
        last_error_code: Option<String>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            source_file_id,
            parser_version,
            committed_offset,
            guard_hash,
            processing_status,
            last_successful_scan_at_ms,
            last_error_code,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        SourceCheckpoint {
            source_file_id: self.source_file_id,
            consumer_kind: ConsumerKind::Metadata,
            parser_version: self.parser_version,
            committed_offset: self.committed_offset,
            guard_hash: self.guard_hash.clone(),
            processing_status: self.processing_status,
            last_successful_scan_at_ms: self.last_successful_scan_at_ms,
            last_error_code: self.last_error_code.clone(),
        }
        .validate()
    }

    pub fn validate_against(&self, source: &SourceFileState) -> Result<(), DomainError> {
        self.validate()?;
        if self.source_file_id != source.source_file_id {
            return Err(DomainError::InvariantViolation {
                invariant: "metadata checkpoint source id must match source file id",
            });
        }
        if self.committed_offset > source.observed_size {
            return Err(DomainError::InvariantViolation {
                invariant: "metadata checkpoint offset must not exceed observed source size",
            });
        }
        Ok(())
    }
}

impl From<SourceCheckpoint> for MetadataCheckpointState {
    fn from(checkpoint: SourceCheckpoint) -> Self {
        Self {
            source_file_id: checkpoint.source_file_id,
            parser_version: checkpoint.parser_version,
            committed_offset: checkpoint.committed_offset,
            guard_hash: checkpoint.guard_hash,
            processing_status: checkpoint.processing_status,
            last_successful_scan_at_ms: checkpoint.last_successful_scan_at_ms,
            last_error_code: checkpoint.last_error_code,
        }
    }
}

/// Checkpoint advance carried by a metadata commit.  The source id is owned by
/// its containing `MetadataSourceCommit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataCheckpointAdvance {
    pub parser_version: i64,
    pub committed_offset: i64,
    pub guard_hash: Option<Vec<u8>>,
    pub processing_status: CheckpointProcessingStatus,
    pub last_successful_scan_at_ms: Option<i64>,
    pub last_error_code: Option<String>,
}

impl MetadataCheckpointAdvance {
    pub fn new(
        parser_version: i64,
        committed_offset: i64,
        guard_hash: Option<Vec<u8>>,
        processing_status: CheckpointProcessingStatus,
    ) -> Result<Self, DomainError> {
        let value = Self {
            parser_version,
            committed_offset,
            guard_hash,
            processing_status,
            last_successful_scan_at_ms: None,
            last_error_code: None,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_negative(self.parser_version, "parser_version")?;
        non_negative(self.committed_offset, "committed_offset")?;
        if self.committed_offset == 0 && self.guard_hash.is_some() {
            return Err(DomainError::InvariantViolation {
                invariant: "zero checkpoint offset must have a null guard hash",
            });
        }
        if self.committed_offset > 0 && self.guard_hash.as_ref().is_none_or(Vec::is_empty) {
            return Err(DomainError::InvariantViolation {
                invariant: "non-zero checkpoint offset must have a guard hash",
            });
        }
        optional_non_negative(
            self.last_successful_scan_at_ms,
            "last_successful_scan_at_ms",
        )?;
        validate_optional_code(&self.last_error_code, "last_error_code")?;
        Ok(())
    }

    pub fn validate_against(
        &self,
        source_file_id: i64,
        source: &SourceFileState,
    ) -> Result<(), DomainError> {
        self.validate()?;
        positive(source_file_id, "source_file_id")?;
        source.validate()?;
        if source.source_file_id != source_file_id {
            return Err(DomainError::InvariantViolation {
                invariant: "metadata checkpoint source id must match source file id",
            });
        }
        if self.committed_offset > source.observed_size {
            return Err(DomainError::InvariantViolation {
                invariant: "metadata checkpoint offset must not exceed observed source size",
            });
        }
        Ok(())
    }
}

/// Minimal structured metadata fact retained for one rollout source.  This
/// intentionally contains no title, message, or raw JSON payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutMetadataFact {
    pub source_file_id: i64,
    pub file_generation: i64,
    pub metadata_parser_version: i64,
    pub resolved_through_offset: i64,
    pub owning_thread_id: String,
    pub continuation_state: ContinuationState,
    pub cwd: Option<String>,
    pub cwd_provenance: Option<CwdProvenance>,
    pub cwd_record_offset: Option<i64>,
    pub created_at_ms: Option<i64>,
    pub latest_context_model: Option<String>,
    pub latest_context_turn_id: Option<String>,
    pub latest_context_at_ms: Option<i64>,
    pub parent_thread_id_hint: Option<String>,
    pub parent_hint_provenance: Option<ParentHintProvenance>,
    pub parent_hint_record_offset: Option<i64>,
    pub agent_role_hint: Option<String>,
    pub agent_role_provenance: Option<AgentRoleProvenance>,
    pub agent_role_record_offset: Option<i64>,
    pub agent_path: Option<String>,
    pub agent_path_provenance: Option<AgentPathProvenance>,
    pub agent_path_record_offset: Option<i64>,
    pub replay_start_offset: Option<i64>,
    pub owning_records_start_offset: Option<i64>,
    pub ownership_confidence: OwnershipConfidence,
    pub fact_quality_status: FactQualityStatus,
    pub relationship_conflict: bool,
    pub updated_at_ms: i64,
}

impl RolloutMetadataFact {
    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.source_file_id, "source_file_id")?;
        positive(self.file_generation, "file_generation")?;
        non_negative(self.metadata_parser_version, "metadata_parser_version")?;
        non_negative(self.resolved_through_offset, "resolved_through_offset")?;
        non_empty(&self.owning_thread_id, "owning_thread_id")?;
        optional_non_negative(self.cwd_record_offset, "cwd_record_offset")?;
        optional_non_negative(self.created_at_ms, "created_at_ms")?;
        optional_non_negative(self.latest_context_at_ms, "latest_context_at_ms")?;
        optional_non_negative(self.parent_hint_record_offset, "parent_hint_record_offset")?;
        optional_non_negative(self.agent_role_record_offset, "agent_role_record_offset")?;
        optional_non_negative(self.agent_path_record_offset, "agent_path_record_offset")?;
        optional_non_negative(self.replay_start_offset, "replay_start_offset")?;
        optional_non_negative(
            self.owning_records_start_offset,
            "owning_records_start_offset",
        )?;
        non_negative(self.updated_at_ms, "updated_at_ms")?;

        validate_provenance(
            self.cwd.as_deref(),
            self.cwd_provenance.is_some(),
            self.cwd_record_offset,
            "cwd",
        )?;
        validate_provenance(
            self.parent_thread_id_hint.as_deref(),
            self.parent_hint_provenance.is_some(),
            self.parent_hint_record_offset,
            "parent_thread_id_hint",
        )?;
        validate_provenance(
            self.agent_role_hint.as_deref(),
            self.agent_role_provenance.is_some(),
            self.agent_role_record_offset,
            "agent_role_hint",
        )?;
        validate_provenance(
            self.agent_path.as_deref(),
            self.agent_path_provenance.is_some(),
            self.agent_path_record_offset,
            "agent_path",
        )?;
        if let Some(cwd) = self.cwd.as_deref() {
            non_empty(cwd, "cwd")?;
        }
        if let Some(model) = self.latest_context_model.as_deref() {
            non_empty(model, "latest_context_model")?;
        }
        if let Some(turn_id) = self.latest_context_turn_id.as_deref() {
            non_empty(turn_id, "latest_context_turn_id")?;
        }
        if let Some(parent) = self.parent_thread_id_hint.as_deref() {
            non_empty(parent, "parent_thread_id_hint")?;
        }
        if let Some(role) = self.agent_role_hint.as_deref() {
            non_empty(role, "agent_role_hint")?;
        }
        if let Some(agent_path) = self.agent_path.as_deref() {
            non_empty(agent_path, "agent_path")?;
        }
        if self.relationship_conflict && self.fact_quality_status != FactQualityStatus::Conflict {
            return Err(DomainError::InvariantViolation {
                invariant: "relationship conflict requires conflict fact quality",
            });
        }
        if matches!(
            self.continuation_state,
            ContinuationState::ReplayedAncestor | ContinuationState::OwningLive
        ) && self.ownership_confidence != OwnershipConfidence::Confirmed
        {
            return Err(DomainError::InvariantViolation {
                invariant: "resumable fact must have confirmed ownership",
            });
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        source: &SourceFileState,
        checkpoint: &MetadataCheckpointState,
    ) -> Result<(), DomainError> {
        self.validate()?;
        source.validate()?;
        checkpoint.validate_against(source)?;
        if self.source_file_id != source.source_file_id
            || self.file_generation != source.file_generation
            || self.metadata_parser_version != checkpoint.parser_version
            || self.resolved_through_offset != checkpoint.committed_offset
            || checkpoint.processing_status == CheckpointProcessingStatus::RebuildRequired
        {
            return Err(DomainError::InvariantViolation {
                invariant: "safe fact does not match source and metadata checkpoint",
            });
        }
        if source.thread_id.as_deref() != Some(self.owning_thread_id.as_str()) {
            return Err(DomainError::InvariantViolation {
                invariant: "safe fact owning thread must match source binding",
            });
        }
        Ok(())
    }
}

fn validate_provenance(
    value: Option<&str>,
    provenance_present: bool,
    record_offset: Option<i64>,
    field: &'static str,
) -> Result<(), DomainError> {
    let complete = value.is_some() && provenance_present && record_offset.is_some();
    let empty = value.is_none() && !provenance_present && record_offset.is_none();
    if !(complete || empty) {
        return Err(DomainError::InvariantViolation {
            invariant: match field {
                "cwd" => "cwd value, provenance and record offset must be all present or absent",
                "parent_thread_id_hint" => {
                    "parent hint value, provenance and record offset must be all present or absent"
                }
                _ => {
                    if field == "agent_path" {
                        "agent path value, provenance and record offset must be all present or absent"
                    } else {
                        "agent role hint value, provenance and record offset must be all present or absent"
                    }
                }
            },
        });
    }
    Ok(())
}

/// One item in the batch metadata scan-state read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataScanStateEntry {
    pub source: SourceFileState,
    pub metadata_checkpoint: Option<MetadataCheckpointState>,
    pub safe_fact: SafeFactState,
}

impl MetadataScanStateEntry {
    pub fn new(
        source: SourceFileState,
        metadata_checkpoint: Option<MetadataCheckpointState>,
        fact: Option<RolloutMetadataFact>,
    ) -> Result<Self, DomainError> {
        source.validate()?;
        if let Some(checkpoint) = metadata_checkpoint.as_ref() {
            checkpoint.validate_against(&source)?;
        }
        let safe_fact = match fact {
            None => SafeFactState::None,
            Some(fact) => match metadata_checkpoint.as_ref() {
                None => SafeFactState::Stale(SafeFactMismatchReason::MissingCheckpoint),
                Some(checkpoint) => match fact.validate_against(&source, checkpoint) {
                    Ok(()) => SafeFactState::Matching(fact),
                    Err(error) => SafeFactState::Stale(mismatch_reason(&error)),
                },
            },
        };
        Ok(Self {
            source,
            metadata_checkpoint,
            safe_fact,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.source.validate()?;
        if let Some(checkpoint) = self.metadata_checkpoint.as_ref() {
            checkpoint.validate_against(&self.source)?;
        }
        if let SafeFactState::Matching(fact) = &self.safe_fact {
            let checkpoint =
                self.metadata_checkpoint
                    .as_ref()
                    .ok_or(DomainError::InvariantViolation {
                        invariant: "matching safe fact requires metadata checkpoint",
                    })?;
            fact.validate_against(&self.source, checkpoint)?;
        }
        Ok(())
    }

    pub fn try_new(
        source: SourceFileState,
        metadata_checkpoint: Option<MetadataCheckpointState>,
        fact: Option<RolloutMetadataFact>,
    ) -> Result<Self, DomainError> {
        Self::new(source, metadata_checkpoint, fact)
    }
}

fn mismatch_reason(error: &DomainError) -> SafeFactMismatchReason {
    match error {
        DomainError::InvariantViolation { invariant } if invariant.contains("generation") => {
            SafeFactMismatchReason::GenerationMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("parser") => {
            SafeFactMismatchReason::ParserVersionMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("offset") => {
            SafeFactMismatchReason::OffsetMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("binding") => {
            SafeFactMismatchReason::BindingMismatch
        }
        DomainError::InvariantViolation { invariant } if invariant.contains("owning thread") => {
            SafeFactMismatchReason::OwningThreadMismatch
        }
        _ => SafeFactMismatchReason::InvalidFact,
    }
}

/// Batch projection returned by `load_metadata_scan_state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataScanState {
    pub entries: Vec<MetadataScanStateEntry>,
}

impl MetadataScanState {
    pub fn new(entries: Vec<MetadataScanStateEntry>) -> Result<Self, DomainError> {
        let value = Self { entries };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let mut seen = Vec::new();
        for entry in &self.entries {
            entry.validate()?;
            let id = entry.source.source_file_id;
            if seen.contains(&id) {
                return Err(DomainError::DuplicateId {
                    kind: "metadata scan state",
                    id: id.to_string(),
                });
            }
            seen.push(id);
        }
        Ok(())
    }

    pub fn try_new(entries: Vec<MetadataScanStateEntry>) -> Result<Self, DomainError> {
        Self::new(entries)
    }

    pub fn get(&self, source_file_id: i64) -> Option<&MetadataScanStateEntry> {
        self.entries
            .iter()
            .find(|entry| entry.source.source_file_id == source_file_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataSourceCommit {
    pub source_file_id: i64,
    pub expected_file_generation: i64,
    pub expected_previous_thread_id: Option<String>,
    pub confirmed_owning_thread_id: String,
    pub safe_fact: RolloutMetadataFact,
    pub metadata_checkpoint_advance: MetadataCheckpointAdvance,
}

impl MetadataSourceCommit {
    pub fn new(
        source_file_id: i64,
        expected_file_generation: i64,
        expected_previous_thread_id: Option<String>,
        confirmed_owning_thread_id: impl Into<String>,
        safe_fact: RolloutMetadataFact,
        metadata_checkpoint_advance: MetadataCheckpointAdvance,
    ) -> Result<Self, DomainError> {
        let value = Self {
            source_file_id,
            expected_file_generation,
            expected_previous_thread_id,
            confirmed_owning_thread_id: confirmed_owning_thread_id.into(),
            safe_fact,
            metadata_checkpoint_advance,
        };
        value.validate_for_thread(&value.confirmed_owning_thread_id.clone())?;
        Ok(value)
    }

    pub fn validate_for_thread(&self, thread_id: &str) -> Result<(), DomainError> {
        positive(self.source_file_id, "source_file_id")?;
        positive(self.expected_file_generation, "expected_file_generation")?;
        non_empty(thread_id, "thread_id")?;
        non_empty(
            &self.confirmed_owning_thread_id,
            "confirmed_owning_thread_id",
        )?;
        if let Some(previous) = self.expected_previous_thread_id.as_deref() {
            non_empty(previous, "expected_previous_thread_id")?;
            if previous != self.confirmed_owning_thread_id {
                return Err(DomainError::InvariantViolation {
                    invariant: "non-null expected previous thread id must equal confirmed owning id",
                });
            }
        }
        if self.confirmed_owning_thread_id != thread_id {
            return Err(DomainError::InvariantViolation {
                invariant: "metadata source owning id must equal metadata thread group id",
            });
        }
        self.safe_fact.validate()?;
        if self.safe_fact.source_file_id != self.source_file_id
            || self.safe_fact.file_generation != self.expected_file_generation
            || self.safe_fact.owning_thread_id != self.confirmed_owning_thread_id
        {
            return Err(DomainError::InvariantViolation {
                invariant: "safe fact identity must match metadata source commit",
            });
        }
        self.metadata_checkpoint_advance.validate_against(
            self.source_file_id,
            &SourceFileState {
                source_file_id: self.source_file_id,
                thread_id: Some(self.confirmed_owning_thread_id.clone()),
                current_path: INTERNAL_VALIDATION_PATH.to_owned(),
                source_area: SourceArea::Sessions,
                device_id: 0,
                inode: 0,
                file_generation: self.expected_file_generation,
                observed_size: self.metadata_checkpoint_advance.committed_offset,
                observed_mtime_ns: 0,
                file_status: FileStatus::Present,
                last_seen_at_ms: self
                    .metadata_checkpoint_advance
                    .last_successful_scan_at_ms
                    .unwrap_or(0),
            },
        )?;
        Ok(())
    }
}

/// A resolver output group.  A patch-only group has no source commits, but an
/// entirely empty group is invalid because it could hide a missed checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataThreadCommit {
    pub thread_id: String,
    pub resolved_patch: Option<ResolvedThreadPatch>,
    pub sources: Vec<MetadataSourceCommit>,
}

impl MetadataThreadCommit {
    pub fn new(
        thread_id: impl Into<String>,
        resolved_patch: Option<ResolvedThreadPatch>,
        sources: Vec<MetadataSourceCommit>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            thread_id: thread_id.into(),
            resolved_patch,
            sources,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.thread_id, "thread_id")?;
        if self.resolved_patch.is_none() && self.sources.is_empty() {
            return Err(DomainError::InvariantViolation {
                invariant: "resolved_patch=None with no source commits is an invalid empty group",
            });
        }
        if let Some(patch) = self.resolved_patch.as_ref() {
            patch.validate()?;
            if patch.thread_id != self.thread_id {
                return Err(DomainError::InvariantViolation {
                    invariant: "resolved patch thread id must match metadata group id",
                });
            }
        }
        let mut seen = Vec::new();
        for source in &self.sources {
            source.validate_for_thread(&self.thread_id)?;
            if seen.contains(&source.source_file_id) {
                return Err(DomainError::DuplicateId {
                    kind: "metadata source",
                    id: source.source_file_id.to_string(),
                });
            }
            seen.push(source.source_file_id);
        }
        Ok(())
    }

    pub fn try_new(
        thread_id: impl Into<String>,
        resolved_patch: Option<ResolvedThreadPatch>,
        sources: Vec<MetadataSourceCommit>,
    ) -> Result<Self, DomainError> {
        Self::new(thread_id, resolved_patch, sources)
    }
}

/// Atomic batch accepted by `commit_metadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataCommitBatch {
    pub groups: Vec<MetadataThreadCommit>,
}

impl MetadataCommitBatch {
    pub fn new(groups: Vec<MetadataThreadCommit>) -> Result<Self, DomainError> {
        if groups.is_empty() {
            return Err(DomainError::EmptyBatch {
                kind: "metadata commit",
            });
        }
        let value = Self { groups };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.groups.is_empty() {
            return Err(DomainError::EmptyBatch {
                kind: "metadata commit",
            });
        }
        let mut seen = Vec::new();
        for group in &self.groups {
            group.validate()?;
            if seen.contains(&group.thread_id) {
                return Err(DomainError::DuplicateId {
                    kind: "metadata thread",
                    id: group.thread_id.clone(),
                });
            }
            seen.push(group.thread_id.clone());
        }
        Ok(())
    }

    pub fn try_new(groups: Vec<MetadataThreadCommit>) -> Result<Self, DomainError> {
        Self::new(groups)
    }
}

/// Result of a successful metadata commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    pub committed_group_count: usize,
    pub data_revision: i64,
    pub data_changed: bool,
}

impl CommitOutcome {
    pub fn new(
        committed_group_count: usize,
        data_revision: i64,
        data_changed: bool,
    ) -> Result<Self, DomainError> {
        if committed_group_count == 0 {
            return Err(DomainError::EmptyBatch {
                kind: "committed metadata groups",
            });
        }
        non_negative(data_revision, "data_revision")?;
        Ok(Self {
            committed_group_count,
            data_revision,
            data_changed,
        })
    }
}

/// Result of marking one consumer's checkpoints for rebuild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointOutcome {
    pub consumer_kind: ConsumerKind,
    pub source_file_ids: Vec<i64>,
}

impl CheckpointOutcome {
    pub fn new(
        consumer_kind: ConsumerKind,
        source_file_ids: Vec<i64>,
    ) -> Result<Self, DomainError> {
        if source_file_ids.is_empty() {
            return Err(DomainError::EmptyBatch {
                kind: "checkpoint rebuild sources",
            });
        }
        let value = Self {
            consumer_kind,
            source_file_ids,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.source_file_ids.is_empty() {
            return Err(DomainError::EmptyBatch {
                kind: "checkpoint rebuild sources",
            });
        }
        let mut seen = Vec::new();
        for id in &self.source_file_ids {
            positive(*id, "source_file_id")?;
            if seen.contains(id) {
                return Err(DomainError::DuplicateId {
                    kind: "checkpoint source",
                    id: id.to_string(),
                });
            }
            seen.push(*id);
        }
        Ok(())
    }
}

/// Command accepted by `require_checkpoint_rebuild`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointRebuildCommand {
    pub consumer_kind: ConsumerKind,
    pub source_file_ids: Vec<i64>,
}

impl CheckpointRebuildCommand {
    pub fn new(
        consumer_kind: ConsumerKind,
        source_file_ids: Vec<i64>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            consumer_kind,
            source_file_ids,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        CheckpointOutcome {
            consumer_kind: self.consumer_kind,
            source_file_ids: self.source_file_ids.clone(),
        }
        .validate()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FollowupStartFailedEvent, ScanFailedEvent};

    fn source() -> SourceFileState {
        SourceFileState::new(
            1,
            Some("thread".to_string()),
            std::env::temp_dir()
                .join("rollout.jsonl")
                .to_string_lossy()
                .into_owned(),
            SourceArea::Sessions,
            1,
            2,
            1,
            100,
            0,
            FileStatus::Present,
            10,
        )
        .unwrap()
    }

    #[test]
    fn source_and_checkpoint_reject_invalid_boundaries() {
        assert!(
            SourceObservation::new("relative.jsonl", SourceArea::Sessions, 1, 1, 0, 0, 0).is_err()
        );
        let source = source();
        let checkpoint = SourceCheckpoint::new(
            1,
            ConsumerKind::Metadata,
            1,
            101,
            Some(vec![1]),
            CheckpointProcessingStatus::Ready,
            Some(10),
            None,
        )
        .unwrap();
        assert!(checkpoint.validate_against(&source).is_err());
    }

    #[test]
    fn safe_fact_requires_all_provenance_columns() {
        let fact = RolloutMetadataFact {
            source_file_id: 1,
            file_generation: 1,
            metadata_parser_version: 1,
            resolved_through_offset: 0,
            owning_thread_id: "thread".to_string(),
            continuation_state: ContinuationState::Unstable,
            cwd: Some("/tmp".to_string()),
            cwd_provenance: Some(CwdProvenance::SessionMeta),
            cwd_record_offset: None,
            created_at_ms: None,
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            parent_thread_id_hint: None,
            parent_hint_provenance: None,
            parent_hint_record_offset: None,
            agent_role_hint: None,
            agent_role_provenance: None,
            agent_role_record_offset: None,
            agent_path: None,
            agent_path_provenance: None,
            agent_path_record_offset: None,
            replay_start_offset: None,
            owning_records_start_offset: None,
            ownership_confidence: OwnershipConfidence::Unresolved,
            fact_quality_status: FactQualityStatus::Partial,
            relationship_conflict: false,
            updated_at_ms: 1,
        };
        assert!(fact.validate().is_err());
    }

    #[test]
    fn agent_path_safe_fact_requires_complete_non_negative_provenance() {
        let valid = RolloutMetadataFact {
            source_file_id: 1,
            file_generation: 1,
            metadata_parser_version: 1,
            resolved_through_offset: 4,
            owning_thread_id: "thread".to_owned(),
            continuation_state: ContinuationState::Unstable,
            cwd: None,
            cwd_provenance: None,
            cwd_record_offset: None,
            created_at_ms: None,
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            parent_thread_id_hint: None,
            parent_hint_provenance: None,
            parent_hint_record_offset: None,
            agent_role_hint: None,
            agent_role_provenance: None,
            agent_role_record_offset: None,
            agent_path: Some("/root/task".to_owned()),
            agent_path_provenance: Some(AgentPathProvenance::SessionMeta),
            agent_path_record_offset: Some(4),
            replay_start_offset: None,
            owning_records_start_offset: None,
            ownership_confidence: OwnershipConfidence::Unresolved,
            fact_quality_status: FactQualityStatus::Partial,
            relationship_conflict: false,
            updated_at_ms: 1,
        };
        valid.validate().unwrap();

        let mut incomplete = valid.clone();
        incomplete.agent_path_record_offset = None;
        assert!(incomplete.validate().is_err());

        let mut negative = valid;
        negative.agent_path_record_offset = Some(-1);
        assert!(negative.validate().is_err());
    }

    #[test]
    fn metadata_group_rejects_empty_patch_only_group() {
        assert!(MetadataThreadCommit::new("thread", None, Vec::new()).is_err());
    }

    #[test]
    fn source_observation_rejects_negative_file_times() {
        assert!(
            SourceObservation::new("/tmp/rollout.jsonl", SourceArea::Sessions, 1, 1, 0, -1, 0)
                .is_err()
        );
    }

    #[test]
    fn source_observation_batch_requires_both_region_proofs() {
        let batch = SourceObservationBatch::new(
            Vec::new(),
            SourceRegionStatus::Complete,
            SourceRegionStatus::Unavailable("PERMISSION_DENIED".to_owned()),
        )
        .unwrap();
        assert!(batch.sessions.is_complete());
        assert!(!batch.archived_sessions.is_complete());
        assert!(SourceRegionStatus::unavailable("").is_err());
        assert!(SourceRegionStatus::unavailable("contains\nnewline").is_err());
    }

    #[test]
    fn replacement_can_report_unchanged_when_usage_build_is_absent() {
        SourceObservationResult::new(
            1,
            2,
            false,
            false,
            true,
            vec![ConsumerKind::Metadata],
            BuildDisposition::Unchanged,
        )
        .unwrap();
    }

    #[test]
    fn persisted_error_codes_reject_text_and_privacy_sentinels() {
        for invalid in [
            "lowercase",
            "1_STARTS_WITH_DIGIT",
            "PROMPT CONTENT",
            "SECRET-PAYLOAD",
            "包含正文",
        ] {
            assert!(ScanFailedEvent::new("scan", 1, invalid).is_err());
            assert!(FollowupStartFailedEvent::new("scan", 1, invalid).is_err());
            assert!(SourceRegionStatus::unavailable(invalid).is_err());
        }
        assert!(ScanFailedEvent::new("scan", 1, format!("A{}", "B".repeat(64))).is_err());
        assert!(ScanFailedEvent::new("scan", 1, "SCAN_FAILED_2").is_ok());

        assert!(
            SourceCheckpoint::new(
                1,
                ConsumerKind::Metadata,
                0,
                0,
                None,
                CheckpointProcessingStatus::Error,
                None,
                Some("private response body".to_owned()),
            )
            .is_err()
        );
        let mut advance =
            MetadataCheckpointAdvance::new(0, 0, None, CheckpointProcessingStatus::Error).unwrap();
        advance.last_error_code = Some("TOOL_OUTPUT!".to_owned());
        assert!(advance.validate().is_err());
    }

    #[test]
    fn metadata_checkpoint_advance_checks_source_id_argument() {
        let advance =
            MetadataCheckpointAdvance::new(0, 0, None, CheckpointProcessingStatus::Pending)
                .unwrap();
        assert!(advance.validate_against(2, &source()).is_err());
        advance.validate_against(1, &source()).unwrap();
    }

    #[test]
    fn internal_metadata_validation_path_is_platform_absolute() {
        assert!(Path::new(INTERNAL_VALIDATION_PATH).is_absolute());
        #[cfg(not(windows))]
        assert_eq!(INTERNAL_VALIDATION_PATH, "/validated/by/storage");
        #[cfg(windows)]
        assert_eq!(INTERNAL_VALIDATION_PATH, r"C:\validated\by\storage");
    }
}
