//! Spec 01 Phase 4: Codex-visible status read model and Public API v1 projection tests (§12.2).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use usagi::{
    domain::{
        FollowupStartFailedEvent, ReserveScanFollowupEvent, ScanCompletedEvent, ScanFailedEvent,
        ScanStartEvent, ScanTrigger,
    },
    storage::{CodexScanStatusSnapshot, Ledger, LedgerOptions, StorageErrorKind},
};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("usagi-phase4-{label}-{stamp}"));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ledger_fixture(label: &str) -> (TempRoot, PathBuf, Arc<Ledger>) {
    let root = TempRoot::new(label);
    let home = root.path().join("codex");
    fs::create_dir_all(&home).expect("create Codex home");
    let db = root.path().join("mu.sqlite3");
    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db, &home)).expect("open ledger"));
    (root, db, ledger)
}

#[test]
fn p4_01_empty_database_defaults() {
    let (_root, _db, ledger) = ledger_fixture("empty");
    let snapshot: CodexScanStatusSnapshot = ledger.codex_scan_status_snapshot().unwrap();
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
fn p4_02_prev11_fallback_running_completed_and_failed() {
    let (_root, _db, ledger) = ledger_fixture("prev11-fallback");

    // 1. Pre-v11 row running (0 children in source_scan_runs)
    ledger
        .mark_scan_started(ScanStartEvent::new("prev11-1", ScanTrigger::Manual, 100).unwrap())
        .unwrap();
    let running = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(running.scan_state, "running");
    assert_eq!(running.last_scan_started_at_ms, Some(100));
    assert_eq!(running.last_finished_scan_result, None);

    // 2. Pre-v11 row completed
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("prev11-1", 150).unwrap())
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

    // 3. Pre-v11 row failed
    ledger
        .mark_scan_started(ScanStartEvent::new("prev11-2", ScanTrigger::Manual, 200).unwrap())
        .unwrap();
    ledger
        .mark_scan_failed(ScanFailedEvent::new("prev11-2", 250, "SCAN_INTERRUPTED").unwrap())
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
fn p4_03_zero_child_queued_and_start_failed_never_enter_prev11_fallback() {
    let (_root, _db, ledger) = ledger_fixture("queued-start-failed");

    // Start direct scan, queue followup, complete direct scan, fail followup start
    ledger
        .mark_scan_started(ScanStartEvent::new("parent-1", ScanTrigger::Manual, 100).unwrap())
        .unwrap();
    ledger
        .reserve_scan_followup(
            ReserveScanFollowupEvent::new("followup-1", ScanTrigger::Scheduled, 110).unwrap(),
        )
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("parent-1", 120).unwrap())
        .unwrap();
    ledger
        .mark_followup_start_failed(
            FollowupStartFailedEvent::new("followup-1", 130, "SCAN_START_FAILED").unwrap(),
        )
        .unwrap();

    let snapshot = ledger.codex_scan_status_snapshot().unwrap();
    // followup-1 is state='start_failed' with 0 child rows.
    // Hard constraint (§12.2.1): zero child + parent queued/start_failed NEVER enters pre-v11 fallback.
    // Therefore the last finished terminal run remains parent-1 (completed), NOT followup-1.
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
    let (_root, _db, ledger) = ledger_fixture("multi-source-example");
    let sources = vec!["codex".to_owned(), "antigravity".to_owned()];

    // Global refresh started at T=100
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("global-scan-1", ScanTrigger::Manual, 100).unwrap(),
            &sources,
        )
        .unwrap();

    // 1. While queued, Public v1 sees scan_state = "running" and started_at = 100 (§12.2.3)
    let queued = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(queued.scan_state, "running");
    assert_eq!(queued.last_scan_started_at_ms, Some(100));

    // 2. Both children transition to running
    ledger
        .mark_source_scan_started("global-scan-1", "codex", 105)
        .unwrap();
    ledger
        .mark_source_scan_started("global-scan-1", "antigravity", 106)
        .unwrap();
    let running = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(running.scan_state, "running");

    // 3. Codex completes at T1 = 110, while antigravity is still running (§12.2.4)
    ledger
        .mark_source_scan_completed("global-scan-1", "codex", 110)
        .unwrap();
    let codex_done = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(codex_done.scan_state, "idle");
    assert_eq!(
        codex_done.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(codex_done.last_scan_completed_at_ms, Some(110));
    assert_eq!(codex_done.last_scan_failed_at_ms, None);
    assert_eq!(codex_done.last_scan_error_code, None);

    // 4. Antigravity fails at T2 = 120 > T1
    ledger
        .mark_source_scan_failed("global-scan-1", "antigravity", 120, "ANTIGRAVITY_CRASH")
        .unwrap();
    // Global terminal aggregation marks global scan as failed / SOURCE_RUN_FAILED
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("global-scan-1", 125).unwrap())
        .unwrap();

    // Spec §12.2.4 Multi-Source contract:
    // Public v1 MUST see:
    // - scan_state = idle
    // - last_finished_scan_result = completed
    // - last_scan_completed_at_ms = T1 (110)
    // - last_scan_error_code = null
    // It must NOT project global failure or T2 to Codex!
    let final_status = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(final_status.scan_state, "idle");
    assert_eq!(
        final_status.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(final_status.last_scan_completed_at_ms, Some(110));
    assert_eq!(final_status.last_scan_failed_at_ms, None);
    assert_eq!(final_status.last_scan_error_code, None);
}

