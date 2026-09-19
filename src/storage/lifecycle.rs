//! Scan lifecycle persistence.
//!
//! The scanner owns scheduling and recovery.  This module only owns the
//! durable scan target rows, the one follow-up slot, and the `app_meta`
//! projection that describes the current status.  Every write below uses one
//! SQLite transaction for the target row and the projection, and returns the
//! projection read from that same transaction.

use crate::domain::{
    AppState, DomainError, FollowupStartFailedEvent, FollowupStartedEvent, FollowupState,
    ReserveScanFollowupEvent, ScanCompletedEvent, ScanFailedEvent, ScanLifecycleState,
    ScanRequestKind, ScanResult, ScanRun, ScanRunState, ScanStartEvent, ScanState,
    ScanStatusSnapshot, ScanTrigger,
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
        let snapshot = ScanStatusSnapshot::new(app_state, target_scan)
            .map_err(|error| StorageError::invalid_state(error.to_string()))?;
        transaction.commit()?;
        Ok(snapshot)
    }

    /// Start a new direct scan.  A prior `start_failed` follow-up is a
    /// terminal historical row; beginning a new direct scan clears only its
    /// app-meta slot projection.
    pub fn mark_scan_started(&self, event: ScanStartEvent) -> Result<ScanState> {
        event.validate().map_err(domain_storage_error)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_app_state(&transaction)?;
        require_ready(self, &transaction, &current)?;
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
        require_ready(self, &transaction, &current)?;
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
        event.validate().map_err(domain_storage_error)?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_app_state(&transaction)?;
        require_ready(self, &transaction, &current)?;
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
        let next_revision = increment_status_revision(current.status_revision)?;

        let changed = transaction.execute(
            "UPDATE scan_runs
             SET state = 'completed', finished_at_ms = ?1,
                 terminal_status_revision = ?2
             WHERE scan_id = ?3 AND state = 'running'",
            params![event.completed_at_ms, next_revision, event.scan_id],
        )?;
        if changed != 1 {
            return Err(StorageError::invalid_state(
                "active scan is no longer running".to_owned(),
            ));
        }
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
}

fn validate_id(value: &str, field: &'static str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(StorageError::invalid_state(format!(
            "invalid {field}: scan id must be non-empty and contain no control characters"
        )));
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

fn require_ready(ledger: &Ledger, transaction: &Transaction<'_>, state: &AppState) -> Result<()> {
    match state.source_binding_status {
        crate::domain::SourceBindingStatus::Ready => {
            let stored_fingerprint: Option<String> = transaction
                .query_row(
                    "SELECT codex_home_fingerprint FROM app_meta WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            match stored_fingerprint {
                Some(expected) if expected == ledger.codex_home_fingerprint => Ok(()),
                Some(expected) => Err(StorageError::source_changed(
                    expected,
                    ledger.codex_home_fingerprint.clone(),
                )),
                None => Err(StorageError::invalid_state(
                    "ready CODEX_HOME binding has no fingerprint",
                )),
            }
        }
        crate::domain::SourceBindingStatus::Unbound => Err(StorageError::source_unbound()),
        crate::domain::SourceBindingStatus::SourceChanged => {
            let stored_fingerprint: Option<String> = transaction
                .query_row(
                    "SELECT codex_home_fingerprint FROM app_meta WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            Err(StorageError::source_changed(
                stored_fingerprint.unwrap_or_default(),
                ledger.codex_home_fingerprint.clone(),
            ))
        }
    }
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
        assert_eq!(ledger.app_state().unwrap().data_revision, 1);

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
        assert_eq!(ledger.app_state().unwrap().data_revision, 1);

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
    fn source_changed_and_invalid_start_failure_do_not_write_lifecycle_state() {
        let root = TempDir::new();
        let db = root.path().join("mu.sqlite3");
        let home_a = root.path().join("codex-a");
        let home_b = root.path().join("codex-b");
        let first = Ledger::open(LedgerOptions::new(&db, &home_a)).unwrap();
        let before = first.app_state().unwrap();
        drop(first);

        let changed = Ledger::open(LedgerOptions::new(&db, &home_b)).unwrap();
        let error = changed
            .mark_scan_started(ScanStartEvent::new("scan-a", ScanTrigger::Manual, 10).unwrap());
        assert_eq!(
            error.unwrap_err().kind(),
            crate::storage::StorageErrorKind::SourceChanged
        );
        assert_eq!(
            changed.app_state().unwrap().status_revision,
            before.status_revision + 1
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
}
