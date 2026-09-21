//! SQLite storage bootstrap for Usagi.
//!
//! This module owns the connection, PRAGMA setup, schema migration, source
//! observation/checkpoint writes, metadata commits, and scan lifecycle.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::domain::{
    DomainError, FollowupState, ScanLifecycleState, ScanResult, ScanState, ScanTrigger,
};
use crate::platform::paths;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use tokio::sync::watch;

pub(crate) mod cost;
mod lifecycle;
mod migrations;

#[cfg(test)]
pub(crate) use migrations::seed_v11_binding_rows;

pub use crate::domain::AppState;

pub type Result<T> = std::result::Result<T, StorageError>;

const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

const REQUIRED_TABLES: &[&str] = &[
    "app_meta",
    "scan_runs",
    "source_scan_runs",
    "threads",
    "usage_events",
    "source_usage_epochs",
    "codex_adapter_state",
    "codex_source_files",
    "codex_source_checkpoints",
    "codex_rollout_metadata_facts",
    "codex_usage_event_occurrences",
    "codex_turns",
    "codex_ingest_anomalies",
    "codex_usage_source_states",
    "codex_usage_build_sources",
    "codex_usage_session_quarantine",
    "codex_usage_session_quarantine_sources",
    "codex_skill_usage_events",
];

/// Stable, opaque categories suitable for API error mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageErrorKind {
    Database,
    DatabaseBusy,
    DatabaseCorrupt,
    Io,
    SchemaTooNew,
    InvalidState,
    LockPoisoned,
}

/// Errors raised by storage without exposing SQLite, SQL text, paths, or
/// internal diagnostic messages through the public API seam.
pub struct StorageError {
    kind: StorageErrorKind,
    schema_versions: Option<(u32, u32)>,
    usage_rebuild_required: bool,
    _source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

#[derive(Debug)]
struct InternalStorageError(String);

impl fmt::Display for InternalStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InternalStorageError {}

impl StorageError {
    pub const fn kind(&self) -> StorageErrorKind {
        self.kind
    }

    pub const fn schema_versions(&self) -> Option<(u32, u32)> {
        self.schema_versions
    }

    pub(crate) fn sqlite(error: rusqlite::Error) -> Self {
        let kind = sqlite_error_kind(&error);
        Self::with_source(kind, error)
    }

    pub(crate) fn io(error: io::Error) -> Self {
        Self::with_source(StorageErrorKind::Io, error)
    }

    pub(crate) fn schema_too_new(found: u32, supported: u32) -> Self {
        Self {
            kind: StorageErrorKind::SchemaTooNew,
            schema_versions: Some((found, supported)),
            usage_rebuild_required: false,
            _source: None,
        }
    }

    pub(crate) fn database_corrupt(error: rusqlite::Error) -> Self {
        Self::with_source(StorageErrorKind::DatabaseCorrupt, error)
    }

    pub(crate) fn invalid_state(message: impl Into<String>) -> Self {
        Self::with_source(
            StorageErrorKind::InvalidState,
            InternalStorageError(message.into()),
        )
    }

    pub(crate) fn usage_conflict(message: impl Into<String>) -> Self {
        Self {
            kind: StorageErrorKind::InvalidState,
            schema_versions: None,
            usage_rebuild_required: true,
            _source: Some(Box::new(InternalStorageError(message.into()))),
        }
    }

    pub(crate) const fn requires_usage_rebuild(&self) -> bool {
        self.usage_rebuild_required
    }

    pub(crate) fn lock_poisoned() -> Self {
        Self::without_source(StorageErrorKind::LockPoisoned)
    }

    fn without_source(kind: StorageErrorKind) -> Self {
        Self {
            kind,
            schema_versions: None,
            usage_rebuild_required: false,
            _source: None,
        }
    }

    fn with_source(
        kind: StorageErrorKind,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            schema_versions: None,
            usage_rebuild_required: false,
            _source: Some(Box::new(source)),
        }
    }
}

