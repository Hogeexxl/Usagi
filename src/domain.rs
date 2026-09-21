//! Domain values shared by the persistence and scanner layers.
//!
//! This module intentionally contains no SQLite or parser code.  The values here
//! are the small, structured commands and projections exchanged with storage.
//! In particular, no rollout body, prompt, response, tool payload, or Token
//! event is represented by Spec 01.

use std::fmt;
use std::ops::Deref;
use std::path::Path;

/// Error returned when a domain value would violate a Spec 01 invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    InvalidValue { field: &'static str, reason: String },
    InvariantViolation { invariant: &'static str },
    EmptyBatch { kind: &'static str },
    DuplicateId { kind: &'static str, id: String },
}

impl DomainError {
    fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidValue {
            field,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, reason } => write!(f, "invalid {field}: {reason}"),
            Self::InvariantViolation { invariant } => write!(f, "invariant violated: {invariant}"),
            Self::EmptyBatch { kind } => write!(f, "{kind} must not be empty"),
            Self::DuplicateId { kind, id } => write!(f, "duplicate {kind} id: {id}"),
        }
    }
}

impl std::error::Error for DomainError {}

fn non_empty(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        Err(DomainError::invalid(field, "must not be empty"))
    } else if value.chars().any(char::is_control) {
        Err(DomainError::invalid(
            field,
            "must not contain control characters",
        ))
    } else {
        Ok(())
    }
}

/// Canonical identity for one source-native session.
///
/// `thread_id` is opaque.  In particular, callers must use the explicit
/// `source` and `native_session_id` fields instead of parsing a namespace out
/// of the canonical id.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SessionIdentity {
    pub thread_id: String,
    pub source: crate::source::SourceId,
    pub native_session_id: String,
}

impl SessionIdentity {
    pub fn new(
        thread_id: impl Into<String>,
        source: crate::source::SourceId,
        native_session_id: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let identity = Self {
            thread_id: thread_id.into(),
            source,
            native_session_id: native_session_id.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Give a source a collision-resistant namespaced canonical id while
    /// retaining its native id as a separate opaque field.
    pub fn namespaced(
        source: impl Into<crate::source::SourceId>,
        native_session_id: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let source = source.into();
        let native_session_id = native_session_id.into();
        let thread_id = format!("{source}:{native_session_id}");
        Self::new(thread_id, source, native_session_id)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.thread_id, "thread_id")?;
        self.source
            .validate()
            .map_err(|error| DomainError::InvalidValue {
                field: "source",
                reason: error.to_string(),
            })?;
        non_empty(&self.native_session_id, "native_session_id")?;
        Ok(())
    }
}

fn non_negative(value: i64, field: &'static str) -> Result<(), DomainError> {
    if value < 0 {
        Err(DomainError::invalid(field, "must be non-negative"))
    } else {
        Ok(())
    }
}

fn positive(value: i64, field: &'static str) -> Result<(), DomainError> {
    if value <= 0 {
        Err(DomainError::invalid(field, "must be positive"))
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
        return Err(DomainError::invalid(field, "must not be empty"));
    }
    if value.len() > 64 {
        return Err(DomainError::invalid(field, "must be at most 64 bytes"));
    }
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_uppercase()) {
        return Err(DomainError::invalid(
            field,
            "must start with an ASCII uppercase letter",
        ));
    }
    if !bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_') {
        return Err(DomainError::invalid(
            field,
            "must contain only ASCII uppercase letters, digits, or underscores",
        ));
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
                    other => Err(DomainError::invalid(
                        $field,
                        format!("unknown value {other:?}"),
                    )),
                }
            }
        }
    };
}

fn absolute_path(value: &str, field: &'static str) -> Result<(), DomainError> {
    non_empty(value, field)?;
    if !Path::new(value).is_absolute() {
        return Err(DomainError::invalid(field, "must be an absolute path"));
    }
    Ok(())
}

#[cfg(windows)]
const INTERNAL_VALIDATION_PATH: &str = r"C:\validated\by\storage";
#[cfg(not(windows))]
const INTERNAL_VALIDATION_PATH: &str = "/validated/by/storage";

/// Source-scoped usage epoch state.  The source is part of the value so an
/// epoch can never be mistaken for a global value or applied to another
/// source's canonical data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceUsageEpochState {
    pub source: crate::source::SourceId,
    pub active_epoch: i64,
    pub build_epoch: Option<i64>,
    pub active_parser_version: i64,
    pub build_parser_version: Option<i64>,
}

