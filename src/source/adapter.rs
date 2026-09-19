//! Source adapter and source-bound storage contracts.

use std::{
    fmt,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use crate::{
    domain::{ResolvedThreadPatch, SessionIdentity, UsageEpochState},
    storage::{Ledger, StorageErrorKind},
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
    code: &'static str,
    message: String,
}

impl SourceAdapterError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::with_code("SOURCE_RUN_FAILED", message)
    }

    pub fn with_code(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
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

/// Durable outcome/diagnostic for one source invocation. Reports are kept in
/// memory by the ingestion handle during Phase 2; v11 will persist child
/// lifecycle rows separately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRunReport {
    pub scan_id: String,
    pub source: SourceId,
    pub state: SourceRunState,
    pub error_code: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRunState {
    Completed,
    Skipped,
    Failed,
}

impl SourceRunReport {
    pub(crate) fn completed(scan_id: &str, source: &SourceId) -> Self {
        Self {
            scan_id: scan_id.to_owned(),
            source: source.clone(),
            state: SourceRunState::Completed,
            error_code: None,
            detail: None,
        }
    }

    pub(crate) fn skipped_with_detail(
        scan_id: &str,
        source: &SourceId,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            scan_id: scan_id.to_owned(),
            source: source.clone(),
            state: SourceRunState::Skipped,
            error_code: None,
            detail: Some(detail.into()),
        }
    }

    pub(crate) fn failed_with_detail(
        scan_id: &str,
        source: &SourceId,
        code: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            scan_id: scan_id.to_owned(),
            source: source.clone(),
            state: SourceRunState::Failed,
            error_code: Some(code.into()),
            detail: Some(detail.into()),
        }
    }
}

impl SourceRunState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

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
    /// A v10 storage failure, retaining the storage category so adapters can
    /// preserve the original failure semantics at their boundary.
    Storage(StorageErrorKind),
    /// The v10 schema has no equivalent operation for this mutation yet.
    UnsupportedOperation(&'static str),
    /// The mature Codex compatibility worker reported its concrete failure;
    /// this is intentionally not treated as a successful transaction.
    CompatibilityOperationFailed(&'static str),
    InvalidRequest(String),
}

impl fmt::Display for SourceStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented => formatter.write_str("source storage runtime is not available"),
            Self::TransactionClosed => formatter.write_str("source write transaction is closed"),
            Self::SourceMismatch => formatter.write_str("source-bound storage invariant violated"),
            Self::Storage(kind) => write!(formatter, "v10 storage operation failed: {kind:?}"),
            Self::UnsupportedOperation(operation) => {
                write!(
                    formatter,
                    "v10 compatibility operation is unavailable: {operation}"
                )
            }
            Self::CompatibilityOperationFailed(code) => {
                write!(formatter, "Codex compatibility operation failed: {code}")
            }
            Self::InvalidRequest(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SourceStorageError {}

/// Source-bound storage capability exposed to adapters.
///
/// The optional ledger is a deliberately small v10 compatibility bridge.  It
/// lets the coordinator construct a capability tied to the existing database
/// without exposing unrestricted SQL or source selection to an adapter.
#[derive(Clone)]
#[allow(dead_code)]
pub struct SourceStorage {
    scan_id: String,
    source: SourceId,
    ledger: Option<Arc<Ledger>>,
}

/// The fixed Codex compatibility request.  It carries only source data and
/// cancellation state; the storage bridge retains the Ledger and invokes the
/// mature v10 worker internally.
pub(crate) struct CodexCompatRequest<'a> {
    pub(crate) codex_home: &'a Path,
    pub(crate) state_index_path: &'a Path,
    pub(crate) session_index_path: &'a Path,
    pub(crate) global_state_path: &'a Path,
    pub(crate) cancellation: &'a AtomicBool,
}

impl fmt::Debug for SourceStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceStorage")
            .field("scan_id", &self.scan_id)
            .field("source", &self.source)
            .field("has_v10_bridge", &self.ledger.is_some())
            .finish()
    }
}

/// Core-owned factory that binds the same storage capability shape to every
/// registry descriptor. The descriptor supplies identity; adapters cannot
/// choose a different source or backend.
#[derive(Clone)]
pub(crate) struct SourceStorageFactory {
    ledger: Arc<Ledger>,
}