impl From<crate::source::SourceStorageError> for StorageError {
    fn from(error: crate::source::SourceStorageError) -> Self {
        match error {
            crate::source::SourceStorageError::Storage(kind) => Self::without_source(kind),
            crate::source::SourceStorageError::NotImplemented
            | crate::source::SourceStorageError::TransactionClosed
            | crate::source::SourceStorageError::TransactionPoisoned
            | crate::source::SourceStorageError::SourceMismatch
            | crate::source::SourceStorageError::UnsupportedOperation(_)
            | crate::source::SourceStorageError::InvalidRequest(_) => {
                Self::invalid_state("source storage operation failed")
            }
        }
    }
}

impl fmt::Debug for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StorageError")
            .field("kind", &self.kind)
            .field("schema_versions", &self.schema_versions)
            .finish()
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self {
                kind: StorageErrorKind::Database,
                ..
            } => formatter.write_str("database operation failed"),
            Self {
                kind: StorageErrorKind::DatabaseBusy,
                ..
            } => formatter.write_str("database is busy"),
            Self {
                kind: StorageErrorKind::DatabaseCorrupt,
                ..
            } => formatter.write_str("database is corrupt"),
            Self {
                kind: StorageErrorKind::Io,
                ..
            } => formatter.write_str("storage I/O operation failed"),
            Self {
                kind: StorageErrorKind::SchemaTooNew,
                schema_versions: Some((found, supported)),
                ..
            } => write!(
                formatter,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self {
                kind: StorageErrorKind::SchemaTooNew,
                ..
            } => formatter.write_str("database schema is newer than supported"),
            Self {
                kind: StorageErrorKind::InvalidState,
                ..
            } => formatter.write_str("storage request or state is invalid"),
            Self {
                kind: StorageErrorKind::LockPoisoned,
                ..
            } => formatter.write_str("database access is unavailable"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::io(error)
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::sqlite(error)
    }
}

fn usage_event_count(path: &Path) -> Result<i64> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(classify_sqlite)?;
    let table_exists: i64 = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'usage_events'
            )",
            [],
            |row| row.get(0),
        )
        .map_err(classify_sqlite)?;
    if table_exists == 0 {
        return Ok(0);
    }
    connection
        .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
        .map_err(classify_sqlite)
}

fn migrate_legacy_database_if_needed(current: &Path, legacy: &Path) -> Result<()> {
    if current == legacy || !legacy.is_file() {
        return Ok(());
    }

    let should_migrate = if !current.is_file() {
        true
    } else {
        usage_event_count(current)? == 0 && usage_event_count(legacy)? > 0
    };
    if !should_migrate {
        return Ok(());
    }

    let current_dir = current.parent().ok_or_else(|| {
        StorageError::invalid_state("canonical database path has no parent directory")
    })?;
    let legacy_dir = legacy.parent().ok_or_else(|| {
        StorageError::invalid_state("legacy database path has no parent directory")
    })?;
    if current_dir == legacy_dir {
        return Ok(());
    }

    if !current_dir.exists() {
        fs::rename(legacy_dir, current_dir)?;
        return Ok(());
    }

    let backup_dir = current_dir.with_file_name(format!(
        ".usagi-empty-before-legacy-migration-{}-{}",
        std::process::id(),
        current_time_ms()
    ));
    fs::rename(current_dir, &backup_dir)?;
    match fs::rename(legacy_dir, current_dir) {
        Ok(()) => {
            let _ = fs::remove_dir_all(backup_dir);
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(&backup_dir, current_dir);
            Err(StorageError::from(error))
        }
    }
}

fn default_database_path_with_legacy_migration() -> Result<PathBuf> {
    let current = paths::default_database_path();
    if let Some(legacy) = paths::legacy_database_path() {
        migrate_legacy_database_if_needed(&current, &legacy)?;
    }
    Ok(current)
}