impl SourceUsageEpochState {
    pub fn new(
        source: crate::source::SourceId,
        active_epoch: i64,
        build_epoch: Option<i64>,
        active_parser_version: i64,
        build_parser_version: Option<i64>,
    ) -> Result<Self, DomainError> {
        source
            .validate()
            .map_err(|error| DomainError::InvalidValue {
                field: "source",
                reason: error.to_string(),
            })?;
        non_negative(active_epoch, "usage_active_epoch")?;
        non_negative(active_parser_version, "usage_parser_version")?;
        if build_epoch.is_some() != build_parser_version.is_some() {
            return Err(DomainError::InvariantViolation {
                invariant: "usage build epoch and parser version must be paired",
            });
        }
        if let Some(build_epoch) = build_epoch {
            positive(build_epoch, "usage_build_epoch")?;
            if active_epoch.checked_add(1) != Some(build_epoch) {
                return Err(DomainError::InvariantViolation {
                    invariant: "usage build epoch must immediately follow active epoch",
                });
            }
        }
        optional_non_negative(build_parser_version, "usage_build_parser_version")?;
        Ok(Self {
            source,
            active_epoch,
            build_epoch,
            active_parser_version,
            build_parser_version,
        })
    }

    pub const fn working_epoch(&self) -> i64 {
        match self.build_epoch {
            Some(epoch) => epoch,
            None => self.active_epoch,
        }
    }

    pub const fn working_parser_version(&self) -> i64 {
        match self.build_parser_version {
            Some(version) => version,
            None => self.active_parser_version,
        }
    }
}

/// Scan trigger persisted in `scan_runs.trigger`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanTrigger {
    Startup,
    Scheduled,
    Manual,
    SourceChanged,
    Rebuild,
}

impl ScanTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "Startup",
            Self::Scheduled => "Scheduled",
            Self::Manual => "Manual",
            Self::SourceChanged => "SourceChanged",
            Self::Rebuild => "Rebuild",
        }
    }
}

impl_string_enum!(
    ScanTrigger,
    "trigger",
    Startup => "Startup",
    Scheduled => "Scheduled",
    Manual => "Manual",
    SourceChanged => "SourceChanged",
    Rebuild => "Rebuild"
);

/// Direct scan versus the one durable coalesced follow-up slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanRequestKind {
    Direct,
    Followup,
}

impl ScanRequestKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Followup => "followup",
        }
    }
}

impl_string_enum!(ScanRequestKind, "request_kind", Direct => "direct", Followup => "followup");

/// State projection in `app_meta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanLifecycleState {
    Idle,
    Running,
    Failed,
}

impl ScanLifecycleState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }
}

impl_string_enum!(ScanLifecycleState, "scan_state", Idle => "idle", Running => "running", Failed => "failed");

/// Durable state of one `scan_runs` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanRunState {
    Queued,
    Running,
    Completed,
    Failed,
    StartFailed,
}

impl ScanRunState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::StartFailed => "start_failed",
        }
    }
}

impl_string_enum!(
    ScanRunState,
    "state",
    Queued => "queued",
    Running => "running",
    Completed => "completed",
    Failed => "failed",
    StartFailed => "start_failed"
);

/// Durable state of one source child in `source_scan_runs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceScanState {
    Queued,
    Running,
    Completed,
    Skipped,
    Failed,
}

impl SourceScanState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

impl_string_enum!(
    SourceScanState,
    "source_scan_state",
    Queued => "queued",
    Running => "running",
    Completed => "completed",
    Skipped => "skipped",
    Failed => "failed"
);

/// Read-only projection of one source child for the internal status API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceScanStatus {
    pub source: String,
    pub state: SourceScanState,
    pub error_code: Option<String>,
}

impl SourceScanStatus {
    pub fn new(
        source: impl Into<String>,
        state: SourceScanState,
        error_code: Option<String>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            source: source.into(),
            state,
            error_code,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.source, "source")?;
        match (self.state, self.error_code.is_some()) {
            (SourceScanState::Failed, true)
            | (SourceScanState::Queued, false)
            | (SourceScanState::Running, false)
            | (SourceScanState::Completed, false)
            | (SourceScanState::Skipped, false) => {}
            (SourceScanState::Failed, false) => {
                return Err(DomainError::InvariantViolation {
                    invariant: "failed source scan requires an error code",
                });
            }
            (_, true) => {
                return Err(DomainError::InvariantViolation {
                    invariant: "non-failed source scan cannot have an error code",
                });
            }
        }
        validate_optional_code(&self.error_code, "source_scan_error_code")?;
        Ok(())
    }
}

