//! Source-neutral adapter context and source-bound canonical transaction.
//!
//! A source adapter receives a `SourceRunContext`; all durable writes then
//! flow through the source-fixed `SourceStorage`/`SourceWriteTxn` seam.  The
//! Codex implementation uses the private-state callback exposed here, while
//! canonical tables, usage epochs, and revisions remain owned by this module.

use std::{
    fmt,
    sync::{Arc, MutexGuard, atomic::AtomicBool},
};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    domain::{Patch, ResolvedThreadPatch, SessionIdentity, SourceUsageEpochState},
    storage::{Ledger, RevisionPublisher, StorageError, StorageErrorKind},
    usage::{NormalizedTokenUsage, event::EventKind},
};

use super::{SourceDescriptor, SourceId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterAvailability {
    Available,
    Unavailable(String),
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

pub type SourceRunResult = Result<(), SourceAdapterError>;

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

pub trait SourceAdapter: Send + Sync + 'static {
    fn descriptor(&self) -> &SourceDescriptor;
    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError>;
    fn run_scan(&self, context: &SourceRunContext, cancellation: &AtomicBool) -> SourceRunResult;
}

impl<T> SourceAdapter for Arc<T>
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

#[derive(Clone, Debug)]
pub struct SourceRunContext {
    scan_id: String,
    source: SourceId,
    storage: SourceStorage,
}

impl SourceRunContext {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceStorageError {
    NotImplemented,
    TransactionClosed,
    TransactionPoisoned,
    SourceMismatch,
    Storage(StorageErrorKind),
    UnsupportedOperation(&'static str),
    InvalidRequest(String),
}

impl fmt::Display for SourceStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotImplemented => formatter.write_str("source storage is unavailable"),
            Self::TransactionClosed => formatter.write_str("source write transaction is closed"),
            Self::TransactionPoisoned => {
                formatter.write_str("source write transaction is poisoned")
            }
            Self::SourceMismatch => formatter.write_str("source-bound storage mismatch"),
            Self::Storage(kind) => write!(formatter, "storage operation failed: {kind:?}"),
            Self::UnsupportedOperation(operation) => {
                write!(formatter, "unsupported source operation: {operation}")
            }
            Self::InvalidRequest(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SourceStorageError {}

#[derive(Clone)]
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
            .finish_non_exhaustive()
    }
}

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

    pub fn load_usage_epoch(&self) -> Result<Option<SourceUsageEpochState>, SourceStorageError> {
        let connection = self.connection()?;
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
            .map_err(map_sql_error)?;
        row.map(|(active, build, parser, build_parser)| {
            SourceUsageEpochState::new(self.source.clone(), active, build, parser, build_parser)
                .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))
        })
        .transpose()
    }

    pub fn begin_write_txn(&self) -> Result<SourceWriteTxn<'_>, SourceStorageError> {
        let mut connection = self.connection()?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(map_sql_error)?;
        Ok(SourceWriteTxn {
            scan_id: self.scan_id.clone(),
            source: self.source.clone(),
            connection: Some(SourceWriteConnection::Locked(connection)),
            revision_publisher: self
                .ledger
                .as_ref()
                .map(|ledger| ledger.revision_publisher()),
            committed: false,
            poisoned: false,
            data_revision_bumped: false,
            status_revision_bumped: false,
        })
    }

    pub(crate) fn with_private_read<T, E>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<SourceStorageError>,
    {
        let connection = self.connection().map_err(E::from)?;
        let mut guard = QueryOnlyGuard::new(&connection).map_err(E::from)?;
        let result = operation(&connection);
        let restore = guard.restore();
        match restore {
            Ok(()) => result,
            Err(error) => Err(E::from(error)),
        }
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, SourceStorageError> {
        self.ledger
            .as_ref()
            .ok_or(SourceStorageError::NotImplemented)?
            .connection()
            .map_err(|error| SourceStorageError::Storage(error.kind()))
    }
}

/// Guard that makes a read closure physically read-only.  Normal return paths
/// call `restore`; Drop is only a best-effort panic/unwind fallback.
struct QueryOnlyGuard<'a> {
    connection: &'a Connection,
    previous: bool,
    armed: bool,
}

