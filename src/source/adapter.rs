//! Source adapter and source-bound storage contracts.

use std::{
    fmt,
    path::Path,
    sync::{Arc, MutexGuard, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{
    domain::{Patch, ResolvedThreadPatch, SessionIdentity, SourceUsageEpochState},
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
    /// The source storage runtime is unavailable for this capability.
    NotImplemented,
    TransactionClosed,
    /// A mutation failed earlier in this transaction.  The transaction is
    /// deliberately poisoned so a caller cannot ignore the error and commit
    /// a partial canonical/private-state batch.
    TransactionPoisoned,
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
            Self::TransactionPoisoned => {
                formatter.write_str("source write transaction is poisoned")
            }
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

    pub fn load_usage_epoch(&self) -> Result<Option<SourceUsageEpochState>, SourceStorageError> {
        let Some(ledger) = self.ledger.as_ref() else {
            return Err(SourceStorageError::NotImplemented);
        };
        let connection = ledger
            .connection()
            .map_err(|error| SourceStorageError::Storage(error.kind()))?;
        let row = connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version,build_parser_version
                 FROM source_usage_epochs WHERE source=?1",
                [self.source.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| {
                SourceStorageError::Storage(crate::storage::StorageError::sqlite(error).kind())
            })?;
        row.map(|(active, build, parser, build_parser)| {
            SourceUsageEpochState::new(self.source.clone(), active, build, parser, build_parser)
                .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))
        })
        .transpose()
    }

    /// Begin the source-bound write seam. The returned transaction owns the
    /// SQLite `BEGIN IMMEDIATE` boundary; all mutation methods remain bound to
    /// this storage's source until `commit` (or rollback on drop).
    pub fn begin_write_txn(&self) -> Result<SourceWriteTxn<'_>, SourceStorageError> {
        let Some(ledger) = self.ledger.as_ref() else {
            return Err(SourceStorageError::NotImplemented);
        };
        let guard = ledger
            .connection()
            .map_err(|error| SourceStorageError::Storage(error.kind()))?;
        guard.execute_batch("BEGIN IMMEDIATE").map_err(|error| {
            SourceStorageError::Storage(crate::storage::StorageError::sqlite(error).kind())
        })?;
        Ok(SourceWriteTxn {
            scan_id: self.scan_id.clone(),
            source: self.source.clone(),
            connection: Some(SourceWriteConnection::Locked(guard)),
            ledger: Some(Arc::clone(ledger)),
            committed: false,
            poisoned: false,
            data_changed: false,
        })
    }
}

impl crate::scanner::MetadataWorker {
    /// Execute the transitional Codex algorithms from the source-bound
    /// storage capability. Reads still reuse the mature Ledger projections,
    /// while every durable Codex mutation is now committed by SourceWriteTxn;
    /// there is no second legacy BEGIN IMMEDIATE/COMMIT seam.
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
enum SourceWriteConnection<'a> {
    Locked(MutexGuard<'a, Connection>),
    Legacy(Transaction<'a>),
}

impl SourceWriteConnection<'_> {
    fn connection(&self) -> &Connection {
        match self {
            Self::Locked(connection) => connection,
            Self::Legacy(transaction) => transaction,
        }
    }

    fn connection_mut(&mut self) -> &Connection {
        match self {
            Self::Locked(connection) => connection,
            Self::Legacy(transaction) => transaction,
        }
    }
}

pub struct SourceWriteTxn<'a> {
    scan_id: String,
    source: SourceId,
    connection: Option<SourceWriteConnection<'a>>,
    ledger: Option<Arc<Ledger>>,
    committed: bool,
    poisoned: bool,
    data_changed: bool,
}

impl fmt::Debug for SourceWriteTxn<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceWriteTxn")
            .field("scan_id", &self.scan_id)
            .field("source", &self.source)
            .field("committed", &self.committed)
            .finish()
    }
}

impl SourceWriteTxn<'static> {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn new(scan_id: impl Into<String>, source: SourceId) -> Self {
        Self {
            scan_id: scan_id.into(),
            source,
            connection: None,
            ledger: None,
            committed: false,
            poisoned: false,
            data_changed: false,
        }
    }
}

impl<'a> SourceWriteTxn<'a> {
    /// Transitional Codex-only constructor used while the mature scanner
    /// algorithms are retained. The returned transaction is still
    /// source-bound and owns the only durable commit boundary; legacy
    /// algorithms receive only the inner rusqlite transaction and therefore
    /// cannot commit independently of SourceWriteTxn.
    pub(crate) fn begin_legacy_codex(
        scan_id: impl Into<String>,
        connection: &'a mut Connection,
    ) -> rusqlite::Result<Self> {
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok(Self {
            scan_id: scan_id.into(),
            source: SourceId::CODEX,
            connection: Some(SourceWriteConnection::Legacy(transaction)),
            ledger: None,
            committed: false,
            poisoned: false,
            data_changed: false,
        })
    }

