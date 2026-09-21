//! Focused Spec 01 Phase 3C lifecycle coverage.

use std::{
    fs,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use usagi::{
    domain::{
        FollowupStartedEvent, ReserveScanFollowupEvent, ScanCompletedEvent, ScanFailedEvent,
        ScanLifecycleState, ScanStartEvent, ScanTrigger,
    },
    ingestion::{IngestionConfig, IngestionCoordinator},
    source::{
        AdapterAvailability, SourceAdapter, SourceAdapterError, SourceDescriptor, SourceId,
        SourceRegistry, SourceRunContext, SourceRunResult,
    },
    storage::{Ledger, LedgerOptions},
};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("usagi-phase3c-{label}-{stamp}"));
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
    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db)).expect("open ledger"));
    (root, db, ledger)
}

fn source_names(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

type ChildRow = (String, String, Option<i64>, Option<i64>, Option<String>);

fn child_rows(db: &Path, scan_id: &str) -> Vec<ChildRow> {
    let connection = Connection::open(db).expect("open lifecycle database");
    let mut statement = connection
        .prepare(
            "SELECT source,state,started_at_ms,finished_at_ms,error_code
             FROM source_scan_runs WHERE scan_id=?1 ORDER BY source",
        )
        .expect("prepare child query");
    statement
        .query_map([scan_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("read child rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect child rows")
}

fn scan_run_state(db: &Path, scan_id: &str) -> Option<String> {
    let connection = Connection::open(db).expect("open lifecycle database");
    connection
        .query_row(
            "SELECT state FROM scan_runs WHERE scan_id=?1",
            [scan_id],
            |row| row.get(0),
        )
        .optional()
        .expect("read parent scan row")
}

fn app_scan_projection(db: &Path) -> (String, Option<String>) {
    let connection = Connection::open(db).expect("open lifecycle database");
    connection
        .query_row(
            "SELECT scan_state,active_scan_id FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read app scan projection")
}

fn install_manifest_abort_trigger(db: &Path, trigger_name: &str) {
    let connection = Connection::open(db).expect("open lifecycle database");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER {trigger_name}
             BEFORE INSERT ON source_scan_runs
             BEGIN
                 SELECT RAISE(ABORT, 'phase3c start transaction failure');
             END;"
        ))
        .expect("install start transaction trigger");
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

type ProbeRun =
    dyn Fn(&SourceRunContext, &std::sync::atomic::AtomicBool) -> SourceRunResult + Send + Sync;

struct ProbeAdapter {
    descriptor: SourceDescriptor,
    availability: AdapterAvailability,
    run: Arc<ProbeRun>,
}

impl ProbeAdapter {
    fn new(
        id: &'static str,
        availability: AdapterAvailability,
        run: impl Fn(&SourceRunContext, &std::sync::atomic::AtomicBool) -> SourceRunResult
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            descriptor: SourceDescriptor::new(SourceId::new(id).expect("valid source id"), id),
            availability,
            run: Arc::new(run),
        }
    }
}

impl SourceAdapter for ProbeAdapter {
    fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        Ok(self.availability.clone())
    }

    fn run_scan(
        &self,
        context: &SourceRunContext,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> SourceRunResult {
        (self.run)(context, cancellation)
    }
}

fn quick_registry(ids: &[&'static str]) -> SourceRegistry {
    let mut registry = SourceRegistry::new();
    for id in ids {
        registry
            .register(ProbeAdapter::new(
                id,
                AdapterAvailability::Available,
                |_, _| Ok(()),
            ))
            .expect("register probe");
    }
    registry
}

#[test]
fn c05_shutdown_terminalizes_queued_and_running_children_as_cancelled() {
    let (_root, db, ledger) = ledger_fixture("cancel");
    let cancellation_seen = Arc::new(AtomicBool::new(false));
    let cancellation_seen_clone = Arc::clone(&cancellation_seen);
    let mut registry = SourceRegistry::new();
    registry
        .register(ProbeAdapter::new(
            "alpha",
            AdapterAvailability::Available,
            move |_, cancellation| {
                while !cancellation.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(2));
                }
                cancellation_seen_clone.store(true, Ordering::Release);
                Ok(())
            },
        ))
        .unwrap();
    registry
        .register(ProbeAdapter::new(
            "beta",
            AdapterAvailability::Available,
            |_, _| Ok(()),
        ))
        .unwrap();

    let handle = IngestionCoordinator::start(
        IngestionConfig::default().with_interval(Duration::from_secs(300)),
        Arc::clone(&ledger),
        registry,
    )
    .expect("start coordinator");
    wait_until(|| {
        child_rows(
            &db,
            &ledger
                .app_state()
                .unwrap()
                .active_scan_id
                .clone()
                .unwrap_or_default(),
        )
        .iter()
        .any(|(_, state, _, _, _)| state == "running")
    });
    let scan_id = ledger.app_state().unwrap().active_scan_id.clone().unwrap();
    handle.shutdown().expect("shutdown coordinator");

    assert!(cancellation_seen.load(Ordering::Acquire));
    let rows = child_rows(&db, &scan_id);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|(_, state, _, _, error)| {
        state == "failed" && error.as_deref() == Some("SCAN_CANCELLED")
    }));
}