impl<'a> QueryOnlyGuard<'a> {
    fn new(connection: &'a Connection) -> Result<Self, SourceStorageError> {
        let previous: i64 = connection
            .pragma_query_value(None, "query_only", |row| row.get(0))
            .map_err(map_sql_error)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(map_sql_error)?;
        Ok(Self {
            connection,
            previous: previous != 0,
            armed: true,
        })
    }

    fn restore(&mut self) -> Result<(), SourceStorageError> {
        if self.armed {
            self.connection
                .pragma_update(None, "query_only", self.previous)
                .map_err(map_sql_error)?;
            self.armed = false;
        }
        Ok(())
    }
}

impl Drop for QueryOnlyGuard<'_> {
    fn drop(&mut self) {
        if self.armed && std::thread::panicking() {
            let _ = self
                .connection
                .pragma_update(None, "query_only", self.previous);
            self.armed = false;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CanonicalWriteOutcome {
    Inserted,
    Duplicate,
}

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
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionMutationOutcome {
    pub visible_changed: bool,
    pub previous_root_session_id: Option<String>,
    pub next_root_session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UsageActivationOutcome {
    pub active_epoch: i64,
    pub data_revision: i64,
    pub visible_changed: bool,
}

enum SourceWriteConnection<'a> {
    Locked(MutexGuard<'a, Connection>),
}

impl SourceWriteConnection<'_> {
    fn connection(&self) -> &Connection {
        match self {
            Self::Locked(connection) => connection,
        }
    }
}

pub struct SourceWriteTxn<'a> {
    scan_id: String,
    source: SourceId,
    connection: Option<SourceWriteConnection<'a>>,
    revision_publisher: Option<RevisionPublisher>,
    committed: bool,
    poisoned: bool,
    data_revision_bumped: bool,
    status_revision_bumped: bool,
}

impl fmt::Debug for SourceWriteTxn<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceWriteTxn")
            .field("scan_id", &self.scan_id)
            .field("source", &self.source)
            .field("committed", &self.committed)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl SourceWriteTxn<'_> {
    pub(crate) fn source(&self) -> &SourceId {
        &self.source
    }

    pub(crate) fn with_private_state<T, E>(
        &mut self,
        operation: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<SourceStorageError>,
    {
        self.require_open().map_err(E::from)?;
        let result = operation(self.connection().map_err(E::from)?.connection());
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    pub(crate) fn upsert_session_metadata_no_revision(
        &mut self,
        identity: &SessionIdentity,
        patch: &ResolvedThreadPatch,
    ) -> Result<SessionMutationOutcome, SourceStorageError> {
        self.require_open()?;
        if identity.source != self.source
            || patch.source != self.source
            || identity.thread_id != patch.thread_id
            || identity.native_session_id != patch.native_session_id
        {
            return Err(SourceStorageError::SourceMismatch);
        }
        patch
            .validate()
            .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))?;
        let connection = self.connection()?.connection();
        let before = read_session_visibility(connection, &identity.thread_id)?;
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT source,native_session_id FROM threads WHERE thread_id=?1",
                [identity.thread_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(map_sql_error)?;
        if let Some((source, native)) = existing {
            if source != self.source.as_str() || native != identity.native_session_id {
                return Err(SourceStorageError::InvalidRequest(
                    "canonical session identity conflict".to_owned(),
                ));
            }
            update_session_row(connection, identity, patch)?;
        } else {
            let collision: Option<String> = connection
                .query_row(
                    "SELECT thread_id FROM threads WHERE source=?1 AND native_session_id=?2",
                    params![self.source.as_str(), identity.native_session_id.as_str()],
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
        Ok(SessionMutationOutcome {
            visible_changed: before != after,
            previous_root_session_id: before.and_then(|row| row.root_session_id),
            next_root_session_id: after.and_then(|row| row.root_session_id),
        })
    }

    pub fn upsert_session_metadata(
        &mut self,
        identity: &SessionIdentity,
        patch: &ResolvedThreadPatch,
    ) -> Result<(), SourceStorageError> {
        let outcome = self.upsert_session_metadata_no_revision(identity, patch)?;
        if outcome.visible_changed {
            self.bump_data_revision()?;
        }
        Ok(())
    }

    pub(crate) fn write_usage_no_revision(
        &mut self,
        target: UsageWriteTarget,
        event: CanonicalUsageEventWrite,
    ) -> Result<CanonicalWriteOutcome, SourceStorageError> {
        self.require_open()?;
        validate_usage_event(&event)?;
        let epoch = self.resolve_usage_write_epoch(target)?;
        let connection = self.connection()?.connection();
        validate_event_session_source(
            connection,
            &self.source,
            &event.thread_id,
            &event.root_session_id,
        )?;
        let event_kind = event.kind.as_str();
        let quality = if event.usage.cache_write_tokens.is_some() {
            "complete"
        } else {
            "partial"
        };
        let existing: Option<CanonicalIdentityRow> = connection
            .query_row(
                "SELECT event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                        reasoning_effort,input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                        reasoning_tokens,total_tokens,quality_status
                 FROM usage_events WHERE source=?1 AND source_epoch=?2 AND event_id=?3",
                params![self.source.as_str(), epoch, event.event_id.as_str()],
                |row| {
                    Ok(CanonicalIdentityRow {
                        event_kind: row.get(0)?,
                        occurred_at_ms: row.get(1)?,
                        thread_id: row.get(2)?,
                        root_session_id: row.get(3)?,
                        turn_key: row.get(4)?,
                        model: row.get(5)?,
                        reasoning_effort: row.get(6)?,
                        input_tokens: row.get(7)?,
                        cached_tokens: row.get(8)?,
                        cache_write_tokens: row.get(9)?,
                        output_tokens: row.get(10)?,
                        reasoning_tokens: row.get(11)?,
                        total_tokens: row.get(12)?,
                        quality_status: row.get(13)?,
                    })
                },
            )
            .optional()
            .map_err(map_sql_error)?;
        let identity = CanonicalIdentityRow {
            event_kind: event_kind.to_owned(),
            occurred_at_ms: event.occurred_at_ms,
            thread_id: event.thread_id.clone(),
            root_session_id: event.root_session_id.clone(),
            turn_key: event.turn_key.clone(),
            model: event.model.clone(),
            reasoning_effort: event.reasoning_effort.clone(),
            input_tokens: event.usage.input_tokens,
            cached_tokens: event.usage.cached_tokens,
            cache_write_tokens: event.usage.cache_write_tokens,
            output_tokens: event.usage.output_tokens,
            reasoning_tokens: event.usage.reasoning_tokens,
            total_tokens: event.usage.total_tokens,
            quality_status: quality.to_owned(),
        };
        if let Some(existing) = existing {
            if existing != identity {
                return Err(SourceStorageError::InvalidRequest(
                    "canonical usage event immutable payload conflict".to_owned(),
                ));
            }
            return Ok(CanonicalWriteOutcome::Duplicate);
        }
        connection
            .execute(
                "INSERT INTO usage_events(
                    source,source_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                    turn_key,model,reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                    cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    self.source.as_str(), epoch, event.event_id, event_kind, event.occurred_at_ms,
                    event.thread_id, event.root_session_id, event.turn_key, event.model,
                    event.reasoning_effort, event.estimated_cost_nanos_usd, event.usage.input_tokens,
                    event.usage.cached_tokens, event.usage.cache_write_tokens, event.usage.output_tokens,
                    event.usage.reasoning_tokens, event.usage.total_tokens, quality, event.created_at_ms,
                ],
            )
            .map_err(map_sql_error)?;
        Ok(CanonicalWriteOutcome::Inserted)
    }

    pub fn write_usage(
        &mut self,
        target: UsageWriteTarget,
        event: CanonicalUsageEventWrite,
    ) -> Result<(), SourceStorageError> {
        let outcome = self.write_usage_no_revision(target, event)?;
        if target == UsageWriteTarget::Active && outcome == CanonicalWriteOutcome::Inserted {
            self.bump_data_revision()?;
        }
        Ok(())
    }

    pub(crate) fn copy_usage_event_no_revision(
        &mut self,
        from: UsageWriteTarget,
        to: UsageWriteTarget,
        event_id: &str,
    ) -> Result<CanonicalWriteOutcome, SourceStorageError> {
        let from_epoch = self.resolve_usage_write_epoch(from)?;
        let to_epoch = self.resolve_usage_write_epoch(to)?;
        if from_epoch == to_epoch {
            return Err(SourceStorageError::InvalidRequest(
                "canonical usage copy requires distinct epochs".to_owned(),
            ));
        }
        let connection = self.connection()?.connection();
        let row: Option<CanonicalUsageEventWrite> = connection
            .query_row(
                "SELECT event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                        reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                        cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,created_at_ms
                 FROM usage_events WHERE source=?1 AND source_epoch=?2 AND event_id=?3",
                params![self.source.as_str(), from_epoch, event_id],
                |row| {
                    Ok(CanonicalUsageEventWrite {
                        event_id: event_id.to_owned(),
                        kind: parse_event_kind(row.get::<_, String>(0)?.as_str())?,
                        occurred_at_ms: row.get(1)?,
                        thread_id: row.get(2)?,
                        root_session_id: row.get(3)?,
                        turn_key: row.get(4)?,
                        model: row.get(5)?,
                        reasoning_effort: row.get(6)?,
                        estimated_cost_nanos_usd: row.get(7)?,
                        usage: NormalizedTokenUsage::new(
                            row.get(8)?,
                            row.get(9)?,
                            row.get(10)?,
                            row.get(11)?,
                            row.get(12)?,
                            row.get(13)?,
                        )
                        .map_err(|error| {
                            rusqlite::Error::InvalidParameterName(error.to_string())
                        })?,
                        created_at_ms: row.get(14)?,
                    })
                },
            )
            .optional()
            .map_err(map_sql_error)?;
        let Some(event) = row else {
            return Err(SourceStorageError::InvalidRequest(
                "canonical usage copy source event is missing".to_owned(),
            ));
        };
        self.write_usage_no_revision(to, event)
    }

    pub(crate) fn delete_usage_events_no_revision(
        &mut self,
        target: UsageWriteTarget,
        event_ids: &[String],
    ) -> Result<usize, SourceStorageError> {
        let epoch = self.resolve_usage_write_epoch(target)?;
        let connection = self.connection()?.connection();
        let mut statement = connection
            .prepare("DELETE FROM usage_events WHERE source=?1 AND source_epoch=?2 AND event_id=?3")
            .map_err(map_sql_error)?;
        let mut deleted = 0usize;
        for event_id in event_ids {
            deleted += statement
                .execute(params![self.source.as_str(), epoch, event_id])
                .map_err(map_sql_error)?;
        }
        Ok(deleted)
    }

    pub(crate) fn delete_inactive_usage_events_no_revision(
        &mut self,
        expected_epoch: i64,
        event_ids: &[String],
    ) -> Result<usize, SourceStorageError> {
        if expected_epoch <= 0 {
            return Err(SourceStorageError::InvalidRequest(
                "inactive usage epoch must be positive".to_owned(),
            ));
        }
        let state = self.usage_epoch_state()?;
        if expected_epoch == state.active_epoch || Some(expected_epoch) == state.build_epoch {
            return Err(SourceStorageError::InvalidRequest(
                "cannot delete active or build usage epoch".to_owned(),
            ));
        }
        let connection = self.connection()?.connection();
        let mut statement = connection
            .prepare("DELETE FROM usage_events WHERE source=?1 AND source_epoch=?2 AND event_id=?3")
            .map_err(map_sql_error)?;
        let mut deleted = 0usize;
        for event_id in event_ids {
            deleted += statement
                .execute(params![self.source.as_str(), expected_epoch, event_id])
                .map_err(map_sql_error)?;
        }
        Ok(deleted)
    }

    pub(crate) fn rebind_usage_root_no_revision(
        &mut self,
        target: UsageWriteTarget,
        thread_id: &str,
        next_root_session_id: &str,
    ) -> Result<usize, SourceStorageError> {
        let epoch = self.resolve_usage_write_epoch(target)?;
        let connection = self.connection()?.connection();
        connection
            .execute(
                "UPDATE usage_events SET root_session_id=?1
                 WHERE source=?2 AND source_epoch=?3 AND thread_id=?4",
                params![next_root_session_id, self.source.as_str(), epoch, thread_id],
            )
            .map_err(map_sql_error)
    }

    pub fn ensure_usage_epoch(&mut self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        let connection = self.connection()?.connection();
        connection
            .execute(
                "INSERT INTO source_usage_epochs(
                    source,active_epoch,build_epoch,active_parser_version,build_parser_version)
                 VALUES(?1,0,NULL,0,NULL) ON CONFLICT(source) DO NOTHING",
                [self.source.as_str()],
            )
            .map_err(map_sql_error)?;
        Ok(())
    }

    pub fn usage_epoch_state(&mut self) -> Result<SourceUsageEpochState, SourceStorageError> {
        self.require_open()?;
        let connection = self.connection()?.connection();
        let values: (i64, Option<i64>, i64, Option<i64>) = connection
            .query_row(
                "SELECT active_epoch,build_epoch,active_parser_version,build_parser_version
                 FROM source_usage_epochs WHERE source=?1",
                [self.source.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(map_sql_error)?;
        SourceUsageEpochState::new(self.source.clone(), values.0, values.1, values.2, values.3)
            .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))
    }

    pub fn begin_or_resume_usage_build(
        &mut self,
        parser_version: i64,
    ) -> Result<i64, SourceStorageError> {
        if parser_version < 0 {
            return Err(SourceStorageError::InvalidRequest(
                "usage parser version must be non-negative".to_owned(),
            ));
        }
        self.ensure_usage_epoch()?;
        let state = self.usage_epoch_state()?;
        let connection = self.connection()?.connection();
        match (state.build_epoch, state.build_parser_version) {
            (Some(epoch), Some(existing)) if existing == parser_version => Ok(epoch),
            (Some(_), Some(_)) => Err(SourceStorageError::InvalidRequest(
                "a different usage build is already active for this source".to_owned(),
            )),
            (None, None) => {
                let build = state.active_epoch.checked_add(1).ok_or_else(|| {
                    SourceStorageError::InvalidRequest("usage epoch overflow".to_owned())
                })?;
                let changed = connection
                    .execute(
                        "UPDATE source_usage_epochs SET build_epoch=?1,build_parser_version=?2
                         WHERE source=?3 AND build_epoch IS NULL AND active_epoch=?4",
                        params![
                            build,
                            parser_version,
                            self.source.as_str(),
                            state.active_epoch
                        ],
                    )
                    .map_err(map_sql_error)?;
                if changed != 1 {
                    return Err(SourceStorageError::InvalidRequest(
                        "usage epoch build CAS failed".to_owned(),
                    ));
                }
                Ok(build)
            }
            _ => Err(SourceStorageError::InvalidRequest(
                "source usage epoch build columns are inconsistent".to_owned(),
            )),
        }
    }

    pub fn resolve_usage_write_epoch(
        &mut self,
        target: UsageWriteTarget,
    ) -> Result<i64, SourceStorageError> {
        let state = self.usage_epoch_state()?;
        match target {
            UsageWriteTarget::Active if state.active_epoch > 0 => Ok(state.active_epoch),
            UsageWriteTarget::Active => Err(SourceStorageError::InvalidRequest(
                "active usage epoch is not initialized".to_owned(),
            )),
            UsageWriteTarget::Build => state.build_epoch.ok_or_else(|| {
                SourceStorageError::InvalidRequest("usage build epoch is not active".to_owned())
            }),
        }
    }

    pub(crate) fn retarget_usage_build(
        &mut self,
        expected_build_epoch: i64,
        expected_old_parser_version: i64,
        new_parser_version: i64,
    ) -> Result<(), SourceStorageError> {
        let connection = self.connection()?.connection();
        let changed = connection
            .execute(
                "UPDATE source_usage_epochs SET build_parser_version=?1
                 WHERE source=?2 AND build_epoch=?3 AND build_parser_version=?4",
                params![
                    new_parser_version,
                    self.source.as_str(),
                    expected_build_epoch,
                    expected_old_parser_version
                ],
            )
            .map_err(map_sql_error)?;
        if changed != 1 {
            return Err(SourceStorageError::InvalidRequest(
                "usage build parser retarget CAS failed".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn activate_usage_build_with_private_visibility<F, E>(
        &mut self,
        expected_build_epoch: i64,
        expected_parser_version: i64,
        private_visibility_equal: F,
    ) -> Result<UsageActivationOutcome, E>
    where
        F: FnOnce(&Connection, &SourceId, i64, i64, i64, i64) -> Result<bool, E>,
        E: From<SourceStorageError>,
    {
        let state = self.usage_epoch_state().map_err(E::from)?;
        let build_epoch = state.build_epoch.ok_or_else(|| {
            E::from(SourceStorageError::InvalidRequest(
                "usage build epoch is not active".to_owned(),
            ))
        })?;
        let build_parser = state.build_parser_version.ok_or_else(|| {
            E::from(SourceStorageError::InvalidRequest(
                "usage build parser version is not active".to_owned(),
            ))
        })?;
        if build_epoch != expected_build_epoch || build_parser != expected_parser_version {
            return Err(E::from(SourceStorageError::InvalidRequest(
                "usage build activation expected pair mismatch".to_owned(),
            )));
        }
        let connection = self.connection().map_err(E::from)?.connection();
        let canonical_equal =
            canonical_projection_equal(connection, &self.source, state.active_epoch, build_epoch)
                .map_err(E::from)?;
        let private_equal = if canonical_equal {
            private_visibility_equal(
                connection,
                &self.source,
                state.active_epoch,
                state.active_parser_version,
                build_epoch,
                build_parser,
            )?
        } else {
            false
        };
        let visible_changed = !(canonical_equal && private_equal);
        let changed = connection
            .execute(
                "UPDATE source_usage_epochs
                 SET active_epoch=build_epoch,active_parser_version=build_parser_version,
                     build_epoch=NULL,build_parser_version=NULL
                 WHERE source=?1 AND build_epoch=?2 AND build_parser_version=?3",
                params![
                    self.source.as_str(),
                    expected_build_epoch,
                    expected_parser_version
                ],
            )
            .map_err(map_sql_error)
            .map_err(E::from)?;
        if changed != 1 {
            return Err(E::from(SourceStorageError::InvalidRequest(
                "usage build activation CAS failed".to_owned(),
            )));
        }
        if visible_changed {
            self.bump_data_revision().map_err(E::from)?;
        }
        let revision = self.current_revisions().map_err(E::from)?.0;
        Ok(UsageActivationOutcome {
            active_epoch: build_epoch,
            data_revision: revision,
            visible_changed,
        })
    }

    pub(crate) fn activate_usage_build(
        &mut self,
        expected_build_epoch: i64,
        expected_parser_version: i64,
    ) -> Result<UsageActivationOutcome, SourceStorageError> {
        self.activate_usage_build_with_private_visibility(
            expected_build_epoch,
            expected_parser_version,
            |_connection, _source, _active, _active_parser, _build, _build_parser| Ok(true),
        )
    }

    pub(crate) fn bump_data_revision(&mut self) -> Result<i64, SourceStorageError> {
        if self.data_revision_bumped {
            return Ok(self.current_revisions()?.0);
        }
        let connection = self.connection()?.connection();
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
                "app meta data revision CAS failed".to_owned(),
            ));
        }
        self.data_revision_bumped = true;
        Ok(next)
    }

    pub(crate) fn bump_status_revision(&mut self) -> Result<i64, SourceStorageError> {
        if self.status_revision_bumped {
            return Ok(self.current_revisions()?.1);
        }
        let connection = self.connection()?.connection();
        let current: i64 = connection
            .query_row(
                "SELECT status_revision FROM app_meta WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(map_sql_error)?;
        let next = current.checked_add(1).ok_or_else(|| {
            SourceStorageError::InvalidRequest("status revision overflow".to_owned())
        })?;
        let changed = connection
            .execute(
                "UPDATE app_meta SET status_revision=?1 WHERE id=1 AND status_revision=?2",
                params![next, current],
            )
            .map_err(map_sql_error)?;
        if changed != 1 {
            return Err(SourceStorageError::InvalidRequest(
                "app meta status revision CAS failed".to_owned(),
            ));
        }
        self.status_revision_bumped = true;
        Ok(next)
    }

    pub(crate) fn commit(mut self) -> Result<(), SourceStorageError> {
        self.require_open()?;
        let revisions = if self.data_revision_bumped || self.status_revision_bumped {
            Some(self.current_revisions()?)
        } else {
            None
        };
        let connection = self
            .connection
            .take()
            .ok_or(SourceStorageError::TransactionClosed)?;
        connection
            .connection()
            .execute_batch("COMMIT")
            .map_err(|error| {
                self.poisoned = true;
                map_sql_error(error)
            })?;
        self.committed = true;
        if let (Some(publisher), Some((data, status))) =
            (self.revision_publisher.as_ref(), revisions)
        {
            publisher.publish(data, status);
        }
        Ok(())
    }

    fn current_revisions(&self) -> Result<(i64, i64), SourceStorageError> {
        let connection = self.connection()?.connection();
        connection
            .query_row(
                "SELECT data_revision,status_revision FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sql_error)
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

    fn connection(&self) -> Result<&SourceWriteConnection<'_>, SourceStorageError> {
        self.connection
            .as_ref()
            .ok_or(SourceStorageError::TransactionClosed)
    }
}

impl Drop for SourceWriteTxn<'_> {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(connection) = self.connection.as_ref() {
                let _ = connection.connection().execute_batch("ROLLBACK");
            }
        }
    }
}