/// Last-finished projection in `app_meta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanResult {
    Completed,
    Failed,
}

impl ScanResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl_string_enum!(ScanResult, "last_finished_scan_result", Completed => "completed", Failed => "failed");

/// State of the durable follow-up slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FollowupState {
    Queued,
    StartFailed,
}

impl FollowupState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::StartFailed => "start_failed",
        }
    }
}

impl_string_enum!(FollowupState, "followup_state", Queued => "queued", StartFailed => "start_failed");

/// Thread relationship stored by the metadata resolver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentRole {
    Main,
    Subagent,
    Unknown,
}

impl AgentRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Subagent => "subagent",
            Self::Unknown => "unknown",
        }
    }
}

impl_string_enum!(AgentRole, "agent_role", Main => "main", Subagent => "subagent", Unknown => "unknown");

/// Stable project-assignment classification for a normalized Thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProjectKind {
    Project,
    Projectless,
    Unknown,
}

impl ProjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Projectless => "projectless",
            Self::Unknown => "unknown",
        }
    }
}

impl_string_enum!(
    ProjectKind,
    "project_kind",
    Project => "project",
    Projectless => "projectless",
    Unknown => "unknown"
);

/// Normalized metadata quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetadataQualityStatus {
    Complete,
    Partial,
    Conflict,
}

impl MetadataQualityStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Conflict => "conflict",
        }
    }
}

impl_string_enum!(
    MetadataQualityStatus,
    "metadata_quality_status",
    Complete => "complete",
    Partial => "partial",
    Conflict => "conflict"
);

/// A normalized field patch.  `Clear` is intentionally distinct from
/// `Set(None)`: it is only legal after a complete all-source resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Patch<T> {
    Keep,
    Set(T),
    Clear,
}

impl<T> Patch<T> {
    pub const fn keep() -> Self {
        Self::Keep
    }

    pub fn set(value: T) -> Self {
        Self::Set(value)
    }

    pub const fn clear() -> Self {
        Self::Clear
    }

    pub const fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }

    pub const fn is_clear(&self) -> bool {
        matches!(self, Self::Clear)
    }

    pub const fn is_set(&self) -> bool {
        matches!(self, Self::Set(_))
    }
}

/// Normalized patch produced after all available sources for a Thread are
/// resolved.  The storage layer does not apply source precedence itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedThreadPatch {
    pub thread_id: String,
    /// Canonical Session identity. These fields are immutable after the
    /// thread is created; source resolution keeps the supplied identity.
    pub source: crate::source::SourceId,
    pub native_session_id: String,
    pub parent_thread_id: Patch<String>,
    pub root_session_id: Patch<String>,
    pub agent_role: Patch<AgentRole>,
    pub title: Patch<String>,
    pub project_name: Patch<String>,
    pub project_path: Patch<String>,
    pub project_kind: Patch<ProjectKind>,
    pub metadata_model: Patch<String>,
    pub created_at_ms: Patch<i64>,
    pub updated_at_ms: Patch<i64>,
    pub archived: Patch<bool>,
    pub metadata_quality_status: MetadataQualityStatus,
    pub resolved_at_ms: i64,
    pub full_resolution: bool,
}

/// Read-only projection of one normalized `threads` row for the metadata
/// resolver.  Storage exposes this structured value rather than a SQL row;
/// it intentionally contains no source payload or rollout body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingThreadProjection {
    pub thread_id: String,
    pub source: crate::source::SourceId,
    pub native_session_id: String,
    pub parent_thread_id: Option<String>,
    pub root_session_id: Option<String>,
    pub agent_role: AgentRole,
    pub title: Option<String>,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub project_kind: ProjectKind,
    pub metadata_model: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub archived: bool,
    pub metadata_quality_status: MetadataQualityStatus,
}

