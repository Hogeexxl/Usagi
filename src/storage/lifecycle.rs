//! Scan lifecycle persistence.
//!
//! The scanner owns scheduling and recovery.  This module only owns the
//! durable scan target rows, the one follow-up slot, and the `app_meta`
//! projection that describes the current status.  Every write below uses one
//! SQLite transaction for the target row and the projection, and returns the
//! projection read from that same transaction.

use crate::domain::{
    AppState, CodexScanStatusSnapshot, DomainError, FollowupStartFailedEvent, FollowupStartedEvent,
    FollowupState, ReserveScanFollowupEvent, ScanCompletedEvent, ScanFailedEvent,
    ScanLifecycleState, ScanRequestKind, ScanResult, ScanRun, ScanRunState, ScanStartEvent,
    ScanState, ScanStatusSnapshot, ScanTrigger, SourceScanState, SourceScanStatus,
};
use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior, params};

use super::{Ledger, Result, StorageError};

const FOLLOWUP_START_FAILED_CODES: &[&str] =
    &["SCAN_START_FAILED", "SCANNER_UNAVAILABLE", "SOURCE_CHANGED"];

impl Ledger {
    /// Read the current app projection and, optionally, one immutable target
    /// row from the same SQLite read transaction.
    pub fn scan_status_snapshot(&self, target_scan_id: Option<&str>) -> Result<ScanStatusSnapshot> {
        if let Some(scan_id) = target_scan_id {
            validate_id(scan_id, "target_scan_id")?;
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let app_state = read_app_state(&transaction)?;
        let target_scan = match target_scan_id {
            Some(scan_id) => read_scan_run(&transaction, scan_id)?,
            None => None,
        };
        let source_scan_id = target_scan_id
            .or(app_state.scan.active_scan_id.as_deref())
            .or(app_state.scan.last_finished_scan_id.as_deref());
        let sources = match source_scan_id {
            Some(scan_id) => read_source_scan_statuses(&transaction, scan_id)?,
            None => Vec::new(),
        };
        let snapshot = ScanStatusSnapshot::new_with_sources(app_state, target_scan, sources)
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        transaction.commit()?;
        Ok(snapshot)
    }

    /// Read the Codex-visible scan status snapshot from a single SQLite Deferred
    /// read transaction (§12.2.5).
    pub fn codex_scan_status_snapshot(&self) -> Result<CodexScanStatusSnapshot> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let snapshot = read_codex_scan_status_snapshot(&transaction)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    /// Start a new direct scan.  A prior `start_failed` follow-up is a
    /// terminal historical row; beginning a new direct scan clears only its
    /// app-meta slot projection.
    pub fn mark_scan_started(&self, event: ScanStartEvent) -> Result<ScanState> {
        // Compatibility seam for callers that only exercise the global v10
        // lifecycle.  The v11 coordinator always calls the explicit
        // registry-snapshot variant below.
        self.mark_scan_started_with_sources(event, &[])
    }

    /// Start a new direct scan and atomically freeze the registry snapshot as
    /// queued source child rows.  The parent row, app projection, and child
    /// manifest all become visible in one transaction.
    pub fn mark_scan_started_with_sources(
        &self,
        event: ScanStartEvent,
        sources: &[String],
    ) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;
        validate_source_manifest(sources)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_app_state(&transaction)?;
        if current.active_scan_id.is_some() || current.scan_state == ScanLifecycleState::Running {
            return Err(StorageError::invalid_state(
                "a scan is already active".to_owned(),
            ));
        }
        if current.followup_state == Some(FollowupState::Queued) {
            return Err(StorageError::invalid_state(
                "a follow-up scan is queued".to_owned(),
            ));
        }
        if current.followup_state == Some(FollowupState::StartFailed) {
            ensure_start_failed_followup_projection(&transaction, &current.scan)?;
        }

        let next_revision = increment_status_revision(current.status_revision)?;
        transaction.execute(
            "INSERT INTO scan_runs (
                scan_id, trigger, request_kind, state, requested_at_ms,
                enqueued_status_revision, started_at_ms, started_status_revision
             ) VALUES (?1, ?2, 'direct', 'running', ?3, NULL, ?4, ?5)",
            params![
                event.scan_id,
                event.trigger.as_str(),
                event.requested_at_ms,
                event.started_at_ms,
                next_revision,
            ],
        )?;
        insert_source_scan_manifest(&transaction, &event.scan_id, sources)?;
        transaction.execute(
            "UPDATE app_meta
             SET status_revision = ?1,
                 scan_state = 'running',
                 active_scan_id = ?2,
                 last_scan_started_at_ms = ?3,
                 followup_scan_id = NULL,
                 followup_state = NULL,
                 followup_trigger = NULL,
                 followup_requested_at_ms = NULL,
                 followup_enqueued_status_revision = NULL,
                 followup_error_code = NULL
             WHERE id = 1",
            params![next_revision, event.scan_id, event.started_at_ms],
        )?;