#[derive(PartialEq, Eq)]
struct CanonicalIdentityRow {
    event_kind: String,
    occurred_at_ms: i64,
    thread_id: String,
    root_session_id: String,
    turn_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    input_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: Option<i64>,
    output_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    quality_status: String,
}

fn validate_usage_event(event: &CanonicalUsageEventWrite) -> Result<(), SourceStorageError> {
    event
        .usage
        .validate()
        .map_err(|error| SourceStorageError::InvalidRequest(error.to_string()))?;
    if event.event_id.trim().is_empty()
        || event.thread_id.trim().is_empty()
        || event.root_session_id.trim().is_empty()
        || event.model.trim().is_empty()
        || event.occurred_at_ms < 0
        || event.created_at_ms < 0
    {
        return Err(SourceStorageError::InvalidRequest(
            "canonical usage event contains an invalid identity or timestamp".to_owned(),
        ));
    }
    Ok(())
}

fn parse_event_kind(value: &str) -> rusqlite::Result<EventKind> {
    match value {
        "normal" => Ok(EventKind::Normal),
        "recovered" => Ok(EventKind::Recovered),
        "turn_compensation" => Ok(EventKind::TurnCompensation),
        _ => Err(rusqlite::Error::InvalidParameterName(
            "invalid canonical event kind".to_owned(),
        )),
    }
}