impl ResolvedThreadPatch {
    /// Create a no-op patch.  Callers fill fields with `Set`/`Clear` values and
    /// can use `full_resolution(true)` before clearing values.
    pub fn new(identity: &SessionIdentity, resolved_at_ms: i64) -> Result<Self, DomainError> {
        identity.validate()?;
        let value = Self {
            thread_id: identity.thread_id.clone(),
            source: identity.source.clone(),
            native_session_id: identity.native_session_id.clone(),
            parent_thread_id: Patch::Keep,
            root_session_id: Patch::Keep,
            agent_role: Patch::Keep,
            title: Patch::Keep,
            project_name: Patch::Keep,
            project_path: Patch::Keep,
            project_kind: Patch::Keep,
            metadata_model: Patch::Keep,
            created_at_ms: Patch::Keep,
            updated_at_ms: Patch::Keep,
            archived: Patch::Keep,
            metadata_quality_status: MetadataQualityStatus::Complete,
            resolved_at_ms,
            full_resolution: false,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn full_resolution(mut self, full_resolution: bool) -> Self {
        self.full_resolution = full_resolution;
        self
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.thread_id, "thread_id")?;
        self.source
            .validate()
            .map_err(|error| DomainError::InvalidValue {
                field: "source",
                reason: error.to_string(),
            })?;
        non_empty(&self.native_session_id, "native_session_id")?;
        non_negative(self.resolved_at_ms, "resolved_at_ms")?;
        validate_patch_string(&self.parent_thread_id, "parent_thread_id")?;
        validate_patch_string(&self.root_session_id, "root_session_id")?;
        validate_patch_string(&self.title, "title")?;
        validate_patch_string(&self.project_name, "project_name")?;
        validate_patch_string(&self.project_path, "project_path")?;
        validate_patch_string(&self.metadata_model, "metadata_model")?;
        validate_patch_path(&self.project_path, "project_path")?;
        validate_patch_time(&self.created_at_ms, "created_at_ms")?;
        validate_patch_time(&self.updated_at_ms, "updated_at_ms")?;
        if self.agent_role.is_clear() || self.project_kind.is_clear() || self.archived.is_clear() {
            return Err(DomainError::InvariantViolation {
                invariant: "agent_role, project_kind, and archived patches cannot be cleared",
            });
        }
        if !self.full_resolution && self.has_clear() {
            return Err(DomainError::InvariantViolation {
                invariant: "Clear requires full-resolution metadata recomputation",
            });
        }
        if let Patch::Set(AgentRole::Unknown) = self.agent_role {
            // Unknown is a legal temporary internal role, but it can never
            // claim a root session through a Set root value in the same patch.
            if matches!(self.root_session_id, Patch::Set(_)) {
                return Err(DomainError::InvariantViolation {
                    invariant: "unknown agent role cannot set root_session_id",
                });
            }
        }
        if let Patch::Set(AgentRole::Main) = self.agent_role
            && matches!(self.parent_thread_id, Patch::Set(_))
        {
            return Err(DomainError::InvariantViolation {
                invariant: "main agent role cannot set a parent thread",
            });
        }
        Ok(())
    }

    pub fn has_clear(&self) -> bool {
        self.parent_thread_id.is_clear()
            || self.root_session_id.is_clear()
            || self.agent_role.is_clear()
            || self.title.is_clear()
            || self.project_name.is_clear()
            || self.project_path.is_clear()
            || self.project_kind.is_clear()
            || self.metadata_model.is_clear()
            || self.created_at_ms.is_clear()
            || self.updated_at_ms.is_clear()
            || self.archived.is_clear()
    }
}

fn validate_patch_string(value: &Patch<String>, field: &'static str) -> Result<(), DomainError> {
    if let Patch::Set(value) = value {
        non_empty(value, field)?;
    }
    Ok(())
}

fn validate_patch_time(value: &Patch<i64>, field: &'static str) -> Result<(), DomainError> {
    if let Patch::Set(value) = value {
        non_negative(*value, field)?;
    }
    Ok(())
}

fn validate_patch_path(value: &Patch<String>, field: &'static str) -> Result<(), DomainError> {
    if let Patch::Set(value) = value {
        absolute_path(value, field)?;
    }
    Ok(())
}

/// Current app-meta scan projection.  It is deliberately separate from
/// `ScanRun`, which remains the immutable target history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanState {
    pub status_revision: i64,
    pub scan_state: ScanLifecycleState,
    pub active_scan_id: Option<String>,
    pub last_finished_scan_id: Option<String>,
    pub last_finished_scan_result: Option<ScanResult>,
    pub last_scan_started_at_ms: Option<i64>,
    pub last_scan_completed_at_ms: Option<i64>,
    pub last_scan_failed_at_ms: Option<i64>,
    pub last_scan_error_code: Option<String>,
    pub followup_scan_id: Option<String>,
    pub followup_state: Option<FollowupState>,
    pub followup_trigger: Option<ScanTrigger>,
    pub followup_requested_at_ms: Option<i64>,
    pub followup_enqueued_status_revision: Option<i64>,
    pub followup_error_code: Option<String>,
}