/// Values used when opening a Ledger.
#[derive(Debug, Clone, Default)]
pub struct LedgerOptions {
    /// Optional path to `mu.sqlite3`.  If omitted, the platform default is
    /// used (`~/Library/Application Support/Usagi/mu.sqlite3` on macOS and
    /// the platform local application-data directory on Windows).
    pub db_path: Option<PathBuf>,
}

impl LedgerOptions {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: Some(db_path.into()),
        }
    }

    pub fn for_database(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: Some(db_path.into()),
        }
    }

    pub fn with_database_path(mut self, db_path: impl Into<PathBuf>) -> Self {
        self.db_path = Some(db_path.into());
        self
    }

    pub fn with_db_path(self, db_path: impl Into<PathBuf>) -> Self {
        self.with_database_path(db_path)
    }

    pub fn database_path(&self) -> Result<PathBuf> {
        self.db_path
            .clone()
            .map(|path| paths::normalize_path(path).map_err(StorageError::from))
            .unwrap_or_else(default_database_path_with_legacy_migration)
    }
}

/// Values verified on every Ledger connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PragmaState {
    pub journal_mode_wal: bool,
    pub synchronous_normal: bool,
    pub foreign_keys: bool,
    pub busy_timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevisionTuple {
    pub data_revision: i64,
    pub status_revision: i64,
}

#[derive(Clone)]
pub(crate) struct RevisionPublisher {
    sender: watch::Sender<RevisionTuple>,
}

impl RevisionPublisher {
    pub(crate) fn publish(&self, data_revision: i64, status_revision: i64) {
        self.sender.send_if_modified(|current| {
            let next = RevisionTuple {
                data_revision: current.data_revision.max(data_revision),
                status_revision: current.status_revision.max(status_revision),
            };
            if *current == next {
                false
            } else {
                *current = next;
                true
            }
        });
    }
}

/// The MU SQLite ledger.  Connections and SQL remain private to this module.
pub struct Ledger {
    db_path: PathBuf,
    connection: Mutex<Connection>,
    revision_sender: watch::Sender<RevisionTuple>,
}

impl fmt::Debug for Ledger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ledger")
            .field("db_path", &self.db_path)
            .finish_non_exhaustive()
    }
}