        let state = read_scan_state(&transaction)?;
        let data_revision = current.data_revision;
        transaction.commit()?;
        self.publish_scan_state(data_revision, &state);
        Ok(state)
    }

    /// Reserve the one durable, coalesced follow-up slot while a scan is
    /// running.  Repeated reservations return the original slot unchanged.
    pub fn reserve_scan_followup(&self, event: ReserveScanFollowupEvent) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_app_state(&transaction)?;
        if current.scan_state != ScanLifecycleState::Running || current.active_scan_id.is_none() {
            return Err(StorageError::invalid_state(
                "follow-up reservation requires an active scan".to_owned(),
            ));
        }
        ensure_scan_row_state(
            &transaction,
            current.active_scan_id.as_deref().ok_or_else(|| {
                StorageError::invalid_state("running scan is missing its active id")
            })?,
            ScanRunState::Running,
        )?;

        if current.followup_state == Some(FollowupState::Queued) {
            ensure_queued_followup_projection(&transaction, &current.scan)?;
            let state = current.scan;
            transaction.commit()?;
            return Ok(state);
        }
        if current.followup_state.is_some() {
            return Err(StorageError::invalid_state(
                "follow-up slot is not available".to_owned(),
            ));
        }

        let next_revision = increment_status_revision(current.status_revision)?;
        transaction.execute(
            "INSERT INTO scan_runs (
                scan_id, trigger, request_kind, state, requested_at_ms,
                enqueued_status_revision
             ) VALUES (?1, ?2, 'followup', 'queued', ?3, ?4)",
            params![
                event.followup_scan_id,
                event.trigger.as_str(),
                event.requested_at_ms,
                next_revision,
            ],
        )?;
        transaction.execute(
            "UPDATE app_meta
             SET status_revision = ?1,
                 followup_scan_id = ?2,
                 followup_state = 'queued',
                 followup_trigger = ?3,
                 followup_requested_at_ms = ?4,
                 followup_enqueued_status_revision = ?1,
                 followup_error_code = NULL
             WHERE id = 1",
            params![
                next_revision,
                event.followup_scan_id,
                event.trigger.as_str(),
                event.requested_at_ms,
            ],
        )?;

        let state = read_scan_state(&transaction)?;
        let data_revision = current.data_revision;
        transaction.commit()?;
        self.publish_scan_state(data_revision, &state);
        Ok(state)
    }

    /// Atomically consume a queued follow-up and make it the active scan.
    pub fn mark_followup_started(&self, event: FollowupStartedEvent) -> Result<ScanState> {
        self.mark_followup_started_with_sources(event, &[])
    }

    /// Atomically consume a queued follow-up, freeze the current registry
    /// snapshot, and make the follow-up the active scan.  Child creation and
    /// the global running transition share one status revision and SQLite
    /// transaction.
    pub fn mark_followup_started_with_sources(
        &self,
        event: FollowupStartedEvent,
        sources: &[String],
    ) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;
        validate_source_manifest(sources)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_app_state(&transaction)?;
        if current.active_scan_id.is_some() || current.scan_state == ScanLifecycleState::Running {
            return Err(StorageError::invalid_state(
                "cannot start a follow-up while another scan is active".to_owned(),
            ));
        }
        if current.followup_state != Some(FollowupState::Queued)
            || current.followup_scan_id.as_deref() != Some(event.scan_id.as_str())
        {
            return Err(StorageError::invalid_state(
                "follow-up reservation does not match the requested scan".to_owned(),
            ));
        }
        ensure_queued_followup_projection(&transaction, &current.scan)?;

        let next_revision = increment_status_revision(current.status_revision)?;
        let changed = transaction.execute(
            "UPDATE scan_runs
             SET state = 'running', started_at_ms = ?1, started_status_revision = ?2
             WHERE scan_id = ?3 AND state = 'queued'",
            params![event.started_at_ms, next_revision, event.scan_id],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "queued follow-up changed before it could start".to_owned(),
            ));
        }
        insert_source_scan_manifest(&transaction, &event.scan_id, sources)?;
        transaction.execute(
            "UPDATE app_meta
             SET status_revision = ?1,
                 scan_state = 'running',
                 active_scan_id = ?2,
                 last_scan_started_at_ms = ?3,
                 followup_scan_id = NULL,
                 followup_state = NULL,
                 followup_trigger = NULL,
                 followup_requested_at_ms = NULL,
                 followup_enqueued_status_revision = NULL,
                 followup_error_code = NULL
             WHERE id = 1",
            params![next_revision, event.scan_id, event.started_at_ms],
        )?;

        let state = read_scan_state(&transaction)?;
        let data_revision = current.data_revision;
        transaction.commit()?;
        self.publish_scan_state(data_revision, &state);
        Ok(state)
    }

    /// Permanently fail a queued follow-up without consuming its projection
    /// slot.  Busy errors arise before this method can commit and therefore
    /// leave the row and slot queued for retry.
    pub fn mark_followup_start_failed(&self, event: FollowupStartFailedEvent) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;
        if !FOLLOWUP_START_FAILED_CODES.contains(&event.error_code.as_str()) {
            return Err(StorageError::invalid_state(format!(
                "invalid follow-up start failure code: {}",
                event.error_code
            )));
        }

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_app_state(&transaction)?;
        if current.followup_state != Some(FollowupState::Queued)
            || current.followup_scan_id.as_deref() != Some(event.scan_id.as_str())
        {
            return Err(StorageError::invalid_state(
                "follow-up reservation does not match the requested scan".to_owned(),
            ));
        }
        ensure_queued_followup_projection(&transaction, &current.scan)?;

        let next_revision = increment_status_revision(current.status_revision)?;
        let changed = transaction.execute(
            "UPDATE scan_runs
             SET state = 'start_failed', finished_at_ms = ?1,
                 terminal_status_revision = ?2, error_code = ?3
             WHERE scan_id = ?4 AND state = 'queued'",
            params![
                event.failed_at_ms,
                next_revision,
                event.error_code,
                event.scan_id
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "queued follow-up changed before start failure was recorded".to_owned(),
            ));
        }
        transaction.execute(
            "UPDATE app_meta
             SET status_revision = ?1,
                 followup_state = 'start_failed',
                 followup_error_code = ?2
             WHERE id = 1",
            params![next_revision, event.error_code],
        )?;

        let state = read_scan_state(&transaction)?;
        let data_revision = current.data_revision;
        transaction.commit()?;
        self.publish_scan_state(data_revision, &state);
        Ok(state)
    }

    /// Mark the current active scan as completed while preserving any queued
    /// or start-failed follow-up in the single slot.
    pub fn mark_scan_completed(&self, event: ScanCompletedEvent) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = require_active_scan(&transaction, &event.scan_id)?;

        let failed_child: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM source_scan_runs
                 WHERE scan_id = ?1 AND state = 'failed' LIMIT 1",
                [&event.scan_id],
                |row| row.get(0),
            )
            .optional()?;
        let unfinished_children: i64 = transaction.query_row(
            "SELECT count(*) FROM source_scan_runs
             WHERE scan_id = ?1 AND state IN ('queued', 'running')",
            [&event.scan_id],
            |row| row.get(0),
        )?;
        if unfinished_children != 0 {
            return Err(StorageError::invalid_state(
                "cannot complete a scan while source runs remain unfinished".to_owned(),
            ));
        }
        let next_revision = increment_status_revision(current.status_revision)?;

        let (global_state, global_error) = if failed_child.is_some() {
            ("failed", Some("SOURCE_RUN_FAILED"))
        } else {
            ("completed", None)
        };
        let changed = if global_state == "failed" {
            transaction.execute(
                "UPDATE scan_runs
                 SET state = 'failed', finished_at_ms = ?1,
                     terminal_status_revision = ?2, error_code = ?3
                 WHERE scan_id = ?4 AND state = 'running'",
                params![
                    event.completed_at_ms,
                    next_revision,
                    global_error,
                    event.scan_id
                ],
            )?
        } else {
            transaction.execute(
                "UPDATE scan_runs
                 SET state = 'completed', finished_at_ms = ?1,
                     terminal_status_revision = ?2
                 WHERE scan_id = ?3 AND state = 'running'",
                params![event.completed_at_ms, next_revision, event.scan_id],
            )?
        };
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "active scan is no longer running".to_owned(),
            ));
        }
        if global_state == "failed" {
            transaction.execute(
                "UPDATE app_meta
                 SET status_revision = ?1,
                     scan_state = 'failed',
                     active_scan_id = NULL,
                     last_scan_failed_at_ms = ?2,
                     last_scan_error_code = 'SOURCE_RUN_FAILED',
                     last_finished_scan_id = ?3,
                     last_finished_scan_result = 'failed'
                 WHERE id = 1",
                params![next_revision, event.completed_at_ms, event.scan_id],
            )?;
        } else {
            transaction.execute(
                "UPDATE app_meta
                 SET status_revision = ?1,
                     scan_state = 'idle',
                     active_scan_id = NULL,
                     last_scan_completed_at_ms = ?2,
                     last_scan_error_code = NULL,
                     last_finished_scan_id = ?3,
                     last_finished_scan_result = 'completed'
                 WHERE id = 1",
                params![next_revision, event.completed_at_ms, event.scan_id],
            )?;
        }

        let state = read_scan_state(&transaction)?;
        let data_revision = current.data_revision;
        transaction.commit()?;
        self.publish_scan_state(data_revision, &state);
        Ok(state)
    }

    /// Mark the current active scan as failed.  Cancellation and startup
    /// interruption are represented by the normal `failed` state and their
    /// structured error codes (`SCAN_CANCELLED` / `SCAN_INTERRUPTED`).
    pub fn mark_scan_failed(&self, event: ScanFailedEvent) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = require_active_scan(&transaction, &event.scan_id)?;
        let next_revision = increment_status_revision(current.status_revision)?;

        let changed = transaction.execute(
            "UPDATE scan_runs
             SET state = 'failed', finished_at_ms = ?1,
                 terminal_status_revision = ?2, error_code = ?3
             WHERE scan_id = ?4 AND state = 'running'",
            params![
                event.failed_at_ms,
                next_revision,
                event.error_code,
                event.scan_id
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "active scan is no longer running".to_owned(),
            ));
        }
        // A global terminal transition owns any source children that have
        // not yet reached a terminal state.  Keeping this in the same
        // transaction prevents shutdown/recovery from exposing a failed
        // parent with queued or running children.
        transaction.execute(
            "UPDATE source_scan_runs
             SET state = 'failed', finished_at_ms = ?1, error_code = ?2
             WHERE scan_id = ?3 AND state IN ('queued', 'running')",
            params![event.failed_at_ms, event.error_code, event.scan_id],
        )?;
        transaction.execute(
            "UPDATE app_meta
             SET status_revision = ?1,
                 scan_state = 'failed',
                 active_scan_id = NULL,
                 last_scan_failed_at_ms = ?2,
                 last_scan_error_code = ?3,
                 last_finished_scan_id = ?4,
                 last_finished_scan_result = 'failed'
             WHERE id = 1",
            params![
                next_revision,
                event.failed_at_ms,
                event.error_code,
                event.scan_id,
            ],
        )?;

        let state = read_scan_state(&transaction)?;
        let data_revision = current.data_revision;
        transaction.commit()?;
        self.publish_scan_state(data_revision, &state);
        Ok(state)
    }

    /// Move one source child from queued to running and publish the resulting
    /// status revision after the transaction commits.
    pub fn mark_source_scan_started(
        &self,
        scan_id: &str,
        source: &str,
        started_at_ms: i64,
    ) -> Result<ScanState> {
        transition_source_scan(
            self,
            scan_id,
            source,
            started_at_ms,
            SourceChildTransition::Start,
        )
    }

    /// Move one source child from running to completed.
    pub fn mark_source_scan_completed(
        &self,
        scan_id: &str,
        source: &str,
        finished_at_ms: i64,
    ) -> Result<ScanState> {
        transition_source_scan(
            self,
            scan_id,
            source,
            finished_at_ms,
            SourceChildTransition::Complete,
        )
    }

    /// Move one source child from queued to skipped.  A skipped child never
    /// receives a started timestamp.
    pub fn mark_source_scan_skipped(
        &self,
        scan_id: &str,
        source: &str,
        finished_at_ms: i64,
    ) -> Result<ScanState> {
        transition_source_scan(
            self,
            scan_id,
            source,
            finished_at_ms,
            SourceChildTransition::Skip,
        )
    }

    /// Move one source child from queued or running to failed, retaining the
    /// source-specific error code.  Global cancellation/recovery terminalizes
    /// all remaining children through `mark_scan_failed` instead.
    pub fn mark_source_scan_failed(
        &self,
        scan_id: &str,
        source: &str,
        finished_at_ms: i64,
        error_code: &str,
    ) -> Result<ScanState> {
        validate_error_code(error_code)?;
        transition_source_scan(
            self,
            scan_id,
            source,
            finished_at_ms,
            SourceChildTransition::Fail(error_code),
        )
    }
}

