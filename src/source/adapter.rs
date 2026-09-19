//! Source adapter and source-bound storage contracts.

use std::{fmt, sync::atomic::AtomicBool};

use crate::{
    domain::{ResolvedThreadPatch, SessionIdentity, UsageEpochState},
    usage::{EventKind, NormalizedTokenUsage},
};

use super::{SourceDescriptor, SourceId};

/// Whether an adapter can run for the current invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterAvailability {
    Available,
    /// The source is known but has no usable installation/configuration for
    /// this invocation.  The coordinator may represent this as a skipped
    /// source run.
    Unavailable(String),
    /// A concise spelling for sources which are not installed.
    NotInstalled,
}

impl AdapterAvailability {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub const fn is_unavailable(&self) -> bool {
        !self.is_available()
    }
}

/// Adapter-level failure.  Source-specific diagnostics remain owned by the
/// adapter; the coordinator can map this to a stable source-run error code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAdapterError {
    message: String,
}

impl SourceAdapterError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SourceAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SourceAdapterError {}

/// Result returned by one source adapter scan.
pub type SourceRunResult = Result<(), SourceAdapterError>;

/// The minimum adapter interface.  A source is fixed by the descriptor and
/// cannot be selected by a scan operation or a canonical write DTO.
pub trait SourceAdapter: Send + Sync + 'static {
    fn descriptor(&self) -> &SourceDescriptor;

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError>;

    fn run_scan(&self, context: &SourceRunContext, cancellation: &AtomicBool) -> SourceRunResult;
}

impl<T> SourceAdapter for std::sync::Arc<T>
where
    T: SourceAdapter + ?Sized,
{
    fn descriptor(&self) -> &SourceDescriptor {
        self.as_ref().descriptor()
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        self.as_ref().availability()
    }

    fn run_scan(&self, context: &SourceRunContext, cancellation: &AtomicBool) -> SourceRunResult {
        self.as_ref().run_scan(context, cancellation)
    }
}

/// Error returned while constructing a source run context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceContextError {
    EmptyScanId,
    ControlCharacterInScanId,
    StorageSourceMismatch,
    StorageScanMismatch,
}

impl fmt::Display for SourceContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScanId => formatter.write_str("scan id must not be empty"),
            Self::ControlCharacterInScanId => {
                formatter.write_str("scan id must not contain control characters")
            }
            Self::StorageSourceMismatch => {
                formatter.write_str("source storage is bound to a different source")
            }
            Self::StorageScanMismatch => {
                formatter.write_str("source storage is bound to a different scan")
            }
        }
    }
}

impl std::error::Error for SourceContextError {}

/// Core-created context passed to one adapter invocation.
#[derive(Clone, Debug)]
pub struct SourceRunContext {
    scan_id: String,
    source: SourceId,
    storage: SourceStorage,
}

#[allow(dead_code)]
impl SourceRunContext {
    /// Construct a context for a source already selected by the registry.
    ///
    /// The constructor is crate-private so adapters cannot manufacture a
    /// context for an arbitrary source.  The coordinator passes the
    /// descriptor belonging to the registered adapter instead.
    pub(crate) fn new(
        scan_id: impl Into<String>,
        descriptor: &SourceDescriptor,
    ) -> Result<Self, SourceContextError> {
        let scan_id = scan_id.into();
        let source = descriptor.id.clone();
        let storage = SourceStorage::new(scan_id.clone(), source.clone());
        Self::with_storage(scan_id, source, storage)
    }

    pub(crate) fn with_storage(
        scan_id: impl Into<String>,
        source: SourceId,
        storage: SourceStorage,
    ) -> Result<Self, SourceContextError> {
        let scan_id = scan_id.into();
        if scan_id.trim().is_empty() {
            return Err(SourceContextError::EmptyScanId);
        }
        if scan_id.chars().any(char::is_control) {
            return Err(SourceContextError::ControlCharacterInScanId);
        }
        if storage.source() != &source {
            return Err(SourceContextError::StorageSourceMismatch);
        }
        if storage.scan_id() != scan_id {
            return Err(SourceContextError::StorageScanMismatch);
        }
        Ok(Self {
            scan_id,
            source,
            storage,
        })
    }

    pub fn source(&self) -> &SourceId {
        &self.source
    }

    pub fn scan_id(&self) -> &str {
        &self.scan_id
    }

    pub fn storage(&self) -> &SourceStorage {
        &self.storage
    }
}

/// Errors from the source-bound storage seam.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceStorageError {
    /// The v11 runtime implementation has not been introduced yet.
    NotImplemented,
    TransactionClosed,
    SourceMismatch,
    InvalidRequest(String),
}