impl Ledger {
    /// Open/create a ledger, configure SQLite, and run migrations.
    pub fn open(options: LedgerOptions) -> Result<Self> {
        let db_path = options.database_path()?;

        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut connection = Connection::open(&db_path).map_err(classify_sqlite)?;
        let current_version = query_user_version(&connection).map_err(classify_sqlite)?;
        let supported_version = migrations::latest_schema_version();
        if current_version > supported_version {
            return Err(StorageError::schema_too_new(
                current_version,
                supported_version,
            ));
        }

        configure_connection(&connection).map_err(classify_sqlite)?;
        migrations::migrate(&mut connection, current_version).map_err(classify_sqlite)?;
        validate_schema(&connection, &db_path).map_err(classify_sqlite)?;
        cost::refresh_usage_costs_if_needed(&mut connection)?;
        let initial_revision = read_revision_tuple(&connection).map_err(classify_sqlite)?;
        let (revision_sender, _) = watch::channel(initial_revision);

        Ok(Self {
            db_path,
            connection: Mutex::new(connection),
            revision_sender,
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn schema_version(&self) -> Result<u32> {
        let connection = self.connection()?;
        query_user_version(&connection).map_err(Into::into)
    }

    pub fn pragma_state(&self) -> Result<PragmaState> {
        let connection = self.connection()?;
        read_pragma_state(&connection).map_err(Into::into)
    }

    pub fn app_state(&self) -> Result<AppState> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT
                data_revision,
                status_revision,
                scan_state,
                active_scan_id,
                last_finished_scan_id,
                last_finished_scan_result,
                last_scan_started_at_ms,
                last_scan_completed_at_ms,
                last_scan_failed_at_ms,
                last_scan_error_code,
                followup_scan_id,
                followup_state,
                followup_trigger,
                followup_requested_at_ms,
                followup_enqueued_status_revision,
                followup_error_code
             FROM app_meta
                WHERE id = 1",
                [],
                |row| {
                    let data_revision: i64 = row.get(0)?;
                    let status_revision: i64 = row.get(1)?;
                    let scan_state: String = row.get(2)?;
                    let last_finished_scan_result: Option<String> = row.get(5)?;
                    let followup_state: Option<String> = row.get(11)?;
                    let followup_trigger: Option<String> = row.get(12)?;
                    let followup_enqueued_status_revision: Option<i64> = row.get(14)?;
                    let scan_state = ScanLifecycleState::try_from(scan_state.as_str())
                        .map_err(to_domain_sql_error)?;
                    let last_finished_scan_result = last_finished_scan_result
                        .as_deref()
                        .map(ScanResult::try_from)
                        .transpose()
                        .map_err(to_domain_sql_error)?;
                    let followup_state = followup_state
                        .as_deref()
                        .map(FollowupState::try_from)
                        .transpose()
                        .map_err(to_domain_sql_error)?;
                    let followup_trigger = followup_trigger
                        .as_deref()
                        .map(ScanTrigger::try_from)
                        .transpose()
                        .map_err(to_domain_sql_error)?;
                    AppState::new(
                        data_revision,
                        ScanState {
                            status_revision,
                            scan_state,
                            active_scan_id: row.get(3)?,
                            last_finished_scan_id: row.get(4)?,
                            last_finished_scan_result,
                            last_scan_started_at_ms: row.get(6)?,
                            last_scan_completed_at_ms: row.get(7)?,
                            last_scan_failed_at_ms: row.get(8)?,
                            last_scan_error_code: row.get(9)?,
                            followup_scan_id: row.get(10)?,
                            followup_state,
                            followup_trigger,
                            followup_requested_at_ms: row.get(13)?,
                            followup_enqueued_status_revision,
                            followup_error_code: row.get(15)?,
                        },
                    )
                    .map_err(to_domain_sql_error)
                },
            )
            .optional()
            .map_err(StorageError::sqlite)?
            .ok_or_else(|| StorageError::invalid_state("app_meta row id=1 is missing"))?;
        Ok(row)
    }

    /// Subscribe to the latest committed `(data_revision,status_revision)`.
    /// The channel is process-local; its initial value is loaded from SQLite.
    pub fn subscribe_revisions(&self) -> watch::Receiver<RevisionTuple> {
        self.revision_sender.subscribe()
    }

    pub fn current_revision(&self) -> RevisionTuple {
        *self.revision_sender.borrow()
    }

    pub(crate) fn revision_publisher(&self) -> RevisionPublisher {
        RevisionPublisher {
            sender: self.revision_sender.clone(),
        }
    }

    pub(crate) fn publish_revisions(&self, data_revision: i64, status_revision: i64) {
        self.revision_publisher()
            .publish(data_revision, status_revision);
    }

    pub(crate) fn publish_scan_state(&self, data_revision: i64, state: &ScanState) {
        self.publish_revisions(data_revision, state.status_revision);
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| StorageError::lock_poisoned())
    }