impl SourceStorageFactory {
    pub(crate) fn new(ledger: Arc<Ledger>) -> Self {
        Self { ledger }
    }

    pub(crate) fn context(
        &self,
        scan_id: &str,
        descriptor: &SourceDescriptor,
    ) -> Result<SourceRunContext, SourceContextError> {
        let storage = SourceStorage::with_ledger(
            scan_id.to_owned(),
            descriptor.id.clone(),
            Arc::clone(&self.ledger),
        );
        SourceRunContext::with_storage(scan_id.to_owned(), descriptor.id.clone(), storage)
    }
}

#[allow(dead_code)]
impl SourceStorage {
    pub(crate) fn new(scan_id: impl Into<String>, source: SourceId) -> Self {
        Self {
            scan_id: scan_id.into(),
            source,
            ledger: None,
        }
    }

    pub(crate) fn with_ledger(
        scan_id: impl Into<String>,
        source: SourceId,
        ledger: Arc<Ledger>,
    ) -> Self {
        Self {
            scan_id: scan_id.into(),
            source,
            ledger: Some(ledger),
        }
    }

    pub(crate) fn scan_id(&self) -> &str {
        &self.scan_id
    }

    pub(crate) fn source(&self) -> &SourceId {
        &self.source
    }

    /// Validate the legacy Codex source path against the Ledger binding before
    /// any discovery or canonical write is attempted.
    pub(crate) fn ensure_codex_home(&self, codex_home: &Path) -> Result<(), SourceStorageError> {
        if self.source != SourceId::CODEX {
            return Err(SourceStorageError::SourceMismatch);
        }
        let Some(ledger) = self.ledger.as_ref() else {
            return Err(SourceStorageError::NotImplemented);
        };
        // Binding readiness is deliberately checked only at this
        // Codex-private compatibility boundary.  Lifecycle seams remain
        // source-agnostic, while a persisted `source_changed` status is
        // surfaced before comparing the caller's path with this connection's
        // current home.
        ledger
            .ensure_source_ready()
            .map_err(|error| SourceStorageError::Storage(error.kind()))?;
        if Ledger::codex_home_fingerprint(codex_home) != ledger.expected_codex_home_fingerprint() {
            return Err(SourceStorageError::Storage(StorageErrorKind::SourceChanged));
        }
        Ok(())
    }

    /// Run the fixed v10 Codex worker without presenting a false single
    /// transaction boundary. The mature worker owns its existing commit
    /// sequence; this facade only enforces source binding and keeps the
    /// Ledger inside the storage boundary.
    pub(crate) fn run_codex_compat(
        &self,
        request: CodexCompatRequest<'_>,
    ) -> Result<(), SourceStorageError> {
        self.ensure_codex_home(request.codex_home)?;
        if self.ledger.is_none() {
            return Err(SourceStorageError::NotImplemented);
        }
        let worker = crate::scanner::MetadataWorker::for_codex_compat(
            request.codex_home,
            crate::scanner::CodexMetadata::with_paths(
                request.state_index_path,
                request.session_index_path,
                request.global_state_path,
            ),
        );
        worker
            .run_round_with_source_storage(self, request.cancellation)
            .map_err(SourceStorageError::CompatibilityOperationFailed)
    }

    pub fn load_usage_epoch(&self) -> Result<Option<UsageEpochState>, SourceStorageError> {
        if self.source != SourceId::CODEX {
            return Err(SourceStorageError::UnsupportedOperation("load_usage_epoch"));
        }
        let Some(ledger) = self.ledger.as_ref() else {
            return Err(SourceStorageError::NotImplemented);
        };
        ledger
            .ensure_source_ready()
            .map_err(|error| SourceStorageError::Storage(error.kind()))?;
        ledger
            .load_usage_epoch_state()
            .map(Some)
            .map_err(|error| SourceStorageError::Storage(error.kind()))
    }

    /// Begin the source-bound write seam. Phase 2 has no generic mutation
    /// implementation yet, so the transaction remains explicitly unsupported.
    pub fn begin_write_txn(&self) -> Result<SourceWriteTxn, SourceStorageError> {
        Err(SourceStorageError::UnsupportedOperation("begin_write_txn"))
    }
}

