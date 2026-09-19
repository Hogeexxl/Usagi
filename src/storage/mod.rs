//! SQLite storage bootstrap for Usagi.
//!
//! This module owns the connection, PRAGMA setup, schema migration, source
//! observation/checkpoint writes, metadata commits, scan lifecycle, and the
//! `CODEX_HOME` binding.  SQL and transactions remain private to storage.

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
use sha2::{Digest, Sha256};
use tokio::sync::watch;

pub(crate) mod cost;
mod lifecycle;
mod metadata;
mod migrations;
pub(crate) mod source;
pub(crate) mod usage;

pub use crate::domain::{AppState, SourceBindingStatus};

pub type Result<T> = std::result::Result<T, StorageError>;

const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;

const REQUIRED_TABLES: &[&str] = &[
    "app_meta",
    "scan_runs",
    "source_files",
    "source_checkpoints",
    "rollout_metadata_facts",
    "threads",
    "usage_events",
    "usage_event_occurrences",
    "turns",
    "ingest_anomalies",
    "usage_source_states",
    "usage_build_sources",
    "skill_usage_events",
];

type BindingRow = (Option<String>, String, i64, Option<String>, Option<String>);

/// Stable, opaque categories suitable for API error mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageErrorKind {
    Database,
    DatabaseBusy,
    DatabaseCorrupt,
    Io,
    SchemaTooNew,
    SourceChanged,
    SourceUnbound,
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

    pub(crate) fn source_changed(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        let diagnostic = InternalStorageError(format!(
            "CODEX_HOME fingerprint changed from {} to {}",
            expected.into(),
            actual.into()
        ));
        Self::with_source(StorageErrorKind::SourceChanged, diagnostic)
    }

    pub(crate) fn source_unbound() -> Self {
        Self::without_source(StorageErrorKind::SourceUnbound)
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
                kind: StorageErrorKind::SourceChanged,
                ..
            } => formatter.write_str("CODEX_HOME does not match the database binding"),
            Self {
                kind: StorageErrorKind::SourceUnbound,
                ..
            } => formatter.write_str("database has no CODEX_HOME binding"),
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
    /// Optional Codex home to bind. If omitted, `CODEX_HOME` or the platform
    /// user's Home/.codex path is used. This directory is never created by the
    /// opener.
    pub codex_home: Option<PathBuf>,
}

impl LedgerOptions {
    pub fn new(db_path: impl Into<PathBuf>, codex_home: impl Into<PathBuf>) -> Self {
        Self {
            db_path: Some(db_path.into()),
            codex_home: Some(codex_home.into()),
        }
    }

    pub fn for_database(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: Some(db_path.into()),
            codex_home: None,
        }
    }

    pub fn with_database_path(mut self, db_path: impl Into<PathBuf>) -> Self {
        self.db_path = Some(db_path.into());
        self
    }

    pub fn with_db_path(self, db_path: impl Into<PathBuf>) -> Self {
        self.with_database_path(db_path)
    }

    pub fn with_codex_home(mut self, codex_home: impl Into<PathBuf>) -> Self {
        self.codex_home = Some(codex_home.into());
        self
    }

    pub fn database_path(&self) -> Result<PathBuf> {
        self.db_path
            .clone()
            .map(|path| paths::normalize_path(path).map_err(StorageError::from))
            .unwrap_or_else(default_database_path_with_legacy_migration)
    }

    pub fn codex_home_path(&self) -> Result<PathBuf> {
        paths::normalize_path(paths::resolve_codex_home(self.codex_home.clone()))
            .map_err(StorageError::from)
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

/// The MU SQLite ledger.  Connections and SQL remain private to this module.
pub struct Ledger {
    db_path: PathBuf,
    codex_home: PathBuf,
    codex_home_fingerprint: String,
    connection: Mutex<Connection>,
    revision_sender: watch::Sender<RevisionTuple>,
}

impl fmt::Debug for Ledger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ledger")
            .field("db_path", &self.db_path)
            .field("codex_home", &self.codex_home)
            .field("codex_home_fingerprint", &self.codex_home_fingerprint)
            .finish_non_exhaustive()
    }
}