#[derive(Clone, Copy)]
enum SourceChildTransition<'a> {
    Start,
    Complete,
    Skip,
    Fail(&'a str),
}

fn transition_source_scan(
    ledger: &Ledger,
    scan_id: &str,
    source: &str,
    at_ms: i64,
    transition: SourceChildTransition<'_>,
) -> Result<ScanState> {
    validate_id(scan_id, "scan_id")?;
    validate_source(source)?;
    if at_ms < 0 {
        return Err(StorageError::invalid_state(
            "source scan timestamp must be non-negative".to_owned(),
        ));
    }

    let mut connection = ledger.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = read_app_state(&transaction)?;
    if current.scan_state != ScanLifecycleState::Running
        || current.active_scan_id.as_deref() != Some(scan_id)
    {
        return Err(StorageError::invalid_state(
            "source child transition requires the active global scan".to_owned(),
        ));
    }
    ensure_scan_row_state(&transaction, scan_id, ScanRunState::Running)?;
    let next_revision = increment_status_revision(current.status_revision)?;
    let changed = match transition {
        SourceChildTransition::Start => transaction.execute(
            "UPDATE source_scan_runs
             SET state = 'running', started_at_ms = ?1
             WHERE scan_id = ?2 AND source = ?3 AND state = 'queued'",
            params![at_ms, scan_id, source],
        )?,
        SourceChildTransition::Complete => transaction.execute(
            "UPDATE source_scan_runs
             SET state = 'completed', finished_at_ms = ?1
             WHERE scan_id = ?2 AND source = ?3 AND state = 'running'",
            params![at_ms, scan_id, source],
        )?,
        SourceChildTransition::Skip => transaction.execute(
            "UPDATE source_scan_runs
             SET state = 'skipped', finished_at_ms = ?1
             WHERE scan_id = ?2 AND source = ?3 AND state = 'queued'",
            params![at_ms, scan_id, source],
        )?,
        SourceChildTransition::Fail(error_code) => transaction.execute(
            "UPDATE source_scan_runs
             SET state = 'failed', finished_at_ms = ?1, error_code = ?2
             WHERE scan_id = ?3 AND source = ?4 AND state IN ('queued', 'running')",
            params![at_ms, error_code, scan_id, source],
        )?,
    };
    if changed != 1 {
        return Err(StorageError::invalid_state(
            "source child is not in the expected state".to_owned(),
        ));
    }
    transaction.execute(
        "UPDATE app_meta SET status_revision = ?1 WHERE id = 1",
        [next_revision],
    )?;
    let state = read_scan_state(&transaction)?;
    let data_revision = current.data_revision;
    transaction.commit()?;
    ledger.publish_scan_state(data_revision, &state);
    Ok(state)
}