impl ScanState {
    pub fn initial() -> Self {
        Self {
            status_revision: 0,
            scan_state: ScanLifecycleState::Idle,
            active_scan_id: None,
            last_finished_scan_id: None,
            last_finished_scan_result: None,
            last_scan_started_at_ms: None,
            last_scan_completed_at_ms: None,
            last_scan_failed_at_ms: None,
            last_scan_error_code: None,
            followup_scan_id: None,
            followup_state: None,
            followup_trigger: None,
            followup_requested_at_ms: None,
            followup_enqueued_status_revision: None,
            followup_error_code: None,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_negative(self.status_revision, "status_revision")?;
        for (value, field) in [
            (self.last_scan_started_at_ms, "last_scan_started_at_ms"),
            (self.last_scan_completed_at_ms, "last_scan_completed_at_ms"),
            (self.last_scan_failed_at_ms, "last_scan_failed_at_ms"),
            (self.followup_requested_at_ms, "followup_requested_at_ms"),
            (
                self.followup_enqueued_status_revision,
                "followup_enqueued_status_revision",
            ),
        ] {
            optional_non_negative(value, field)?;
        }
        validate_optional_id(&self.active_scan_id, "active_scan_id")?;
        validate_optional_id(&self.last_finished_scan_id, "last_finished_scan_id")?;
        validate_optional_id(&self.followup_scan_id, "followup_scan_id")?;
        validate_optional_code(&self.last_scan_error_code, "last_scan_error_code")?;
        validate_optional_code(&self.followup_error_code, "followup_error_code")?;

        match self.scan_state {
            ScanLifecycleState::Running if self.active_scan_id.is_none() => {
                return Err(DomainError::InvariantViolation {
                    invariant: "running scan state requires active scan id",
                });
            }
            ScanLifecycleState::Idle | ScanLifecycleState::Failed
                if self.active_scan_id.is_some() =>
            {
                return Err(DomainError::InvariantViolation {
                    invariant: "idle or failed scan state cannot have active scan id",
                });
            }
            _ => {}
        }
        if self.last_finished_scan_id.is_some() != self.last_finished_scan_result.is_some() {
            return Err(DomainError::InvariantViolation {
                invariant: "last finished scan id and result must be both null or non-null",
            });
        }
        if let (Some(active), Some(followup)) = (
            self.active_scan_id.as_deref(),
            self.followup_scan_id.as_deref(),
        ) && active == followup
        {
            return Err(DomainError::InvariantViolation {
                invariant: "active and follow-up scan ids must differ",
            });
        }
        match self.followup_state {
            None => {
                if self.followup_scan_id.is_some()
                    || self.followup_trigger.is_some()
                    || self.followup_requested_at_ms.is_some()
                    || self.followup_enqueued_status_revision.is_some()
                    || self.followup_error_code.is_some()
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "empty follow-up state requires all follow-up fields null",
                    });
                }
            }
            Some(FollowupState::Queued) => {
                if self.followup_scan_id.is_none()
                    || self.followup_trigger.is_none()
                    || self.followup_requested_at_ms.is_none()
                    || self.followup_enqueued_status_revision.is_none()
                    || self.followup_error_code.is_some()
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "queued follow-up requires id, trigger, requested time and revision",
                    });
                }
            }
            Some(FollowupState::StartFailed) => {
                if self.followup_scan_id.is_none()
                    || self.followup_trigger.is_none()
                    || self.followup_requested_at_ms.is_none()
                    || self.followup_enqueued_status_revision.is_none()
                    || self.followup_error_code.is_none()
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "start-failed follow-up requires queued fields and an error code",
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_optional_id(value: &Option<String>, field: &'static str) -> Result<(), DomainError> {
    if let Some(value) = value.as_deref() {
        non_empty(value, field)?;
    }
    Ok(())
}

/// Top-level app state returned by `Ledger::app_state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppState {
    pub data_revision: i64,
    pub scan: ScanState,
}

impl AppState {
    pub fn new(data_revision: i64, scan: ScanState) -> Result<Self, DomainError> {
        non_negative(data_revision, "data_revision")?;
        scan.validate()?;
        Ok(Self {
            data_revision,
            scan,
        })
    }

