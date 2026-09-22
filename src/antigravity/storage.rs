//! Storage helpers and epoch validation for Antigravity source-private state.

use std::{collections::HashMap, fmt};

use rusqlite::{Connection, OptionalExtension};

use crate::antigravity::normalization::AntigravityQuarantineRecord;

pub const ANTIGRAVITY_USAGE_EPOCH_INVALID: &str = "ANTIGRAVITY_USAGE_EPOCH_INVALID";

const VALID_REASON_CODES: &[&str] = &[
    "GEN_METADATA_MALFORMED",
    "USAGE_RESPONSE_ID_MISSING",
    "USAGE_RESPONSE_ID_INVALID",
    "USAGE_RESPONSE_ID_CONFLICT",
    "USAGE_MODEL_MISSING",
    "USAGE_MODEL_INVALID",
    "USAGE_STEP_NOT_FOUND",
    "USAGE_STEP_NOT_UNIQUE",
    "USAGE_STEP_KIND_MISMATCH",
    "USAGE_STEP_METADATA_INVALID",
    "USAGE_TIMESTAMP_INVALID",
    "USAGE_TOKEN_INVALID",
    "USAGE_EVENT_MUTATION_CONFLICT",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AntigravityStorageError {
    EpochInvalid(String),
    DigestInvalid(String),
    ReasonInvalid(String),
    Sqlite(String),
}

impl AntigravityStorageError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EpochInvalid(_) => ANTIGRAVITY_USAGE_EPOCH_INVALID,
            Self::DigestInvalid(_) => "ANTIGRAVITY_DIGEST_INVALID",
            Self::ReasonInvalid(_) => "ANTIGRAVITY_REASON_INVALID",
            Self::Sqlite(_) => "ANTIGRAVITY_DATABASE_INVALID",
        }
    }
}

impl fmt::Display for AntigravityStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EpochInvalid(msg) => write!(f, "{ANTIGRAVITY_USAGE_EPOCH_INVALID}: {msg}"),
            Self::DigestInvalid(msg) => write!(f, "invalid digest: {msg}"),
            Self::ReasonInvalid(msg) => write!(f, "invalid reason code: {msg}"),
            Self::Sqlite(msg) => write!(f, "sqlite error: {msg}"),
        }
    }
}

impl std::error::Error for AntigravityStorageError {}

impl From<rusqlite::Error> for AntigravityStorageError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err.to_string())
    }
}

impl From<crate::source::adapter::SourceStorageError> for AntigravityStorageError {
    fn from(err: crate::source::adapter::SourceStorageError) -> Self {
        Self::Sqlite(err.to_string())
    }
}

/// Validate a payload digest per [INV-DB-06] (lowercase [0-9a-f]{64}).
pub fn validate_payload_digest(digest: &str) -> Result<(), AntigravityStorageError> {
    if digest.len() != 64 {
        return Err(AntigravityStorageError::DigestInvalid(format!(
            "digest length must be 64, got {}",
            digest.len()
        )));
    }
    if !digest
        .bytes()
        .all(|b| (b'0'..=b'9').contains(&b) || (b'a'..=b'f').contains(&b))
    {
        return Err(AntigravityStorageError::DigestInvalid(format!(
            "digest contains uppercase or non-hex characters: {digest}"
        )));
    }
    Ok(())
}

/// Upsert `antigravity_conversation_state` row.
pub fn upsert_conversation_state(
    conn: &Connection,
    conversation_id: &str,
    observed_gen_max_idx: i64,
    observed_step_max_idx: i64,
    last_scanned_at_ms: i64,
) -> Result<(), AntigravityStorageError> {
    conn.execute(
        "INSERT INTO antigravity_conversation_state (
            conversation_id, observed_gen_max_idx, observed_step_max_idx, last_scanned_at_ms
         ) VALUES (?, ?, ?, ?)
         ON CONFLICT(conversation_id) DO UPDATE SET
            observed_gen_max_idx = excluded.observed_gen_max_idx,
            observed_step_max_idx = excluded.observed_step_max_idx,
            last_scanned_at_ms = excluded.last_scanned_at_ms",
        (
            conversation_id,
            observed_gen_max_idx,
            observed_step_max_idx,
            last_scanned_at_ms.max(0),
        ),
    )?;
    Ok(())
}