fn canonical_projection_equal(
    connection: &Connection,
    source: &SourceId,
    active_epoch: i64,
    build_epoch: i64,
) -> Result<bool, SourceStorageError> {
    let columns = "event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                   reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                   cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status";
    let sql = format!(
        "SELECT NOT EXISTS(
             SELECT {columns} FROM usage_events WHERE source=?1 AND source_epoch=?2
             EXCEPT SELECT {columns} FROM usage_events WHERE source=?1 AND source_epoch=?3
         ) AND NOT EXISTS(
             SELECT {columns} FROM usage_events WHERE source=?1 AND source_epoch=?3
             EXCEPT SELECT {columns} FROM usage_events WHERE source=?1 AND source_epoch=?2
         )"
    );
    let equal: i64 = connection
        .query_row(
            &sql,
            params![source.as_str(), active_epoch, build_epoch],
            |row| row.get(0),
        )
        .map_err(map_sql_error)?;
    Ok(equal != 0)
}

fn read_session_visibility(
    connection: &Connection,
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

fn update_session_row(
    connection: &Connection,
    identity: &SessionIdentity,
    patch: &ResolvedThreadPatch,
) -> Result<(), SourceStorageError> {
    let mut updates = Vec::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    macro_rules! optional {
        ($column:literal, $field:expr) => {
            match $field {
                Patch::Keep => {}
                Patch::Set(value) => {
                    updates.push(concat!($column, "=?"));
                    values.push(rusqlite::types::Value::Text(value.clone()));
                }
                Patch::Clear => updates.push(concat!($column, "=NULL")),
            }
        };
    }
    macro_rules! required_text {
        ($column:literal, $field:expr) => {
            if let Patch::Set(value) = $field {
                updates.push(concat!($column, "=?"));
                values.push(rusqlite::types::Value::Text(value.as_str().to_owned()));
            }
        };
    }
    optional!("parent_thread_id", &patch.parent_thread_id);
    optional!("root_session_id", &patch.root_session_id);
    optional!("title", &patch.title);
    optional!("project_name", &patch.project_name);
    optional!("project_path", &patch.project_path);
    optional!("metadata_model", &patch.metadata_model);
    required_text!("agent_role", &patch.agent_role);
    required_text!("project_kind", &patch.project_kind);
    if let Patch::Set(value) = patch.archived {
        updates.push("archived=?");
        values.push(rusqlite::types::Value::Integer(i64::from(value)));
    }
    match patch.created_at_ms {
        Patch::Keep => {}
        Patch::Set(value) => {
            updates.push("created_at_ms=?");
            values.push(rusqlite::types::Value::Integer(value));
        }
        Patch::Clear => updates.push("created_at_ms=NULL"),
    }
    match patch.updated_at_ms {
        Patch::Keep => {}
        Patch::Set(value) => {
            updates.push("updated_at_ms=?");
            values.push(rusqlite::types::Value::Integer(value));
        }
        Patch::Clear => updates.push("updated_at_ms=NULL"),
    }
    updates.push("metadata_quality_status=?");
    values.push(rusqlite::types::Value::Text(
        patch.metadata_quality_status.as_str().to_owned(),
    ));
    updates.push("metadata_resolved_at_ms=?");
    values.push(rusqlite::types::Value::Integer(patch.resolved_at_ms));
    updates.push("native_session_id=?");
    values.push(rusqlite::types::Value::Text(
        identity.native_session_id.clone(),
    ));
    let mut sql = format!("UPDATE threads SET {}", updates.join(","));
    values.push(rusqlite::types::Value::Text(identity.thread_id.clone()));
    sql.push_str(" WHERE thread_id=?");
    connection
        .execute(&sql, rusqlite::params_from_iter(values))
        .map_err(map_sql_error)?;
    Ok(())
}

fn insert_session_row(
    connection: &Connection,
    identity: &SessionIdentity,
    patch: &ResolvedThreadPatch,
) -> Result<(), SourceStorageError> {
    let role = match &patch.agent_role {
        Patch::Set(value) => value.as_str(),
        _ => "unknown",
    };
    let project_kind = match &patch.project_kind {
        Patch::Set(value) => value.as_str(),
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
    connection: &Connection,
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
        if let Some(source) = source.as_deref()
            && source != identity.source.as_str()
        {
            return Err(SourceStorageError::InvalidRequest(
                "session parent/root source mismatch".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_event_session_source(
    connection: &Connection,
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

fn map_sql_error(error: rusqlite::Error) -> SourceStorageError {
    SourceStorageError::Storage(StorageError::sqlite(error).kind())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageWriteTarget {
    Active,
    Build,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LedgerOptions;
    use std::{
        fs,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn private_read_is_query_only_and_restores_after_dml_or_error() {
        let root = std::env::temp_dir().join(format!(
            "usagi-source-read-seam-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create temporary read seam directory");
        let db_path = root.join("mu.sqlite3");
        let ledger =
            Arc::new(Ledger::open(LedgerOptions::new(&db_path)).expect("open temporary ledger"));
        let storage = SourceStorage::with_ledger(
            "query-only-seam",
            SourceId::new("test").expect("valid test source"),
            Arc::clone(&ledger),
        );

        let dml_result = storage.with_private_read(|connection| {
            let error = connection
                .execute(
                    "UPDATE app_meta SET data_revision=data_revision WHERE id=1",
                    [],
                )
                .expect_err("query_only must reject DML");
            assert!(matches!(error, rusqlite::Error::SqliteFailure(..)));
            Ok::<_, SourceStorageError>(())
        });
        assert_eq!(dml_result, Ok(()));
        let query_only: i64 = ledger
            .connection()
            .expect("lock ledger connection")
            .pragma_query_value(None, "query_only", |row| row.get(0))
            .expect("read query_only after DML subcase");
        assert_eq!(query_only, 0);

        let expected = SourceStorageError::InvalidRequest("operation failed".to_owned());
        let operation_result: Result<(), SourceStorageError> =
            storage.with_private_read(|_| Err(expected.clone()));
        assert_eq!(operation_result, Err(expected));
        let query_only: i64 = ledger
            .connection()
            .expect("lock ledger connection")
            .pragma_query_value(None, "query_only", |row| row.get(0))
            .expect("read query_only after operation error subcase");
        assert_eq!(query_only, 0);

        drop(storage);
        drop(ledger);
        fs::remove_dir_all(root).expect("remove temporary read seam directory");
    }
}
