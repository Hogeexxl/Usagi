//! Codex Public v1 status projection.
//!
//! The snapshot is intentionally private implementation state. Public API v1
//! converts it to its historical status contract while this module owns the
//! Codex child-history tie-break and pre-v11 fallback rules.

use crate::storage::{Ledger, Result, StorageError};
use rusqlite::OptionalExtension;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexScanStatusSnapshot {
    pub(crate) data_revision: i64,
    pub(crate) status_revision: i64,
    pub(crate) scan_state: String,
    pub(crate) source_binding_status: String,
    pub(crate) last_finished_scan_result: Option<String>,
    pub(crate) last_scan_started_at_ms: Option<i64>,
    pub(crate) last_scan_completed_at_ms: Option<i64>,
    pub(crate) last_scan_failed_at_ms: Option<i64>,
    pub(crate) last_scan_error_code: Option<String>,
}

impl CodexScanStatusSnapshot {
    fn validate(&self) -> std::result::Result<(), StorageError> {
        if self.data_revision < 0 || self.status_revision < 0 {
            return Err(StorageError::invalid_state(
                "status revisions must be non-negative",
            ));
        }
        if !matches!(self.scan_state.as_str(), "running" | "idle" | "failed") {
            return Err(StorageError::invalid_state("invalid Codex scan state"));
        }
        if !matches!(
            self.source_binding_status.as_str(),
            "unbound" | "ready" | "source_changed"
        ) {
            return Err(StorageError::invalid_state(
                "invalid Codex source binding status",
            ));
        }
        if let Some(result) = self.last_finished_scan_result.as_deref() {
            if !matches!(result, "completed" | "failed") {
                return Err(StorageError::invalid_state("invalid Codex terminal result"));
            }
        }
        for timestamp in [
            self.last_scan_started_at_ms,
            self.last_scan_completed_at_ms,
            self.last_scan_failed_at_ms,
        ] {
            if timestamp.is_some_and(|value| value < 0) {
                return Err(StorageError::invalid_state(
                    "status timestamps must be non-negative",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn snapshot(ledger: &Ledger) -> Result<CodexScanStatusSnapshot> {
    ledger.with_read_transaction(read_snapshot)
}

fn read_snapshot(transaction: &rusqlite::Transaction<'_>) -> Result<CodexScanStatusSnapshot> {
    let (data_revision, status_revision, active_scan_id): (i64, i64, Option<String>) = transaction
        .query_row(
            "SELECT data_revision, status_revision, active_scan_id
             FROM app_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| StorageError::invalid_state("app_meta row id=1 is missing"))?;
    let source_binding_status: String = transaction
        .query_row(
            "SELECT binding_status FROM codex_adapter_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StorageError::invalid_state("codex adapter state row id=1 is missing"))?;

    let any_codex_skipped: bool = transaction
        .query_row(
            "SELECT 1 FROM source_scan_runs WHERE source = 'codex' AND state = 'skipped' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if any_codex_skipped {
        return Err(StorageError::invalid_state(
            "invariant violation: codex source scan run cannot be skipped",
        ));
    }

    let (last_scan_started_at_ms, last_scan_completed_at_ms, last_scan_failed_at_ms): (
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = transaction.query_row(
        "WITH codex_history AS (
            SELECT
                sr.scan_id,
                sr.started_at_ms,
                ssr.state AS effective_state,
                ssr.finished_at_ms
            FROM scan_runs sr
            JOIN source_scan_runs ssr ON ssr.scan_id = sr.scan_id
            WHERE ssr.source = 'codex'

            UNION ALL

            SELECT
                sr.scan_id,
                sr.started_at_ms,
                sr.state AS effective_state,
                sr.finished_at_ms
            FROM scan_runs sr
            WHERE sr.state IN ('running', 'completed', 'failed')
              AND NOT EXISTS (
                  SELECT 1 FROM source_scan_runs child WHERE child.scan_id = sr.scan_id
              )
        )
        SELECT
            MAX(started_at_ms),
            MAX(CASE WHEN effective_state = 'completed' THEN finished_at_ms ELSE NULL END),
            MAX(CASE WHEN effective_state = 'failed' THEN finished_at_ms ELSE NULL END)
        FROM codex_history",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let latest_terminal: Option<(String, Option<String>)> = transaction
        .query_row(
            "WITH codex_history AS (
                SELECT
                    sr.scan_id,
                    ssr.state AS effective_state,
                    ssr.finished_at_ms,
                    ssr.error_code
                FROM scan_runs sr
                JOIN source_scan_runs ssr ON ssr.scan_id = sr.scan_id
                WHERE ssr.source = 'codex'

                UNION ALL

                SELECT
                    sr.scan_id,
                    sr.state AS effective_state,
                    sr.finished_at_ms,
                    sr.error_code
                FROM scan_runs sr
                WHERE sr.state IN ('running', 'completed', 'failed')
                  AND NOT EXISTS (
                      SELECT 1 FROM source_scan_runs child WHERE child.scan_id = sr.scan_id
                  )
            )
            SELECT effective_state, error_code
            FROM codex_history
            WHERE effective_state IN ('completed', 'failed', 'skipped')
              AND finished_at_ms IS NOT NULL
            ORDER BY finished_at_ms DESC, scan_id DESC
            LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    let (last_finished_scan_result, last_scan_error_code) = match latest_terminal.as_ref() {
        Some((state, _)) if state == "skipped" => {
            return Err(StorageError::invalid_state(
                "invariant violation: codex source scan run cannot be skipped",
            ));
        }
        Some((state, _)) if state == "completed" => (Some("completed".to_owned()), None),
        Some((state, error_code)) if state == "failed" => {
            (Some("failed".to_owned()), error_code.clone())
        }
        Some((other, _)) => {
            return Err(StorageError::invalid_state(format!(
                "unexpected terminal state in codex history: {other}"
            )));
        }
        None => (None, None),
    };

    let scan_state = match active_scan_id.as_deref() {
        Some(scan_id) => {
            let codex_child_state: Option<String> = transaction
                .query_row(
                    "SELECT state FROM source_scan_runs WHERE scan_id = ?1 AND source = 'codex'",
                    [scan_id],
                    |row| row.get(0),
                )
                .optional()?;

            match codex_child_state.as_deref() {
                Some("queued") | Some("running") => "running".to_owned(),
                Some("failed") => "failed".to_owned(),
                Some("skipped") => {
                    return Err(StorageError::invalid_state(
                        "invariant violation: codex source scan run cannot be skipped",
                    ));
                }
                Some("completed") => "idle".to_owned(),
                Some(other) => {
                    return Err(StorageError::invalid_state(format!(
                        "unexpected active codex child state: {other}"
                    )));
                }
                None => {
                    let has_any_children: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM source_scan_runs WHERE scan_id = ?1)",
                        [scan_id],
                        |row| row.get(0),
                    )?;

                    if has_any_children {
                        match latest_terminal.as_ref() {
                            Some((state, _)) if state == "failed" => "failed".to_owned(),
                            _ => "idle".to_owned(),
                        }
                    } else {
                        let parent_state: Option<String> = transaction
                            .query_row(
                                "SELECT state FROM scan_runs WHERE scan_id = ?1",
                                [scan_id],
                                |row| row.get(0),
                            )
                            .optional()?;

                        if parent_state.as_deref() == Some("running") {
                            "running".to_owned()
                        } else {
                            match latest_terminal.as_ref() {
                                Some((state, _)) if state == "failed" => "failed".to_owned(),
                                _ => "idle".to_owned(),
                            }
                        }
                    }
                }
            }
        }
        None => match latest_terminal.as_ref() {
            Some((state, _)) if state == "failed" => "failed".to_owned(),
            _ => "idle".to_owned(),
        },
    };

    let snapshot = CodexScanStatusSnapshot {
        data_revision,
        status_revision,
        scan_state,
        source_binding_status,
        last_finished_scan_result,
        last_scan_started_at_ms,
        last_scan_completed_at_ms,
        last_scan_failed_at_ms,
        last_scan_error_code,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
