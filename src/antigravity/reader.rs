//! Low-level SQLite reader for external Antigravity databases.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use chrono::DateTime;
use rusqlite::{Connection, DatabaseName, OpenFlags};
use uuid::Uuid;

use crate::antigravity::normalization::{
    AntigravityQuarantineRecord, DiscoveredSummaryRow, GEN_METADATA_MALFORMED, StepIndexEntry,
    USAGE_RESPONSE_ID_INVALID,
};
use crate::antigravity::protobuf::{
    MAX_PROTO_MESSAGE_BYTES, ParseGenMetadataResult, RawAntigravityUsageCandidate,
    SQLITE_BLOB_DIGEST_CHUNK_BYTES, parse_gen_metadata_data, parse_step_payload_response_id,
    parse_trajectory_metadata_blob_workspace,
};

pub const ANTIGRAVITY_DATABASE_BUSY: &str = "ANTIGRAVITY_DATABASE_BUSY";
pub const ANTIGRAVITY_DATABASE_INVALID: &str = "ANTIGRAVITY_DATABASE_INVALID";
pub const ANTIGRAVITY_SCHEMA_UNSUPPORTED: &str = "ANTIGRAVITY_SCHEMA_UNSUPPORTED";
pub const ANTIGRAVITY_SUMMARY_ID_CONFLICT: &str = "ANTIGRAVITY_SUMMARY_ID_CONFLICT";
pub const ANTIGRAVITY_CONVERSATION_ID_MISSING: &str = "ANTIGRAVITY_CONVERSATION_ID_MISSING";
pub const ANTIGRAVITY_CONVERSATION_ID_AMBIGUOUS: &str = "ANTIGRAVITY_CONVERSATION_ID_AMBIGUOUS";
pub const ANTIGRAVITY_CONVERSATION_ID_INVALID: &str = "ANTIGRAVITY_CONVERSATION_ID_INVALID";
pub const ANTIGRAVITY_CONVERSATION_ID_CONFLICT: &str = "ANTIGRAVITY_CONVERSATION_ID_CONFLICT";
pub const ANTIGRAVITY_EXTERNAL_INDEX_INVALID: &str = "ANTIGRAVITY_EXTERNAL_INDEX_INVALID";
pub const ANTIGRAVITY_GEN_METADATA_DATA_NULL: &str = "ANTIGRAVITY_GEN_METADATA_DATA_NULL";
pub const ANTIGRAVITY_STEP_INDEX_INVALID: &str = "ANTIGRAVITY_STEP_INDEX_INVALID";
pub const OPERATION_CANCELLED: &str = "OPERATION_CANCELLED";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReaderError {
    Busy(String),
    Invalid(String),
    SchemaUnsupported(String),
    SummaryIdConflict(String),
    ConversationIdMissing(String),
    ConversationIdAmbiguous(String),
    ConversationIdInvalid(String),
    ConversationIdConflict(String),
    ExternalIndexInvalid(String),
    GenMetadataDataNull(String),
    StepIndexInvalid(String),
    Cancelled,
}

impl ReaderError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Busy(_) => ANTIGRAVITY_DATABASE_BUSY,
            Self::Invalid(_) => ANTIGRAVITY_DATABASE_INVALID,
            Self::SchemaUnsupported(_) => ANTIGRAVITY_SCHEMA_UNSUPPORTED,
            Self::SummaryIdConflict(_) => ANTIGRAVITY_SUMMARY_ID_CONFLICT,
            Self::ConversationIdMissing(_) => ANTIGRAVITY_CONVERSATION_ID_MISSING,
            Self::ConversationIdAmbiguous(_) => ANTIGRAVITY_CONVERSATION_ID_AMBIGUOUS,
            Self::ConversationIdInvalid(_) => ANTIGRAVITY_CONVERSATION_ID_INVALID,
            Self::ConversationIdConflict(_) => ANTIGRAVITY_CONVERSATION_ID_CONFLICT,
            Self::ExternalIndexInvalid(_) => ANTIGRAVITY_EXTERNAL_INDEX_INVALID,
            Self::GenMetadataDataNull(_) => ANTIGRAVITY_GEN_METADATA_DATA_NULL,
            Self::StepIndexInvalid(_) => ANTIGRAVITY_STEP_INDEX_INVALID,
            Self::Cancelled => OPERATION_CANCELLED,
        }
    }
}

