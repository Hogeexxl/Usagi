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
#[path = "state_index/tests.rs"]
mod tests;
