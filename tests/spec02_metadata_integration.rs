use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde_json::json;
use usagi::{
    domain::{ScanResult, ScanTrigger},
    scanner::{CodexMetadata, RequestDisposition, ScanConfig, ScanCoordinator},
    storage::{Ledger, LedgerOptions},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const OWNER_ID: &str = "00000000-0000-7000-8000-000000000001";
const FOREIGN_ID: &str = "00000000-0000-7000-8000-000000000099";

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec02-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary test directory");
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

struct ScannerFixture {
    _root: TempRoot,
    home: PathBuf,
    db_path: PathBuf,
    rollout_path: PathBuf,
    project_path: PathBuf,
}

impl ScannerFixture {
    fn new(label: &str, state_title: &str) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let sessions = home.join("sessions");
        let archived = home.join("archived_sessions");
        let project_path = root.path().join("project-private");
        fs::create_dir_all(&sessions).expect("create sessions");
        fs::create_dir_all(&archived).expect("create archived sessions");
        fs::create_dir_all(&project_path).expect("create project fixture");
        fs::write(project_path.join("private.txt"), b"PROJECT_BODY_SENTINEL")
            .expect("write project sentinel");

        let rollout_path = sessions.join(format!("rollout-{OWNER_ID}.jsonl"));
        fs::write(
            &rollout_path,
            rollout_bytes(OWNER_ID, project_path.to_str().unwrap()),
        )
        .expect("write rollout");
        write_state(&home, &rollout_path, state_title, &project_path);
        write_session_index(&home, "Session index title");

        Self {
            db_path: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            rollout_path,
            project_path,
        }
    }

    fn open_ledger(&self) -> Arc<Ledger> {
        Arc::new(
            Ledger::open(LedgerOptions::new(&self.db_path, &self.home))
                .expect("open scanner fixture ledger"),
        )
    }

    fn start(&self, ledger: Arc<Ledger>) -> usagi::scanner::ScanHandle {
        ScanCoordinator::start(
            ScanConfig::new(self.home.clone()),
            ledger,
            CodexMetadata::from_home(self.home.clone()),
        )
        .expect("start public scan coordinator")
    }

    fn wait_for_startup(&self, ledger: &Ledger) {
        wait_for_scan(ledger, None, Duration::from_secs(5));
    }

    fn wait_for_new_startup(&self, ledger: &Ledger, previous_finished_scan_id: Option<&str>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = ledger.app_state().expect("read scan state").scan;
            let changed = state.last_finished_scan_id.as_deref() != previous_finished_scan_id
                && state.last_finished_scan_id.is_some();
            if changed && state.active_scan_id.is_none() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for new startup scan"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn request_and_wait(&self, handle: &usagi::scanner::ScanHandle, ledger: &Ledger) -> String {
        let disposition = handle
            .request(ScanTrigger::Manual)
            .expect("request manual scan");
        let scan_id = match disposition {
            RequestDisposition::Started { scan_id, .. } => scan_id,
            RequestDisposition::Coalesced {
                followup_scan_id, ..
            } => followup_scan_id,
        };
        wait_for_scan(ledger, Some(&scan_id), Duration::from_secs(5));
        scan_id
    }

    fn source_row(&self) -> (i64, i64, String, Option<String>) {
        let connection = Connection::open(&self.db_path).expect("open ledger query");
        connection
            .query_row(
                "SELECT sf.source_file_id, sc.committed_offset, sc.processing_status, sf.thread_id
                 FROM source_files sf
                 JOIN source_checkpoints sc USING (source_file_id)
                 WHERE sf.current_path = ?1 AND sc.consumer_kind = 'metadata'",
                [self.rollout_path.to_str().unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read source checkpoint")
    }

    fn thread_title(&self) -> String {
        Connection::open(&self.db_path)
            .expect("open ledger query")
            .query_row(
                "SELECT title FROM threads WHERE thread_id = ?1",
                [OWNER_ID],
                |row| row.get(0),
            )
            .expect("read thread title")
    }

    fn update_state_title(&self, title: &str) {
        Connection::open(self.home.join("state_5.sqlite"))
            .expect("open state fixture")
            .execute(
                "UPDATE threads SET title = ?1 WHERE id = ?2",
                params![title, OWNER_ID],
            )
            .expect("update state title");
    }

    fn append_json(&self, value: serde_json::Value) {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.rollout_path)
            .expect("open rollout append");
        file.write_all(&serde_json::to_vec(&value).expect("serialize append"))
            .expect("append json");
        file.write_all(b"\n").expect("append newline");
        file.sync_all().expect("sync append");
    }
}

fn wait_for_scan(ledger: &Ledger, target_scan_id: Option<&str>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let state = ledger.app_state().expect("read scan state").scan;
        let finished_target = match target_scan_id {
            Some(target) => state.last_finished_scan_id.as_deref() == Some(target),
            None => state.last_finished_scan_id.is_some(),
        };
        if finished_target && state.active_scan_id.is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for scan");
        thread::sleep(Duration::from_millis(10));
    }
}