    /// Execute a read-only operation in one deferred SQLite transaction.
    /// `query_only` is restored explicitly on every normal return path and
    /// best-effort during unwinding.
    pub(crate) fn with_read_transaction<T, E>(
        &self,
        operation: impl FnOnce(&rusqlite::Transaction<'_>) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<StorageError>,
    {
        let connection = self.connection().map_err(E::from)?;
        let mut guard = QueryOnlyGuard::new(&connection).map_err(E::from)?;
        let result = {
            let transaction = connection
                .unchecked_transaction()
                .map_err(StorageError::sqlite)
                .map_err(E::from)?;
            let operation_result = operation(&transaction);
            match operation_result {
                Ok(value) => transaction
                    .commit()
                    .map(|()| value)
                    .map_err(StorageError::sqlite)
                    .map_err(E::from),
                Err(error) => {
                    let _ = transaction.rollback();
                    Err(error)
                }
            }
        };
        let restore = guard.restore();
        match restore {
            Ok(()) => result,
            Err(error) => Err(E::from(error)),
        }
    }
}

struct QueryOnlyGuard<'a> {
    connection: &'a Connection,
    previous: bool,
    armed: bool,
}

impl<'a> QueryOnlyGuard<'a> {
    fn new(connection: &'a Connection) -> Result<Self> {
        let previous: i64 = connection
            .pragma_query_value(None, "query_only", |row| row.get(0))
            .map_err(StorageError::sqlite)?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(StorageError::sqlite)?;
        Ok(Self {
            connection,
            previous: previous != 0,
            armed: true,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.armed {
            self.connection
                .pragma_update(None, "query_only", self.previous)
                .map_err(StorageError::sqlite)?;
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

fn read_revision_tuple(connection: &Connection) -> rusqlite::Result<RevisionTuple> {
    connection.query_row(
        "SELECT data_revision, status_revision FROM app_meta WHERE id = 1",
        [],
        |row| {
            Ok(RevisionTuple {
                data_revision: row.get(0)?,
                status_revision: row.get(1)?,
            })
        },
    )
}

fn query_user_version(connection: &Connection) -> rusqlite::Result<u32> {
    connection.pragma_query_value(None, "user_version", |row| {
        let version: i64 = row.get(0)?;
        u32::try_from(version).map_err(|_| {
            rusqlite::Error::InvalidParameterName("invalid negative user_version".to_owned())
        })
    })
}

fn configure_connection(connection: &Connection) -> rusqlite::Result<PragmaState> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(Duration::from_millis(DEFAULT_BUSY_TIMEOUT_MS))?;
    read_pragma_state(connection)
}

fn read_pragma_state(connection: &Connection) -> rusqlite::Result<PragmaState> {
    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    let foreign_keys: i64 =
        connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    let busy_timeout_ms: i64 =
        connection.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
    Ok(PragmaState {
        journal_mode_wal: journal_mode.eq_ignore_ascii_case("wal"),
        synchronous_normal: synchronous == 1,
        foreign_keys: foreign_keys == 1,
        busy_timeout_ms: u64::try_from(busy_timeout_ms).unwrap_or_default(),
    })
}

pub(crate) fn validate_schema(connection: &Connection, db_path: &Path) -> rusqlite::Result<()> {
    let quick_check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if !quick_check.eq_ignore_ascii_case("ok") {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "database integrity check failed: {quick_check}"
        )));
    }

    for table in REQUIRED_TABLES {
        let found: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "required table {table} is missing from {}",
                db_path.display()
            )));
        }
    }

    for table in [
        "source_files",
        "source_checkpoints",
        "rollout_metadata_facts",
        "usage_event_occurrences",
        "turns",
        "ingest_anomalies",
        "usage_source_states",
        "usage_build_sources",
        "usage_session_quarantine",
        "usage_session_quarantine_sources",
        "skill_usage_events",
    ] {
        let found: i64 = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
            )",
            [table],
            |row| row.get(0),
        )?;
        if found != 0 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "legacy private table {table} is still present"
            )));
        }
    }

    require_primary_key(connection, "codex_adapter_state", &["id"])?;
    require_check_fragment(connection, "codex_adapter_state", "id=1")?;
    require_check_fragment(
        connection,
        "codex_adapter_state",
        "binding_statusin('unbound','ready','source_changed')",
    )?;
    require_trigger(connection, "codex_source_checkpoints_offset_insert")?;
    require_trigger(connection, "codex_source_checkpoints_offset_update")?;
    for trigger in [
        "source_checkpoints_offset_insert",
        "source_checkpoints_offset_update",
    ] {
        let found: i64 = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1
            )",
            [trigger],
            |row| row.get(0),
        )?;
        if found != 0 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "legacy checkpoint trigger {trigger} is still present"
            )));
        }
    }

    require_not_null_columns(connection, "threads", &["source", "native_session_id"])?;
    require_non_empty_check(connection, "threads", "source")?;
    require_non_empty_check(connection, "threads", "native_session_id")?;
    require_unique_key(connection, "threads", &["source", "native_session_id"])?;

    require_non_empty_check(connection, "source_usage_epochs", "source")?;
    require_primary_key(connection, "source_usage_epochs", &["source"])?;

    require_not_null_columns(connection, "usage_events", &["source", "source_epoch"])?;
    require_non_empty_check(connection, "usage_events", "source")?;
    require_check_fragment(connection, "usage_events", "source_epoch>0")?;
    require_primary_key(
        connection,
        "usage_events",
        &["source", "source_epoch", "event_id"],
    )?;
    for check in [
        "cached_tokens<=input_tokens",
        "cache_write_tokensisnullorcached_tokens+cache_write_tokens<=input_tokens",
        "reasoning_tokens<=output_tokens",
        "total_tokens=input_tokens+output_tokens",
    ] {
        require_check_fragment(connection, "usage_events", check)?;
    }
    require_foreign_key(
        connection,
        "usage_events",
        "source_usage_epochs",
        &[("source", "source")],
    )?;

    require_not_null_columns(connection, "codex_usage_event_occurrences", &["source"])?;
    require_check_fragment(
        connection,
        "codex_usage_event_occurrences",
        "source='codex'",
    )?;
    require_primary_key(
        connection,
        "codex_usage_event_occurrences",
        &[
            "source",
            "ledger_epoch",
            "source_file_id",
            "file_generation",
            "source_start_offset",
        ],
    )?;
    require_foreign_key(
        connection,
        "codex_usage_event_occurrences",
        "usage_events",
        &[
            ("source", "source"),
            ("ledger_epoch", "source_epoch"),
            ("event_id", "event_id"),
        ],
    )?;

    require_not_null_columns(connection, "source_scan_runs", &["scan_id", "source"])?;
    require_non_empty_check(connection, "source_scan_runs", "source")?;
    require_primary_key(connection, "source_scan_runs", &["scan_id", "source"])?;
    for check in [
        "statein('queued','running','completed','skipped','failed')",
        "state='queued'andstarted_at_msisnullandfinished_at_msisnullanderror_codeisnull",
        "state='running'andstarted_at_msisnotnullandfinished_at_msisnullanderror_codeisnull",
        "state='completed'andstarted_at_msisnotnullandfinished_at_msisnotnullanderror_codeisnull",
        "state='skipped'andstarted_at_msisnullandfinished_at_msisnotnullanderror_codeisnull",
        "state='failed'andfinished_at_msisnotnullanderror_codeisnotnullandlength(error_code)>0",
        "started_at_msisnullorstarted_at_ms>=0",
        "finished_at_msisnullorfinished_at_ms>=0",
        "started_at_msisnullorfinished_at_msisnullorfinished_at_ms>=started_at_ms",
    ] {
        require_check_fragment(connection, "source_scan_runs", check)?;
    }

    Ok(())
}