impl fmt::Display for ReaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(msg) => write!(formatter, "{ANTIGRAVITY_DATABASE_BUSY}: {msg}"),
            Self::Invalid(msg) => write!(formatter, "{ANTIGRAVITY_DATABASE_INVALID}: {msg}"),
            Self::SchemaUnsupported(msg) => {
                write!(formatter, "{ANTIGRAVITY_SCHEMA_UNSUPPORTED}: {msg}")
            }
            Self::SummaryIdConflict(msg) => {
                write!(formatter, "{ANTIGRAVITY_SUMMARY_ID_CONFLICT}: {msg}")
            }
            Self::ConversationIdMissing(msg) => {
                write!(formatter, "{ANTIGRAVITY_CONVERSATION_ID_MISSING}: {msg}")
            }
            Self::ConversationIdAmbiguous(msg) => {
                write!(formatter, "{ANTIGRAVITY_CONVERSATION_ID_AMBIGUOUS}: {msg}")
            }
            Self::ConversationIdInvalid(msg) => {
                write!(formatter, "{ANTIGRAVITY_CONVERSATION_ID_INVALID}: {msg}")
            }
            Self::ConversationIdConflict(msg) => {
                write!(formatter, "{ANTIGRAVITY_CONVERSATION_ID_CONFLICT}: {msg}")
            }
            Self::ExternalIndexInvalid(msg) => {
                write!(formatter, "{ANTIGRAVITY_EXTERNAL_INDEX_INVALID}: {msg}")
            }
            Self::GenMetadataDataNull(msg) => {
                write!(formatter, "{ANTIGRAVITY_GEN_METADATA_DATA_NULL}: {msg}")
            }
            Self::StepIndexInvalid(msg) => {
                write!(formatter, "{ANTIGRAVITY_STEP_INDEX_INVALID}: {msg}")
            }
            Self::Cancelled => formatter.write_str("operation cancelled"),
        }
    }
}

impl std::error::Error for ReaderError {}

pub fn map_rusqlite_error(err: rusqlite::Error) -> ReaderError {
    match &err {
        rusqlite::Error::SqliteFailure(e, _) => {
            if e.extended_code == rusqlite::ffi::SQLITE_BUSY
                || e.extended_code == rusqlite::ffi::SQLITE_LOCKED
                || e.code == rusqlite::ErrorCode::DatabaseBusy
                || e.code == rusqlite::ErrorCode::DatabaseLocked
            {
                ReaderError::Busy(e.to_string())
            } else if e.code == rusqlite::ErrorCode::DatabaseCorrupt
                || e.code == rusqlite::ErrorCode::NotADatabase
                || e.code == rusqlite::ErrorCode::CannotOpen
            {
                ReaderError::Invalid(e.to_string())
            } else {
                ReaderError::Invalid(err.to_string())
            }
        }
        _ => ReaderError::Invalid(err.to_string()),
    }
}

/// Open an external SQLite database read-only with query_only=ON [INV-WAL-01].
pub fn open_external_db(path: impl AsRef<Path>) -> Result<Connection, ReaderError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(map_rusqlite_error)?;

    conn.pragma_update(None, "query_only", "ON")
        .map_err(map_rusqlite_error)?;

    Ok(conn)
}

/// Discovered summary lookup table.
#[derive(Clone, Debug, Default)]
pub struct DiscoveredSummaryMap {
    pub rows: HashMap<String, DiscoveredSummaryRow>,
}