impl Ledger {
    /// Open/create a ledger, configure SQLite, run migrations, and bind the
    /// normalized Codex home.  A mismatch is persisted as `source_changed` and
    /// still returns a readable Ledger; mutating consumers must call
    /// [`Ledger::ensure_source_ready`] before writing.
    pub fn open(options: LedgerOptions) -> Result<Self> {
        let db_path = options.database_path()?;
        let codex_home = options.codex_home_path()?;
        let codex_home_fingerprint = fingerprint_for_path(&codex_home);

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
        bind_codex_home(&mut connection, &codex_home_fingerprint).map_err(classify_sqlite)?;
        cost::refresh_usage_costs_if_needed(&mut connection)?;
        let initial_revision = read_revision_tuple(&connection).map_err(classify_sqlite)?;
        let (revision_sender, _) = watch::channel(initial_revision);

        Ok(Self {
            db_path,
            codex_home,
            codex_home_fingerprint,
            connection: Mutex::new(connection),
            revision_sender,
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub fn expected_codex_home_fingerprint(&self) -> &str {
        &self.codex_home_fingerprint
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
                followup_error_code,
                codex_home_fingerprint,
                source_binding_status
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
                    let source_binding_status: String = row.get(17)?;
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
                    let source_binding_status =
                        SourceBindingStatus::try_from(source_binding_status.as_str())
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
                            source_binding_status,
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

    pub fn source_binding_status(&self) -> Result<SourceBindingStatus> {
        Ok(self.app_state()?.source_binding_status)
    }

    /// Return an error for consumers that would write source-derived facts.
    /// A `source_changed` Ledger remains usable for read-only queries.
    pub fn ensure_source_ready(&self) -> Result<()> {
        match self.source_binding_status()? {
            SourceBindingStatus::Ready => {
                let connection = self.connection()?;
                let stored_fingerprint: Option<String> = connection
                    .query_row(
                        "SELECT codex_home_fingerprint FROM app_meta WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                match stored_fingerprint {
                    Some(expected) if expected == self.codex_home_fingerprint => Ok(()),
                    Some(expected) => Err(StorageError::source_changed(
                        expected,
                        self.codex_home_fingerprint.clone(),
                    )),
                    None => Err(StorageError::invalid_state(
                        "ready CODEX_HOME binding has no fingerprint",
                    )),
                }
            }
            SourceBindingStatus::Unbound => Err(StorageError::source_unbound()),
            SourceBindingStatus::SourceChanged => {
                let connection = self.connection()?;
                let stored_fingerprint: Option<String> = connection
                    .query_row(
                        "SELECT codex_home_fingerprint FROM app_meta WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                Err(StorageError::source_changed(
                    stored_fingerprint.unwrap_or_default(),
                    self.codex_home_fingerprint.clone(),
                ))
            }
        }
    }

    /// Stable fingerprint for a path after absolute/lexical normalization.
    pub fn codex_home_fingerprint(path: impl AsRef<Path>) -> String {
        fingerprint_for_path(
            &paths::normalize_path(path.as_ref().to_path_buf())
                .unwrap_or_else(|_| path.as_ref().to_path_buf()),
        )
    }

    /// Subscribe to the latest committed `(data_revision,status_revision)`.
    /// The channel is process-local; its initial value is loaded from SQLite.
    pub fn subscribe_revisions(&self) -> watch::Receiver<RevisionTuple> {
        self.revision_sender.subscribe()
    }

    pub fn current_revision(&self) -> RevisionTuple {
        *self.revision_sender.borrow()
    }

    pub(crate) fn publish_revisions(&self, data_revision: i64, status_revision: i64) {
        self.revision_sender.send_if_modified(|current| {
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

    pub(crate) fn publish_scan_state(&self, data_revision: i64, state: &ScanState) {
        self.publish_revisions(data_revision, state.status_revision);
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| StorageError::lock_poisoned())
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

fn validate_schema(connection: &Connection, db_path: &Path) -> rusqlite::Result<()> {
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
    Ok(())
}

fn bind_codex_home(connection: &mut Connection, fingerprint: &str) -> rusqlite::Result<()> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current: Option<BindingRow> = transaction
        .query_row(
            "SELECT
                codex_home_fingerprint,
                source_binding_status,
                status_revision,
                followup_state,
                followup_scan_id
             FROM app_meta
             WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (stored_fingerprint, status, status_revision, followup_state, followup_scan_id) = current
        .ok_or_else(
        || rusqlite::Error::InvalidParameterName("app_meta row id=1 is missing".to_owned()),
    )?;

    match (stored_fingerprint, status.as_str()) {
        (None, "unbound") => {
            transaction.execute(
                "UPDATE app_meta
                 SET codex_home_fingerprint = ?1, source_binding_status = 'ready'
                 WHERE id = 1",
                [fingerprint],
            )?;
        }
        (Some(stored), "ready") if stored == fingerprint => {}
        (Some(stored), "ready") => {
            let next_revision = status_revision.checked_add(1).ok_or_else(|| {
                rusqlite::Error::InvalidParameterName("status_revision overflow".to_owned())
            })?;
            if followup_state.as_deref() == Some("queued") {
                let followup_scan_id = followup_scan_id.ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "queued follow-up is missing its scan id".to_owned(),
                    )
                })?;
                let changed = transaction.execute(
                    "UPDATE scan_runs
                     SET state = 'start_failed',
                         finished_at_ms = ?2,
                         terminal_status_revision = ?3,
                         error_code = 'SOURCE_CHANGED'
                     WHERE scan_id = ?1 AND state = 'queued'",
                    params![followup_scan_id, current_time_ms(), next_revision],
                )?;
                if changed != 1 {
                    return Err(rusqlite::Error::InvalidParameterName(
                        "queued follow-up has no matching scan run".to_owned(),
                    ));
                }
                transaction.execute(
                    "UPDATE app_meta
                     SET source_binding_status = 'source_changed',
                         status_revision = ?1,
                         followup_state = 'start_failed',
                         followup_error_code = 'SOURCE_CHANGED'
                     WHERE id = 1",
                    [next_revision],
                )?;
            } else {
                transaction.execute(
                    "UPDATE app_meta
                     SET source_binding_status = 'source_changed',
                         status_revision = ?1
                     WHERE id = 1",
                    [next_revision],
                )?;
            }
            // Keep the original fingerprint in app_meta.  It is the expected
            // source identity and lets the caller diagnose the mismatch.
            let _ = stored;
        }
        (Some(stored), "source_changed") if stored == fingerprint => {
            // Recovery is deliberately explicit; reopening with the old home
            // must not silently clear source_changed.
        }
        (Some(_), "source_changed") => {}
        (None, _) | (Some(_), "unbound") => {
            return Err(rusqlite::Error::InvalidParameterName(
                "inconsistent app_meta source binding".to_owned(),
            ));
        }
        (_, other) => {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unknown source binding status {other:?}"
            )));
        }
    }
    transaction.commit()
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

fn fingerprint_for_path(path: &Path) -> String {
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use rusqlite::Connection;

    use super::{
        Ledger, LedgerOptions, PragmaState, SourceBindingStatus, StorageErrorKind,
        migrate_legacy_database_if_needed, usage_event_count,
    };
    use crate::domain::{AppState, FollowupState, ScanTrigger};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("usagi-storage-{unique}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn options(root: &TempDir) -> LedgerOptions {
        LedgerOptions::new(
            root.path().join("nested/db/mu.sqlite3"),
            root.path().join("codex"),
        )
    }

    fn seed_usage_database(path: &Path, rows: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("CREATE TABLE usage_events (event_id TEXT PRIMARY KEY);")
            .unwrap();
        for index in 0..rows {
            connection
                .execute(
                    "INSERT INTO usage_events(event_id) VALUES (?1)",
                    [format!("event-{index}")],
                )
                .unwrap();
        }
    }

    #[test]
    fn rename_migration_moves_legacy_directory_when_new_database_is_missing() {
        let root = TempDir::new();
        let legacy = root.path().join("legacy-name").join("mu.sqlite3");
        let current = root.path().join("Usagi").join("mu.sqlite3");
        seed_usage_database(&legacy, 2);
        fs::write(legacy.parent().unwrap().join("sidecar-state"), b"keep").unwrap();

        migrate_legacy_database_if_needed(&current, &legacy).unwrap();

        assert_eq!(usage_event_count(&current).unwrap(), 2);
        assert_eq!(
            fs::read(current.parent().unwrap().join("sidecar-state")).unwrap(),
            b"keep"
        );
        assert!(!legacy.parent().unwrap().exists());
    }

    #[test]
    fn rename_migration_replaces_empty_database_created_by_broken_rename_build() {
        let root = TempDir::new();
        let legacy = root.path().join("legacy-name").join("mu.sqlite3");
        let current = root.path().join("Usagi").join("mu.sqlite3");
        seed_usage_database(&legacy, 3);
        seed_usage_database(&current, 0);

        migrate_legacy_database_if_needed(&current, &legacy).unwrap();

        assert_eq!(usage_event_count(&current).unwrap(), 3);
        assert!(!legacy.parent().unwrap().exists());
    }

    #[test]
    fn rename_migration_never_overwrites_nonempty_usagi_database() {
        let root = TempDir::new();
        let legacy = root.path().join("legacy-name").join("mu.sqlite3");
        let current = root.path().join("Usagi").join("mu.sqlite3");
        seed_usage_database(&legacy, 3);
        seed_usage_database(&current, 1);

        migrate_legacy_database_if_needed(&current, &legacy).unwrap();

        assert_eq!(usage_event_count(&current).unwrap(), 1);
        assert_eq!(usage_event_count(&legacy).unwrap(), 3);
    }

    #[test]
    fn opens_nested_database_and_verifies_pragmas() {
        let root = TempDir::new();
        let ledger = Ledger::open(options(&root)).unwrap();
        assert!(ledger.database_path().exists());
        assert_eq!(ledger.schema_version().unwrap(), 10);
        assert_eq!(
            ledger.pragma_state().unwrap(),
            PragmaState {
                journal_mode_wal: true,
                synchronous_normal: true,
                foreign_keys: true,
                busy_timeout_ms: 5_000,
            }
        );
        let state = ledger.app_state().unwrap();
        assert_eq!(state.data_revision, 1);
        assert_eq!(state.status_revision, 0);
        assert_eq!(state.source_binding_status, SourceBindingStatus::Ready);
        assert!(!root.path().join("codex").is_dir());
    }

    #[test]
    fn reopening_preserves_binding_and_schema() {
        let root = TempDir::new();
        let opts = options(&root);
        let first = Ledger::open(opts.clone()).unwrap();
        let fingerprint = first.expected_codex_home_fingerprint().to_owned();
        drop(first);
        let second = Ledger::open(opts).unwrap();
        assert_eq!(second.expected_codex_home_fingerprint(), fingerprint);
        assert_eq!(second.schema_version().unwrap(), 10);
        assert_eq!(second.app_state().unwrap().status_revision, 0);
    }

    #[test]
    fn mismatched_home_is_readable_but_not_writable() {
        let root = TempDir::new();
        let db = root.path().join("mu.sqlite3");
        let home_a = root.path().join("codex-a");
        let home_b = root.path().join("codex-b");
        let first = Ledger::open(LedgerOptions::new(&db, &home_a)).unwrap();
        drop(first);

        let changed = Ledger::open(LedgerOptions::new(&db, &home_b)).unwrap();
        assert_eq!(
            changed.app_state().unwrap().source_binding_status,
            SourceBindingStatus::SourceChanged
        );
        assert_eq!(changed.app_state().unwrap().status_revision, 1);
        assert_eq!(
            changed.ensure_source_ready().unwrap_err().kind(),
            StorageErrorKind::SourceChanged
        );
        drop(changed);

        // Reopening with the original source remains readable, while explicit
        // recovery is still required to clear source_changed.
        let original = Ledger::open(LedgerOptions::new(db, home_a)).unwrap();
        assert_eq!(
            original.app_state().unwrap().source_binding_status,
            SourceBindingStatus::SourceChanged
        );
    }

    #[test]
    fn app_state_uses_domain_projection_and_source_change_fails_queued_followup() {
        let root = TempDir::new();
        let db = root.path().join("mu.sqlite3");
        let home_a = root.path().join("codex-a");
        let home_b = root.path().join("codex-b");
        let first = Ledger::open(LedgerOptions::new(&db, &home_a)).unwrap();

        let _: AppState = first.app_state().unwrap();
        let connection = first.connection().unwrap();
        connection
            .execute(
                "INSERT INTO scan_runs (
                    scan_id, trigger, request_kind, state, requested_at_ms,
                    enqueued_status_revision
                 ) VALUES ('followup-1', 'Manual', 'followup', 'queued', 100, 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE app_meta
                 SET followup_scan_id = 'followup-1',
                     followup_state = 'queued',
                     followup_trigger = 'Manual',
                     followup_requested_at_ms = 100,
                     followup_enqueued_status_revision = 0
                 WHERE id = 1",
                [],
            )
            .unwrap();
        drop(connection);
        drop(first);

        let changed = Ledger::open(LedgerOptions::new(&db, &home_b)).unwrap();
        let state = changed.app_state().unwrap();
        assert_eq!(
            state.source_binding_status,
            SourceBindingStatus::SourceChanged
        );
        assert_eq!(state.status_revision, 1);
        assert_eq!(state.followup_state, Some(FollowupState::StartFailed));
        assert_eq!(state.followup_trigger, Some(ScanTrigger::Manual));
        assert_eq!(state.followup_error_code.as_deref(), Some("SOURCE_CHANGED"));

        let connection = changed.connection().unwrap();
        let row: (String, Option<i64>, Option<i64>, Option<String>) = connection
            .query_row(
                "SELECT state, started_at_ms, terminal_status_revision, error_code
                 FROM scan_runs WHERE scan_id = 'followup-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(row.0, "start_failed");
        assert!(row.1.is_none());
        assert_eq!(row.2, Some(1));
        assert_eq!(row.3.as_deref(), Some("SOURCE_CHANGED"));
    }

    #[test]
    fn newer_schema_is_rejected_without_deleting_database() {
        let root = TempDir::new();
        let db = root.path().join("future.sqlite3");
        let connection = Connection::open(&db).unwrap();
        connection
            .pragma_update(None, "user_version", 99_i64)
            .unwrap();
        drop(connection);
        let before = fs::read(&db).unwrap();
        let error = Ledger::open(LedgerOptions::new(&db, root.path().join("codex"))).unwrap_err();
        assert_eq!(error.kind(), StorageErrorKind::SchemaTooNew);
        assert_eq!(error.schema_versions(), Some((99, 10)));
        assert_eq!(fs::read(&db).unwrap(), before);
    }

    #[test]
    fn corrupt_database_is_not_removed() {
        let root = TempDir::new();
        let db = root.path().join("corrupt.sqlite3");
        let bytes = b"not a sqlite database";
        fs::write(&db, bytes).unwrap();
        let error = Ledger::open(LedgerOptions::new(&db, root.path().join("codex"))).unwrap_err();
        assert!(matches!(
            error.kind(),
            StorageErrorKind::DatabaseCorrupt | StorageErrorKind::Database
        ));
        assert_eq!(fs::read(&db).unwrap(), bytes);
    }

    #[test]
    fn migration_failure_rolls_back_schema_and_version() {
        let root = TempDir::new();
        let db = root.path().join("migration-failure.sqlite3");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE source_files (sentinel INTEGER); PRAGMA user_version = 0;")
            .unwrap();
        drop(connection);

        assert!(Ledger::open(LedgerOptions::new(&db, root.path().join("codex"))).is_err());
        let connection = Connection::open(&db).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
        let sentinel: i64 = connection
            .query_row("SELECT count(*) FROM source_files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sentinel, 0);
        assert!(
            connection
                .query_row::<i64, _, _>("SELECT count(*) FROM app_meta", [], |row| row.get(0))
                .is_err()
        );
    }

    fn seed_cost_events(ledger: &Ledger, overflow: bool) -> i64 {
        let connection = ledger.connection().unwrap();
        connection
            .execute(
                "INSERT INTO threads(
                    thread_id,parent_thread_id,root_session_id,agent_role,archived,
                    project_kind,metadata_quality_status,metadata_resolved_at_ms
                 ) VALUES ('cost-root',NULL,'cost-root','main',0,'unknown','complete',0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_files(
                    source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                 ) VALUES (1,'cost-root','/tmp/usagi-cost.jsonl','sessions',1,1,1,100,0,'present',0)",
                [],
            )
            .unwrap();
        if overflow {
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,quality_status,source_file_id,file_generation,source_start_offset,
                        source_end_offset,created_at_ms
                     ) VALUES (1,'auto-review','normal',0,'cost-root','cost-root',NULL,'codex-auto-review',NULL,NULL,
                               1000,200,100,50,20,1050,'complete',1,1,0,1,0),
                              (1,'overflow','normal',0,'cost-root','cost-root',NULL,'gpt-5.6-sol',NULL,NULL,
                               9000000000000000,0,0,0,0,9000000000000000,'complete',1,1,1,2,0)",
                    [],
                )
                .unwrap();
        } else {
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
                        turn_key,model,reasoning_effort,estimated_cost_nanos_usd,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,quality_status,source_file_id,file_generation,source_start_offset,
                        source_end_offset,created_at_ms
                     ) VALUES (1,'known','normal',0,'cost-root','cost-root',NULL,'gpt-5.6-sol','high',NULL,
                               1000,200,100,50,20,1050,'complete',1,1,0,1,0),
                              (1,'auto-review','normal',0,'cost-root','cost-root',NULL,'codex-auto-review','high',NULL,
                               1000,200,100,50,20,1050,'complete',1,1,1,2,0),
                              (1,'unknown','recovered',0,'cost-root','cost-root',NULL,'unknown-model',NULL,NULL,
                               1000,200,100,50,20,1050,'complete',1,1,2,3,0)",
                    [],
                )
                .unwrap();
        }
        let revision: i64 = connection
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        connection
            .execute(
                "UPDATE app_meta SET cost_algorithm_version=0,pricing_catalog_version=0 WHERE id=1",
                [],
            )
            .unwrap();
        revision
    }