fn rollout_bytes(thread_id: &str, cwd: &str) -> Vec<u8> {
    let records = [
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": thread_id,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": cwd,
                "agent_role": "main"
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {
                "turn_id": "00000000-0000-7000-8000-000000000010",
                "cwd": cwd,
                "model": "rollout-model"
            }
        }),
        json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "sentinel": "ROLL_OUT_BODY_SENTINEL"}
        }),
        json!({
            "type": "response_item",
            "payload": {"text": "ROLL_OUT_BODY_SENTINEL"}
        }),
    ];
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(&record).expect("serialize rollout fixture"));
        bytes.push(b'\n');
    }
    bytes
}

fn write_state(home: &Path, rollout_path: &Path, title: &str, cwd: &Path) {
    let connection = Connection::open(home.join("state_5.sqlite")).expect("create state fixture");
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
        .expect("create state schema");
    connection
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms,
                archived, cwd, title, name, model, agent_role
             ) VALUES (?1, ?2, 1700000000000, 1700000000100,
                       0, ?3, ?4, NULL, 'state-model', 'main')",
            params![
                OWNER_ID,
                rollout_path.to_str().unwrap(),
                cwd.to_str().unwrap(),
                title,
            ],
        )
        .expect("insert state thread");
}

fn write_session_index(home: &Path, title: &str) {
    let value = json!({
        "id": OWNER_ID,
        "thread_name": title,
        "updated_at": "2026-08-08T01:02:05Z",
        "preview": "SESSION_INDEX_BODY_SENTINEL"
    });
    let mut bytes = serde_json::to_vec(&value).expect("serialize session index");
    bytes.push(b'\n');
    fs::write(home.join("session_index.jsonl"), bytes).expect("write session index");
}

#[cfg(unix)]
#[test]
fn t_s02_014_present_rollout_missing_fact_blocks_patch_only_commit() {
    let fixture = ScannerFixture::new("patch-boundary", "Original state title");
    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    assert_eq!(fixture.thread_title(), "Original state title");

    let source_id = fixture.source_row().0;
    Connection::open(&fixture.db_path)
        .unwrap()
        .execute(
            "DELETE FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_id],
        )
        .unwrap();
    fixture.update_state_title("Changed state title must not commit");

    let original_mode = fs::metadata(&fixture.rollout_path)
        .unwrap()
        .permissions()
        .mode();
    let mut permissions = fs::metadata(&fixture.rollout_path).unwrap().permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&fixture.rollout_path, permissions).unwrap();

    let scan_id = fixture.request_and_wait(&handle, &ledger);
    let state = ledger.app_state().unwrap().scan;
    assert_eq!(
        state.last_finished_scan_id.as_deref(),
        Some(scan_id.as_str())
    );
    assert_eq!(state.last_finished_scan_result, Some(ScanResult::Failed));
    assert_eq!(fixture.thread_title(), "Original state title");

    let mut permissions = fs::metadata(&fixture.rollout_path).unwrap().permissions();
    permissions.set_mode(original_mode);
    fs::set_permissions(&fixture.rollout_path, permissions).unwrap();
    handle.shutdown().unwrap();
}