fn validate_id(value: &str, field: &'static str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(StorageError::invalid_state(format!(
            "invalid {field}: scan id must be non-empty and contain no control characters"
        )));
    }
    Ok(())
}

fn validate_source(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(StorageError::invalid_state(
            "source must be non-empty and contain no control characters".to_owned(),
        ));
    }
    Ok(())
}

fn validate_error_code(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_uppercase())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(StorageError::invalid_state(
            "source scan error code is not safe".to_owned(),
        ));
    }
    Ok(())
}

fn validate_source_manifest(sources: &[String]) -> Result<()> {
    for source in sources {
        validate_source(source)?;
    }
    for (index, source) in sources.iter().enumerate() {
        if sources[..index].iter().any(|prior| prior == source) {
            return Err(StorageError::invalid_state(format!(
                "duplicate source in registry snapshot: {source}"
            )));
        }
    }
    Ok(())
}

fn insert_source_scan_manifest(
    transaction: &Transaction<'_>,
    scan_id: &str,
    sources: &[String],
) -> Result<()> {
    for source in sources {
        transaction.execute(
            "INSERT INTO source_scan_runs (
                scan_id, source, state, started_at_ms, finished_at_ms, error_code
             ) VALUES (?1, ?2, 'queued', NULL, NULL, NULL)",
            params![scan_id, source],
        )?;
    }
    Ok(())
}

fn domain_storage_error(error: DomainError) -> StorageError {
    StorageError::invalid_state(error.to_string())
}

fn increment_status_revision(current: i64) -> Result<i64> {
    current
        .checked_add(1)
        .ok_or_else(|| StorageError::invalid_state("status_revision overflow"))
}

fn require_active_scan(transaction: &Transaction<'_>, scan_id: &str) -> Result<AppState> {
    let current = read_app_state(transaction)?;
    if current.scan_state != ScanLifecycleState::Running
        || current.active_scan_id.as_deref() != Some(scan_id)
    {
        return Err(StorageError::invalid_state(
            "scan ID is not the current active scan".to_owned(),
        ));
    }
    ensure_scan_row_state(transaction, scan_id, ScanRunState::Running)?;
    Ok(current)
}

fn ensure_scan_row_state(
    transaction: &Transaction<'_>,
    scan_id: &str,
    expected: ScanRunState,
) -> Result<()> {
    let state: Option<String> = transaction
        .query_row(
            "SELECT state FROM scan_runs WHERE scan_id = ?1",
            [scan_id],
            |row| row.get(0),
        )
        .optional()?;
    let actual = state.ok_or_else(|| {
        StorageError::invalid_state(format!("scan row {scan_id:?} does not exist"))
    })?;
    if actual != expected.as_str() {
        return Err(StorageError::invalid_state(format!(
            "scan row {scan_id:?} is {actual}, expected {}",
            expected.as_str()
        )));
    }
    Ok(())
}