/// Read summary snapshot from `conversation_summaries.db`.
pub fn read_summary_snapshot(
    conn: &Connection,
    cancellation: &AtomicBool,
) -> Result<DiscoveredSummaryMap, ReaderError> {
    if cancellation.load(Ordering::Relaxed) {
        return Err(ReaderError::Cancelled);
    }

    // Probe summary schema [INV-EXT-01]
    probe_summary_schema(conn)?;

    let tx = conn.unchecked_transaction().map_err(map_rusqlite_error)?;

    let mut stmt = tx
        .prepare(
            "SELECT typeof(conversation_id), conversation_id, typeof(title), title,
                    typeof(workspace_uris), workspace_uris,
                    typeof(last_modified_time), last_modified_time
             FROM conversation_summaries",
        )
        .map_err(map_rusqlite_error)?;

    let mut rows_map = HashMap::new();

    let mut rows = stmt.query([]).map_err(map_rusqlite_error)?;
    while let Some(row) = rows.next().map_err(map_rusqlite_error)? {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }

        let id_type: String = row.get(0).map_err(map_rusqlite_error)?;
        if id_type != "text" {
            // Ignore non-text summary ID
            continue;
        }

        let raw_id: String = row.get(1).map_err(map_rusqlite_error)?;
        let trimmed_id = raw_id.trim();
        if trimmed_id.is_empty() {
            continue;
        }

        let parsed_uuid = match Uuid::parse_str(trimmed_id) {
            Ok(u) => u.hyphenated().to_string(),
            Err(_) => continue, // Ignore invalid UUID summary row per [INV-ID-06]
        };

        // Title
        let title_type: String = row.get(2).map_err(map_rusqlite_error)?;
        let title = if title_type == "text" {
            Some(row.get::<_, String>(3).map_err(map_rusqlite_error)?)
        } else {
            None
        };

        // Workspace URIs
        let ws_type: String = row.get(4).map_err(map_rusqlite_error)?;
        let workspace_uris = if ws_type == "text" {
            Some(row.get::<_, String>(5).map_err(map_rusqlite_error)?)
        } else {
            None
        };

        // Last modified time
        let time_type: String = row.get(6).map_err(map_rusqlite_error)?;
        let last_modified_time_ms = if time_type == "text" {
            let time_str: String = row.get(7).map_err(map_rusqlite_error)?;
            parse_datetime_to_ms(&time_str)
        } else if time_type == "integer" {
            let time_int: i64 = row.get(7).map_err(map_rusqlite_error)?;
            Some(time_int)
        } else {
            None
        };

        // Check summary uniqueness [INV-EXT-03]
        if rows_map.contains_key(&parsed_uuid) {
            return Err(ReaderError::SummaryIdConflict(format!(
                "duplicate valid summary ID: {parsed_uuid}"
            )));
        }

        rows_map.insert(
            parsed_uuid,
            DiscoveredSummaryRow {
                title,
                workspace_uris,
                last_modified_time_ms,
            },
        );
    }

    if cancellation.load(Ordering::Relaxed) {
        return Err(ReaderError::Cancelled);
    }

    Ok(DiscoveredSummaryMap { rows: rows_map })
}

fn probe_summary_schema(conn: &Connection) -> Result<(), ReaderError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(conversation_summaries)")
        .map_err(map_rusqlite_error)?;

    let mut col_names = Vec::new();
    let mut rows = stmt.query([]).map_err(map_rusqlite_error)?;
    while let Some(row) = rows.next().map_err(map_rusqlite_error)? {
        let name: String = row.get(1).map_err(map_rusqlite_error)?;
        col_names.push(name);
    }

    if col_names.is_empty() {
        return Err(ReaderError::SchemaUnsupported(
            "missing required table: conversation_summaries".into(),
        ));
    }

    for req in [
        "conversation_id",
        "title",
        "workspace_uris",
        "last_modified_time",
    ] {
        if !col_names.iter().any(|c| c == req) {
            return Err(ReaderError::SchemaUnsupported(format!(
                "conversation_summaries missing required column: {req}"
            )));
        }
    }

    Ok(())
}