    pub fn initial() -> Result<Self, DomainError> {
        Self::new(0, ScanState::initial())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_negative(self.data_revision, "data_revision")?;
        self.scan.validate()
    }
}

impl Deref for AppState {
    type Target = ScanState;

    fn deref(&self) -> &Self::Target {
        &self.scan
    }
}

/// Current app projection plus one optional immutable scan target row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanStatusSnapshot {
    pub app_state: AppState,
    pub target_scan: Option<ScanRun>,
    pub sources: Vec<SourceScanStatus>,
}

impl ScanStatusSnapshot {
    pub fn new(app_state: AppState, target_scan: Option<ScanRun>) -> Result<Self, DomainError> {
        Self::new_with_sources(app_state, target_scan, Vec::new())
    }

    pub fn new_with_sources(
        app_state: AppState,
        target_scan: Option<ScanRun>,
        mut sources: Vec<SourceScanStatus>,
    ) -> Result<Self, DomainError> {
        app_state.validate()?;
        if let Some(scan) = target_scan.as_ref() {
            scan.validate()?;
        }
        sources.sort_by(|left, right| left.source.cmp(&right.source));
        let mut previous_source = None;
        for source in &sources {
            source.validate()?;
            if previous_source.is_some_and(|previous| previous >= source.source.as_str()) {
                return Err(DomainError::InvariantViolation {
                    invariant: "source scan statuses must be sorted and unique",
                });
            }
            previous_source = Some(source.source.as_str());
        }
        Ok(Self {
            app_state,
            target_scan,
            sources,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.app_state.validate()?;
        if let Some(scan) = self.target_scan.as_ref() {
            scan.validate()?;
        }
        let mut previous_source = None;
        for source in &self.sources {
            source.validate()?;
            if previous_source.is_some_and(|previous| previous >= source.source.as_str()) {
                return Err(DomainError::InvariantViolation {
                    invariant: "source scan statuses must be sorted and unique",
                });
            }
            previous_source = Some(source.source.as_str());
        }
        Ok(())
    }
}

impl Deref for ScanStatusSnapshot {
    type Target = AppState;

    fn deref(&self) -> &Self::Target {
        &self.app_state
    }
}

/// Durable scan target row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRun {
    pub scan_id: String,
    pub trigger: ScanTrigger,
    pub request_kind: ScanRequestKind,
    pub state: ScanRunState,
    pub requested_at_ms: i64,
    pub enqueued_status_revision: Option<i64>,
    pub started_at_ms: Option<i64>,
    pub started_status_revision: Option<i64>,
    pub finished_at_ms: Option<i64>,
    pub terminal_status_revision: Option<i64>,
    pub error_code: Option<String>,
}

impl ScanRun {
    pub fn new(
        scan_id: impl Into<String>,
        trigger: ScanTrigger,
        request_kind: ScanRequestKind,
        state: ScanRunState,
        requested_at_ms: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            trigger,
            request_kind,
            state,
            requested_at_ms,
            enqueued_status_revision: None,
            started_at_ms: None,
            started_status_revision: None,
            finished_at_ms: None,
            terminal_status_revision: None,
            error_code: None,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn new_queued_followup(
        scan_id: impl Into<String>,
        trigger: ScanTrigger,
        requested_at_ms: i64,
        enqueued_status_revision: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            trigger,
            request_kind: ScanRequestKind::Followup,
            state: ScanRunState::Queued,
            requested_at_ms,
            enqueued_status_revision: Some(enqueued_status_revision),
            started_at_ms: None,
            started_status_revision: None,
            finished_at_ms: None,
            terminal_status_revision: None,
            error_code: None,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn new_running_direct(
        scan_id: impl Into<String>,
        trigger: ScanTrigger,
        requested_at_ms: i64,
        started_at_ms: i64,
        started_status_revision: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            trigger,
            request_kind: ScanRequestKind::Direct,
            state: ScanRunState::Running,
            requested_at_ms,
            enqueued_status_revision: None,
            started_at_ms: Some(started_at_ms),
            started_status_revision: Some(started_status_revision),
            finished_at_ms: None,
            terminal_status_revision: None,
            error_code: None,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.scan_id, "scan_id")?;
        non_negative(self.requested_at_ms, "requested_at_ms")?;
        for (value, field) in [
            (self.enqueued_status_revision, "enqueued_status_revision"),
            (self.started_at_ms, "started_at_ms"),
            (self.started_status_revision, "started_status_revision"),
            (self.finished_at_ms, "finished_at_ms"),
            (self.terminal_status_revision, "terminal_status_revision"),
        ] {
            optional_non_negative(value, field)?;
        }
        validate_optional_code(&self.error_code, "error_code")?;
        let valid_enqueued_revision = match self.request_kind {
            ScanRequestKind::Direct => self.enqueued_status_revision.is_none(),
            ScanRequestKind::Followup => self.enqueued_status_revision.is_some(),
        };
        match self.state {
            ScanRunState::Queued => {
                if self.request_kind != ScanRequestKind::Followup
                    || self.enqueued_status_revision.is_none()
                    || self.started_at_ms.is_some()
                    || self.started_status_revision.is_some()
                    || self.finished_at_ms.is_some()
                    || self.terminal_status_revision.is_some()
                    || self.error_code.is_some()
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "queued scan row requires follow-up enqueue fields only",
                    });
                }
            }
            ScanRunState::Running => {
                if self.started_at_ms.is_none()
                    || self.started_status_revision.is_none()
                    || self.finished_at_ms.is_some()
                    || self.terminal_status_revision.is_some()
                    || self.error_code.is_some()
                    || !valid_enqueued_revision
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "running scan row has invalid start/terminal fields",
                    });
                }
            }
            ScanRunState::Completed => {
                if self.started_at_ms.is_none()
                    || self.started_status_revision.is_none()
                    || self.finished_at_ms.is_none()
                    || self.terminal_status_revision.is_none()
                    || self.error_code.is_some()
                    || !valid_enqueued_revision
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "completed scan row requires started and terminal fields",
                    });
                }
            }
            ScanRunState::Failed => {
                if self.started_at_ms.is_none()
                    || self.started_status_revision.is_none()
                    || self.finished_at_ms.is_none()
                    || self.terminal_status_revision.is_none()
                    || self.error_code.is_none()
                    || !valid_enqueued_revision
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "failed scan row requires started, terminal and error fields",
                    });
                }
            }
            ScanRunState::StartFailed => {
                if self.request_kind != ScanRequestKind::Followup
                    || self.enqueued_status_revision.is_none()
                    || self.started_at_ms.is_some()
                    || self.started_status_revision.is_some()
                    || self.finished_at_ms.is_none()
                    || self.terminal_status_revision.is_none()
                    || self.error_code.is_none()
                {
                    return Err(DomainError::InvariantViolation {
                        invariant: "start-failed scan row requires enqueue and terminal fields",
                    });
                }
            }
        }
        Ok(())
    }
}