    /// Borrow the transitional Codex transaction. This accessor is crate-only
    /// and cannot select a source; it exists solely so the current mature
    /// Codex private-state algorithms can be migrated without reimplementing
    /// their SQL/CAS rules.
    pub(crate) fn legacy_transaction(&self) -> Option<&Transaction<'_>> {
        match self.connection.as_ref()? {
            SourceWriteConnection::Legacy(transaction) => Some(transaction),
            SourceWriteConnection::Locked(_) => None,
        }
    }

    /// Commit a transitional Codex batch. Revision publication remains owned
    /// by the existing caller until that caller is moved fully onto the
    /// source-bound facade, but the SQLite COMMIT itself is owned here.
    pub(crate) fn commit_legacy(mut self) -> rusqlite::Result<()> {
        if self.committed || self.connection.is_none() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let connection = self.connection.take().ok_or(rusqlite::Error::InvalidQuery)?;
        match connection {
            SourceWriteConnection::Legacy(transaction) => transaction.commit()?,
            SourceWriteConnection::Locked(connection) => {
                connection.execute_batch("COMMIT")?;
            }
        }
        self.committed = true;
        Ok(())
    }

    pub fn scan_id(&self) -> &str {
        &self.scan_id
    }

    pub fn source(&self) -> &SourceId {
        &self.source
    }

    pub fn ensure_usage_epoch(&mut self) -> Result<(), SourceStorageError> {
        self.mutate(|transaction| transaction.ensure_usage_epoch_inner())
    }

    fn ensure_usage_epoch_inner(&mut self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        let source = self.source.as_str().to_owned();
        let connection = self.connection_mut()?;
        connection
            .execute(
                "INSERT INTO source_usage_epochs(
                    source,active_epoch,build_epoch,active_parser_version,build_parser_version)
                 VALUES(?1,0,NULL,0,NULL) ON CONFLICT(source) DO NOTHING",
                [source.as_str()],
            )
            .map_err(map_sql_error)?;
        Ok(())
    }

    pub fn upsert_session_metadata(
        &mut self,
        identity: &SessionIdentity,
        patch: &ResolvedThreadPatch,
    ) -> Result<(), SourceStorageError> {
        self.mutate(|transaction| transaction.upsert_session_metadata_inner(identity, patch))
    }

    fn upsert_session_metadata_inner(
        &mut self,
        identity: &SessionIdentity,
        patch: &ResolvedThreadPatch,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        if identity.source != self.source {
            return Err(SourceStorageError::SourceMismatch);
        }
        if identity.thread_id != patch.thread_id {
            return Err(SourceStorageError::InvalidRequest(
                "session identity and metadata patch thread ids differ".to_owned(),
            ));
        }
        if patch.source != self.source || patch.native_session_id != identity.native_session_id {
            return Err(SourceStorageError::InvalidRequest(
                "metadata patch attempts to change canonical session identity".to_owned(),
            ));
        }
        patch
            .validate()
            .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))?;
        let bound_source = self.source.as_str().to_owned();
        let connection = self.connection_mut()?;
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT source,native_session_id FROM threads WHERE thread_id=?1",
                [identity.thread_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sql_error)?;
        let before = read_session_visibility(connection, &identity.thread_id)?;
        if let Some((source, native)) = existing {
            if source != bound_source || native != identity.native_session_id {
                return Err(SourceStorageError::InvalidRequest(
                    "canonical session identity conflict".to_owned(),
                ));
            }
            update_session_row(connection, identity, patch)?;
        } else {
            let collision: Option<String> = connection
                .query_row(
                    "SELECT thread_id FROM threads WHERE source=?1 AND native_session_id=?2",
                    params![bound_source.as_str(), identity.native_session_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_sql_error)?;
            if collision.is_some_and(|thread_id| thread_id != identity.thread_id) {
                return Err(SourceStorageError::InvalidRequest(
                    "source/native session identity already maps to another thread".to_owned(),
                ));
            }
            insert_session_row(connection, identity, patch)?;
        }
        validate_session_relationships(connection, identity, patch)?;
        let after = read_session_visibility(connection, &identity.thread_id)?;
        if before != after {
            self.bump_data_revision()?;
        }
        Ok(())
    }

    pub fn write_usage(
        &mut self,
        target: UsageWriteTarget,
        event: CanonicalUsageEventWrite,
    ) -> Result<(), SourceStorageError> {
        self.mutate(|transaction| transaction.write_usage_inner(target, event).map(|_| ()))
    }

    fn write_usage_inner(
        &mut self,
        target: UsageWriteTarget,
        event: CanonicalUsageEventWrite,
    ) -> Result<bool, SourceStorageError> {
        self.require_open()?;
        event
            .usage
            .validate()
            .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))?;
        if event.event_id.trim().is_empty()
            || event.thread_id.trim().is_empty()
            || event.root_session_id.trim().is_empty()
            || event.model.trim().is_empty()
            || event.occurred_at_ms < 0
        {
            return Err(SourceStorageError::InvalidRequest(
                "canonical usage event contains an invalid identity or timestamp".to_owned(),
            ));
        }
        let epoch = self.resolve_usage_write_epoch(target)?;
        let bound_source = self.source.as_str().to_owned();
        let connection = self.connection_mut()?;
        validate_event_session_source(
            connection,
            &SourceId::new(bound_source.clone())
                .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))?,
            &event.thread_id,
            &event.root_session_id,
        )?;
        let quality = if event.usage.cache_write_tokens.is_some() {
            "complete"
        } else {
            "partial"
        };
        let created_at_ms = now_ms();
        let event_kind = match event.kind {
            EventKind::Normal => "normal",
            EventKind::Recovered => "recovered",
            EventKind::TurnCompensation => "turn_compensation",
        };
        let existing = connection
            .query_row(
                "SELECT event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                        reasoning_effort,input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                        reasoning_tokens,total_tokens,quality_status
                 FROM usage_events
                 WHERE source=?1 AND source_epoch=?2 AND event_id=?3",
                params![bound_source.as_str(), epoch, event.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, i64>(12)?,
                        row.get::<_, String>(13)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sql_error)?;
        if let Some(existing) = existing {
            let same = existing.0 == event_kind
                && existing.1 == event.occurred_at_ms
                && existing.2 == event.thread_id
                && existing.3 == event.root_session_id
                && existing.4 == event.turn_key
                && existing.5 == event.model
                && existing.6 == event.reasoning_effort
                && existing.7 == event.usage.input_tokens
                && existing.8 == event.usage.cached_tokens
                && existing.9 == event.usage.cache_write_tokens
                && existing.10 == event.usage.output_tokens
                && existing.11 == event.usage.reasoning_tokens
                && existing.12 == event.usage.total_tokens
                && existing.13 == quality;
            if !same {
                return Err(SourceStorageError::InvalidRequest(
                    "canonical usage event immutable payload conflict".to_owned(),
                ));
            }
            return Ok(false);
        }
        connection
            .execute(
                "INSERT INTO usage_events(
                    source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                    turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                    cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    bound_source.as_str(), epoch, event.event_id, event_kind, event.occurred_at_ms,
                    event.thread_id, event.root_session_id, event.turn_key, event.model,
                    event.reasoning_effort, event.estimated_cost_nanos_usd, event.usage.input_tokens,
                    event.usage.cached_tokens, event.usage.cache_write_tokens, event.usage.output_tokens,
                    event.usage.reasoning_tokens, event.usage.total_tokens, quality, created_at_ms,
                ],
            )
            .map_err(map_sql_error)?;
        if target == UsageWriteTarget::Active {
            self.bump_data_revision()?;
        }
        Ok(true)
    }

    pub fn begin_or_resume_usage_build(
        &mut self,
        parser_version: i64,
    ) -> Result<i64, SourceStorageError> {
        self.mutate(|transaction| transaction.begin_or_resume_usage_build_inner(parser_version))
    }

    fn begin_or_resume_usage_build_inner(
        &mut self,
        parser_version: i64,
    ) -> Result<i64, SourceStorageError> {
        self.require_open()?;
        if parser_version < 0 {
            return Err(SourceStorageError::InvalidRequest(
                "usage parser version must be non-negative".to_owned(),
            ));
        }
        self.ensure_usage_epoch()?;
        let bound_source = self.source.as_str().to_owned();
        let connection = self.connection_mut()?;
        let state: (i64, Option<i64>, i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version,build_parser_version
                 FROM source_usage_epochs WHERE source=?1",
                [bound_source.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(map_sql_error)?;
        match (state.1, state.3) {
            (Some(epoch), Some(existing_parser)) if existing_parser == parser_version => Ok(epoch),
            (Some(_), Some(_)) => Err(SourceStorageError::InvalidRequest(
                "a different usage build is already active for this source".to_owned(),
            )),
            (None, None) => {
                let build = state.0.checked_add(1).ok_or_else(|| {
                    SourceStorageError::InvalidRequest("usage epoch overflow".to_owned())
                })?;
                connection
                    .execute(
                        "UPDATE source_usage_epochs SET build_epoch=?1,build_parser_version=?2
                         WHERE source=?3 AND build_epoch IS NULL AND active_epoch=?4",
                        params![build, parser_version, bound_source.as_str(), state.0],
                    )
                    .map_err(map_sql_error)?;
                Ok(build)
            }
            _ => Err(SourceStorageError::InvalidRequest(
                "source usage epoch build columns are inconsistent".to_owned(),
            )),
        }
    }

    pub fn activate_usage_build(
        &mut self,
        expected_epoch: i64,
        expected_parser_version: i64,
    ) -> Result<(), SourceStorageError> {
        self.mutate(|transaction| {
            transaction.activate_usage_build_inner(expected_epoch, expected_parser_version)
        })
    }

    fn activate_usage_build_inner(
        &mut self,
        expected_epoch: i64,
        expected_parser_version: i64,
    ) -> Result<(), SourceStorageError> {
        self.require_open()?;
        let bound_source = self.source.as_str().to_owned();
        let connection = self.connection_mut()?;
        let current_epochs: (i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source=?1",
                [bound_source.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sql_error)?;
        let build_epoch = current_epochs.1.ok_or_else(|| {
            SourceStorageError::InvalidRequest("usage build epoch is not active".to_owned())
        })?;
        let visible_changed = !Self::canonical_epochs_equal(
            connection,
            bound_source.as_str(),
            current_epochs.0,
            build_epoch,
        )?;
        let changed = connection
            .execute(
                "UPDATE source_usage_epochs
                 SET active_epoch=build_epoch,active_parser_version=build_parser_version,
                     build_epoch=NULL,build_parser_version=NULL
                 WHERE source=?1 AND build_epoch=?2 AND build_parser_version=?3",
                params![
                    bound_source.as_str(),
                    expected_epoch,
                    expected_parser_version
                ],
            )
            .map_err(map_sql_error)?;
        if changed != 1 {
            return Err(SourceStorageError::InvalidRequest(
                "usage build activation CAS failed".to_owned(),
            ));
        }
        if visible_changed {
            // Keep the revision bump in the same transaction as activation so
            // readers cannot observe a new active epoch with an old revision.
            self.bump_data_revision()?;
        }
        Ok(())
    }

    pub fn resolve_usage_write_epoch(
        &mut self,
        target: UsageWriteTarget,
    ) -> Result<i64, SourceStorageError> {
        self.mutate(|transaction| transaction.resolve_usage_write_epoch_inner(target))
    }

    fn resolve_usage_write_epoch_inner(
        &mut self,
        target: UsageWriteTarget,
    ) -> Result<i64, SourceStorageError> {
        self.require_open()?;
        let bound_source = self.source.as_str().to_owned();
        let connection = self.connection_mut()?;
        let (active, build): (i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch FROM source_usage_epochs WHERE source=?1",
                [bound_source.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sql_error)?;
        match target {
            UsageWriteTarget::Active if active > 0 => Ok(active),
            UsageWriteTarget::Active => Err(SourceStorageError::InvalidRequest(
                "active usage epoch is not initialized".to_owned(),
            )),
            UsageWriteTarget::Build => build.ok_or_else(|| {
                SourceStorageError::InvalidRequest("usage build epoch is not active".to_owned())
            }),
        }
    }

    /// Run one source-private mutation inside this already source-bound
    /// `BEGIN IMMEDIATE` transaction.  The seam is crate-private on purpose:
    /// adapters cannot obtain the raw connection or select another source,
    /// while Codex provenance/checkpoints and test-only private state can
    /// participate in the same atomic commit.
    #[allow(dead_code)]
    pub(crate) fn with_private_state<T>(
        &mut self,
        operation: impl FnOnce(&rusqlite::Connection) -> Result<T, SourceStorageError>,
    ) -> Result<T, SourceStorageError> {
        self.mutate(|transaction| {
            let connection = transaction.connection_mut()?;
            operation(connection)
        })
    }

    fn bump_data_revision(&mut self) -> Result<(), SourceStorageError> {
        let connection = self.connection_mut()?;
        let current: i64 = connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(map_sql_error)?;
        let next = current.checked_add(1).ok_or_else(|| {
            SourceStorageError::InvalidRequest("data revision overflow".to_owned())
        })?;
        let changed = connection
            .execute(
                "UPDATE app_meta SET data_revision=?1 WHERE id=1 AND data_revision=?2",
                params![next, current],
            )
            .map_err(map_sql_error)?;
        if changed != 1 {
            return Err(SourceStorageError::InvalidRequest(
                "app meta revision changed during source write".to_owned(),
            ));
        }
        self.data_changed = true;
        Ok(())
    }

    fn canonical_epochs_equal(
        connection: &rusqlite::Connection,
        source: &str,
        active_epoch: i64,
        build_epoch: i64,
    ) -> Result<bool, SourceStorageError> {
        let columns = "event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                       reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                       cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status";
        let sql = format!(
            "SELECT NOT EXISTS(
                 SELECT {columns} FROM usage_events
                 WHERE source=?1 AND source_epoch=?2
                 EXCEPT
                 SELECT {columns} FROM usage_events
                 WHERE source=?1 AND source_epoch=?3
             ) AND NOT EXISTS(
                 SELECT {columns} FROM usage_events
                 WHERE source=?1 AND source_epoch=?3
                 EXCEPT
                 SELECT {columns} FROM usage_events
                 WHERE source=?1 AND source_epoch=?2
             )"
        );
        let equal: i64 = connection
            .query_row(&sql, params![source, active_epoch, build_epoch], |row| {
                row.get(0)
            })
            .map_err(map_sql_error)?;
        Ok(equal != 0)
    }

    pub fn commit(mut self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        let revisions = if self.data_changed {
            let connection = self.connection_mut()?;
            Some(
                connection
                    .query_row(
                        "SELECT data_revision,status_revision FROM app_meta WHERE id=1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(map_sql_error)?,
            )
        } else {
            None
        };
        let connection = self
            .connection
            .take()
            .ok_or(SourceStorageError::TransactionClosed)?;
        let commit_result = match connection {
            SourceWriteConnection::Locked(connection) => connection.execute_batch("COMMIT"),
            SourceWriteConnection::Legacy(transaction) => transaction.commit(),
        };
        commit_result.map_err(|error| {
            self.poisoned = true;
            map_sql_error(error)
        })?;
        self.committed = true;
        if let (Some(ledger), Some((data_revision, status_revision))) =
            (self.ledger.as_ref(), revisions)
        {
            ledger.publish_revisions(data_revision, status_revision);
        }
        Ok(())
    }

    fn require_open(&self) -> Result<(), SourceStorageError> {
        if self.poisoned {
            Err(SourceStorageError::TransactionPoisoned)
        } else if self.committed || self.connection.is_none() {
            Err(SourceStorageError::TransactionClosed)
        } else {
            Ok(())
        }
    }

    fn mutate<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, SourceStorageError>,
    ) -> Result<T, SourceStorageError> {
        self.require_open()?;
        let result = operation(self);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn connection_mut(&mut self) -> Result<&Connection, SourceStorageError> {
        self.connection
            .as_mut()
            .map(SourceWriteConnection::connection_mut)
            .ok_or(SourceStorageError::TransactionClosed)
    }
}

impl Drop for SourceWriteTxn<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(connection) = self.connection.as_mut() {
            match connection {
                SourceWriteConnection::Locked(connection) => {
                    let _ = connection.execute_batch("ROLLBACK");
                }
                SourceWriteConnection::Legacy(_) => {
                    // rusqlite::Transaction rolls back on drop.
                }
            }
        }
    }
}

fn map_sql_error(error: rusqlite::Error) -> SourceStorageError {
    SourceStorageError::Storage(crate::storage::StorageError::sqlite(error).kind())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionVisibility {
    source: String,
    native_session_id: String,
    parent_thread_id: Option<String>,
    root_session_id: Option<String>,
    agent_role: String,
    title: Option<String>,
    project_name: Option<String>,
    project_path: Option<String>,
    project_kind: String,
    metadata_model: Option<String>,
    created_at_ms: Option<i64>,
    updated_at_ms: Option<i64>,
    archived: i64,
    metadata_quality_status: String,
    metadata_resolved_at_ms: i64,
}

fn read_session_visibility(
    connection: &rusqlite::Connection,
    thread_id: &str,
) -> Result<Option<SessionVisibility>, SourceStorageError> {
    connection
        .query_row(
            "SELECT source,native_session_id,parent_thread_id,root_session_id,agent_role,
                    title,project_name,project_path,project_kind,metadata_model,
                    created_at_ms,updated_at_ms,archived,metadata_quality_status,
                    metadata_resolved_at_ms
             FROM threads WHERE thread_id=?1",
            [thread_id],
            |row| {
                Ok(SessionVisibility {
                    source: row.get(0)?,
                    native_session_id: row.get(1)?,
                    parent_thread_id: row.get(2)?,
                    root_session_id: row.get(3)?,
                    agent_role: row.get(4)?,
                    title: row.get(5)?,
                    project_name: row.get(6)?,
                    project_path: row.get(7)?,
                    project_kind: row.get(8)?,
                    metadata_model: row.get(9)?,
                    created_at_ms: row.get(10)?,
                    updated_at_ms: row.get(11)?,
                    archived: row.get(12)?,
                    metadata_quality_status: row.get(13)?,
                    metadata_resolved_at_ms: row.get(14)?,
                })
            },
        )
        .optional()
        .map_err(map_sql_error)
}

fn update_session_row(
    connection: &rusqlite::Connection,
    identity: &SessionIdentity,
    patch: &ResolvedThreadPatch,
) -> Result<(), SourceStorageError> {
    let mut updates = Vec::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    macro_rules! add_optional {
        ($column:literal, $patch:expr) => {
            match $patch {
                Patch::Keep => {}
                Patch::Set(value) => {
                    updates.push(concat!($column, "=?"));
                    values.push(rusqlite::types::Value::Text(value.clone()));
                }
                Patch::Clear => {
                    updates.push(concat!($column, "=NULL"));
                }
            }
        };
    }
    macro_rules! add_required {
        ($column:literal, $patch:expr, $value:expr) => {
            match $patch {
                Patch::Keep => {}
                Patch::Set(_) => {
                    updates.push(concat!($column, "=?"));
                    values.push($value);
                }
                Patch::Clear => {}
            }
        };
    }
    add_optional!("parent_thread_id", &patch.parent_thread_id);
    add_optional!("root_session_id", &patch.root_session_id);
    add_optional!("title", &patch.title);
    add_optional!("project_name", &patch.project_name);
    add_optional!("project_path", &patch.project_path);
    add_optional!("metadata_model", &patch.metadata_model);
    add_required!(
        "agent_role",
        &patch.agent_role,
        rusqlite::types::Value::Text(match &patch.agent_role {
            Patch::Set(value) => value.as_str().to_owned(),
            _ => unreachable!(),
        })
    );
    add_required!(
        "project_kind",
        &patch.project_kind,
        rusqlite::types::Value::Text(match &patch.project_kind {
            Patch::Set(value) => value.as_str().to_owned(),
            _ => unreachable!(),
        })
    );
    add_required!(
        "archived",
        &patch.archived,
        rusqlite::types::Value::Integer(match patch.archived {
            Patch::Set(value) => i64::from(value),
            _ => unreachable!(),
        })
    );
    if !patch.created_at_ms.is_keep() {
        match patch.created_at_ms {
            Patch::Set(value) => {
                updates.push("created_at_ms=?");
                values.push(rusqlite::types::Value::Integer(value));
            }
            Patch::Clear => updates.push("created_at_ms=NULL"),
            Patch::Keep => unreachable!(),
        }
    }
    if !patch.updated_at_ms.is_keep() {
        match patch.updated_at_ms {
            Patch::Set(value) => {
                updates.push("updated_at_ms=?");
                values.push(rusqlite::types::Value::Integer(value));
            }
            Patch::Clear => updates.push("updated_at_ms=NULL"),
            Patch::Keep => unreachable!(),
        }
    }
    updates.push("metadata_quality_status=?");
    values.push(rusqlite::types::Value::Text(
        patch.metadata_quality_status.as_str().to_owned(),
    ));
    updates.push("metadata_resolved_at_ms=?");
    values.push(rusqlite::types::Value::Integer(patch.resolved_at_ms));
    let mut sql = format!("UPDATE threads SET {}", updates.join(","));
    let mut bind = values;
    sql.push_str(" WHERE thread_id=?");
    bind.push(rusqlite::types::Value::Text(identity.thread_id.clone()));
    connection
        .execute(&sql, rusqlite::params_from_iter(bind))
        .map_err(map_sql_error)?;
    Ok(())
}

fn insert_session_row(
    connection: &rusqlite::Connection,
    identity: &SessionIdentity,
    patch: &ResolvedThreadPatch,
) -> Result<(), SourceStorageError> {
    let role = match &patch.agent_role {
        Patch::Set(role) => role.as_str(),
        _ => "unknown",
    };
    let project_kind = match &patch.project_kind {
        Patch::Set(kind) => kind.as_str(),
        _ => "unknown",
    };
    let archived = match patch.archived {
        Patch::Set(value) => i64::from(value),
        _ => 0,
    };
    let root = match &patch.root_session_id {
        Patch::Set(value) => Some(value.as_str()),
        _ if role == "main" => Some(identity.thread_id.as_str()),
        _ => None,
    };
    let parent = match &patch.parent_thread_id {
        Patch::Set(value) => Some(value.as_str()),
        _ => None,
    };
    connection
        .execute(
            "INSERT INTO threads(
                thread_id,source,native_session_id,parent_thread_id,root_session_id,agent_role,
                title,project_name,project_path,project_kind,metadata_model,created_at_ms,updated_at_ms,
                archived,metadata_quality_status,metadata_resolved_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                identity.thread_id,
                identity.source.as_str(),
                identity.native_session_id,
                parent,
                root,
                role,
                option_patch(&patch.title),
                option_patch(&patch.project_name),
                option_patch(&patch.project_path),
                project_kind,
                option_patch(&patch.metadata_model),
                option_i64_patch(&patch.created_at_ms),
                option_i64_patch(&patch.updated_at_ms),
                archived,
                patch.metadata_quality_status.as_str(),
                patch.resolved_at_ms,
            ],
        )
        .map_err(map_sql_error)?;
    Ok(())
}

fn option_patch(patch: &Patch<String>) -> Option<&str> {
    match patch {
        Patch::Set(value) => Some(value.as_str()),
        Patch::Keep | Patch::Clear => None,
    }
}

fn option_i64_patch(patch: &Patch<i64>) -> Option<i64> {
    match patch {
        Patch::Set(value) => Some(*value),
        Patch::Keep | Patch::Clear => None,
    }
}

fn validate_session_relationships(
    connection: &rusqlite::Connection,
    identity: &SessionIdentity,
    patch: &ResolvedThreadPatch,
) -> Result<(), SourceStorageError> {
    for related in [&patch.parent_thread_id, &patch.root_session_id] {
        let Some(id) = (match related {
            Patch::Set(value) => Some(value.as_str()),
            Patch::Keep | Patch::Clear => None,
        }) else {
            continue;
        };
        let source: Option<String> = connection
            .query_row(
                "SELECT source FROM threads WHERE thread_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql_error)?;
        if source.as_deref() != Some(identity.source.as_str()) {
            return Err(SourceStorageError::InvalidRequest(
                "session parent/root source mismatch".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_event_session_source(
    connection: &rusqlite::Connection,
    source: &SourceId,
    thread_id: &str,
    root_session_id: &str,
) -> Result<(), SourceStorageError> {
    for id in [thread_id, root_session_id] {
        let row: Option<String> = connection
            .query_row(
                "SELECT source FROM threads WHERE thread_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sql_error)?;
        if row.as_deref() != Some(source.as_str()) {
            return Err(SourceStorageError::InvalidRequest(
                "canonical usage source/session mismatch".to_owned(),
            ));
        }
    }
    Ok(())
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
    use crate::domain::{AgentRole, Patch, ProjectKind};
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

    fn main_identity_patch(
        source: &SourceId,
        thread_id: &str,
        native_session_id: &str,
    ) -> (SessionIdentity, ResolvedThreadPatch) {
        let identity = SessionIdentity::new(thread_id, source.clone(), native_session_id).unwrap();
        let mut patch = ResolvedThreadPatch::new(thread_id, 1).unwrap();
        patch.source = source.clone();
        patch.native_session_id = native_session_id.to_owned();
        patch.agent_role = Patch::Set(AgentRole::Main);
        patch.root_session_id = Patch::Set(thread_id.to_owned());
        patch.project_kind = Patch::Set(ProjectKind::Unknown);
        (identity, patch)
    }

    fn usage_event(event_id: &str) -> CanonicalUsageEventWrite {
        CanonicalUsageEventWrite {
            event_id: event_id.to_owned(),
            kind: EventKind::Normal,
            occurred_at_ms: 10,
            thread_id: "fake-thread".to_owned(),
            root_session_id: "fake-thread".to_owned(),
            turn_key: Some("turn-1".to_owned()),
            model: "model-a".to_owned(),
            reasoning_effort: Some("medium".to_owned()),
            estimated_cost_nanos_usd: Some(1),
            usage: NormalizedTokenUsage::new(10, 2, Some(1), 4, 1, 14).unwrap(),
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
    fn source_bound_epoch_mutations_are_atomic_and_bootstrap_from_zero() {
        let temp = TempPath::new("epoch");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let storage = SourceStorage::with_ledger("scan-1", SourceId::CODEX, ledger);
        let epoch = storage.load_usage_epoch().unwrap().unwrap();
        assert_eq!(epoch.active_epoch, 0);
        assert_eq!(epoch.build_epoch, None);

        {
            let mut txn = storage.begin_write_txn().unwrap();
            txn.ensure_usage_epoch().unwrap();
            assert_eq!(txn.begin_or_resume_usage_build(7).unwrap(), 1);
            assert_eq!(
                txn.resolve_usage_write_epoch(UsageWriteTarget::Build)
                    .unwrap(),
                1
            );
            assert!(matches!(
                txn.resolve_usage_write_epoch(UsageWriteTarget::Active),
                Err(SourceStorageError::InvalidRequest(message)) if message.contains("not initialized")
            ));
            // No commit: both the bootstrap row and build marker must roll back.
        }
        let rolled_back = storage.load_usage_epoch().unwrap().unwrap();
        assert_eq!(rolled_back.active_epoch, 0);
        assert_eq!(rolled_back.build_epoch, None);

        let mut txn = storage.begin_write_txn().unwrap();
        txn.ensure_usage_epoch().unwrap();
        assert_eq!(txn.begin_or_resume_usage_build(7).unwrap(), 1);
        txn.activate_usage_build(1, 7).unwrap();
        txn.commit().unwrap();
        let active = storage.load_usage_epoch().unwrap().unwrap();
        assert_eq!(
            (
                active.active_epoch,
                active.active_parser_version,
                active.build_epoch,
                active.build_parser_version,
            ),
            (1, 7, None, None)
        );
    }

    #[test]
    fn s01_sessions_allow_cross_source_native_collision_but_reject_same_source_remap() {
        let temp = TempPath::new("s01");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let codex = SourceStorage::with_ledger("scan-codex", SourceId::CODEX, Arc::clone(&ledger));
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake =
            SourceStorage::with_ledger("scan-fake", fake_source.clone(), Arc::clone(&ledger));
        let (codex_identity, codex_patch) =
            main_identity_patch(&SourceId::CODEX, "codex-thread", "native-x");
        let (fake_identity, fake_patch) =
            main_identity_patch(&fake_source, "fake-thread", "native-x");
        let mut codex_tx = codex.begin_write_txn().unwrap();
        codex_tx
            .upsert_session_metadata(&codex_identity, &codex_patch)
            .unwrap();
        codex_tx.commit().unwrap();
        let after_codex_insert: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_codex_insert, 1);
        let mut fake_tx = fake.begin_write_txn().unwrap();
        fake_tx
            .upsert_session_metadata(&fake_identity, &fake_patch)
            .unwrap();
        fake_tx.commit().unwrap();
        let after_fake_insert: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_fake_insert, 2);

        let mut repeat_tx = codex.begin_write_txn().unwrap();
        repeat_tx
            .upsert_session_metadata(&codex_identity, &codex_patch)
            .unwrap();
        repeat_tx.commit().unwrap();
        let after_repeat: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_repeat, after_fake_insert);

        let mut metadata_change = codex_patch.clone();
        metadata_change.title = Patch::Set("metadata-only change".to_owned());
        let mut metadata_tx = codex.begin_write_txn().unwrap();
        metadata_tx
            .upsert_session_metadata(&codex_identity, &metadata_change)
            .unwrap();
        metadata_tx.commit().unwrap();
        let after_metadata_change: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_metadata_change, after_repeat + 1);

        let remap = SessionIdentity::new("other-thread", fake_source.clone(), "native-x").unwrap();
        let remap_patch = main_identity_patch(&fake_source, "other-thread", "native-x").1;
        let mut tx = fake.begin_write_txn().unwrap();
        assert!(matches!(
            tx.upsert_session_metadata(&remap, &remap_patch),
            Err(SourceStorageError::InvalidRequest(message))
                if message.contains("already maps")
        ));
    }

    #[test]
    fn s02_session_parent_and_root_must_stay_within_source() {
        let temp = TempPath::new("s02");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let codex = SourceStorage::with_ledger("scan-codex", SourceId::CODEX, Arc::clone(&ledger));
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake = SourceStorage::with_ledger("scan-fake", fake_source.clone(), ledger);
        let (root_identity, root_patch) =
            main_identity_patch(&SourceId::CODEX, "codex-root", "root");
        let mut root_tx = codex.begin_write_txn().unwrap();
        root_tx
            .upsert_session_metadata(&root_identity, &root_patch)
            .unwrap();
        root_tx.commit().unwrap();

        let child_identity =
            SessionIdentity::new("fake-child", fake_source.clone(), "child").unwrap();
        let mut child_patch = ResolvedThreadPatch::new("fake-child", 1).unwrap();
        child_patch.source = fake_source;
        child_patch.native_session_id = "child".to_owned();
        child_patch.agent_role = Patch::Set(AgentRole::Subagent);
        child_patch.parent_thread_id = Patch::Set("codex-root".to_owned());
        child_patch.root_session_id = Patch::Set("codex-root".to_owned());
        assert!(matches!(
            fake.begin_write_txn()
                .unwrap()
                .upsert_session_metadata(&child_identity, &child_patch),
            Err(SourceStorageError::InvalidRequest(message))
                if message.contains("source mismatch")
        ));
    }

    #[test]
    fn s03_fake_source_can_write_canonical_usage_without_codex_provenance() {
        let temp = TempPath::new("s03");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake =
            SourceStorage::with_ledger("scan-fake", fake_source.clone(), Arc::clone(&ledger));
        let (identity, patch) = main_identity_patch(&fake_source, "fake-thread", "native");
        let before_revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut tx = fake.begin_write_txn().unwrap();
        tx.ensure_usage_epoch().unwrap();
        assert_eq!(tx.begin_or_resume_usage_build(1).unwrap(), 1);
        tx.upsert_session_metadata(&identity, &patch).unwrap();
        tx.write_usage(UsageWriteTarget::Build, usage_event("fake-event"))
            .unwrap();
        tx.activate_usage_build(1, 1).unwrap();
        tx.commit().unwrap();
        let connection = ledger.connection().unwrap();
        let after_revision: i64 = connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_revision, before_revision + 2);
        let row: (String, i64) = connection
            .query_row(
                "SELECT source,source_epoch FROM usage_events WHERE event_id='fake-event'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("fake-source".to_owned(), 1));
        let mut statement = connection
            .prepare("PRAGMA table_info(usage_events)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "source_file_id"));
    }

    #[test]
    fn s04_source_write_transaction_rolls_back_epoch_session_and_usage_on_error() {
        let temp = TempPath::new("s04");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake =
            SourceStorage::with_ledger("scan-fake", fake_source.clone(), Arc::clone(&ledger));
        let (identity, patch) = main_identity_patch(&fake_source, "fake-thread", "native");
        {
            let mut tx = fake.begin_write_txn().unwrap();
            tx.ensure_usage_epoch().unwrap();
            tx.begin_or_resume_usage_build(1).unwrap();
            tx.upsert_session_metadata(&identity, &patch).unwrap();
            tx.write_usage(UsageWriteTarget::Build, usage_event("rollback-event"))
                .unwrap();
            let mut conflict = usage_event("rollback-event");
            conflict.model = "different-model".to_owned();
            assert!(tx.write_usage(UsageWriteTarget::Build, conflict).is_err());
            // Ignoring the mutation error must not make a partial batch
            // committable; the transaction is poisoned and rolls back all
            // prior epoch/session/event writes.
            assert!(matches!(
                tx.commit(),
                Err(SourceStorageError::TransactionPoisoned)
            ));
        }
        let connection = ledger.connection().unwrap();
        let epoch: Option<i64> = connection
            .query_row(
                "SELECT active_epoch FROM source_usage_epochs WHERE source='fake-source'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(epoch, None);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM threads WHERE source='fake-source'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM usage_events WHERE source='fake-source'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(connection);

        // The same rollback guarantee covers adapter-private state executed
        // through the crate-private transaction seam.
        {
            let mut tx = fake.begin_write_txn().unwrap();
            tx.ensure_usage_epoch().unwrap();
            tx.begin_or_resume_usage_build(1).unwrap();
            tx.upsert_session_metadata(&identity, &patch).unwrap();
            tx.write_usage(UsageWriteTarget::Build, usage_event("private-rollback"))
                .unwrap();
            let private_failure = tx.with_private_state(|connection| {
                connection
                    .execute(
                        "UPDATE app_meta SET status_revision=status_revision+1
                         WHERE id=1",
                        [],
                    )
                    .map_err(map_sql_error)?;
                Err::<(), _>(SourceStorageError::InvalidRequest(
                    "PRIVATE_STATE_CAS_FAILED".to_owned(),
                ))
            });
            assert!(matches!(
                private_failure,
                Err(SourceStorageError::InvalidRequest(message))
                    if message == "PRIVATE_STATE_CAS_FAILED"
            ));
            assert!(matches!(
                tx.commit(),
                Err(SourceStorageError::TransactionPoisoned)
            ));
        }
        let connection = ledger.connection().unwrap();
        let private_epoch: Option<i64> = connection
            .query_row(
                "SELECT active_epoch FROM source_usage_epochs WHERE source='fake-source'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(private_epoch, None);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM usage_events
                     WHERE source='fake-source' AND event_id='private-rollback'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn s05_s06_epoch_build_and_activation_are_source_local() {
        let temp = TempPath::new("s05-s06");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let codex = SourceStorage::with_ledger("scan-codex", SourceId::CODEX, Arc::clone(&ledger));
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake =
            SourceStorage::with_ledger("scan-fake", fake_source.clone(), Arc::clone(&ledger));
        let before_revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let bootstrap =
            |storage: &SourceStorage, source: &SourceId, thread_id: &str, event_id: &str| {
                let mut tx = storage.begin_write_txn().unwrap();
                tx.ensure_usage_epoch().unwrap();
                assert_eq!(tx.begin_or_resume_usage_build(1).unwrap(), 1);
                let (identity, patch) = main_identity_patch(source, thread_id, thread_id);
                tx.upsert_session_metadata(&identity, &patch).unwrap();
                let mut event = usage_event(event_id);
                event.thread_id = thread_id.to_owned();
                event.root_session_id = thread_id.to_owned();
                tx.write_usage(UsageWriteTarget::Build, event).unwrap();
                tx.activate_usage_build(1, 1).unwrap();
                tx.commit().unwrap();
            };
        bootstrap(&codex, &SourceId::CODEX, "codex-s05", "codex-s05-event");
        bootstrap(&fake, &fake_source, "fake-s05", "fake-s05-event");
        let after_bootstrap_revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after_bootstrap_revision, before_revision + 4);
        let mut codex_tx = codex.begin_write_txn().unwrap();
        assert_eq!(codex_tx.begin_or_resume_usage_build(2).unwrap(), 2);
        codex_tx.commit().unwrap();
        let codex_build = codex.load_usage_epoch().unwrap().unwrap();
        let fake_active = fake.load_usage_epoch().unwrap().unwrap();
        assert_eq!(
            (codex_build.active_epoch, codex_build.build_epoch),
            (1, Some(2))
        );
        assert_eq!(
            (fake_active.active_epoch, fake_active.build_epoch),
            (1, None)
        );
        let mut codex_tx = codex.begin_write_txn().unwrap();
        codex_tx.activate_usage_build(2, 2).unwrap();
        codex_tx.commit().unwrap();
        let after_empty_activation_revision: i64 = ledger
            .connection()
            .unwrap()
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            after_empty_activation_revision,
            after_bootstrap_revision + 1
        );
        let codex_active = codex.load_usage_epoch().unwrap().unwrap();
        let fake_unchanged = fake.load_usage_epoch().unwrap().unwrap();
        assert_eq!(
            (
                codex_active.active_epoch,
                codex_active.active_parser_version
            ),
            (2, 2)
        );
        assert_eq!(
            (
                fake_unchanged.active_epoch,
                fake_unchanged.active_parser_version
            ),
            (1, 1)
        );
    }

    #[test]
    fn s08_source_bound_transaction_rejects_identity_from_another_source() {
        let temp = TempPath::new("s08");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake = SourceStorage::with_ledger("scan-fake", fake_source, ledger);
        let (identity, patch) = main_identity_patch(&SourceId::CODEX, "codex-thread", "native");
        let mut tx = fake.begin_write_txn().unwrap();
        tx.ensure_usage_epoch().unwrap();
        assert!(matches!(
            tx.upsert_session_metadata(&identity, &patch),
            Err(SourceStorageError::SourceMismatch)
        ));
    }

    #[test]
    fn s10_duplicate_ignores_cost_and_creation_time_but_rejects_immutable_change() {
        let temp = TempPath::new("s10");
        let home = temp.path().join("codex");
        fs::create_dir_all(&home).unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(temp.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let fake_source = SourceId::new("fake-source").unwrap();
        let fake =
            SourceStorage::with_ledger("scan-fake", fake_source.clone(), Arc::clone(&ledger));
        let (identity, patch) = main_identity_patch(&fake_source, "fake-thread", "native");
        let mut tx = fake.begin_write_txn().unwrap();
        tx.ensure_usage_epoch().unwrap();
        tx.begin_or_resume_usage_build(1).unwrap();
        tx.upsert_session_metadata(&identity, &patch).unwrap();
        tx.write_usage(UsageWriteTarget::Build, usage_event("duplicate-event"))
            .unwrap();
        tx.activate_usage_build(1, 1).unwrap();
        tx.commit().unwrap();

        let mut duplicate = usage_event("duplicate-event");
        duplicate.estimated_cost_nanos_usd = Some(99);
        let mut tx = fake.begin_write_txn().unwrap();
        tx.write_usage(UsageWriteTarget::Active, duplicate).unwrap();
        tx.commit().unwrap();
        let connection = ledger.connection().unwrap();
        let cost: Option<i64> = connection
            .query_row(
                "SELECT estimated_cost_nanos_usd FROM usage_events
                 WHERE source='fake-source' AND source_epoch=1 AND event_id='duplicate-event'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cost, Some(1));
        drop(connection);

        let mut conflict = usage_event("duplicate-event");
        conflict.model = "different-model".to_owned();
        let mut tx = fake.begin_write_txn().unwrap();
        assert!(tx.write_usage(UsageWriteTarget::Active, conflict).is_err());
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
        assert!(storage.load_usage_epoch().unwrap().is_some());
    }
}