    #[test]
    fn t_mu04_a02_open_reprices_pricing_catalog_atomically() {
        let root = TempDir::new();
        let opts = options(&root);
        let first = Ledger::open(opts.clone()).unwrap();
        let parser_version_before: i64 = first
            .connection()
            .unwrap()
            .query_row(
                "SELECT usage_parser_version FROM app_meta WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let before_revision = seed_cost_events(&first, false);
        drop(first);

        let reopened = Ledger::open(opts).unwrap();
        let connection = reopened.connection().unwrap();
        type RepricedCosts = (
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
        );
        let costs: RepricedCosts = connection
            .query_row(
                "SELECT
                    (SELECT estimated_cost_nanos_usd FROM usage_events WHERE event_id='known'),
                    (SELECT estimated_cost_nanos_usd FROM usage_events WHERE event_id='unknown'),
                    (SELECT estimated_cost_nanos_usd FROM usage_events WHERE event_id='auto-review'),
                    (SELECT model FROM usage_events WHERE event_id='auto-review'),
                    (SELECT cost_algorithm_version FROM app_meta WHERE id=1),
                    (SELECT pricing_catalog_version FROM app_meta WHERE id=1),
                    (SELECT data_revision FROM app_meta WHERE id=1),
                    (SELECT usage_parser_version FROM app_meta WHERE id=1)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            costs,
            (
                Some(4_380_000),
                None,
                Some(229_000),
                Some("codex-auto-review".to_owned()),
                1,
                4,
                before_revision + 1,
                parser_version_before,
            )
        );
        drop(connection);
        assert_eq!(
            reopened.current_revision().data_revision,
            before_revision + 1
        );
    }

    #[test]
    fn t_mu03_b05_open_reprice_rolls_back_on_overflow() {
        let root = TempDir::new();
        let opts = options(&root);
        let first = Ledger::open(opts.clone()).unwrap();
        let before_revision = seed_cost_events(&first, true);
        drop(first);

        assert!(Ledger::open(opts.clone()).is_err());
        let connection = Connection::open(root.path().join("nested/db/mu.sqlite3")).unwrap();
        let state: (Option<i64>, Option<i64>, i64, i64, i64) = connection
            .query_row(
                "SELECT
                        (SELECT estimated_cost_nanos_usd FROM usage_events
                         WHERE event_id='auto-review'),
                        (SELECT estimated_cost_nanos_usd FROM usage_events
                         WHERE event_id='overflow'),
                        cost_algorithm_version,
                        pricing_catalog_version,data_revision
                 FROM usage_events JOIN app_meta ON app_meta.id=1
                 WHERE usage_events.event_id='overflow'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, (None, None, 0, 0, before_revision));
    }
}