/// Commands for scan lifecycle writes.  Each command contains only safe
/// timestamps/IDs/error codes; no scanner payload is persisted here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanStartEvent {
    pub scan_id: String,
    pub trigger: ScanTrigger,
    pub requested_at_ms: i64,
    pub started_at_ms: i64,
}

impl ScanStartEvent {
    pub fn new(
        scan_id: impl Into<String>,
        trigger: ScanTrigger,
        started_at_ms: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            trigger,
            requested_at_ms: started_at_ms,
            started_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn with_requested_at(mut self, requested_at_ms: i64) -> Result<Self, DomainError> {
        self.requested_at_ms = requested_at_ms;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.scan_id, "scan_id")?;
        non_negative(self.requested_at_ms, "requested_at_ms")?;
        non_negative(self.started_at_ms, "started_at_ms")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveScanFollowupEvent {
    pub followup_scan_id: String,
    pub trigger: ScanTrigger,
    pub requested_at_ms: i64,
}

impl ReserveScanFollowupEvent {
    pub fn new(
        followup_scan_id: impl Into<String>,
        trigger: ScanTrigger,
        requested_at_ms: i64,
    ) -> Result<Self, DomainError> {
        let value = Self {
            followup_scan_id: followup_scan_id.into(),
            trigger,
            requested_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.followup_scan_id, "followup_scan_id")?;
        non_negative(self.requested_at_ms, "requested_at_ms")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowupStartedEvent {
    pub scan_id: String,
    pub started_at_ms: i64,
}

impl FollowupStartedEvent {
    pub fn new(scan_id: impl Into<String>, started_at_ms: i64) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            started_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.scan_id, "scan_id")?;
        non_negative(self.started_at_ms, "started_at_ms")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowupStartFailedEvent {
    pub scan_id: String,
    pub failed_at_ms: i64,
    pub error_code: String,
}

impl FollowupStartFailedEvent {
    pub fn new(
        scan_id: impl Into<String>,
        failed_at_ms: i64,
        error_code: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            failed_at_ms,
            error_code: error_code.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.scan_id, "scan_id")?;
        non_negative(self.failed_at_ms, "failed_at_ms")?;
        safe_code(&self.error_code, "error_code")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanCompletedEvent {
    pub scan_id: String,
    pub completed_at_ms: i64,
}

impl ScanCompletedEvent {
    pub fn new(scan_id: impl Into<String>, completed_at_ms: i64) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            completed_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.scan_id, "scan_id")?;
        non_negative(self.completed_at_ms, "completed_at_ms")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFailedEvent {
    pub scan_id: String,
    pub failed_at_ms: i64,
    pub error_code: String,
}

impl ScanFailedEvent {
    pub fn new(
        scan_id: impl Into<String>,
        failed_at_ms: i64,
        error_code: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let value = Self {
            scan_id: scan_id.into(),
            failed_at_ms,
            error_code: error_code.into(),
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        non_empty(&self.scan_id, "scan_id")?;
        non_negative(self.failed_at_ms, "failed_at_ms")?;
        safe_code(&self.error_code, "error_code")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_requires_full_resolution_and_role_cannot_clear() {
        let identity = SessionIdentity::new(
            "thread",
            crate::source::SourceId::new("test").unwrap(),
            "native",
        )
        .unwrap();
        let mut patch = ResolvedThreadPatch::new(&identity, 1).unwrap();
        patch.title = Patch::Clear;
        assert!(patch.validate().is_err());
        patch.full_resolution = true;
        patch.validate().unwrap();
        patch.agent_role = Patch::Clear;
        assert!(patch.validate().is_err());
    }

    #[test]
    fn scan_state_enforces_projection_invariants() {
        let mut state = ScanState::initial();
        state.scan_state = ScanLifecycleState::Running;
        assert!(state.validate().is_err());
        state.active_scan_id = Some("scan".to_string());
        state.validate().unwrap();
    }

    #[test]
    fn scan_run_state_checks_terminal_fields() {
        let run = ScanRun::new(
            "scan",
            ScanTrigger::Manual,
            ScanRequestKind::Direct,
            ScanRunState::Queued,
            1,
        );
        assert!(run.is_err());
    }

    #[test]
    fn scan_trigger_serialization_matches_schema_case() {
        assert_eq!(ScanTrigger::Manual.as_str(), "Manual");
        assert_eq!(ScanTrigger::try_from("Manual"), Ok(ScanTrigger::Manual));
        assert!(ScanTrigger::try_from("manual").is_err());
    }

    #[test]
    fn canonical_algorithm_and_usage_epoch_are_versioned() {
        let active = SourceUsageEpochState::new(
            crate::source::SourceId::new("test").unwrap(),
            3,
            None,
            7,
            None,
        )
        .unwrap();
        assert_eq!(
            (active.working_epoch(), active.working_parser_version()),
            (3, 7)
        );
        let building = SourceUsageEpochState::new(
            crate::source::SourceId::new("test").unwrap(),
            3,
            Some(4),
            7,
            Some(8),
        )
        .unwrap();
        assert_eq!(
            (building.working_epoch(), building.working_parser_version()),
            (4, 8)
        );
        assert!(
            SourceUsageEpochState::new(
                crate::source::SourceId::new("test").unwrap(),
                3,
                Some(5),
                7,
                Some(8)
            )
            .is_err()
        );
        assert!(
            SourceUsageEpochState::new(
                crate::source::SourceId::new("test").unwrap(),
                3,
                Some(4),
                7,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn session_identity_preserves_namespaced_rules() {
        let source = crate::source::SourceId::new("antigravity").unwrap();
        let namespaced = SessionIdentity::namespaced(&source, "conversation-1").unwrap();
        assert_eq!(namespaced.thread_id, "antigravity:conversation-1");
        assert_eq!(namespaced.source, source);
        assert_eq!(namespaced.native_session_id, "conversation-1");
    }

    #[test]
    fn session_identity_rejects_empty_identity_parts() {
        let source = crate::source::SourceId::new("test").unwrap();
        assert!(SessionIdentity::new("", source.clone(), "native").is_err());
        assert!(SessionIdentity::new("thread", source.clone(), "").is_err());
    }
}