fn parse_datetime_to_ms(s: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    // Try formats with space instead of T, e.g. "2026-09-15 03:46:33.54883+00:00"
    let with_t = s.replace(' ', "T");
    if let Ok(dt) = DateTime::parse_from_rfc3339(&with_t) {
        return Some(dt.timestamp_millis());
    }
    None
}

/// Read the intrinsic conversation ID from `trajectory_meta`.
pub fn read_intrinsic_id(conn: &Connection) -> Result<String, ReaderError> {
    probe_conversation_schema(conn)?;

    let mut stmt = conn
        .prepare("SELECT typeof(cascade_id), cascade_id FROM trajectory_meta")
        .map_err(map_rusqlite_error)?;

    let mut rows = stmt.query([]).map_err(map_rusqlite_error)?;

    let first = rows.next().map_err(map_rusqlite_error)?;
    let row = match first {
        None => {
            return Err(ReaderError::ConversationIdMissing(
                "trajectory_meta table has 0 rows".into(),
            ));
        }
        Some(r) => r,
    };

    let col_type: String = row.get(0).map_err(map_rusqlite_error)?;
    if col_type != "text" {
        return Err(ReaderError::ConversationIdInvalid(format!(
            "cascade_id has non-text SQLite type: {col_type}"
        )));
    }

    let raw_id: String = row.get(1).map_err(map_rusqlite_error)?;
    let trimmed = raw_id.trim();
    if trimmed.is_empty() {
        return Err(ReaderError::ConversationIdInvalid(
            "cascade_id is empty".into(),
        ));
    }

    let parsed_uuid = Uuid::parse_str(trimmed).map_err(|err| {
        ReaderError::ConversationIdInvalid(format!("cascade_id is not a valid UUID: {err}"))
    })?;

    // Check if more than 1 row exists
    if rows.next().map_err(map_rusqlite_error)?.is_some() {
        return Err(ReaderError::ConversationIdAmbiguous(
            "trajectory_meta table has multiple rows".into(),
        ));
    }

    Ok(parsed_uuid.hyphenated().to_string())
}

/// Probe conversation DB schema for required tables and columns [INV-EXT-02].
pub fn probe_conversation_schema(conn: &Connection) -> Result<(), ReaderError> {
    for (table, cols) in [
        ("trajectory_meta", vec!["cascade_id"]),
        (
            "steps",
            vec!["idx", "step_type", "metadata", "step_payload"],
        ),
        ("gen_metadata", vec!["idx", "data"]),
        ("trajectory_metadata_blob", vec!["id", "data"]),
    ] {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(map_rusqlite_error)?;
        let mut col_names = Vec::new();
        let mut rows = stmt.query([]).map_err(map_rusqlite_error)?;
        while let Some(row) = rows.next().map_err(map_rusqlite_error)? {
            let name: String = row.get(1).map_err(map_rusqlite_error)?;
            col_names.push(name);
        }

        if col_names.is_empty() {
            return Err(ReaderError::SchemaUnsupported(format!(
                "missing required table: {table}"
            )));
        }

        for req in cols {
            if !col_names.iter().any(|c| c == req) {
                return Err(ReaderError::SchemaUnsupported(format!(
                    "table {table} missing required column: {req}"
                )));
            }
        }
    }

    // Verify gen_metadata is a rowid table
    let is_without_rowid: bool = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_list('gen_metadata') WHERE without_rowid = 1",
            [],
            |r| r.get::<_, i64>(0).map(|c| c > 0),
        )
        .unwrap_or(false);

    if is_without_rowid {
        return Err(ReaderError::SchemaUnsupported(
            "gen_metadata must be a rowid table for incremental blob reading".into(),
        ));
    }

    Ok(())
}

/// Snapshot data extracted from a single conversation DB before transaction.
#[derive(Clone, Debug)]
pub struct RawConversationSnapshotData {
    pub candidates: Vec<RawAntigravityUsageCandidate>,
    pub initial_quarantines: Vec<AntigravityQuarantineRecord>,
    pub step_index: BTreeMap<String, Vec<StepIndexEntry>>,
    pub trajectory_workspace: Option<String>,
    pub observed_gen_max_idx: i64,
    pub observed_step_max_idx: i64,
}