fn ensure_queued_followup_projection(
    transaction: &Transaction<'_>,
    state: &ScanState,
) -> Result<()> {
    let scan_id = state
        .followup_scan_id
        .as_deref()
        .ok_or_else(|| StorageError::invalid_state("queued follow-up is missing its scan id"))?;
    let run = read_scan_run(transaction, scan_id)?.ok_or_else(|| {
        StorageError::invalid_state(format!("scan row {scan_id:?} does not exist"))
    })?;
    if run.state != ScanRunState::Queued
        || run.request_kind != ScanRequestKind::Followup
        || state.followup_trigger != Some(run.trigger)
        || state.followup_requested_at_ms != Some(run.requested_at_ms)
        || state.followup_enqueued_status_revision != run.enqueued_status_revision
    {
        return Err(StorageError::invalid_state(
            "queued follow-up projection does not match its scan row".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_start_failed_followup_projection(
    transaction: &Transaction<'_>,
    state: &ScanState,
) -> Result<()> {
    let scan_id = state.followup_scan_id.as_deref().ok_or_else(|| {
        StorageError::invalid_state("start-failed follow-up is missing its scan id")
    })?;
    let run = read_scan_run(transaction, scan_id)?.ok_or_else(|| {
        StorageError::invalid_state(format!("scan row {scan_id:?} does not exist"))
    })?;
    if run.state != ScanRunState::StartFailed
        || run.request_kind != ScanRequestKind::Followup
        || state.followup_trigger != Some(run.trigger)
        || state.followup_requested_at_ms != Some(run.requested_at_ms)
        || state.followup_enqueued_status_revision != run.enqueued_status_revision
        || state.followup_error_code != run.error_code
    {
        return Err(StorageError::invalid_state(
            "start-failed follow-up projection does not match its scan row".to_owned(),
        ));
    }
    Ok(())
}

fn read_scan_state(transaction: &Transaction<'_>) -> Result<ScanState> {
    Ok(read_app_state(transaction)?.scan)
}

fn read_app_state(transaction: &Transaction<'_>) -> Result<AppState> {
    let result = transaction
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
            source_binding_status
         FROM app_meta WHERE id = 1",
            [],
            |row| {
                let data_revision: i64 = row.get(0)?;
                let status_revision: i64 = row.get(1)?;
                let scan_state: ScanLifecycleState = parse_enum(row.get::<_, String>(2)?.as_str())?;
                let last_finished_scan_result = parse_optional_enum::<ScanResult>(row.get(5)?)?;
                let followup_state = parse_optional_enum::<FollowupState>(row.get(11)?)?;
                let followup_trigger = parse_optional_enum::<ScanTrigger>(row.get(12)?)?;
                let source_binding_status = parse_enum(row.get::<_, String>(16)?.as_str())?;

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
                        followup_enqueued_status_revision: row.get(14)?,
                        followup_error_code: row.get(15)?,
                        source_binding_status,
                    },
                )
                .map_err(domain_sql_error)
            },
        )
        .optional()?
        .ok_or_else(|| {
            rusqlite::Error::InvalidParameterName("app_meta row id=1 is missing".to_owned())
        })?;
    Ok(result)
}

fn read_codex_scan_status_snapshot(
    transaction: &Transaction<'_>,
) -> Result<CodexScanStatusSnapshot> {
    let (data_revision, status_revision, active_scan_id, source_binding_status): (
        i64,
        i64,
        Option<String>,
        String,
    ) = transaction
        .query_row(
            "SELECT data_revision, status_revision, active_scan_id, source_binding_status
             FROM app_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| StorageError::invalid_state("app_meta row id=1 is missing"))?;

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
    snapshot.validate().map_err(domain_storage_error)?;
    Ok(snapshot)
}

fn read_scan_run(transaction: &Transaction<'_>, scan_id: &str) -> Result<Option<ScanRun>> {
    transaction
        .query_row(
            "SELECT
                scan_id, trigger, request_kind, state, requested_at_ms,
                enqueued_status_revision, started_at_ms, started_status_revision,
                finished_at_ms, terminal_status_revision, error_code
             FROM scan_runs WHERE scan_id = ?1",
            [scan_id],
            scan_run_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn read_source_scan_statuses(
    transaction: &Transaction<'_>,
    scan_id: &str,
) -> Result<Vec<SourceScanStatus>> {
    let mut statement = transaction.prepare(
        "SELECT source, state, error_code
         FROM source_scan_runs
         WHERE scan_id = ?1
         ORDER BY source COLLATE BINARY ASC",
    )?;
    let rows = statement.query_map([scan_id], |row| {
        let source: String = row.get(0)?;
        let state = SourceScanState::try_from(row.get::<_, String>(1)?.as_str())
            .map_err(domain_sql_error)?;
        let error_code: Option<String> = row.get(2)?;
        SourceScanStatus::new(source, state, error_code).map_err(domain_sql_error)
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn scan_run_from_row(row: &Row<'_>) -> rusqlite::Result<ScanRun> {
    let scan_id: String = row.get(0)?;
    let trigger: ScanTrigger = parse_enum(row.get::<_, String>(1)?.as_str())?;
    let request_kind: ScanRequestKind = parse_enum(row.get::<_, String>(2)?.as_str())?;
    let state: ScanRunState = parse_enum(row.get::<_, String>(3)?.as_str())?;
    let value = ScanRun {
        scan_id,
        trigger,
        request_kind,
        state,
        requested_at_ms: row.get(4)?,
        enqueued_status_revision: row.get(5)?,
        started_at_ms: row.get(6)?,
        started_status_revision: row.get(7)?,
        finished_at_ms: row.get(8)?,
        terminal_status_revision: row.get(9)?,
        error_code: row.get(10)?,
    };
    value.validate().map_err(domain_sql_error)?;
    Ok(value)
}

fn parse_enum<T>(value: &str) -> rusqlite::Result<T>
where
    T: for<'a> TryFrom<&'a str, Error = DomainError>,
{
    T::try_from(value).map_err(domain_sql_error)
}

fn parse_enum_owned<T>(value: String) -> rusqlite::Result<T>
where
    T: for<'a> TryFrom<&'a str, Error = DomainError>,
{
    parse_enum(value.as_str())
}

fn parse_optional_enum<T>(value: Option<String>) -> rusqlite::Result<Option<T>>
where
    T: for<'a> TryFrom<&'a str, Error = DomainError>,
{
    value.map(parse_enum_owned).transpose()
}

fn domain_sql_error(error: DomainError) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        domain::{
            FollowupStartFailedEvent, FollowupStartedEvent, ReserveScanFollowupEvent,
            ScanCompletedEvent, ScanFailedEvent, ScanLifecycleState, ScanRunState, ScanStartEvent,
            ScanTrigger,
        },
        storage::LedgerOptions,
    };

    struct TempDir(PathBuf);

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    impl TempDir {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos();
            let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("usagi-lifecycle-{timestamp}-{sequence}"));
            fs::create_dir_all(&path).expect("create temporary lifecycle directory");
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

    fn ledger() -> (TempDir, Ledger) {
        let root = TempDir::new();
        let ledger = Ledger::open(LedgerOptions::new(
            root.path().join("mu.sqlite3"),
            root.path().join("codex"),
        ))
        .expect("open temporary lifecycle ledger");
        (root, ledger)
    }

    fn start(ledger: &Ledger, id: &str, at: i64) {
        ledger
            .mark_scan_started(ScanStartEvent::new(id, ScanTrigger::Manual, at).unwrap())
            .unwrap();
    }

    #[test]
    fn direct_start_and_target_snapshot_share_started_revision() {
        let (_root, ledger) = ledger();

        let state = ledger
            .mark_scan_started(ScanStartEvent::new("scan-a", ScanTrigger::Startup, 10).unwrap())
            .unwrap();
        assert_eq!(state.status_revision, 1);
        assert_eq!(state.scan_state, ScanLifecycleState::Running);
        assert_eq!(state.active_scan_id.as_deref(), Some("scan-a"));
        // Starting a scan is status-only; stable query data is unchanged.
        assert_eq!(ledger.app_state().unwrap().data_revision, 0);

        let snapshot = ledger.scan_status_snapshot(Some("scan-a")).unwrap();
        assert_eq!(snapshot.status_revision, 1);
        let target = snapshot.target_scan.unwrap();
        assert_eq!(target.scan_id, "scan-a");
        assert_eq!(target.state, ScanRunState::Running);
        assert_eq!(target.started_status_revision, Some(1));
        assert_eq!(target.terminal_status_revision, None);
    }

    #[test]
    fn followup_reservation_coalesces_without_revision_or_id_churn() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);

        let first = ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-a", ScanTrigger::Manual, 20).unwrap(),
            )
            .unwrap();
        assert_eq!(first.status_revision, 2);
        assert_eq!(first.followup_scan_id.as_deref(), Some("followup-a"));
        assert_eq!(first.followup_enqueued_status_revision, Some(2));

        let second = ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-b", ScanTrigger::Scheduled, 30).unwrap(),
            )
            .unwrap();
        assert_eq!(second, first);

        let target = ledger
            .scan_status_snapshot(Some("followup-a"))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(target.state, ScanRunState::Queued);
        assert_eq!(target.enqueued_status_revision, Some(2));
        assert!(
            ledger
                .scan_status_snapshot(Some("followup-b"))
                .unwrap()
                .target_scan
                .is_none()
        );
    }

    #[test]
    fn terminal_active_scan_keeps_queued_followup_and_followup_start_is_atomic() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-a", ScanTrigger::Manual, 20).unwrap(),
            )
            .unwrap();

        let completed = ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-a", 30).unwrap())
            .unwrap();
        assert_eq!(completed.status_revision, 3);
        assert_eq!(completed.scan_state, ScanLifecycleState::Idle);
        assert!(completed.active_scan_id.is_none());
        assert_eq!(completed.followup_scan_id.as_deref(), Some("followup-a"));
        assert_eq!(completed.followup_state, Some(FollowupState::Queued));

        let started = ledger
            .mark_followup_started(FollowupStartedEvent::new("followup-a", 40).unwrap())
            .unwrap();
        assert_eq!(started.status_revision, 4);
        assert_eq!(started.scan_state, ScanLifecycleState::Running);
        assert_eq!(started.active_scan_id.as_deref(), Some("followup-a"));
        assert!(started.followup_scan_id.is_none());

        let followup = ledger
            .scan_status_snapshot(Some("followup-a"))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(followup.state, ScanRunState::Running);
        assert_eq!(followup.started_at_ms, Some(40));
        assert_eq!(followup.started_status_revision, Some(4));
        assert_eq!(followup.enqueued_status_revision, Some(2));
    }

    #[test]
    fn failed_scan_accepts_cancelled_and_interrupted_codes_and_preserves_history() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);
        let failed = ledger
            .mark_scan_failed(ScanFailedEvent::new("scan-a", 20, "SCAN_CANCELLED").unwrap())
            .unwrap();
        assert_eq!(failed.status_revision, 2);
        assert_eq!(failed.scan_state, ScanLifecycleState::Failed);
        assert_eq!(failed.last_finished_scan_id.as_deref(), Some("scan-a"));
        assert_eq!(
            failed.last_scan_error_code.as_deref(),
            Some("SCAN_CANCELLED")
        );

        start(&ledger, "scan-b", 30);
        let interrupted = ledger
            .mark_scan_failed(ScanFailedEvent::new("scan-b", 40, "SCAN_INTERRUPTED").unwrap())
            .unwrap();
        assert_eq!(interrupted.status_revision, 4);
        assert_eq!(interrupted.last_finished_scan_id.as_deref(), Some("scan-b"));
        assert_eq!(
            interrupted.last_scan_error_code.as_deref(),
            Some("SCAN_INTERRUPTED")
        );

        let old = ledger
            .scan_status_snapshot(Some("scan-a"))
            .unwrap()
            .target_scan
            .unwrap();
        assert_eq!(old.state, ScanRunState::Failed);
        assert_eq!(old.error_code.as_deref(), Some("SCAN_CANCELLED"));
    }

    #[test]
    fn start_failed_is_terminal_but_direct_start_clears_only_the_projection_slot() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-a", ScanTrigger::Manual, 20).unwrap(),
            )
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-a", 30).unwrap())
            .unwrap();
        let failed = ledger
            .mark_followup_start_failed(
                FollowupStartFailedEvent::new("followup-a", 40, "SCANNER_UNAVAILABLE").unwrap(),
            )
            .unwrap();
        assert_eq!(failed.status_revision, 4);
        assert_eq!(failed.followup_scan_id.as_deref(), Some("followup-a"));
        assert_eq!(failed.followup_state, Some(FollowupState::StartFailed));
        assert_eq!(
            failed.followup_error_code.as_deref(),
            Some("SCANNER_UNAVAILABLE")
        );

        let started = ledger
            .mark_scan_started(ScanStartEvent::new("scan-b", ScanTrigger::Startup, 50).unwrap())
            .unwrap();
        assert_eq!(started.status_revision, 5);
        assert!(started.followup_scan_id.is_none());
        assert!(started.followup_state.is_none());
        assert_eq!(
            ledger
                .scan_status_snapshot(Some("followup-a"))
                .unwrap()
                .target_scan
                .unwrap()
                .state,
            ScanRunState::StartFailed
        );
    }

    #[test]
    fn stale_target_cannot_complete_new_active_scan_and_failed_status_does_not_change_data_revision()
     {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);
        ledger
            .mark_scan_failed(ScanFailedEvent::new("scan-a", 20, "SCAN_INTERRUPTED").unwrap())
            .unwrap();
        // Failed lifecycle transitions are status-only as well.
        assert_eq!(ledger.app_state().unwrap().data_revision, 0);

        start(&ledger, "scan-b", 30);
        let stale = ledger.mark_scan_completed(ScanCompletedEvent::new("scan-a", 40).unwrap());
        assert!(stale.is_err());
        assert_eq!(ledger.app_state().unwrap().status_revision, 3);
        assert_eq!(
            ledger.app_state().unwrap().active_scan_id.as_deref(),
            Some("scan-b")
        );

        let unknown = ledger.scan_status_snapshot(Some("does-not-exist")).unwrap();
        assert!(unknown.target_scan.is_none());
        assert!(ledger.scan_status_snapshot(Some("")).is_err());
    }

    #[test]
    fn source_changed_does_not_gate_global_lifecycle_state() {
        let root = TempDir::new();
        let db = root.path().join("mu.sqlite3");
        let home_a = root.path().join("codex-a");
        let home_b = root.path().join("codex-b");
        let first = Ledger::open(LedgerOptions::new(&db, &home_a)).unwrap();
        let before = first.app_state().unwrap();
        drop(first);

        let changed = Ledger::open(LedgerOptions::new(&db, &home_b)).unwrap();
        let state = changed
            .mark_scan_started(ScanStartEvent::new("scan-a", ScanTrigger::Manual, 10).unwrap());
        assert_eq!(state.unwrap().active_scan_id.as_deref(), Some("scan-a"));
        assert_eq!(
            changed.app_state().unwrap().status_revision,
            before.status_revision + 2
        );
    }

    #[test]
    fn unsafe_error_code_is_rejected_before_any_database_write() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);
        let before = ledger.scan_status_snapshot(Some("scan-a")).unwrap();

        let error = ledger
            .mark_scan_failed(ScanFailedEvent {
                scan_id: "scan-a".to_owned(),
                failed_at_ms: 20,
                error_code: "private prompt sentinel".to_owned(),
            })
            .unwrap_err();
        assert_eq!(error.kind(), crate::storage::StorageErrorKind::InvalidState);
        assert_eq!(ledger.scan_status_snapshot(Some("scan-a")).unwrap(), before);
    }

    #[test]
    fn failed_cas_and_duplicate_scan_id_roll_back_row_and_projection_together() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-a", 10);
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-a", 20).unwrap())
            .unwrap();
        start(&ledger, "scan-b", 30);
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-b", 40).unwrap())
            .unwrap();

        let duplicate = ledger
            .mark_scan_started(ScanStartEvent::new("scan-a", ScanTrigger::Manual, 50).unwrap());
        assert!(duplicate.is_err());
        assert_eq!(ledger.app_state().unwrap().status_revision, 4);
        assert!(ledger.app_state().unwrap().active_scan_id.is_none());
        assert_eq!(
            ledger
                .scan_status_snapshot(Some("scan-a"))
                .unwrap()
                .target_scan
                .unwrap()
                .state,
            ScanRunState::Completed
        );

        start(&ledger, "scan-c", 60);
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-c", ScanTrigger::Manual, 70).unwrap(),
            )
            .unwrap();
        let wrong_id = ledger.mark_followup_start_failed(
            FollowupStartFailedEvent::new("other-followup", 80, "SCAN_START_FAILED").unwrap(),
        );
        assert!(wrong_id.is_err());
        let state = ledger.app_state().unwrap();
        assert_eq!(state.status_revision, 6);
        assert_eq!(state.followup_state, Some(FollowupState::Queued));
        assert_eq!(state.followup_scan_id.as_deref(), Some("followup-c"));
        assert_eq!(
            ledger
                .scan_status_snapshot(Some("followup-c"))
                .unwrap()
                .target_scan
                .unwrap()
                .state,
            ScanRunState::Queued
        );
    }

    #[test]
    fn codex_status_snapshot_on_empty_ledger() {
        let (_root, ledger) = ledger();
        let snapshot = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(snapshot.data_revision, 0);
        assert_eq!(snapshot.status_revision, 0);
        assert_eq!(snapshot.scan_state, "idle");
        assert_eq!(snapshot.source_binding_status, "ready");
        assert_eq!(snapshot.last_finished_scan_result, None);
        assert_eq!(snapshot.last_scan_started_at_ms, None);
        assert_eq!(snapshot.last_scan_completed_at_ms, None);
        assert_eq!(snapshot.last_scan_failed_at_ms, None);
        assert_eq!(snapshot.last_scan_error_code, None);
    }

    #[test]
    fn codex_status_snapshot_prev11_fallback_running_completed_and_failed() {
        let (_root, ledger) = ledger();

        // 1. Pre-v11 scan running with 0 child manifest
        start(&ledger, "prev11-a", 100);
        let running = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(running.scan_state, "running");
        assert_eq!(running.last_scan_started_at_ms, Some(100));
        assert_eq!(running.last_finished_scan_result, None);

        // 2. Pre-v11 scan completed
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("prev11-a", 150).unwrap())
            .unwrap();
        let completed = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(completed.scan_state, "idle");
        assert_eq!(
            completed.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        assert_eq!(completed.last_scan_started_at_ms, Some(100));
        assert_eq!(completed.last_scan_completed_at_ms, Some(150));
        assert_eq!(completed.last_scan_failed_at_ms, None);
        assert_eq!(completed.last_scan_error_code, None);

        // 3. Pre-v11 scan failed
        start(&ledger, "prev11-b", 200);
        ledger
            .mark_scan_failed(ScanFailedEvent::new("prev11-b", 250, "SCAN_INTERRUPTED").unwrap())
            .unwrap();
        let failed = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(failed.scan_state, "failed");
        assert_eq!(failed.last_finished_scan_result.as_deref(), Some("failed"));
        assert_eq!(failed.last_scan_started_at_ms, Some(200));
        assert_eq!(failed.last_scan_completed_at_ms, Some(150));
        assert_eq!(failed.last_scan_failed_at_ms, Some(250));
        assert_eq!(
            failed.last_scan_error_code.as_deref(),
            Some("SCAN_INTERRUPTED")
        );
    }

    #[test]
    fn codex_status_snapshot_prev11_start_failed_excluded_from_fallback() {
        let (_root, ledger) = ledger();
        start(&ledger, "scan-1", 100);
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-1", ScanTrigger::Manual, 110).unwrap(),
            )
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-1", 120).unwrap())
            .unwrap();
        ledger
            .mark_followup_start_failed(
                FollowupStartFailedEvent::new("followup-1", 130, "SCAN_START_FAILED").unwrap(),
            )
            .unwrap();

        let snapshot = ledger.codex_scan_status_snapshot().unwrap();
        // The start_failed followup has 0 child rows and state='start_failed'.
        // It must NOT enter pre-v11 fallback. The last terminal scan is scan-1 (completed).
        assert_eq!(snapshot.scan_state, "idle");
        assert_eq!(
            snapshot.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        assert_eq!(snapshot.last_scan_completed_at_ms, Some(120));
        assert_eq!(snapshot.last_scan_failed_at_ms, None);
        assert_eq!(snapshot.last_scan_error_code, None);
    }

    #[test]
    fn codex_status_snapshot_v11_multisource_isolation_and_early_completion() {
        let (_root, ledger) = ledger();
        let sources = vec!["codex".to_owned(), "fake".to_owned()];

        // 1. Direct start with codex and fake: both queued initially
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("v11-multi", ScanTrigger::Manual, 100).unwrap(),
                &sources,
            )
            .unwrap();
        let queued = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(queued.scan_state, "running");
        assert_eq!(queued.last_scan_started_at_ms, Some(100));

        // 2. Start codex child
        ledger
            .mark_source_scan_started("v11-multi", "codex", 110)
            .unwrap();
        let codex_running = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(codex_running.scan_state, "running");

        // 3. Codex child completes early while fake is not yet finished
        ledger
            .mark_source_scan_completed("v11-multi", "codex", 120)
            .unwrap();
        let codex_done = ledger.codex_scan_status_snapshot().unwrap();
        // Since codex child finished and completed, codex scan_state is idle
        assert_eq!(codex_done.scan_state, "idle");
        assert_eq!(
            codex_done.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        assert_eq!(codex_done.last_scan_completed_at_ms, Some(120));
        assert_eq!(codex_done.last_scan_failed_at_ms, None);
        assert_eq!(codex_done.last_scan_error_code, None);

        // 4. Fake source starts and fails at 130; global scan fails with SOURCE_RUN_FAILED
        ledger
            .mark_source_scan_started("v11-multi", "fake", 125)
            .unwrap();
        ledger
            .mark_source_scan_failed("v11-multi", "fake", 130, "FAKE_SOURCE_ERR")
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("v11-multi", 135).unwrap())
            .unwrap();

        // Codex perspective must remain completed at T=120 and idle, NOT polluted by fake failure (§12.2.4)
        let codex_final = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(codex_final.scan_state, "idle");
        assert_eq!(
            codex_final.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        assert_eq!(codex_final.last_scan_completed_at_ms, Some(120));
        assert_eq!(codex_final.last_scan_failed_at_ms, None);
        assert_eq!(codex_final.last_scan_error_code, None);
    }

    #[test]
    fn codex_status_snapshot_v11_scan_without_codex_does_not_affect_codex_status() {
        let (_root, ledger) = ledger();
        let codex_only = vec!["codex".to_owned()];
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("scan-codex", ScanTrigger::Manual, 100).unwrap(),
                &codex_only,
            )
            .unwrap();
        ledger
            .mark_source_scan_started("scan-codex", "codex", 110)
            .unwrap();
        ledger
            .mark_source_scan_completed("scan-codex", "codex", 120)
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-codex", 125).unwrap())
            .unwrap();

        // Now start another scan that ONLY includes fake (no codex)
        let fake_only = vec!["fake".to_owned()];
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("scan-fake", ScanTrigger::Scheduled, 200).unwrap(),
                &fake_only,
            )
            .unwrap();

        // While scan-fake is active, Codex is not in it -> scan_state remains idle!
        let snapshot = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(snapshot.scan_state, "idle");
        assert_eq!(
            snapshot.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        assert_eq!(snapshot.last_scan_completed_at_ms, Some(120));
        // scan-fake started at 200 does NOT affect Codex started_at_ms
        assert_eq!(snapshot.last_scan_started_at_ms, Some(100));

        // scan-fake fails
        ledger
            .mark_source_scan_started("scan-fake", "fake", 210)
            .unwrap();
        ledger
            .mark_source_scan_failed("scan-fake", "fake", 220, "CRASH")
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-fake", 225).unwrap())
            .unwrap();

        // Codex remains completed and unaffected
        let after_fake = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(after_fake.scan_state, "idle");
        assert_eq!(
            after_fake.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        assert_eq!(after_fake.last_scan_completed_at_ms, Some(120));
        assert_eq!(after_fake.last_scan_error_code, None);
    }

    #[test]
    fn codex_status_snapshot_codex_skipped_is_invariant_violation() {
        let (root, ledger) = ledger();
        let sources = vec!["codex".to_owned()];
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("scan-skip", ScanTrigger::Manual, 100).unwrap(),
                &sources,
            )
            .unwrap();

        // Force a skipped state into source_scan_runs
        let db_path = root.path().join("mu.sqlite3");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE source_scan_runs SET state='skipped', finished_at_ms=110 WHERE scan_id='scan-skip' AND source='codex'",
            [],
        )
        .unwrap();

        let result = ledger.codex_scan_status_snapshot();
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            crate::storage::StorageErrorKind::InvalidState
        );
    }

    #[test]
    fn codex_status_snapshot_clears_error_code_on_subsequent_success() {
        let (_root, ledger) = ledger();
        let sources = vec!["codex".to_owned()];

        // Scan 1 fails
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("scan-1", ScanTrigger::Manual, 100).unwrap(),
                &sources,
            )
            .unwrap();
        ledger
            .mark_source_scan_started("scan-1", "codex", 105)
            .unwrap();
        ledger
            .mark_source_scan_failed("scan-1", "codex", 110, "CODEX_ERR")
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-1", 115).unwrap())
            .unwrap();

        let failed_snapshot = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(failed_snapshot.scan_state, "failed");
        assert_eq!(
            failed_snapshot.last_finished_scan_result.as_deref(),
            Some("failed")
        );
        assert_eq!(
            failed_snapshot.last_scan_error_code.as_deref(),
            Some("CODEX_ERR")
        );
        assert_eq!(failed_snapshot.last_scan_failed_at_ms, Some(110));

        // Scan 2 succeeds
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("scan-2", ScanTrigger::Manual, 200).unwrap(),
                &sources,
            )
            .unwrap();
        ledger
            .mark_source_scan_started("scan-2", "codex", 205)
            .unwrap();
        ledger
            .mark_source_scan_completed("scan-2", "codex", 210)
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("scan-2", 215).unwrap())
            .unwrap();

        let success_snapshot = ledger.codex_scan_status_snapshot().unwrap();
        assert_eq!(success_snapshot.scan_state, "idle");
        assert_eq!(
            success_snapshot.last_finished_scan_result.as_deref(),
            Some("completed")
        );
        // Error code MUST be cleared (None), preserving v10 semantics!
        assert_eq!(success_snapshot.last_scan_error_code, None);
        assert_eq!(success_snapshot.last_scan_failed_at_ms, Some(110));
        assert_eq!(success_snapshot.last_scan_completed_at_ms, Some(210));
        assert_eq!(success_snapshot.last_scan_started_at_ms, Some(200));
    }
}