#[cfg(unix)]
#[test]
fn t_s02_018_scanner_does_not_require_access_to_project_files() {
    let fixture = ScannerFixture::new("privacy-project-io", "Privacy title");
    let private_file = fixture.project_path.join("private.txt");
    assert_eq!(fs::read(&private_file).unwrap(), b"PROJECT_BODY_SENTINEL");

    let original_mode = fs::metadata(&fixture.project_path)
        .unwrap()
        .permissions()
        .mode();
    let mut permissions = fs::metadata(&fixture.project_path).unwrap().permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&fixture.project_path, permissions).unwrap();

    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    let state = ledger.app_state().unwrap().scan;
    assert_eq!(state.last_finished_scan_result, Some(ScanResult::Completed));
    assert_eq!(fixture.thread_title(), "Privacy title");

    let database = fs::read(&fixture.db_path).unwrap();
    assert!(!String::from_utf8_lossy(&database).contains("PROJECT_BODY_SENTINEL"));

    handle.shutdown().unwrap();
    let mut permissions = fs::metadata(&fixture.project_path).unwrap().permissions();
    permissions.set_mode(original_mode);
    fs::set_permissions(&fixture.project_path, permissions).unwrap();
    assert_eq!(fs::read(&private_file).unwrap(), b"PROJECT_BODY_SENTINEL");
}

#[test]
fn t_s02_019_reopen_resumes_from_persisted_nonzero_safe_fact() {
    let fixture = ScannerFixture::new("reopen-resume", "Resume title");

    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    let (source_id, initial_offset, status, thread_id) = fixture.source_row();
    assert!(initial_offset > 0);
    assert_eq!(status, "ready");
    assert_eq!(thread_id.as_deref(), Some(OWNER_ID));

    Connection::open(&fixture.db_path)
        .unwrap()
        .execute(
            "UPDATE rollout_metadata_facts
             SET latest_context_model = 'persisted-resume-marker'
             WHERE source_file_id = ?1",
            [source_id],
        )
        .unwrap();
    handle.shutdown().unwrap();
    drop(ledger);

    fixture.append_json(json!({
        "type": "event_msg",
        "payload": {"type": "token_count"}
    }));
    let appended_size = fs::metadata(&fixture.rollout_path).unwrap().len() as i64;
    assert!(appended_size > initial_offset);

    let reopened = fixture.open_ledger();
    let previous_finished_scan_id = reopened
        .app_state()
        .unwrap()
        .scan
        .last_finished_scan_id
        .clone();
    let reopened_handle = fixture.start(Arc::clone(&reopened));
    fixture.wait_for_new_startup(&reopened, previous_finished_scan_id.as_deref());
    assert_eq!(
        reopened.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    let (_, resumed_offset, resumed_status, _) = fixture.source_row();
    assert_eq!(resumed_offset, appended_size);
    assert_eq!(resumed_status, "ready");
    let persisted_model: String = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT latest_context_model FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted_model, "persisted-resume-marker");
    reopened_handle.shutdown().unwrap();
}

#[test]
fn t_s02_019_late_foreign_meta_marks_metadata_rebuild() {
    let fixture = ScannerFixture::new("late-foreign", "Foreign title");
    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    let (_, initial_offset, initial_status, _) = fixture.source_row();
    assert!(initial_offset > 0);
    assert_eq!(initial_status, "ready");

    fixture.append_json(json!({
        "type": "session_meta",
        "timestamp": "2026-08-08T01:03:00Z",
        "payload": {
            "id": FOREIGN_ID,
            "timestamp": "2026-08-08T01:03:00Z",
            "cwd": "/foreign-private"
        }
    }));

    let scan_id = fixture.request_and_wait(&handle, &ledger);
    let state = ledger.app_state().unwrap().scan;
    assert_eq!(
        state.last_finished_scan_id.as_deref(),
        Some(scan_id.as_str())
    );
    assert_eq!(state.last_finished_scan_result, Some(ScanResult::Failed));
    assert_eq!(
        state.last_scan_error_code.as_deref(),
        Some("METADATA_CONTINUATION_UNSTABLE")
    );

    let (_, committed_offset, processing_status, thread_id) = fixture.source_row();
    assert_eq!(thread_id.as_deref(), Some(OWNER_ID));
    assert_eq!(
        committed_offset, 0,
        "late foreign meta must force rebuild from offset 0"
    );
    assert_eq!(processing_status, "rebuild_required");
    handle.shutdown().unwrap();
}
