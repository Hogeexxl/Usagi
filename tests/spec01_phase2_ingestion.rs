use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde_json::json;
use usagi::{
    domain::{
        FollowupStartedEvent, ReserveScanFollowupEvent, ScanFailedEvent, ScanStartEvent,
        ScanTrigger,
    },
    ingestion::{IngestionConfig, IngestionCoordinator, LegacyCodexSourceAdapter},
    scanner::CodexMetadata,
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
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-phase2-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
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
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::create_dir_all(home.join("archived_sessions")).unwrap();
    let ledger =
        Arc::new(Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"), &home)).unwrap());
    (root, home, ledger)
}

fn write_state_schema(home: &Path) -> Connection {
    let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT NOT NULL,
                rollout_path TEXT,
                created_at_ms INTEGER,
                updated_at_ms INTEGER,
                archived INTEGER,
                cwd TEXT,
                title TEXT,
                name TEXT,
                model TEXT,
                agent_role TEXT
            );
            CREATE TABLE thread_spawn_edges (
                parent_thread_id TEXT NOT NULL,
                child_thread_id TEXT NOT NULL,
                status TEXT,
                observed_at_ms INTEGER
            );",
        )
        .unwrap();
    connection
}

fn write_metadata_indexes(home: &Path, state_thread_id: &str, rollout_path: &Path) {
    let connection = write_state_schema(home);
    connection
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms,
                archived, cwd, title, name, model, agent_role
             ) VALUES (?1, ?2, 1700000000000, 1700000000100,
                       0, '/state/main', 'State title', NULL, 'state-model', 'main')",
            params![state_thread_id, rollout_path.to_str().unwrap()],
        )
        .unwrap();
    drop(connection);

    let session_index = json!({
        "id": state_thread_id,
        "thread_name": "Session title",
        "updated_at": "2026-08-08T01:02:05Z"
    });
    let mut bytes = serde_json::to_vec(&session_index).unwrap();
    bytes.push(b'\n');
    fs::write(home.join("session_index.jsonl"), bytes).unwrap();
    fs::write(home.join(".codex-global-state.json"), b"{}").unwrap();
}

fn touch_codex_metadata(home: &Path) {
    fs::write(home.join("state_5.sqlite"), b"").unwrap();
    fs::write(home.join("session_index.jsonl"), b"").unwrap();
    fs::write(home.join(".codex-global-state.json"), b"{}").unwrap();
}