/// Read conversation data snapshot in a Deferred read transaction.
pub fn read_conversation_snapshot(
    conn: &Connection,
    conversation_id: &str,
    cancellation: &AtomicBool,
) -> Result<RawConversationSnapshotData, ReaderError> {
    if cancellation.load(Ordering::Relaxed) {
        return Err(ReaderError::Cancelled);
    }

    let tx = conn.unchecked_transaction().map_err(map_rusqlite_error)?;

    // 1. Preflight steps table scalars in strict order per [INV-EXT-08]
    // First preflight: idx must be integer and >= 0
    let bad_idx: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM steps WHERE typeof(idx) <> 'integer' OR idx < 0 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if bad_idx.is_some() {
        return Err(ReaderError::ExternalIndexInvalid(
            "steps table contains non-integer or negative idx".into(),
        ));
    }

    // Second preflight: step_type must be integer
    let bad_type: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM steps WHERE typeof(step_type) <> 'integer' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if bad_type.is_some() {
        return Err(ReaderError::StepIndexInvalid(
            "steps table contains non-integer step_type".into(),
        ));
    }

    // Preflight gen_metadata idx
    let bad_gen_idx: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM gen_metadata WHERE typeof(idx) <> 'integer' OR idx < 0 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if bad_gen_idx.is_some() {
        return Err(ReaderError::ExternalIndexInvalid(
            "gen_metadata table contains non-integer or negative idx".into(),
        ));
    }

    // 2. Build step index from step_type = 15 rows
    let mut step_stmt = tx
        .prepare(
            "SELECT idx, typeof(step_payload), length(step_payload), step_payload,
                    typeof(metadata), length(metadata), metadata
             FROM steps WHERE step_type = 15 ORDER BY idx ASC",
        )
        .map_err(map_rusqlite_error)?;

    let mut step_index: BTreeMap<String, Vec<StepIndexEntry>> = BTreeMap::new();
    let mut observed_step_max_idx: i64 = -1;

    let mut step_rows = step_stmt.query([]).map_err(map_rusqlite_error)?;
    while let Some(row) = step_rows.next().map_err(map_rusqlite_error)? {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }

        let idx: i64 = row.get(0).map_err(map_rusqlite_error)?;
        if idx > observed_step_max_idx {
            observed_step_max_idx = idx;
        }

        // Check step_payload blob preflight [INV-EXT-07]
        let sp_type: String = row.get(1).map_err(map_rusqlite_error)?;
        if sp_type == "null" || sp_type != "blob" {
            return Err(ReaderError::StepIndexInvalid(format!(
                "step_type 15 row idx {idx} step_payload is {sp_type}, expected blob"
            )));
        }

        let sp_len: usize = row.get(2).map_err(map_rusqlite_error)?;
        if sp_len > MAX_PROTO_MESSAGE_BYTES {
            return Err(ReaderError::StepIndexInvalid(format!(
                "step_type 15 row idx {idx} step_payload exceeds limit: {sp_len}"
            )));
        }

        let sp_bytes: Vec<u8> = row.get(3).map_err(map_rusqlite_error)?;
        let resp_id = match parse_step_payload_response_id(&sp_bytes) {
            Ok(Some(id)) => id,
            Ok(None) => continue, // empty/trim empty does not enter index
            Err(err) => {
                return Err(ReaderError::StepIndexInvalid(format!(
                    "failed to parse responseId from step_payload at idx {idx}: {err}"
                )));
            }
        };

        // Check metadata blob preflight [INV-EXT-07]
        let md_type: String = row.get(4).map_err(map_rusqlite_error)?;
        let md_bytes = if md_type == "blob" {
            let md_len: usize = row.get(5).map_err(map_rusqlite_error)?;
            if md_len <= MAX_PROTO_MESSAGE_BYTES {
                Some(row.get::<_, Vec<u8>>(6).map_err(map_rusqlite_error)?)
            } else {
                None // exceeds limit -> will be handled as invalid
            }
        } else {
            None // NULL or non-blob -> will be handled as invalid
        };

        step_index.entry(resp_id).or_default().push(StepIndexEntry {
            step_idx: idx,
            metadata_bytes: md_bytes,
        });
    }

    // 3. Read gen_metadata rows
    let mut gen_stmt = tx
        .prepare(
            "SELECT rowid, idx, typeof(data), length(data)
             FROM gen_metadata ORDER BY idx ASC",
        )
        .map_err(map_rusqlite_error)?;

    let mut candidates = Vec::new();
    let mut initial_quarantines = Vec::new();
    let mut observed_gen_max_idx: i64 = -1;

    let mut gen_rows = gen_stmt.query([]).map_err(map_rusqlite_error)?;
    while let Some(row) = gen_rows.next().map_err(map_rusqlite_error)? {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }

        let rowid: i64 = row.get(0).map_err(map_rusqlite_error)?;
        let idx: i64 = row.get(1).map_err(map_rusqlite_error)?;
        if idx > observed_gen_max_idx {
            observed_gen_max_idx = idx;
        }

        let data_type: String = row.get(2).map_err(map_rusqlite_error)?;
        if data_type == "null" {
            return Err(ReaderError::GenMetadataDataNull(format!(
                "gen_metadata rowid {rowid} idx {idx} data is NULL"
            )));
        }
        if data_type != "blob" {
            return Err(ReaderError::Invalid(format!(
                "gen_metadata rowid {rowid} idx {idx} data is {data_type}, expected blob"
            )));
        }

        let data_len: usize = row.get(3).map_err(map_rusqlite_error)?;

        if data_len > MAX_PROTO_MESSAGE_BYTES {
            // Incremental BLOB read in 64 KiB chunks [INV-PB-03]
            let digest = stream_blob_digest(conn, rowid, data_len, cancellation)?;
            initial_quarantines.push(AntigravityQuarantineRecord {
                conversation_id: conversation_id.to_string(),
                payload_digest: digest,
                gen_idx: Some(idx),
                response_id: None,
                reason_code: GEN_METADATA_MALFORMED,
            });
            continue;
        }

        // Single row materialize -> digest -> parse -> release [INV-PB-04]
        let blob_bytes: Vec<u8> = tx
            .query_row(
                "SELECT data FROM gen_metadata WHERE rowid = ?",
                [rowid],
                |r| r.get(0),
            )
            .map_err(map_rusqlite_error)?;

        let digest = blake3::hash(&blob_bytes).to_hex().to_string();

        match parse_gen_metadata_data(&blob_bytes, idx, digest.clone()) {
            ParseGenMetadataResult::Candidate(c) => {
                candidates.push(c);
            }
            ParseGenMetadataResult::ResponseIdInvalid {
                gen_idx,
                payload_digest,
            } => {
                initial_quarantines.push(AntigravityQuarantineRecord {
                    conversation_id: conversation_id.to_string(),
                    payload_digest,
                    gen_idx: Some(gen_idx),
                    response_id: None,
                    reason_code: USAGE_RESPONSE_ID_INVALID,
                });
            }
            ParseGenMetadataResult::Placeholder => {
                // Ignore placeholder per [INV-USAGE-01]
            }
            ParseGenMetadataResult::Malformed(_) => {
                initial_quarantines.push(AntigravityQuarantineRecord {
                    conversation_id: conversation_id.to_string(),
                    payload_digest: digest,
                    gen_idx: Some(idx),
                    response_id: None,
                    reason_code: GEN_METADATA_MALFORMED,
                });
            }
        }
        // blob_bytes dropped here!
    }

    // 4. Read trajectory_metadata_blob
    let mut trajectory_workspace: Option<String> = None;
    let blob_row: Result<(String, usize, Vec<u8>), _> = tx.query_row(
        "SELECT typeof(data), length(data), data FROM trajectory_metadata_blob WHERE id = 'main'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    );

    if let Ok((b_type, b_len, b_data)) = blob_row {
        if b_type == "blob" && b_len <= MAX_PROTO_MESSAGE_BYTES {
            if let Ok(ws) = parse_trajectory_metadata_blob_workspace(&b_data) {
                trajectory_workspace = ws;
            }
        }
    }

    if cancellation.load(Ordering::Relaxed) {
        return Err(ReaderError::Cancelled);
    }

    Ok(RawConversationSnapshotData {
        candidates,
        initial_quarantines,
        step_index,
        trajectory_workspace,
        observed_gen_max_idx,
        observed_step_max_idx,
    })
}