fn require_not_null_columns(
    connection: &Connection,
    table: &str,
    columns: &[&str],
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info('{table}')"))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)? != 0))
    })?;
    let mut found = std::collections::HashMap::new();
    for row in rows {
        let (name, not_null) = row?;
        found.insert(name, not_null);
    }
    for column in columns {
        if !found.get(*column).copied().unwrap_or(false) {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "required NOT NULL column {table}.{column} is missing"
            )));
        }
    }
    Ok(())
}

fn require_primary_key(
    connection: &Connection,
    table: &str,
    expected: &[&str],
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info('{table}')"))?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
    })?;
    let mut actual = rows
        .map(|row| row.map(|(name, order)| (order, name)))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    actual.retain(|(order, _)| *order > 0);
    actual.sort_by_key(|(order, _)| *order);
    let actual = actual.into_iter().map(|(_, name)| name).collect::<Vec<_>>();
    if actual
        != expected
            .iter()
            .map(|column| (*column).to_owned())
            .collect::<Vec<_>>()
    {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "{table} primary key does not match required schema"
        )));
    }
    Ok(())
}

fn require_unique_key(
    connection: &Connection,
    table: &str,
    expected: &[&str],
) -> rusqlite::Result<()> {
    let mut indexes = connection.prepare(&format!("PRAGMA index_list('{table}')"))?;
    let index_rows = indexes.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)? != 0))
    })?;
    for index_row in index_rows {
        let (index, unique) = index_row?;
        if !unique {
            continue;
        }
        let mut info = connection.prepare(&format!("PRAGMA index_info('{index}')"))?;
        let columns = info
            .query_map([], |row| row.get::<_, String>(2))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if columns == expected {
            return Ok(());
        }
    }
    Err(rusqlite::Error::InvalidParameterName(format!(
        "{table} is missing required unique key"
    )))
}