impl fmt::Display for SourceStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented => formatter.write_str("source storage runtime is not available"),
            Self::TransactionClosed => formatter.write_str("source write transaction is closed"),
            Self::SourceMismatch => formatter.write_str("source-bound storage invariant violated"),
            Self::InvalidRequest(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SourceStorageError {}

/// Source-bound storage capability exposed to adapters.
///
/// Phase 1 intentionally carries no database handle.  The concrete v10/v11
/// bridge is introduced with the runtime cutover; keeping this handle opaque
/// now prevents a second, autocommit storage path from appearing.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SourceStorage {
    scan_id: String,
    source: SourceId,
}

#[allow(dead_code)]
impl SourceStorage {
    pub(crate) fn new(scan_id: impl Into<String>, source: SourceId) -> Self {
        Self {
            scan_id: scan_id.into(),
            source,
        }
    }

    pub(crate) fn scan_id(&self) -> &str {
        &self.scan_id
    }

    pub(crate) fn source(&self) -> &SourceId {
        &self.source
    }

    /// Load this source's usage epoch.  No v11 runtime implementation exists
    /// in Phase 1, so the operation is intentionally not wired to the v10 DB.
    pub fn load_usage_epoch(&self) -> Result<Option<UsageEpochState>, SourceStorageError> {
        Err(SourceStorageError::NotImplemented)
    }

    /// Begin the only source-bound write seam available to adapters.
    pub fn begin_write_txn(&self) -> Result<SourceWriteTxn, SourceStorageError> {
        Err(SourceStorageError::NotImplemented)
    }
}

/// Whether canonical usage is written to the active or shadow-build epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageWriteTarget {
    Active,
    Build,
}

/// Canonical usage write DTO.  Physical provenance and source/epoch selection
/// deliberately do not appear here; those are owned by the bound transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalUsageEventWrite {
    pub event_id: String,
    pub kind: EventKind,
    pub occurred_at_ms: i64,
    pub thread_id: String,
    pub root_session_id: String,
    pub turn_key: Option<String>,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub estimated_cost_nanos_usd: Option<i64>,
    pub usage: NormalizedTokenUsage,
}

/// Source-bound transaction.  All mutation methods live here; the source is
/// captured when the transaction is created and is not accepted by any method.
#[derive(Debug)]
pub struct SourceWriteTxn {
    scan_id: String,
    source: SourceId,
    committed: bool,
}

impl SourceWriteTxn {
    #[expect(
        dead_code,
        reason = "the runtime bridge constructs this source-bound transaction in a later phase"
    )]
    pub(crate) fn new(scan_id: impl Into<String>, source: SourceId) -> Self {
        Self {
            scan_id: scan_id.into(),
            source,
            committed: false,
        }
    }

    pub fn scan_id(&self) -> &str {
        &self.scan_id
    }

    pub fn source(&self) -> &SourceId {
        &self.source
    }

    pub fn ensure_usage_epoch(&mut self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::NotImplemented)
    }

    pub fn upsert_session_metadata(
        &mut self,
        _identity: &SessionIdentity,
        _patch: &ResolvedThreadPatch,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::NotImplemented)
    }

    pub fn write_usage(
        &mut self,
        _target: UsageWriteTarget,
        _event: CanonicalUsageEventWrite,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::NotImplemented)
    }

    pub fn begin_or_resume_usage_build(
        &mut self,
        _parser_version: i64,
    ) -> Result<i64, SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::NotImplemented)
    }

    pub fn activate_usage_build(
        &mut self,
        _expected_epoch: i64,
        _expected_parser_version: i64,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::NotImplemented)
    }

    pub fn resolve_usage_write_epoch(
        &mut self,
        _target: UsageWriteTarget,
    ) -> Result<i64, SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::NotImplemented)
    }

    /// Commit this transaction.  Phase 1 has no storage implementation, so a
    /// transaction can only be created by the future runtime bridge.
    pub fn commit(mut self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        self.committed = true;
        Err(SourceStorageError::NotImplemented)
    }

    fn require_open(&self) -> Result<(), SourceStorageError> {
        if self.committed {
            Err(SourceStorageError::TransactionClosed)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_binds_storage_to_its_source() {
        let descriptor = SourceDescriptor::codex();
        let context = SourceRunContext::new("scan-1", &descriptor).unwrap();
        assert_eq!(context.source(), &SourceId::CODEX);
        assert_eq!(context.scan_id(), "scan-1");
        assert_eq!(context.storage().scan_id(), "scan-1");
        assert_eq!(context.storage().source(), &SourceId::CODEX);
        assert!(matches!(
            context.storage().load_usage_epoch(),
            Err(SourceStorageError::NotImplemented)
        ));
    }

    #[test]
    fn context_rejects_invalid_scan_ids() {
        let descriptor = SourceDescriptor::codex();
        assert!(matches!(
            SourceRunContext::new("", &descriptor),
            Err(SourceContextError::EmptyScanId)
        ));
        assert!(matches!(
            SourceRunContext::new("   ", &descriptor),
            Err(SourceContextError::EmptyScanId)
        ));
        assert!(matches!(
            SourceRunContext::new("scan\n", &descriptor),
            Err(SourceContextError::ControlCharacterInScanId)
        ));
    }
}