#[test]
fn c06_recovery_terminalizes_only_unfinished_children_as_interrupted() {
    let (root, db, ledger) = ledger_fixture("recovery");
    let scan_id = "old-active";
    let sources = source_names(&["alpha", "beta", "gamma"]);
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new(scan_id, ScanTrigger::Startup, 1).unwrap(),
            &sources,
        )
        .unwrap();
    ledger
        .mark_source_scan_started(scan_id, "alpha", 2)
        .unwrap();
    ledger
        .mark_source_scan_started(scan_id, "gamma", 3)
        .unwrap();
    ledger
        .mark_source_scan_completed(scan_id, "gamma", 4)
        .unwrap();
    drop(ledger);

    let home = root.path().join("codex");
    let reopened = Arc::new(Ledger::open(LedgerOptions::new(&db)).unwrap());
    let handle = IngestionCoordinator::start(
        IngestionConfig::default().with_interval(Duration::from_secs(300)),
        Arc::clone(&reopened),
        quick_registry(&["alpha", "beta", "gamma"]),
    )
    .unwrap();
    wait_until(|| {
        child_rows(&db, scan_id)
            .iter()
            .all(|(_, state, _, _, _)| state != "queued" && state != "running")
    });
    handle.shutdown().unwrap();

    let rows = child_rows(&db, scan_id);
    assert_eq!(rows.len(), 3);
    for (source, state, _, _, error) in rows {
        if source == "gamma" {
            assert_eq!(state, "completed");
            assert_eq!(error, None);
        } else {
            assert_eq!(state, "failed");
            assert_eq!(error.as_deref(), Some("SCAN_INTERRUPTED"));
        }
    }
}

