use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use rusqlite::Connection;

use crate::{
    domain::{
        FollowupStartFailedEvent, ReserveScanFollowupEvent, ScanCompletedEvent, ScanFailedEvent,
        ScanStartEvent, ScanTrigger,
    },
    storage::LedgerOptions,
};

struct TempDir(PathBuf);

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    fn new() -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "usagi-codex-status-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            sequence,
        ));
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

fn ledger() -> (TempDir, Ledger) {
    let root = TempDir::new();
    let ledger = Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"))).unwrap();
    (root, ledger)
}

fn start(ledger: &Ledger, id: &str, at: i64) {
    ledger
        .mark_scan_started(ScanStartEvent::new(id, ScanTrigger::Manual, at).unwrap())
        .unwrap();
}

#[test]
fn p4_01_empty_database_defaults() {
    let (_root, ledger) = ledger();
    let snapshot = snapshot(&ledger).unwrap();
    assert_eq!(snapshot.data_revision, 0);
    assert_eq!(snapshot.status_revision, 0);
    assert_eq!(snapshot.scan_state, "idle");
    assert_eq!(snapshot.last_finished_scan_result, None);
    assert_eq!(snapshot.last_scan_started_at_ms, None);
    assert_eq!(snapshot.last_scan_completed_at_ms, None);
    assert_eq!(snapshot.last_scan_failed_at_ms, None);
    assert_eq!(snapshot.last_scan_error_code, None);
}