fn stream_blob_digest(
    conn: &Connection,
    rowid: i64,
    total_len: usize,
    cancellation: &AtomicBool,
) -> Result<String, ReaderError> {
    let mut blob = conn
        .blob_open(DatabaseName::Main, "gen_metadata", "data", rowid, true)
        .map_err(map_rusqlite_error)?;

    let mut hasher = blake3::Hasher::new();
    let mut chunk = vec![0u8; SQLITE_BLOB_DIGEST_CHUNK_BYTES];
    let mut offset = 0;

    while offset < total_len {
        if cancellation.load(Ordering::Relaxed) {
            return Err(ReaderError::Cancelled);
        }
        let to_read = (total_len - offset).min(chunk.len());
        blob.read_exact(&mut chunk[..to_read])
            .map_err(|err| ReaderError::Invalid(err.to_string()))?;
        hasher.update(&chunk[..to_read]);
        offset += to_read;
    }

    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_test_db(name: &str) -> (rusqlite::Connection, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let c = COUNTER.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ag-reader-{name}-{}-{}.db", t, c));
        let conn = rusqlite::Connection::open(&path).unwrap();
        (conn, path)
    }

    #[test]
    fn test_td_p1_idx_01_negative_indices_rejected() {
        let (conn, path) = temp_test_db("neg-idx");
        conn.execute(
            "CREATE TABLE steps (idx INT, step_type INT, metadata BLOB, step_payload BLOB)",
            [],
        )
        .unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INT, data BLOB)", [])
            .unwrap();
        conn.execute("CREATE TABLE trajectory_meta (cascade_id TEXT)", [])
            .unwrap();
        conn.execute(
            "CREATE TABLE trajectory_metadata_blob (id TEXT, data BLOB)",
            [],
        )
        .unwrap();

        // 1. steps.idx = -1
        conn.execute("INSERT INTO steps VALUES (-1, 15, x'', x'')", [])
            .unwrap();
        let ext_conn = open_external_db(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let res = read_conversation_snapshot(&ext_conn, "test-conv", &cancel);
        assert_eq!(res.unwrap_err().code(), ANTIGRAVITY_EXTERNAL_INDEX_INVALID);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_td_p1_blob_null_01_gen_metadata_null() {
        let (conn, path) = temp_test_db("null-blob");
        conn.execute(
            "CREATE TABLE steps (idx INT, step_type INT, metadata BLOB, step_payload BLOB)",
            [],
        )
        .unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INT, data BLOB)", [])
            .unwrap();
        conn.execute("CREATE TABLE trajectory_meta (cascade_id TEXT)", [])
            .unwrap();
        conn.execute(
            "CREATE TABLE trajectory_metadata_blob (id TEXT, data BLOB)",
            [],
        )
        .unwrap();

        conn.execute("INSERT INTO gen_metadata VALUES (0, NULL)", [])
            .unwrap();
        let ext_conn = open_external_db(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let res = read_conversation_snapshot(&ext_conn, "test-conv", &cancel);
        assert_eq!(res.unwrap_err().code(), ANTIGRAVITY_GEN_METADATA_DATA_NULL);
        let _ = fs::remove_file(&path);
    }
}