/// Replace all quarantine records for a conversation [INV-QUAR-05], [INV-QUAR-06].
pub fn replace_conversation_quarantine(
    conn: &Connection,
    conversation_id: &str,
    quarantine_records: &[AntigravityQuarantineRecord],
    observed_now_ms: i64,
) -> Result<(), AntigravityStorageError> {
    // 1. Validate all digests and reason codes
    for q in quarantine_records {
        validate_payload_digest(&q.payload_digest)?;
        if !VALID_REASON_CODES.contains(&q.reason_code) {
            return Err(AntigravityStorageError::ReasonInvalid(format!(
                "unknown quarantine reason code: {}",
                q.reason_code
            )));
        }
    }

    // 2. Fold duplicates by (conversation_id, payload_digest, reason_code) [INV-QUAR-06]
    // gen_idx takes minimum non-null idx
    #[derive(Clone)]
    struct FoldedQuarantine {
        payload_digest: String,
        gen_idx: Option<i64>,
        response_id: Option<String>,
        reason_code: &'static str,
    }

    let mut folded_map: HashMap<(String, &'static str), FoldedQuarantine> = HashMap::new();
    for q in quarantine_records {
        let key = (q.payload_digest.clone(), q.reason_code);
        match folded_map.get_mut(&key) {
            Some(existing) => {
                match (existing.gen_idx, q.gen_idx) {
                    (Some(e), Some(n)) => existing.gen_idx = Some(e.min(n)),
                    (None, Some(n)) => existing.gen_idx = Some(n),
                    _ => {}
                }
                if existing.response_id.is_none() && q.response_id.is_some() {
                    existing.response_id = q.response_id.clone();
                }
            }
            None => {
                folded_map.insert(
                    key,
                    FoldedQuarantine {
                        payload_digest: q.payload_digest.clone(),
                        gen_idx: q.gen_idx,
                        response_id: q.response_id.clone(),
                        reason_code: q.reason_code,
                    },
                );
            }
        }
    }

    // 3. Load previous seen times for this conversation
    let mut stmt = conn.prepare(
        "SELECT payload_digest, reason_code, first_seen_at_ms, last_seen_at_ms
         FROM antigravity_usage_quarantine WHERE conversation_id = ?",
    )?;

    let mut previous_seen: HashMap<(String, String), (i64, i64)> = HashMap::new();
    let mut rows = stmt.query([conversation_id])?;
    while let Some(row) = rows.next()? {
        let d: String = row.get(0)?;
        let r: String = row.get(1)?;
        let first: i64 = row.get(2)?;
        let last: i64 = row.get(3)?;
        previous_seen.insert((d, r), (first, last));
    }

    // 4. Delete existing quarantine records for this conversation
    conn.execute(
        "DELETE FROM antigravity_usage_quarantine WHERE conversation_id = ?",
        [conversation_id],
    )?;

    // 5. Insert replacement records with monotonic seen time formulas [INV-QUAR-05]
    let mut insert_stmt = conn.prepare(
        "INSERT INTO antigravity_usage_quarantine (
            conversation_id, payload_digest, gen_idx, response_id, reason_code, first_seen_at_ms, last_seen_at_ms
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;

    for folded in folded_map.values() {
        let key = (
            folded.payload_digest.clone(),
            folded.reason_code.to_string(),
        );
        let (first_seen, last_seen) = match previous_seen.get(&key) {
            Some(&(prev_first, prev_last)) => (prev_first, prev_last.max(observed_now_ms)),
            None => {
                let now = observed_now_ms.max(0);
                (now, now)
            }
        };

        insert_stmt.execute((
            conversation_id,
            &folded.payload_digest,
            folded.gen_idx,
            folded.response_id.as_deref(),
            folded.reason_code,
            first_seen,
            last_seen,
        ))?;
    }

    Ok(())
}

/// Target and plan for the Usage epoch state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UsageEpochPlan {
    /// Initial scan: will create transient Build 1 / 1 in transaction.
    InitialCreate { next_build_epoch: i64 },
    /// Active scan: already established active epoch, compare and write to Active target directly.
    Active { epoch: i64 },
}

/// Validate scan entry against Section 4.10.2 Usage epoch state machine.
pub fn validate_usage_epoch(conn: &Connection) -> Result<UsageEpochPlan, AntigravityStorageError> {
    let row: Option<(i64, i64, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT active_epoch, active_parser_version, build_epoch, build_parser_version
             FROM source_usage_epochs WHERE source = 'antigravity'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    match row {
        None => {
            // Valid initial: no row
            Ok(UsageEpochPlan::InitialCreate {
                next_build_epoch: 1,
            })
        }
        Some((0, 0, None, None)) => {
            // Valid initial: 0 / 0 / NULL / NULL
            Ok(UsageEpochPlan::InitialCreate {
                next_build_epoch: 1,
            })
        }
        Some((n, 1, None, None)) if n > 0 => {
            // Valid active: n > 0 / 1 / NULL / NULL
            Ok(UsageEpochPlan::Active { epoch: n })
        }
        Some((_, _, Some(_), _)) | Some((_, _, _, Some(_))) => {
            Err(AntigravityStorageError::EpochInvalid(
                "durable build epoch or parser version is non-null".into(),
            ))
        }
        Some((0, parser, None, None)) if parser != 0 => Err(AntigravityStorageError::EpochInvalid(
            format!("invalid parser version {parser} for active epoch 0"),
        )),
        Some((n, parser, None, None)) if n > 0 && parser != 1 => {
            Err(AntigravityStorageError::EpochInvalid(format!(
                "invalid parser version {parser} for active epoch {n}"
            )))
        }
        Some((a, p, b, bp)) => Err(AntigravityStorageError::EpochInvalid(format!(
            "impossible epoch combination: active=({a},{p}), build=({b:?},{bp:?})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_validation_double_check() {
        // Valid 64 lowercase hex
        let valid = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(validate_payload_digest(valid).is_ok());

        // Uppercase rejected
        let upper = "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(validate_payload_digest(upper).is_err());

        // Length not 64 rejected
        assert!(validate_payload_digest("abcd").is_err());

        // Non-hex rejected
        let non_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg";
        assert!(validate_payload_digest(non_hex).is_err());
    }

    #[test]
    fn test_epoch_state_machine_validation() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE source_usage_epochs (
                source TEXT PRIMARY KEY,
                active_epoch INTEGER NOT NULL DEFAULT 0,
                active_parser_version INTEGER NOT NULL DEFAULT 0,
                build_epoch INTEGER,
                build_parser_version INTEGER
            )",
            [],
        )
        .unwrap();

        // 1. No row -> InitialCreate
        assert_eq!(
            validate_usage_epoch(&conn).unwrap(),
            UsageEpochPlan::InitialCreate {
                next_build_epoch: 1
            }
        );

        // 2. 0 / 0 / NULL / NULL -> InitialCreate
        conn.execute(
            "INSERT INTO source_usage_epochs VALUES ('antigravity', 0, 0, NULL, NULL)",
            [],
        )
        .unwrap();
        assert_eq!(
            validate_usage_epoch(&conn).unwrap(),
            UsageEpochPlan::InitialCreate {
                next_build_epoch: 1
            }
        );

        // 3. Active 1 / 1 / NULL / NULL -> Active { epoch: 1 }
        conn.execute(
            "UPDATE source_usage_epochs SET active_epoch = 1, active_parser_version = 1 WHERE source = 'antigravity'",
            [],
        )
        .unwrap();
        assert_eq!(
            validate_usage_epoch(&conn).unwrap(),
            UsageEpochPlan::Active { epoch: 1 }
        );

        // 4. Durable build non-null -> Error
        conn.execute(
            "UPDATE source_usage_epochs SET build_epoch = 2, build_parser_version = 1 WHERE source = 'antigravity'",
            [],
        )
        .unwrap();
        assert_eq!(
            validate_usage_epoch(&conn).unwrap_err().code(),
            ANTIGRAVITY_USAGE_EPOCH_INVALID
        );

        // 5. Active epoch 1 with wrong parser version -> Error
        conn.execute(
            "UPDATE source_usage_epochs SET build_epoch = NULL, build_parser_version = NULL, active_parser_version = 99 WHERE source = 'antigravity'",
            [],
        )
        .unwrap();
        assert_eq!(
            validate_usage_epoch(&conn).unwrap_err().code(),
            ANTIGRAVITY_USAGE_EPOCH_INVALID
        );
    }
}