fn require_foreign_key(
    connection: &Connection,
    table: &str,
    parent: &str,
    expected: &[(&str, &str)],
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA foreign_key_list('{table}')"))?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let ids = rows
        .iter()
        .filter(|(_, table_name, _, _)| table_name == parent)
        .map(|(id, _, _, _)| *id)
        .collect::<std::collections::BTreeSet<_>>();
    if ids.iter().any(|id| {
        expected.iter().all(|(from, to)| {
            rows.iter().any(|(row_id, table_name, row_from, row_to)| {
                row_id == id && table_name == parent && row_from == from && row_to == to
            })
        })
    }) {
        return Ok(());
    }
    Err(rusqlite::Error::InvalidParameterName(format!(
        "{table} is missing required foreign key to {parent}"
    )))
}

fn require_non_empty_check(
    connection: &Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<()> {
    require_check_fragment(connection, table, &format!("length({column})>0"))
}

fn require_check_fragment(
    connection: &Connection,
    table: &str,
    fragment: &str,
) -> rusqlite::Result<()> {
    let sql: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let normalized_sql = normalize(&sql);
    if normalized_sql.contains(&normalize(fragment)) {
        return Ok(());
    }
    Err(rusqlite::Error::InvalidParameterName(format!(
        "{table} is missing required CHECK constraint"
    )))
}

fn require_trigger(connection: &Connection, trigger: &str) -> rusqlite::Result<()> {
    let found: i64 = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1
        )",
        [trigger],
        |row| row.get(0),
    )?;
    if found == 0 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "required trigger {trigger} is missing"
        )));
    }
    Ok(())
}

fn to_domain_sql_error(error: DomainError) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(error.to_string())
}

fn sqlite_error_kind(error: &rusqlite::Error) -> StorageErrorKind {
    if let rusqlite::Error::SqliteFailure(code, _) = error {
        if matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ) {
            return StorageErrorKind::DatabaseBusy;
        }
        if matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
        ) {
            return StorageErrorKind::DatabaseCorrupt;
        }
    }
    let reason = error.to_string();
    let lower = reason.to_ascii_lowercase();
    if lower.contains("not a database")
        || lower.contains("database disk image is malformed")
        || lower.contains("database corruption")
    {
        StorageErrorKind::DatabaseCorrupt
    } else {
        StorageErrorKind::Database
    }
}

fn classify_sqlite(error: rusqlite::Error) -> StorageError {
    if sqlite_error_kind(&error) == StorageErrorKind::DatabaseCorrupt {
        StorageError::database_corrupt(error)
    } else {
        StorageError::sqlite(error)
    }
}

fn current_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
