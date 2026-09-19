//! Read-only adapter for Codex's `state_5.sqlite` index.
//!
//! The adapter deliberately has its own small source types.  They are facts
//! from Codex, not normalized MU rows, and contain no message, preview, or
//! prompt fields.  The resolver can therefore consume this module without
//! giving the SQLite schema a place in the rest of the application.

use std::{collections::HashMap, fmt, path::Path, time::Duration};

use chrono::DateTime;
use rusqlite::{Connection, OpenFlags, TransactionBehavior, types::ValueRef};

use super::{DiagnosticSeverity, SourceAvailability};
use crate::platform::paths;

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_millis(2_000);
const MAX_REASONABLE_EPOCH_MS: i64 = 253_402_300_799_999;

/// A privacy-safe state-index diagnostic.  It carries only a code and
/// optional identifiers/field names; it never stores SQL, a row, or a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub thread_id: Option<String>,
    pub field: Option<String>,
    pub source_kind: &'static str,
}

impl StateDiagnostic {
    fn new(code: impl Into<String>, severity: DiagnosticSeverity) -> Self {
        Self {
            code: code.into(),
            severity,
            thread_id: None,
            field: None,
            source_kind: "state_5_sqlite",
        }
    }

    fn field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    fn thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

/// Whether the required state source was available for this snapshot.
pub type StateSourceStatus = SourceAvailability;

/// One non-sensitive row from `threads`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateThreadFact {
    pub thread_id: String,
    pub rollout_path: Option<String>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    pub archived: Option<bool>,
    pub title: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub metadata_model: Option<String>,
    pub agent_role_hint: Option<String>,
    pub agent_path: Option<String>,
}

impl StateThreadFact {
    pub fn id(&self) -> &str {
        &self.thread_id
    }
}

/// Source marker for an edge read from the state database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnEdgeSource {
    StateSpawnEdge,
}

impl SpawnEdgeSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StateSpawnEdge => "state_spawn_edge",
        }
    }
}

/// One parent → child relation from `thread_spawn_edges`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnEdgeFact {
    pub parent_thread_id: String,
    pub child_thread_id: String,
    pub status: Option<String>,
    pub source: SpawnEdgeSource,
    /// State edges do not always carry an event timestamp.  `None` means no
    /// trustworthy timestamp was present; it is never filled with wall time.
    pub observed_at_ms: Option<i64>,
}

/// A complete read-only view of the state source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSnapshot {
    pub status: StateSourceStatus,
    pub threads: Vec<StateThreadFact>,
    pub spawn_edges: Vec<SpawnEdgeFact>,
    pub spawn_edges_status: StateSourceStatus,
    pub diagnostics: Vec<StateDiagnostic>,
}

impl StateSnapshot {
    pub fn unavailable(diagnostics: Vec<StateDiagnostic>) -> Self {
        Self {
            status: StateSourceStatus::Unavailable,
            threads: Vec::new(),
            spawn_edges: Vec::new(),
            spawn_edges_status: StateSourceStatus::Unavailable,
            diagnostics,
        }
    }

    pub fn is_available(&self) -> bool {
        self.status.is_complete()
    }

    pub fn thread(&self, thread_id: &str) -> Option<&StateThreadFact> {
        self.threads
            .iter()
            .find(|thread| thread.thread_id == thread_id)
    }
}

/// Errors which prevent opening or querying the source at all.  Schema
/// incompatibilities are represented in `StateSnapshot::status` so a caller
/// can keep existing normalized values without manufacturing a Clear.
#[derive(Debug)]
pub enum StateIndexError {
    Sqlite(rusqlite::Error),
}

impl fmt::Display for StateIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(_) => formatter.write_str("state index could not be read"),
        }
    }
}

impl std::error::Error for StateIndexError {}

impl From<rusqlite::Error> for StateIndexError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Read-only state-index adapter.
#[derive(Clone, Copy, Debug)]
pub struct StateIndexReader {
    busy_timeout: Duration,
}

impl Default for StateIndexReader {
    fn default() -> Self {
        Self::new()
    }
}