#[test]
fn c09_start_transactions_have_no_manifest_crash_window() {
    let sources = source_names(&["alpha", "beta"]);

    // A direct start failure in the manifest INSERT rolls back the parent
    // INSERT and app_meta projection as one transaction.
    {
        let (_root, db, ledger) = ledger_fixture("manifest-direct-before-commit");
        install_manifest_abort_trigger(&db, "phase3c_direct_manifest_abort");
        assert!(
            ledger
                .mark_scan_started_with_sources(
                    ScanStartEvent::new("direct-before", ScanTrigger::Manual, 10).unwrap(),
                    &sources,
                )
                .is_err()
        );
        assert_eq!(scan_run_state(&db, "direct-before"), None);
        assert!(child_rows(&db, "direct-before").is_empty());
        assert_eq!(app_scan_projection(&db), ("idle".to_owned(), None));
    }

    // A follow-up start failure leaves the already queued historical row in
    // place, but does not expose a running parent or a partial child manifest.
    {
        let (_root, db, ledger) = ledger_fixture("manifest-followup-before-commit");
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("base", ScanTrigger::Manual, 20).unwrap(),
                &[],
            )
            .unwrap();
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-before", ScanTrigger::Manual, 21).unwrap(),
            )
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("base", 22).unwrap())
            .unwrap();
        install_manifest_abort_trigger(&db, "phase3c_followup_manifest_abort");
        assert!(
            ledger
                .mark_followup_started_with_sources(
                    FollowupStartedEvent::new("followup-before", 23).unwrap(),
                    &sources,
                )
                .is_err()
        );
        assert_eq!(
            scan_run_state(&db, "followup-before").as_deref(),
            Some("queued")
        );
        assert!(child_rows(&db, "followup-before").is_empty());
        assert_eq!(app_scan_projection(&db), ("idle".to_owned(), None));
    }

    // A panic immediately after a successful start call models a process
    // crash after commit.  Reopening the raw database must still reveal the
    // running parent and every queued source in the frozen snapshot.
    {
        let (_root, db, ledger) = ledger_fixture("manifest-direct-after-commit");
        let crashed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            ledger
                .mark_scan_started_with_sources(
                    ScanStartEvent::new("direct-after", ScanTrigger::Manual, 30).unwrap(),
                    &sources,
                )
                .unwrap();
            panic!("simulated process crash after direct start commit");
        }));
        assert!(
            crashed.is_err(),
            "post-commit crash simulation did not fire"
        );
        drop(ledger);
        assert_eq!(
            app_scan_projection(&db),
            ("running".to_owned(), Some("direct-after".to_owned()))
        );
        let rows = child_rows(&db, "direct-after");
        assert_eq!(rows.len(), sources.len());
        assert!(rows.iter().all(|(_, state, started, finished, error)| {
            state == "queued" && started.is_none() && finished.is_none() && error.is_none()
        }));
    }

    // The same post-commit crash contract applies when consuming a queued
    // follow-up: the queued row becomes running together with its full child
    // manifest, never in two visible phases.
    {
        let (_root, db, ledger) = ledger_fixture("manifest-followup-after-commit");
        ledger
            .mark_scan_started_with_sources(
                ScanStartEvent::new("base-after", ScanTrigger::Manual, 40).unwrap(),
                &[],
            )
            .unwrap();
        ledger
            .reserve_scan_followup(
                ReserveScanFollowupEvent::new("followup-after", ScanTrigger::Manual, 41).unwrap(),
            )
            .unwrap();
        ledger
            .mark_scan_completed(ScanCompletedEvent::new("base-after", 42).unwrap())
            .unwrap();
        let crashed = std::panic::catch_unwind(AssertUnwindSafe(|| {
            ledger
                .mark_followup_started_with_sources(
                    FollowupStartedEvent::new("followup-after", 43).unwrap(),
                    &sources,
                )
                .unwrap();
            panic!("simulated process crash after follow-up start commit");
        }));
        assert!(
            crashed.is_err(),
            "post-commit crash simulation did not fire"
        );
        drop(ledger);
        assert_eq!(
            app_scan_projection(&db),
            ("running".to_owned(), Some("followup-after".to_owned()))
        );
        let rows = child_rows(&db, "followup-after");
        assert_eq!(rows.len(), sources.len());
        assert!(rows.iter().all(|(_, state, started, finished, error)| {
            state == "queued" && started.is_none() && finished.is_none() && error.is_none()
        }));
    }
}

#[test]
fn c10_child_transitions_bump_and_publish_status_revision() {
    let (_root, _db, ledger) = ledger_fixture("revision");
    let sources = source_names(&["alpha", "beta"]);
    ledger
        .mark_scan_started_with_sources(
            ScanStartEvent::new("scan", ScanTrigger::Manual, 10).unwrap(),
            &sources,
        )
        .unwrap();
    let mut revisions = ledger.subscribe_revisions();
    let first = ledger.app_state().unwrap().status_revision;
    let started = ledger
        .mark_source_scan_started("scan", "alpha", 11)
        .unwrap();
    assert_eq!(started.status_revision, first + 1);
    assert!(revisions.has_changed().unwrap());
    assert_eq!(revisions.borrow_and_update().status_revision, first + 1);
    assert_eq!(ledger.current_revision().status_revision, first + 1);
    let completed = ledger
        .mark_source_scan_completed("scan", "alpha", 12)
        .unwrap();
    assert_eq!(completed.status_revision, first + 2);
    assert!(revisions.has_changed().unwrap());
    assert_eq!(revisions.borrow_and_update().status_revision, first + 2);
    assert_eq!(ledger.current_revision().status_revision, first + 2);
    ledger
        .mark_scan_failed(ScanFailedEvent::new("scan", 13, "SCAN_CANCELLED").unwrap())
        .unwrap();
}