#[test]
fn p4_02_prev11_fallback_running_completed_and_failed() {
    let (_root, ledger) = ledger();

    // 1. Pre-v11 scan running with 0 child manifest
    start(&ledger, "prev11-a", 100);
    let running = snapshot(&ledger).unwrap();
    assert_eq!(running.scan_state, "running");
    assert_eq!(running.last_scan_started_at_ms, Some(100));
    assert_eq!(running.last_finished_scan_result, None);

    // 2. Pre-v11 scan completed
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("prev11-a", 150).unwrap())
        .unwrap();
    let completed = snapshot(&ledger).unwrap();
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
    let failed = snapshot(&ledger).unwrap();
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
fn p4_03_zero_child_queued_and_start_failed_never_enter_prev11_fallback() {
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

    let snapshot = snapshot(&ledger).unwrap();
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
fn p4_04_v11_multisource_isolation_and_spec12_2_4_example() {
    let (_root, ledger) = ledger();
    let sources = vec!["codex".to_owned(), "fake".to_owned()];

    // 1. Direct start with codex and fake: both queued initially
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("v11-multi", ScanTrigger::Manual, 100).unwrap(),
            &sources,
        )
        .unwrap();
    let queued = snapshot(&ledger).unwrap();
    assert_eq!(queued.scan_state, "running");
    assert_eq!(queued.last_scan_started_at_ms, Some(100));

    // 2. Start codex child
    ledger
        .mark_source_scan_started("v11-multi", "codex", 110)
        .unwrap();
    let codex_running = snapshot(&ledger).unwrap();
    assert_eq!(codex_running.scan_state, "running");

    // 3. Codex child completes early while fake is not yet finished
    ledger
        .mark_source_scan_completed("v11-multi", "codex", 120)
        .unwrap();
    let codex_done = snapshot(&ledger).unwrap();
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
    let codex_final = snapshot(&ledger).unwrap();
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
fn p4_05_active_scan_without_codex_leaves_codex_status_idle() {
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
    let current = snapshot(&ledger).unwrap();
    assert_eq!(current.scan_state, "idle");
    assert_eq!(
        current.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(current.last_scan_completed_at_ms, Some(120));
    // scan-fake started at 200 does NOT affect Codex started_at_ms
    assert_eq!(current.last_scan_started_at_ms, Some(100));

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
    let after_fake = snapshot(&ledger).unwrap();
    assert_eq!(after_fake.scan_state, "idle");
    assert_eq!(
        after_fake.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(after_fake.last_scan_completed_at_ms, Some(120));
    assert_eq!(after_fake.last_scan_error_code, None);
}

#[test]
fn p4_06_codex_skipped_is_invariant_violation() {
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

    let result = snapshot(&ledger);
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

    let failed_snapshot = snapshot(&ledger).unwrap();
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

    let success_snapshot = snapshot(&ledger).unwrap();
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

#[test]
fn p4_07_deterministic_tie_break_and_error_code_cleared() {
    let (_root, ledger) = ledger();
    let sources = vec!["codex".to_owned()];

    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("scan-a", ScanTrigger::Manual, 90).unwrap(),
            &sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("scan-a", "codex", 95)
        .unwrap();
    ledger
        .mark_source_scan_completed("scan-a", "codex", 100)
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("scan-a", 105).unwrap())
        .unwrap();

    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("scan-z", ScanTrigger::Manual, 91).unwrap(),
            &sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("scan-z", "codex", 96)
        .unwrap();
    ledger
        .mark_source_scan_failed("scan-z", "codex", 100, "ERROR_Z")
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("scan-z", 105).unwrap())
        .unwrap();

    let status = snapshot(&ledger).unwrap();
    assert_eq!(status.scan_state, "failed");
    assert_eq!(status.last_finished_scan_result.as_deref(), Some("failed"));
    assert_eq!(status.last_scan_error_code.as_deref(), Some("ERROR_Z"));
    assert_eq!(status.last_scan_failed_at_ms, Some(100));
    assert_eq!(status.last_scan_completed_at_ms, Some(100));
    assert_eq!(status.last_scan_started_at_ms, Some(91));
    assert_eq!(snapshot(&ledger).unwrap(), status);
}

#[test]
fn q05_d_status_read_uses_one_sqlite_snapshot_for_revision_and_children() {
    let (_root, ledger) = ledger();
    let sources = vec!["codex".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("torn-scan", ScanTrigger::Manual, 100).unwrap(),
            &sources,
        )
        .unwrap();

    let base_revision = ledger.app_state().unwrap().scan.status_revision;
    let db_path = _root.path().join("mu.sqlite3");
    let writer = thread::spawn(move || {
        let mut connection = Connection::open(db_path).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        for step in 0..64_i64 {
            let transaction = connection.transaction().unwrap();
            if step % 2 == 0 {
                transaction
                    .execute(
                        "UPDATE source_scan_runs
                         SET state='running', started_at_ms=?1,
                             finished_at_ms=NULL, error_code=NULL
                         WHERE scan_id='torn-scan' AND source='codex'",
                        [200 + step],
                    )
                    .unwrap();
            } else {
                transaction
                    .execute(
                        "UPDATE source_scan_runs
                         SET state='failed', started_at_ms=?1,
                             finished_at_ms=?2, error_code='CODEX_FAILED'
                         WHERE scan_id='torn-scan' AND source='codex'",
                        rusqlite::params![200 + step, 300 + step],
                    )
                    .unwrap();
            }
            transaction
                .execute(
                    "UPDATE app_meta SET status_revision=?1 WHERE id=1",
                    [base_revision + step + 1],
                )
                .unwrap();
            transaction.commit().unwrap();
        }
    });

    for _ in 0..256 {
        let snapshot = snapshot(&ledger).unwrap();
        let delta = snapshot.status_revision - base_revision;
        assert!((0..=64).contains(&delta));
        let expected_failed = delta > 0 && delta % 2 == 0;
        if expected_failed {
            assert_eq!(snapshot.scan_state, "failed");
            assert_eq!(
                snapshot.last_finished_scan_result.as_deref(),
                Some("failed")
            );
            assert_eq!(
                snapshot.last_scan_error_code.as_deref(),
                Some("CODEX_FAILED")
            );
        } else {
            assert_eq!(snapshot.scan_state, "running");
            assert_eq!(snapshot.last_finished_scan_result, None);
            assert_eq!(snapshot.last_scan_error_code, None);
        }
    }
    writer.join().unwrap();
}

#[test]
fn p4_08_active_codex_completed_is_idle_even_after_historical_failure() {
    let (_root, ledger) = ledger();
    let codex_only = vec!["codex".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("failed-history", ScanTrigger::Manual, 100).unwrap(),
            &codex_only,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("failed-history", "codex", 101)
        .unwrap();
    ledger
        .mark_source_scan_failed("failed-history", "codex", 102, "CODEX_FAILED")
        .unwrap();
    ledger
        .mark_scan_failed(ScanFailedEvent::new("failed-history", 103, "SOURCE_RUN_FAILED").unwrap())
        .unwrap();

    let sources = vec!["codex".to_owned(), "fake".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("active-completed", ScanTrigger::Manual, 200).unwrap(),
            &sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("active-completed", "codex", 201)
        .unwrap();
    ledger
        .mark_source_scan_completed("active-completed", "codex", 202)
        .unwrap();

    let snapshot = snapshot(&ledger).unwrap();
    assert_eq!(snapshot.scan_state, "idle");
    assert_eq!(
        snapshot.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(snapshot.last_scan_error_code, None);
}