impl StateIndexReader {
    pub const fn new() -> Self {
        Self {
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }

    pub const fn with_busy_timeout(busy_timeout: Duration) -> Self {
        Self { busy_timeout }
    }

    /// Read one consistent, read-only snapshot.
    pub fn read_snapshot<P: AsRef<Path>>(path: P) -> Result<StateSnapshot, StateIndexError> {
        Self::new().read_snapshot_with_options(path)
    }

    pub fn read_snapshot_with_options<P: AsRef<Path>>(
        self,
        path: P,
    ) -> Result<StateSnapshot, StateIndexError> {
        let mut connection = open_read_only_connection(path.as_ref(), self.busy_timeout)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;

        let mut diagnostics = Vec::new();
        let thread_columns = table_columns(&transaction, "threads")?;
        if thread_columns.is_empty() {
            diagnostics.push(StateDiagnostic::new(
                "missing_required_table",
                DiagnosticSeverity::Error,
            ));
            return Ok(StateSnapshot::unavailable(diagnostics));
        }
        if !thread_columns.iter().any(|column| column == "id") {
            diagnostics.push(
                StateDiagnostic::new("missing_required_column", DiagnosticSeverity::Error)
                    .field("threads.id"),
            );
            return Ok(StateSnapshot::unavailable(diagnostics));
        }

        let threads = read_threads(&transaction, &thread_columns, &mut diagnostics)?;

        let edge_columns = table_columns(&transaction, "thread_spawn_edges")?;
        let (spawn_edges, spawn_edges_status) = if edge_columns.is_empty() {
            diagnostics.push(StateDiagnostic::new(
                "missing_spawn_edges_table",
                DiagnosticSeverity::Warning,
            ));
            (Vec::new(), StateSourceStatus::Unavailable)
        } else if !has_edge_ids(&edge_columns) {
            diagnostics.push(
                StateDiagnostic::new("missing_spawn_edge_columns", DiagnosticSeverity::Warning)
                    .field("thread_spawn_edges.parent_thread_id/child_thread_id"),
            );
            (Vec::new(), StateSourceStatus::Unavailable)
        } else {
            (
                read_spawn_edges(&transaction, &edge_columns, &mut diagnostics)?,
                StateSourceStatus::Complete,
            )
        };

        transaction.commit()?;
        Ok(StateSnapshot {
            status: StateSourceStatus::Complete,
            threads,
            spawn_edges,
            spawn_edges_status,
            diagnostics,
        })
    }
}

fn open_read_only_connection(
    path: &Path,
    busy_timeout: Duration,
) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(busy_timeout)?;
    // Keep query_only as a second boundary in case a later query is added.
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

fn table_columns(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = transaction.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

const THREAD_ALLOWLIST: &[&str] = &[
    "id",
    "rollout_path",
    "created_at",
    "created_at_ms",
    "updated_at",
    "updated_at_ms",
    "archived",
    "cwd",
    "title",
    "name",
    "model",
    "agent_role",
    "agent_path",
];

const EDGE_ALLOWLIST: &[&str] = &[
    "parent_thread_id",
    "child_thread_id",
    "status",
    "observed_at",
    "observed_at_ms",
    "created_at",
    "created_at_ms",
    "updated_at",
    "updated_at_ms",
];

fn selected_columns<'a>(available: &'a [String], allowlist: &[&str]) -> Vec<&'a str> {
    allowlist
        .iter()
        .filter_map(|candidate| {
            available
                .iter()
                .find(|column| column.as_str() == *candidate)
                .map(String::as_str)
        })
        .collect()
}