fn metadata_fixture(
    label: &str,
    filename_thread_id: &str,
    state_thread_id: &str,
    rollout_bytes: &[u8],
) -> (TempRoot, PathBuf, Arc<Ledger>, PathBuf) {
    let root = TempRoot::new(label);
    let home = root.path().join("codex");
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(home.join("archived_sessions")).unwrap();
    let rollout_path = sessions.join(format!("rollout-{filename_thread_id}.jsonl"));
    fs::write(&rollout_path, rollout_bytes).unwrap();
    write_metadata_indexes(&home, state_thread_id, &rollout_path);
    let ledger =
        Arc::new(Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"), &home)).unwrap());
    (root, home, ledger, rollout_path)
}

fn wait_for_terminal(ledger: &Ledger) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = ledger.app_state().unwrap().scan;
        if state.active_scan_id.is_none() && state.last_finished_scan_id.is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for scan terminal state"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

struct ProbeAdapter {
    descriptor: SourceDescriptor,
    availability: AdapterAvailability,
    run: Arc<dyn Fn(&SourceRunContext) -> SourceRunResult + Send + Sync>,
}

impl ProbeAdapter {
    fn new(
        id: &'static str,
        availability: AdapterAvailability,
        run: impl Fn(&SourceRunContext) -> SourceRunResult + Send + Sync + 'static,
    ) -> Self {
        Self {
            descriptor: SourceDescriptor::new(SourceId::new(id).unwrap(), id),
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
        _cancellation: &std::sync::atomic::AtomicBool,
    ) -> SourceRunResult {
        (self.run)(context)
    }
}

fn register_probe(registry: &mut SourceRegistry, adapter: ProbeAdapter) {
    registry.register(adapter).unwrap();
}

#[test]
fn phase2_c01_duplicate_codex_registration_is_rejected_before_startup() {
    let mut registry = SourceRegistry::new();
    register_probe(
        &mut registry,
        ProbeAdapter::new("codex", AdapterAvailability::Available, |_| Ok(())),
    );
    assert!(
        registry
            .register(ProbeAdapter::new(
                "codex",
                AdapterAvailability::Available,
                |_| Ok(())
            ))
            .is_err()
    );
}

#[test]
fn phase2_c02_ingestion_runs_sources_in_stable_order() {
    let (_root, _home, ledger) = ledger_fixture("order");
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut registry = SourceRegistry::new();
    for id in ["zeta", "alpha"] {
        let order = Arc::clone(&order);
        register_probe(
            &mut registry,
            ProbeAdapter::new(id, AdapterAvailability::Available, move |_| {
                order.lock().unwrap().push(id.to_owned());
                Ok(())
            }),
        );
    }
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    assert_eq!(*order.lock().unwrap(), ["alpha", "zeta"]);
    scanner.shutdown().unwrap();
}

#[test]
fn phase2_c03_failure_does_not_cancel_later_source() {
    let (_root, _home, ledger) = ledger_fixture("continue");
    let ran_later = Arc::new(AtomicUsize::new(0));
    let committed = Arc::new(Mutex::new(Vec::new()));
    let mut registry = SourceRegistry::new();
    register_probe(
        &mut registry,
        ProbeAdapter::new("alpha", AdapterAvailability::Available, |_| {
            Err(SourceAdapterError::with_code(
                "ALPHA_FAILED",
                "alpha failed",
            ))
        }),
    );
    let ran_later_clone = Arc::clone(&ran_later);
    let committed_clone = Arc::clone(&committed);
    register_probe(
        &mut registry,
        ProbeAdapter::new("beta", AdapterAvailability::Available, move |_| {
            ran_later_clone.fetch_add(1, Ordering::Relaxed);
            committed_clone.lock().unwrap().push("beta-commit");
            Ok(())
        }),
    );
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    assert_eq!(ran_later.load(Ordering::Relaxed), 1);
    assert_eq!(*committed.lock().unwrap(), ["beta-commit"]);
    let state = ledger.app_state().unwrap().scan;
    assert_eq!(
        state.last_scan_error_code.as_deref(),
        Some("SOURCE_RUN_FAILED")
    );
    let reports = scanner.source_reports();
    assert!(reports.iter().any(|report| {
        report.source.as_str() == "alpha"
            && report.state.as_str() == "failed"
            && report.error_code.as_deref() == Some("ALPHA_FAILED")
    }));
    assert!(reports.iter().any(|report| {
        report.source.as_str() == "beta"
            && report.state.as_str() == "completed"
            && report.error_code.is_none()
    }));
    scanner.shutdown().unwrap();
}

#[test]
fn phase2_c04_unavailable_source_is_skipped() {
    let (_root, _home, ledger) = ledger_fixture("unavailable");
    let called = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let called_clone = Arc::clone(&called);
    let mut registry = SourceRegistry::new();
    register_probe(
        &mut registry,
        ProbeAdapter::new(
            "future",
            AdapterAvailability::Unavailable("not installed".to_owned()),
            move |_| {
                called_clone.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        ),
    );
    let completed_clone = Arc::clone(&completed);
    register_probe(
        &mut registry,
        ProbeAdapter::new("beta", AdapterAvailability::Available, move |_| {
            completed_clone.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
    );
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    assert_eq!(called.load(Ordering::Relaxed), 0);
    assert_eq!(completed.load(Ordering::Relaxed), 1);
    let reports = scanner.source_reports();
    assert!(reports.iter().any(|report| {
        report.source.as_str() == "future"
            && report.state.as_str() == "skipped"
            && report.error_code.is_none()
    }));
    assert!(reports.iter().any(|report| {
        report.source.as_str() == "beta"
            && report.state.as_str() == "completed"
            && report.error_code.is_none()
    }));
    assert_eq!(ledger.app_state().unwrap().scan.last_scan_error_code, None);
    scanner.shutdown().unwrap();
}

#[test]
fn phase2_c07_global_failure_code_is_stable_aggregate() {
    type SourceReport = (String, String, Option<String>, Option<String>);
    type AggregateRun = (Option<String>, Vec<&'static str>, Vec<SourceReport>);

    fn run_with_registration_order(registration_order: &[&str]) -> AggregateRun {
        let (_root, _home, ledger) = ledger_fixture("aggregate");
        let errors = Arc::new(Mutex::new(Vec::new()));
        let mut registry = SourceRegistry::new();
        for id in registration_order {
            let (id, code, detail) = match *id {
                "alpha" => ("alpha", "ALPHA_FAILED", "alpha adapter failed"),
                "beta" => ("beta", "BETA_FAILED", "beta adapter failed"),
                other => panic!("unexpected source in test order: {other}"),
            };
            let errors = Arc::clone(&errors);
            register_probe(
                &mut registry,
                ProbeAdapter::new(id, AdapterAvailability::Available, move |_| {
                    errors.lock().unwrap().push(code);
                    Err(SourceAdapterError::with_code(code, detail))
                }),
            );
        }
        let scanner =
            IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
                .unwrap();
        wait_for_terminal(&ledger);
        let global_error = ledger.app_state().unwrap().scan.last_scan_error_code;
        let reports = scanner
            .source_reports()
            .into_iter()
            .map(|report| {
                (
                    report.source.as_str().to_owned(),
                    report.state.as_str().to_owned(),
                    report.error_code,
                    report.detail,
                )
            })
            .collect();
        let errors = errors.lock().unwrap().clone();
        scanner.shutdown().unwrap();
        (global_error, errors, reports)
    }

    // SourceRegistry stores adapters by SourceId, so both opposite insertion
    // orders must execute in the same stable source order and retain each
    // adapter's own failure code and detail.
    let first = run_with_registration_order(&["alpha", "beta"]);
    let second = run_with_registration_order(&["beta", "alpha"]);

    for (global_error, errors, reports) in [&first, &second] {
        assert_eq!(global_error.as_deref(), Some("SOURCE_RUN_FAILED"));
        assert_eq!(*errors, ["ALPHA_FAILED", "BETA_FAILED"]);
        assert_eq!(
            reports,
            &vec![
                (
                    "alpha".to_owned(),
                    "failed".to_owned(),
                    Some("ALPHA_FAILED".to_owned()),
                    Some("alpha adapter failed".to_owned()),
                ),
                (
                    "beta".to_owned(),
                    "failed".to_owned(),
                    Some("BETA_FAILED".to_owned()),
                    Some("beta adapter failed".to_owned()),
                ),
            ]
        );
    }
    assert_eq!(first.1, second.1);
    assert_eq!(first.2, second.2);
}

#[test]
fn phase2_c08_source_change_does_not_gate_global_request_or_followup() {
    let (root, _home_a, _first_ledger) = ledger_fixture("source-change");
    let home_b = root.path().join("codex-b");
    fs::create_dir_all(home_b.join("sessions")).unwrap();
    fs::create_dir_all(home_b.join("archived_sessions")).unwrap();
    touch_codex_metadata(&home_b);
    let changed_ledger = Arc::new(
        Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"), &home_b)).unwrap(),
    );
    // The three global lifecycle seams remain source-neutral even while this
    // Ledger reports Codex SOURCE_CHANGED.
    let started = changed_ledger
        .mark_scan_started(ScanStartEvent::new("seam-a", ScanTrigger::Manual, 1).unwrap())
        .unwrap();
    assert_eq!(started.active_scan_id.as_deref(), Some("seam-a"));
    let reserved = changed_ledger
        .reserve_scan_followup(
            ReserveScanFollowupEvent::new("seam-b", ScanTrigger::Manual, 2).unwrap(),
        )
        .unwrap();
    assert_eq!(reserved.followup_scan_id.as_deref(), Some("seam-b"));
    changed_ledger
        .mark_scan_failed(ScanFailedEvent::new("seam-a", 3, "SOURCE_CHANGED").unwrap())
        .unwrap();
    let followup = changed_ledger
        .mark_followup_started(FollowupStartedEvent::new("seam-b", 4).unwrap())
        .unwrap();
    assert_eq!(followup.active_scan_id.as_deref(), Some("seam-b"));
    changed_ledger
        .mark_scan_failed(ScanFailedEvent::new("seam-b", 5, "SOURCE_CHANGED").unwrap())
        .unwrap();
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let runs = Arc::new(AtomicUsize::new(0));
    let entered_clone = Arc::clone(&entered);
    let release_clone = Arc::clone(&release);
    let runs_clone = Arc::clone(&runs);
    let committed = Arc::new(AtomicUsize::new(0));
    let committed_clone = Arc::clone(&committed);
    let mut registry = SourceRegistry::new();
    registry
        .register(LegacyCodexSourceAdapter::from_home(home_b))
        .unwrap();
    register_probe(
        &mut registry,
        ProbeAdapter::new("maka", AdapterAvailability::Available, move |_| {
            let run = runs_clone.fetch_add(1, Ordering::Relaxed);
            if run == 0 {
                let (lock, wake) = &*entered_clone;
                *lock.lock().unwrap() = true;
                wake.notify_all();
                let (lock, wake) = &*release_clone;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            committed_clone.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
    );
    let scanner = IngestionCoordinator::start(
        IngestionConfig::default(),
        Arc::clone(&changed_ledger),
        registry,
    )
    .unwrap();
    {
        let (lock, wake) = &*entered;
        let mut ready = lock.lock().unwrap();
        while !*ready {
            ready = wake.wait(ready).unwrap();
        }
    }
    let followup = scanner.request(usagi::domain::ScanTrigger::Manual).unwrap();
    let followup_id = match followup {
        usagi::scanner::RequestDisposition::Coalesced {
            followup_scan_id, ..
        } => followup_scan_id,
        other => panic!("expected one coalesced follow-up, got {other:?}"),
    };
    {
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let target = changed_ledger
            .scan_status_snapshot(Some(&followup_id))
            .unwrap()
            .target_scan;
        if target.is_some_and(|run| {
            matches!(
                run.state,
                usagi::domain::ScanRunState::Completed
                    | usagi::domain::ScanRunState::Failed
                    | usagi::domain::ScanRunState::StartFailed
            )
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for follow-up terminal state"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(runs.load(Ordering::Relaxed) >= 2);
    assert!(committed.load(Ordering::Relaxed) >= 2);
    assert_eq!(
        changed_ledger
            .app_state()
            .unwrap()
            .scan
            .last_scan_error_code
            .as_deref(),
        Some("SOURCE_RUN_FAILED")
    );
    let reports = scanner.last_source_reports();
    assert!(reports.iter().any(|report| {
        report.source.as_str() == "codex"
            && report.state.as_str() == "failed"
            && report.error_code.as_deref() == Some("SOURCE_CHANGED")
    }));
    assert!(reports.iter().any(|report| {
        report.source.as_str() == "maka"
            && report.state.as_str() == "completed"
            && report.error_code.is_none()
    }));
    scanner.shutdown().unwrap();
}

#[test]
fn phase2_c11_legacy_codex_source_changed_is_failed_not_skipped() {
    let (root, _home_a, ledger) = ledger_fixture("legacy-source-changed");
    let home_b = root.path().join("codex-b");
    fs::create_dir_all(home_b.join("sessions")).unwrap();
    fs::create_dir_all(home_b.join("archived_sessions")).unwrap();
    touch_codex_metadata(&home_b);
    let mut registry = SourceRegistry::new();
    registry
        .register(LegacyCodexSourceAdapter::from_home(home_b))
        .unwrap();
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    let state = ledger.app_state().unwrap().scan;
    assert_eq!(
        state.last_scan_error_code.as_deref(),
        Some("SOURCE_RUN_FAILED")
    );
    let report = scanner
        .source_reports()
        .into_iter()
        .find(|report| report.source.as_str() == "codex")
        .unwrap();
    assert_eq!(report.state.as_str(), "failed");
    assert_eq!(report.error_code.as_deref(), Some("SOURCE_CHANGED"));
    scanner.shutdown().unwrap();
}

#[test]
fn phase2_c11_legacy_codex_binding_failure_is_failed_not_skipped() {
    let (root, home, ledger) = ledger_fixture("legacy-binding-failure");
    let outside = root.path().join("metadata-outside");
    fs::create_dir_all(&outside).unwrap();
    let metadata = CodexMetadata::with_paths(
        outside.join("state_5.sqlite"),
        home.join("session_index.jsonl"),
        home.join(".codex-global-state.json"),
    );
    let mut registry = SourceRegistry::new();
    registry
        .register(LegacyCodexSourceAdapter::new(home, metadata))
        .unwrap();
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    assert_eq!(
        ledger
            .app_state()
            .unwrap()
            .scan
            .last_scan_error_code
            .as_deref(),
        Some("SOURCE_RUN_FAILED")
    );
    let report = scanner
        .source_reports()
        .into_iter()
        .find(|report| report.source.as_str() == "codex")
        .unwrap();
    assert_eq!(report.state.as_str(), "failed");
    assert_eq!(
        report.error_code.as_deref(),
        Some("CODEX_METADATA_HOME_MISMATCH")
    );
    scanner.shutdown().unwrap();
}

#[test]
fn phase2_c11_legacy_codex_discovery_unavailable_is_failed_not_skipped() {
    let (root, home, ledger, rollout_path) = metadata_fixture(
        "legacy-discovery-unavailable",
        "00000000-0000-7000-8000-000000000001",
        "00000000-0000-7000-8000-000000000001",
        b"",
    );
    fs::remove_file(rollout_path).unwrap();
    fs::remove_dir(home.join("archived_sessions")).unwrap();
    fs::write(home.join("archived_sessions"), b"not a directory").unwrap();

    let mut registry = SourceRegistry::new();
    registry
        .register(LegacyCodexSourceAdapter::from_home(home))
        .unwrap();
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    assert_eq!(
        ledger
            .app_state()
            .unwrap()
            .scan
            .last_scan_error_code
            .as_deref(),
        Some("SOURCE_RUN_FAILED")
    );
    let report = scanner
        .source_reports()
        .into_iter()
        .find(|report| report.source.as_str() == "codex")
        .unwrap();
    assert_eq!(report.state.as_str(), "failed");
    assert_eq!(
        report.error_code.as_deref(),
        Some("SOURCE_AREA_UNAVAILABLE")
    );
    scanner.shutdown().unwrap();
    drop(root);
}

#[test]
fn phase2_c11_legacy_codex_parser_failure_is_failed_not_skipped() {
    let (_root, home, ledger, _rollout_path) = metadata_fixture(
        "legacy-parser-failure",
        "00000000-0000-7000-8000-000000000001",
        "00000000-0000-7000-8000-000000000002",
        b"{}\n",
    );
    let mut registry = SourceRegistry::new();
    registry
        .register(LegacyCodexSourceAdapter::from_home(home))
        .unwrap();
    let scanner =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_terminal(&ledger);
    assert_eq!(
        ledger
            .app_state()
            .unwrap()
            .scan
            .last_scan_error_code
            .as_deref(),
        Some("SOURCE_RUN_FAILED")
    );
    let report = scanner
        .source_reports()
        .into_iter()
        .find(|report| report.source.as_str() == "codex")
        .unwrap();
    assert_eq!(report.state.as_str(), "failed");
    assert_eq!(
        report.error_code.as_deref(),
        Some("METADATA_CONTINUATION_UNSTABLE")
    );
    scanner.shutdown().unwrap();
}