#[test]
fn p4_05_active_scan_without_codex_leaves_codex_status_idle() {
    let (_root, _db, ledger) = ledger_fixture("scan-without-codex");

    // Establish prior Codex completed scan
    let codex_sources = vec!["codex".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("codex-prior", ScanTrigger::Manual, 50).unwrap(),
            &codex_sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("codex-prior", "codex", 55)
        .unwrap();
    ledger
        .mark_source_scan_completed("codex-prior", "codex", 60)
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("codex-prior", 65).unwrap())
        .unwrap();

    // Now start an active scan with ONLY antigravity (no codex child)
    let non_codex = vec!["antigravity".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("other-scan", ScanTrigger::Scheduled, 100).unwrap(),
            &non_codex,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("other-scan", "antigravity", 105)
        .unwrap();

    // Global scan is actively running, but Codex is not part of it!
    // Codex-visible status MUST be idle, with last completed scan at T=60.
    let snapshot = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(snapshot.scan_state, "idle");
    assert_eq!(
        snapshot.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(snapshot.last_scan_started_at_ms, Some(50));
    assert_eq!(snapshot.last_scan_completed_at_ms, Some(60));

    // other-scan fails at T=110
    ledger
        .mark_source_scan_failed("other-scan", "antigravity", 110, "OTHER_FAILED")
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("other-scan", 115).unwrap())
        .unwrap();

    // Codex status remains unchanged and completed
    let after_other = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(after_other.scan_state, "idle");
    assert_eq!(
        after_other.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(after_other.last_scan_completed_at_ms, Some(60));
    assert_eq!(after_other.last_scan_error_code, None);
}

#[test]
fn p4_06_codex_skipped_is_invariant_violation() {
    let (root, _db, ledger) = ledger_fixture("skipped-invariant");
    let sources = vec!["codex".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("skipped-scan", ScanTrigger::Manual, 100).unwrap(),
            &sources,
        )
        .unwrap();

    // Inject skipped row into source_scan_runs
    let db_path = root.path().join("mu.sqlite3");
    let connection = Connection::open(&db_path).unwrap();
    connection
        .execute(
            "UPDATE source_scan_runs SET state='skipped', finished_at_ms=110
             WHERE scan_id='skipped-scan' AND source='codex'",
            [],
        )
        .unwrap();

    // Query MUST fail with InvalidState, not silently mapped (§12.2.2)
    let error = ledger.codex_scan_status_snapshot().unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::InvalidState);
}

#[test]
fn p4_07_deterministic_tie_break_and_error_code_cleared() {
    let (_root, _db, ledger) = ledger_fixture("tie-break");
    let sources = vec!["codex".to_owned()];

    // 1. Scan 1 fails at T=100 with ERROR_ONE
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("scan-1", ScanTrigger::Manual, 90).unwrap(),
            &sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("scan-1", "codex", 95)
        .unwrap();
    ledger
        .mark_source_scan_failed("scan-1", "codex", 100, "ERROR_ONE")
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("scan-1", 105).unwrap())
        .unwrap();

    let fail_snap = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(fail_snap.scan_state, "failed");
    assert_eq!(
        fail_snap.last_finished_scan_result.as_deref(),
        Some("failed")
    );
    assert_eq!(fail_snap.last_scan_error_code.as_deref(), Some("ERROR_ONE"));
    assert_eq!(fail_snap.last_scan_failed_at_ms, Some(100));

    // 2. Scan 2 succeeds at T=200
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("scan-2", ScanTrigger::Manual, 190).unwrap(),
            &sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started("scan-2", "codex", 195)
        .unwrap();
    ledger
        .mark_source_scan_completed("scan-2", "codex", 200)
        .unwrap();
    ledger
        .mark_scan_completed(ScanCompletedEvent::new("scan-2", 205).unwrap())
        .unwrap();

    let success_snap = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(success_snap.scan_state, "idle");
    assert_eq!(
        success_snap.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    // Success clears error_code (NULL), preserving v10 semantics (§12.2.3)
    assert_eq!(success_snap.last_scan_error_code, None);
    assert_eq!(success_snap.last_scan_failed_at_ms, Some(100));
    assert_eq!(success_snap.last_scan_completed_at_ms, Some(200));
    assert_eq!(success_snap.last_scan_started_at_ms, Some(190));
}

#[test]
fn q05_d_status_read_uses_one_sqlite_snapshot_for_revision_and_children() {
    let (_root, db, ledger) = ledger_fixture("torn-snapshot");
    let sources = vec!["codex".to_owned()];
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("torn-scan", ScanTrigger::Manual, 100).unwrap(),
            &sources,
        )
        .unwrap();

    let writer = thread::spawn(move || {
        let mut connection = Connection::open(db).unwrap();
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        for step in 0..64_i64 {
            let transaction = connection.transaction().unwrap();
            if step % 2 == 0 {
                transaction
                    .execute(
                        "UPDATE source_scan_runs
                         SET state='running', started_at_ms=?1
                         WHERE scan_id='torn-scan' AND source='codex'",
                        [200 + step],
                    )
                    .unwrap();
            } else {
                transaction
                    .execute(
                        "UPDATE source_scan_runs
                         SET state='queued', started_at_ms=NULL
                         WHERE scan_id='torn-scan' AND source='codex'",
                        [],
                    )
                    .unwrap();
            }
            transaction
                .execute(
                    "UPDATE app_meta SET status_revision=status_revision+1 WHERE id=1",
                    [],
                )
                .unwrap();
            transaction.commit().unwrap();
        }
    });

    for _ in 0..256 {
        let snapshot = ledger.scan_status_snapshot(None).unwrap();
        assert_eq!(snapshot.sources.len(), 1);
        let source = &snapshot.sources[0];
        assert_eq!(source.source, "codex");
        let expected_state = if snapshot.status_revision % 2 == 1 {
            "queued"
        } else {
            "running"
        };
        assert_eq!(source.state.as_str(), expected_state);
        assert_eq!(source.error_code, None);
    }
    writer.join().unwrap();
}

#[test]
fn p4_08_active_codex_completed_is_idle_even_after_historical_failure() {
    let (_root, _db, ledger) = ledger_fixture("active-completed-after-failure");
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

    let snapshot = ledger.codex_scan_status_snapshot().unwrap();
    assert_eq!(snapshot.scan_state, "idle");
    assert_eq!(
        snapshot.last_finished_scan_result.as_deref(),
        Some("completed")
    );
    assert_eq!(snapshot.last_scan_error_code, None);
}