fn read_threads(
    transaction: &rusqlite::Transaction<'_>,
    available: &[String],
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Result<Vec<StateThreadFact>, rusqlite::Error> {
    let columns = selected_columns(available, THREAD_ALLOWLIST);
    let sql = format!("SELECT {} FROM threads ORDER BY id", columns.join(", "));
    let mut statement = transaction.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let positions: HashMap<&str, usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| (*column, index))
        .collect();
    let mut facts = Vec::new();

    while let Some(row) = rows.next()? {
        let id = match positions
            .get("id")
            .and_then(|index| row.get_ref(*index).ok())
            .and_then(value_string)
            .map(|value| value.trim().to_owned())
        {
            Some(id) if valid_identifier(&id) => id,
            _ => {
                diagnostics.push(
                    StateDiagnostic::new("invalid_thread_id", DiagnosticSeverity::Warning)
                        .field("threads.id"),
                );
                continue;
            }
        };

        let mut fact = StateThreadFact {
            thread_id: id.clone(),
            rollout_path: None,
            created_at_ms: None,
            updated_at_ms: None,
            archived: None,
            title: None,
            name: None,
            cwd: None,
            metadata_model: None,
            agent_role_hint: None,
            agent_path: None,
        };

        fact.rollout_path = optional_path(
            row_value(&positions, row, "rollout_path"),
            &id,
            "rollout_path",
            diagnostics,
        );
        fact.created_at_ms = optional_time(
            row_value(&positions, row, "created_at_ms"),
            row_value(&positions, row, "created_at"),
            true,
            &id,
            "created_at",
            diagnostics,
        );
        fact.updated_at_ms = optional_time(
            row_value(&positions, row, "updated_at_ms"),
            row_value(&positions, row, "updated_at"),
            true,
            &id,
            "updated_at",
            diagnostics,
        );
        fact.archived = optional_archived(row_value(&positions, row, "archived"), &id, diagnostics);
        fact.title = optional_text(
            row_value(&positions, row, "title"),
            &id,
            "title",
            diagnostics,
        );
        fact.name = optional_text(row_value(&positions, row, "name"), &id, "name", diagnostics);
        fact.cwd = optional_path(row_value(&positions, row, "cwd"), &id, "cwd", diagnostics);
        fact.metadata_model = optional_text(
            row_value(&positions, row, "model"),
            &id,
            "model",
            diagnostics,
        );
        fact.agent_role_hint = optional_text(
            row_value(&positions, row, "agent_role"),
            &id,
            "agent_role",
            diagnostics,
        );
        fact.agent_path =
            optional_agent_path(row_value(&positions, row, "agent_path"), &id, diagnostics);
        facts.push(fact);
    }
    Ok(facts)
}

fn has_edge_ids(columns: &[String]) -> bool {
    columns.iter().any(|column| column == "parent_thread_id")
        && columns.iter().any(|column| column == "child_thread_id")
}

fn read_spawn_edges(
    transaction: &rusqlite::Transaction<'_>,
    available: &[String],
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Result<Vec<SpawnEdgeFact>, rusqlite::Error> {
    let columns = selected_columns(available, EDGE_ALLOWLIST);
    let sql = format!(
        "SELECT {} FROM thread_spawn_edges ORDER BY parent_thread_id, child_thread_id",
        columns.join(", ")
    );
    let mut statement = transaction.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let positions: HashMap<&str, usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| (*column, index))
        .collect();
    let mut facts = Vec::new();
    while let Some(row) = rows.next()? {
        let parent = row_value(&positions, row, "parent_thread_id")
            .and_then(value_string)
            .map(|value| value.trim().to_owned());
        let child = row_value(&positions, row, "child_thread_id")
            .and_then(value_string)
            .map(|value| value.trim().to_owned());
        let (Some(parent), Some(child)) = (parent, child) else {
            diagnostics.push(StateDiagnostic::new(
                "invalid_spawn_edge_id",
                DiagnosticSeverity::Warning,
            ));
            continue;
        };
        if !valid_identifier(&parent) || !valid_identifier(&child) {
            diagnostics.push(StateDiagnostic::new(
                "invalid_spawn_edge_id",
                DiagnosticSeverity::Warning,
            ));
            continue;
        }
        let status = row_value(&positions, row, "status")
            .and_then(value_string)
            .and_then(clean_text);
        let observed_at_ms = optional_time(
            row_value(&positions, row, "observed_at_ms"),
            row_value(&positions, row, "observed_at"),
            true,
            &child,
            "observed_at",
            diagnostics,
        )
        .or_else(|| {
            optional_time(
                row_value(&positions, row, "created_at_ms"),
                row_value(&positions, row, "created_at"),
                true,
                &child,
                "created_at",
                diagnostics,
            )
        })
        .or_else(|| {
            optional_time(
                row_value(&positions, row, "updated_at_ms"),
                row_value(&positions, row, "updated_at"),
                true,
                &child,
                "updated_at",
                diagnostics,
            )
        });
        facts.push(SpawnEdgeFact {
            parent_thread_id: parent,
            child_thread_id: child,
            status,
            source: SpawnEdgeSource::StateSpawnEdge,
            observed_at_ms,
        });
    }
    Ok(facts)
}

fn row_value<'a>(
    positions: &HashMap<&str, usize>,
    row: &'a rusqlite::Row<'_>,
    column: &str,
) -> Option<ValueRef<'a>> {
    positions
        .get(column)
        .and_then(|index| row.get_ref(*index).ok())
}