#[test]
fn c10_codex_completion_is_observable_while_fake_child_remains_running() {
    let (_root, db, ledger) = ledger_fixture("revision-coordinator");
    let fake_started = Arc::new(AtomicBool::new(false));
    let fake_release = Arc::new(AtomicBool::new(false));
    let fake_started_clone = Arc::clone(&fake_started);
    let fake_release_clone = Arc::clone(&fake_release);

    let mut registry = SourceRegistry::new();
    registry
        .register(ProbeAdapter::new(
            "codex",
            AdapterAvailability::Available,
            |_, _| Ok(()),
        ))
        .unwrap();
    registry
        .register(ProbeAdapter::new(
            "fake",
            AdapterAvailability::Available,
            move |_, cancellation| {
                fake_started_clone.store(true, Ordering::Release);
                while !fake_release_clone.load(Ordering::Acquire)
                    && !cancellation.load(Ordering::Acquire)
                {
                    thread::sleep(Duration::from_millis(2));
                }
                Ok(())
            },
        ))
        .unwrap();

    let mut revisions = ledger.subscribe_revisions();
    let initial_revision = ledger.app_state().unwrap().status_revision;
    let handle = IngestionCoordinator::start(
        IngestionConfig::default().with_interval(Duration::from_secs(300)),
        Arc::clone(&ledger),
        registry,
    )
    .expect("start coordinator");

    wait_until(|| {
        if !fake_started.load(Ordering::Acquire) {
            return false;
        }
        let rows = child_rows(
            &db,
            ledger
                .app_state()
                .unwrap()
                .active_scan_id
                .as_deref()
                .unwrap(),
        );
        rows.iter()
            .any(|(source, state, _, _, _)| source == "codex" && state == "completed")
            && rows
                .iter()
                .any(|(source, state, _, _, _)| source == "fake" && state == "running")
    });

    let scan_id = ledger.app_state().unwrap().active_scan_id.clone().unwrap();
    let rows = child_rows(&db, &scan_id);
    assert!(
        rows.iter()
            .any(|(source, state, _, _, _)| { source == "codex" && state == "completed" })
    );
    assert!(
        rows.iter()
            .any(|(source, state, _, _, _)| { source == "fake" && state == "running" })
    );
    let app = ledger.app_state().unwrap();
    assert_eq!(app.scan.scan_state, ScanLifecycleState::Running);
    assert!(app.status_revision >= initial_revision + 4);
    assert!(revisions.has_changed().unwrap());
    assert_eq!(
        revisions.borrow_and_update().status_revision,
        app.status_revision
    );
    assert_eq!(
        ledger.current_revision().status_revision,
        app.status_revision
    );

    fake_release.store(true, Ordering::Release);
    wait_until(|| ledger.app_state().unwrap().scan.scan_state == ScanLifecycleState::Idle);
    handle.shutdown().expect("shutdown coordinator");
}

#[test]
fn c12_source_scan_runs_constraints_reject_invalid_shapes_and_accept_valid_rows() {
    let (_root, db, _ledger) = ledger_fixture("constraints");
    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "INSERT INTO scan_runs(
                scan_id,trigger,request_kind,state,requested_at_ms,enqueued_status_revision
             ) VALUES('constraint-parent','Manual','followup','queued',0,0)",
            [],
        )
        .unwrap();

    let mut id = 0_i64;
    let mut rejected =
        |state: &str, started: Option<i64>, finished: Option<i64>, error: Option<&str>| {
            id += 1;
            let result = connection.execute(
                "INSERT INTO source_scan_runs(
                scan_id,source,state,started_at_ms,finished_at_ms,error_code
             ) VALUES('constraint-parent',?1,?2,?3,?4,?5)",
                params![format!("invalid-{id}"), state, started, finished, error],
            );
            assert!(result.is_err(), "invalid child shape was accepted: {state}");
        };
    rejected("queued", Some(1), None, None);
    rejected("queued", None, Some(1), None);
    rejected("running", None, None, None);
    rejected("running", Some(1), Some(2), None);
    rejected("completed", None, Some(2), None);
    rejected("completed", Some(1), None, None);
    rejected("skipped", Some(1), Some(2), None);
    rejected("skipped", None, None, None);
    rejected("failed", Some(1), None, Some("FAILED"));
    rejected("failed", None, Some(2), None);
    rejected("queued", Some(-1), None, None);
    rejected("running", Some(-1), None, None);
    rejected("completed", Some(1), Some(-1), None);
    rejected("", None, None, None);

    let valid = [
        ("queued", None, None, None),
        ("running", Some(1), None, None),
        ("completed", Some(1), Some(2), None),
        ("skipped", None, Some(2), None),
        ("failed", None, Some(2), Some("SOURCE_RUN_FAILED")),
    ];
    for (index, (state, started, finished, error)) in valid.into_iter().enumerate() {
        connection
            .execute(
                "INSERT INTO source_scan_runs(
                    scan_id,source,state,started_at_ms,finished_at_ms,error_code
                 ) VALUES('constraint-parent',?1,?2,?3,?4,?5)",
                params![format!("valid-{index}"), state, started, finished, error],
            )
            .expect("valid child shape rejected");
    }
}