impl crate::scanner::MetadataWorker {
    /// Execute the existing v10 pipeline from the storage boundary. The
    /// adapter-facing path receives only this opaque storage capability; the
    /// Ledger never crosses back into scanner/adapter code.
    pub(crate) fn run_round_with_source_storage(
        &self,
        storage: &SourceStorage,
        cancellation: &AtomicBool,
    ) -> Result<(), &'static str> {
        let Some(ledger) = storage.ledger.as_ref() else {
            return Err("CODEX_LEDGER_UNAVAILABLE");
        };
        self.run_round_with_ledger(ledger, cancellation)
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
    #[allow(dead_code)]
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
        Err(SourceStorageError::UnsupportedOperation(
            "ensure_usage_epoch",
        ))
    }

    pub fn upsert_session_metadata(
        &mut self,
        identity: &SessionIdentity,
        _patch: &ResolvedThreadPatch,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        if identity.source != self.source {
            return Err(SourceStorageError::SourceMismatch);
        }
        Err(SourceStorageError::UnsupportedOperation(
            "upsert_session_metadata",
        ))
    }

    pub fn write_usage(
        &mut self,
        _target: UsageWriteTarget,
        _event: CanonicalUsageEventWrite,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::UnsupportedOperation("write_usage"))
    }

    pub fn begin_or_resume_usage_build(
        &mut self,
        _parser_version: i64,
    ) -> Result<i64, SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::UnsupportedOperation(
            "begin_or_resume_usage_build",
        ))
    }

    pub fn activate_usage_build(
        &mut self,
        _expected_epoch: i64,
        _expected_parser_version: i64,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::UnsupportedOperation(
            "activate_usage_build",
        ))
    }

    pub fn resolve_usage_write_epoch(
        &mut self,
        _target: UsageWriteTarget,
    ) -> Result<i64, SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::UnsupportedOperation(
            "resolve_usage_write_epoch",
        ))
    }

    pub fn commit(self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        Err(SourceStorageError::UnsupportedOperation("commit"))
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
    use std::{
        fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::storage::{Ledger, LedgerOptions, StorageErrorKind};

    struct TempPath(PathBuf);

    impl TempPath {
        fn new(label: &str) -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("usagi-source-{label}-{stamp}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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

    #[test]
    fn v10_bridge_reads_epoch_and_rejects_unimplemented_mutations_explicitly() {
        let temp = TempPath::new("v10");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let storage = SourceStorage::with_ledger("scan-1", SourceId::CODEX, ledger);
        let epoch = storage.load_usage_epoch().unwrap().unwrap();
        assert!(matches!(
            storage.begin_write_txn(),
            Err(SourceStorageError::UnsupportedOperation("begin_write_txn"))
        ));
        let mut txn = SourceWriteTxn::new("scan-1", SourceId::CODEX);
        assert!(matches!(
            txn.ensure_usage_epoch(),
            Err(SourceStorageError::UnsupportedOperation(
                "ensure_usage_epoch"
            ))
        ));
        assert!(epoch.active_epoch >= 0);
    }

    #[test]
    fn v10_bridge_preserves_source_storage_error_category() {
        let temp = TempPath::new("changed");
        let home_a = temp.path().join("codex-a");
        let home_b = temp.path().join("codex-b");
        fs::create_dir_all(&home_a).unwrap();
        fs::create_dir_all(&home_b).unwrap();
        let db = temp.path().join("mu.sqlite3");
        let first = Ledger::open(LedgerOptions::new(&db, &home_a)).unwrap();
        drop(first);
        let changed = Arc::new(Ledger::open(LedgerOptions::new(&db, &home_b)).unwrap());
        let storage = SourceStorage::with_ledger("scan-1", SourceId::CODEX, changed);
        assert!(matches!(
            storage.ensure_codex_home(&home_b),
            Err(SourceStorageError::Storage(StorageErrorKind::SourceChanged))
        ));
        assert!(matches!(
            storage.load_usage_epoch(),
            Err(SourceStorageError::Storage(StorageErrorKind::SourceChanged))
        ));
    }
}