fn value_string(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().map(ToOwned::to_owned),
        _ => None,
    }
}

fn clean_text(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() || value.chars().any(char::is_control) {
        None
    } else {
        Some(value)
    }
}

fn optional_text(
    value: Option<ValueRef<'_>>,
    thread_id: &str,
    field: &str,
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Option<String> {
    let value = value?;
    if matches!(value, ValueRef::Null) {
        return None;
    }
    let result = value_string(value).and_then(clean_text);
    if result.is_none() {
        diagnostics.push(
            StateDiagnostic::new("invalid_field", DiagnosticSeverity::Warning)
                .thread(thread_id)
                .field(field),
        );
    }
    result
}

/// Normalize state-index path metadata lexically so the normalized value is
/// independent of whether the referenced file/directory currently exists.
/// Filesystem canonicalization here would make a disappearing source able to
/// change a Thread field (for example macOS /var -> /private/var), causing a
/// spurious data_revision change during usage carry.
fn optional_path(
    value: Option<ValueRef<'_>>,
    thread_id: &str,
    field: &str,
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Option<String> {
    let value = value?;
    if matches!(value, ValueRef::Null) {
        return None;
    }
    let result = value_string(value).and_then(|value| {
        if value.chars().any(char::is_control) {
            return None;
        }
        paths::normalize_absolute_path(Path::new(value.trim()))
            .and_then(|path| path.to_str().map(ToOwned::to_owned))
    });
    if result.is_none() {
        diagnostics.push(
            StateDiagnostic::new("invalid_path", DiagnosticSeverity::Warning)
                .thread(thread_id)
                .field(field),
        );
    }
    result
}

fn optional_agent_path(
    value: Option<ValueRef<'_>>,
    thread_id: &str,
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Option<String> {
    let value = value?;
    if matches!(value, ValueRef::Null) {
        return None;
    }
    let result = value_string(value).and_then(|value| super::rollout::normalize_agent_path(&value));
    if result.is_none() {
        diagnostics.push(
            StateDiagnostic::new("invalid_agent_path", DiagnosticSeverity::Warning)
                .thread(thread_id)
                .field("agent_path"),
        );
    }
    result
}

fn optional_archived(
    value: Option<ValueRef<'_>>,
    thread_id: &str,
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Option<bool> {
    let value = value?;
    let result = match value {
        ValueRef::Null => None,
        ValueRef::Integer(0) => Some(false),
        ValueRef::Integer(1) => Some(true),
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes)
            .ok()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("0") | Some("false") => Some(false),
            Some("1") | Some("true") => Some(true),
            _ => None,
        },
        _ => None,
    };
    if result.is_none() && !matches!(value, ValueRef::Null) {
        diagnostics.push(
            StateDiagnostic::new("invalid_boolean", DiagnosticSeverity::Warning)
                .thread(thread_id)
                .field("archived"),
        );
    }
    result
}

fn optional_time(
    preferred: Option<ValueRef<'_>>,
    fallback: Option<ValueRef<'_>>,
    preferred_is_millis: bool,
    thread_id: &str,
    field: &str,
    diagnostics: &mut Vec<StateDiagnostic>,
) -> Option<i64> {
    let parse = |value: ValueRef<'_>, as_millis: bool| parse_time_value(value, as_millis);
    if let Some(value) = preferred
        && !matches!(value, ValueRef::Null)
        && let Some(parsed) = parse(value, preferred_is_millis)
    {
        return Some(parsed);
    }
    if let Some(value) = fallback
        && !matches!(value, ValueRef::Null)
        && let Some(parsed) = parse(value, false)
    {
        return Some(parsed);
    }
    if preferred.is_some_and(|value| !matches!(value, ValueRef::Null))
        || fallback.is_some_and(|value| !matches!(value, ValueRef::Null))
    {
        diagnostics.push(
            StateDiagnostic::new("invalid_time", DiagnosticSeverity::Warning)
                .thread(thread_id)
                .field(field),
        );
    }
    None
}

fn parse_time_value(value: ValueRef<'_>, as_millis: bool) -> Option<i64> {
    match value {
        ValueRef::Integer(number) => {
            if number < 0 {
                return None;
            }
            if as_millis {
                (number <= MAX_REASONABLE_EPOCH_MS).then_some(number)
            } else {
                number
                    .checked_mul(1_000)
                    .filter(|millis| *millis <= MAX_REASONABLE_EPOCH_MS)
            }
        }
        ValueRef::Real(number) if number.is_finite() && number >= 0.0 => {
            let scaled = if as_millis { number } else { number * 1_000.0 };
            if scaled > MAX_REASONABLE_EPOCH_MS as f64 {
                None
            } else {
                Some(scaled.round() as i64)
            }
        }
        ValueRef::Text(bytes) => {
            let text = std::str::from_utf8(bytes).ok()?.trim();
            if let Ok(number) = text.parse::<i64>() {
                return parse_time_value(ValueRef::Integer(number), as_millis);
            }
            let parsed = DateTime::parse_from_rfc3339(text).ok()?;
            let millis = parsed.timestamp_millis();
            (millis >= 0).then_some(millis)
        }
        _ => None,
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        codex::{
            metadata::{ExistingThread, ResolutionInput, ThreadMetadataResolver},
            rollout::{
                AgentRoleProvenance, Candidate, OwnershipBoundary, OwnershipConfidence,
                ParentHintProvenance, RolloutThreadFact,
            },
            session_index::{SessionNameSnapshot, SessionSourceStatus},
        },
        domain::{AgentRole, MetadataQualityStatus, Patch, ProjectKind},
    };

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-state-index")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "usagi-state-index-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&directory).unwrap();
            let path = directory.join("state_5.sqlite");
            Self { directory, path }
        }

        fn connection(&self) -> Connection {
            Connection::open(&self.path).unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn empty_sessions() -> SessionNameSnapshot {
        SessionNameSnapshot {
            names: Default::default(),
            facts: Vec::new(),
            diagnostics: Vec::new(),
            status: SessionSourceStatus::Complete,
        }
    }

    fn existing(thread_id: &str) -> ExistingThread {
        ExistingThread {
            thread_id: thread_id.to_owned(),
            parent_thread_id: None,
            root_session_id: None,
            agent_role: AgentRole::Unknown,
            title: None,
            project_name: None,
            project_path: None,
            project_kind: ProjectKind::Unknown,
            metadata_model: None,
            created_at_ms: None,
            updated_at_ms: None,
            archived: false,
            current_rollout_path: None,
            metadata_quality_status: MetadataQualityStatus::Partial,
        }
    }

    #[test]
    fn complete_schema_reads_only_allowlisted_thread_and_edge_facts() {
        let database = TempDb::new();
        let connection = database.connection();
        let rollout_path = fixture_path("sessions/rollout-child.jsonl");
        let cwd_path = fixture_path("work/./project");
        let expected_rollout_path = paths::normalize_source_path(Path::new(&rollout_path))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let expected_cwd_path =
            paths::normalize_source_path(Path::new(&fixture_path("work/project")))
                .unwrap()
                .to_string_lossy()
                .into_owned();
        connection
            .execute_batch(&format!(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY, rollout_path TEXT, created_at INTEGER,
                    created_at_ms INTEGER, updated_at INTEGER, updated_at_ms INTEGER,
                    archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT,
                    agent_role TEXT, first_user_message TEXT, preview TEXT,
                    sandbox_policy TEXT, approval_mode TEXT
                 );
                 CREATE TABLE thread_spawn_edges (
                    parent_thread_id TEXT, child_thread_id TEXT, status TEXT,
                    observed_at INTEGER, observed_at_ms INTEGER
                 );
                 INSERT INTO threads VALUES (
                    'child', '{rollout_path}', 1, 2000, 3, 4000,
                    1, '{cwd_path}', 'Title', 'Name', 'model-a', 'subagent',
                    'SECRET_BODY', 'SECRET_PREVIEW', 'danger', 'never'
                 );
                 INSERT INTO thread_spawn_edges VALUES ('parent', 'child', 'ready', 5, 6000);"
            ))
            .unwrap();
        drop(connection);

        let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
        assert!(snapshot.is_available());
        let fact = snapshot.thread("child").unwrap();
        assert_eq!(
            fact.rollout_path.as_deref(),
            Some(expected_rollout_path.as_str())
        );
        assert_eq!(fact.created_at_ms, Some(2000));
        assert_eq!(fact.updated_at_ms, Some(4000));
        assert_eq!(fact.archived, Some(true));
        assert_eq!(fact.cwd.as_deref(), Some(expected_cwd_path.as_str()));
        assert_eq!(fact.title.as_deref(), Some("Title"));
        assert_eq!(fact.name.as_deref(), Some("Name"));
        assert_eq!(fact.metadata_model.as_deref(), Some("model-a"));
        assert_eq!(fact.agent_role_hint.as_deref(), Some("subagent"));
        assert_eq!(fact.agent_path, None);
        assert_eq!(snapshot.spawn_edges.len(), 1);
        assert_eq!(snapshot.spawn_edges[0].parent_thread_id, "parent");
        assert_eq!(snapshot.spawn_edges[0].observed_at_ms, Some(6000));
        assert!(!format!("{snapshot:?}").contains("SECRET"));
    }

    #[test]
    fn reads_optional_agent_path_only_when_schema_exposes_it() {
        let database = TempDb::new();
        let connection = database.connection();
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    agent_path TEXT,
                    unallowlisted_secret TEXT
                 );
                 INSERT INTO threads VALUES ('child', '  /root/group/./task  ', 'do-not-read');",
            )
            .unwrap();
        drop(connection);

        let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
        let fact = snapshot.thread("child").unwrap();
        assert_eq!(fact.agent_path.as_deref(), Some("/root/group/task"));
        assert!(!format!("{snapshot:?}").contains("do-not-read"));
    }

    #[test]
    fn id_only_and_missing_optional_columns_degrade_without_guessing() {
        let database = TempDb::new();
        let connection = database.connection();
        connection
            .execute_batch("CREATE TABLE threads (id TEXT); INSERT INTO threads VALUES ('only');")
            .unwrap();
        drop(connection);

        let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
        let fact = snapshot.thread("only").unwrap();
        assert!(snapshot.is_available());
        assert_eq!(snapshot.spawn_edges_status, StateSourceStatus::Unavailable);
        assert_eq!(
            fact,
            &StateThreadFact {
                thread_id: "only".to_owned(),
                rollout_path: None,
                created_at_ms: None,
                updated_at_ms: None,
                archived: None,
                title: None,
                name: None,
                cwd: None,
                metadata_model: None,
                agent_role_hint: None,
                agent_path: None,
            }
        );
    }

    #[test]
    fn missing_threads_table_or_id_marks_the_source_unavailable() {
        for schema in [
            "CREATE TABLE other (id TEXT);",
            "CREATE TABLE threads (title TEXT);",
        ] {
            let database = TempDb::new();
            let connection = database.connection();
            connection.execute_batch(schema).unwrap();
            drop(connection);

            let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
            assert_eq!(snapshot.status, StateSourceStatus::Unavailable);
            assert!(snapshot.threads.is_empty());
            assert!(snapshot.diagnostics.iter().any(|diagnostic| {
                matches!(
                    diagnostic.code.as_str(),
                    "missing_required_table" | "missing_required_column"
                )
            }));
        }
    }

    #[test]
    fn absent_spawn_table_allows_rollout_parent_but_never_infers_main() {
        let database = TempDb::new();
        let connection = database.connection();
        connection
            .execute_batch(
                "CREATE TABLE threads (id TEXT); INSERT INTO threads VALUES ('parent');
                 INSERT INTO threads VALUES ('child'); INSERT INTO threads VALUES ('orphan');",
            )
            .unwrap();
        drop(connection);
        let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
        assert_eq!(snapshot.spawn_edges_status, StateSourceStatus::Unavailable);

        let rollout = RolloutThreadFact {
            source_file_id: 1,
            owning_thread_id: "child".to_owned(),
            cwd: None,
            created_at_ms: None,
            latest_context_model: None,
            latest_context_turn_id: None,
            latest_context_at_ms: None,
            latest_context_record_offset: None,
            parent_thread_id_hint: Some(Candidate {
                value: "parent".to_owned(),
                provenance: ParentHintProvenance::SubagentSource,
                record_offset: 1,
            }),
            agent_role_hint: Some(Candidate {
                value: "subagent".to_owned(),
                provenance: AgentRoleProvenance::SubagentSource,
                record_offset: 1,
            }),
            agent_path: None,
            ownership_boundary: OwnershipBoundary {
                replay_start_offset: None,
                owning_records_start_offset: Some(0),
                confidence: OwnershipConfidence::Confirmed,
            },
            has_conflict: false,
            relationship_conflict: false,
        };
        let result = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: snapshot,
            session_name_snapshot: empty_sessions(),
            global_state_snapshot: crate::codex::GlobalStateSnapshot::unavailable(
                crate::codex::GlobalStateStatus::NotPresent,
                Vec::new(),
            ),
            rollout_facts: vec![rollout],
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("parent"), existing("child"), existing("orphan")],
            resolved_at_ms: 10,
        });
        let child = result
            .patches
            .iter()
            .find(|patch| patch.thread_id == "child")
            .unwrap();
        assert_eq!(child.parent_thread_id, Patch::Set("parent".to_owned()));
        assert_eq!(child.agent_role, Patch::Set(AgentRole::Subagent));
        assert!(result.patches.iter().all(|patch| {
            patch.thread_id != "orphan" || patch.agent_role != Patch::Set(AgentRole::Main)
        }));

        let unavailable = ThreadMetadataResolver::resolve(ResolutionInput {
            state_snapshot: StateSnapshot::unavailable(Vec::new()),
            session_name_snapshot: empty_sessions(),
            global_state_snapshot: crate::codex::GlobalStateSnapshot::unavailable(
                crate::codex::GlobalStateStatus::NotPresent,
                Vec::new(),
            ),
            rollout_facts: Vec::new(),
            source_file_observations: Vec::new(),
            existing_threads: vec![existing("unavailable")],
            resolved_at_ms: 11,
        });
        assert!(unavailable.patches.iter().all(|patch| {
            patch.agent_role != Patch::Set(AgentRole::Main)
                && !matches!(patch.root_session_id, Patch::Set(_))
        }));
    }

    #[test]
    fn seconds_and_milliseconds_are_normalized_with_milliseconds_preferred() {
        let database = TempDb::new();
        let connection = database.connection();
        connection
            .execute_batch(
                "CREATE TABLE threads (
                    id TEXT, created_at INTEGER, created_at_ms INTEGER,
                    updated_at INTEGER, updated_at_ms INTEGER
                 );
                 INSERT INTO threads VALUES ('milliseconds', 1, 2345, 3, 4567);
                 INSERT INTO threads VALUES ('seconds', 7, NULL, 8, NULL);
                 INSERT INTO threads VALUES ('fallback', 9, -1, 10, -1);",
            )
            .unwrap();
        drop(connection);

        let snapshot = StateIndexReader::read_snapshot(&database.path).unwrap();
        assert_eq!(
            snapshot.thread("milliseconds").unwrap().created_at_ms,
            Some(2345)
        );
        assert_eq!(
            snapshot.thread("milliseconds").unwrap().updated_at_ms,
            Some(4567)
        );
        assert_eq!(
            snapshot.thread("seconds").unwrap().created_at_ms,
            Some(7000)
        );
        assert_eq!(
            snapshot.thread("seconds").unwrap().updated_at_ms,
            Some(8000)
        );
        assert_eq!(
            snapshot.thread("fallback").unwrap().created_at_ms,
            Some(9000)
        );
        assert_eq!(
            snapshot.thread("fallback").unwrap().updated_at_ms,
            Some(10_000)
        );
    }

    #[test]
    fn query_columns_are_allowlisted_and_adapter_connection_is_read_only() {
        let available = THREAD_ALLOWLIST
            .iter()
            .copied()
            .chain([
                "first_user_message",
                "preview",
                "sandbox_policy",
                "approval_mode",
            ])
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let selected = selected_columns(&available, THREAD_ALLOWLIST);
        assert_eq!(selected, THREAD_ALLOWLIST);
        for forbidden in [
            "first_user_message",
            "preview",
            "sandbox_policy",
            "approval_mode",
        ] {
            assert!(!selected.contains(&forbidden));
        }

        let database = TempDb::new();
        let connection = database.connection();
        connection
            .execute("CREATE TABLE threads (id TEXT)", [])
            .unwrap();
        drop(connection);
        let read_only = open_read_only_connection(&database.path, DEFAULT_BUSY_TIMEOUT).unwrap();
        assert!(
            read_only
                .execute("INSERT INTO threads VALUES ('write')", [])
                .is_err()
        );
        let query_only: i64 = read_only
            .pragma_query_value(None, "query_only", |row| row.get(0))
            .unwrap();
        assert_eq!(query_only, 1);
    }
}
