use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;
use usagi::codex::rollout::{
    OwningCandidateConfidence, OwningThreadCandidate, OwningThreadCandidates,
};
use usagi::{
    codex::{CompleteRolloutLine, ResumeState, RolloutMetadataParser, RolloutParseContext},
    domain::{
        ScanResult, ScanTrigger, SourceArea, SourceObservation, SourceObservationBatch,
        SourceRegionStatus,
    },
    platform::file_identity,
    scanner::{CodexMetadata, RequestDisposition, ScanConfig, ScanCoordinator},
    storage::{Ledger, LedgerOptions},
};
use uuid::Uuid;

const OWNER_ID: &str = "00000000-0000-7000-8000-000000000001";

type MetadataFactRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
    String,
);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec03-{label}-{}-{stamp}",
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
}

impl ScannerFixture {
    fn with_rollout(label: &str, thread_id: &str, bytes: &[u8]) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let sessions = home.join("sessions");
        fs::create_dir_all(&sessions).expect("create sessions");
        fs::create_dir_all(home.join("archived_sessions")).expect("create archived sessions");
        let rollout_path = sessions.join(format!("rollout-{thread_id}.jsonl"));
        fs::write(&rollout_path, bytes).expect("write rollout fixture");
        Self {
            db_path: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            rollout_path,
        }
    }

    fn main(label: &str) -> Self {
        let fixture = Self::with_rollout(label, OWNER_ID, &main_rollout_bytes(OWNER_ID));
        write_state_main(
            &fixture.home,
            &fixture.rollout_path,
            OWNER_ID,
            "State title",
        );
        write_session_index(&fixture.home, OWNER_ID, "Session title");
        fixture
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
        Connection::open(&self.db_path)
            .expect("open ledger query")
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

    fn data_revision(&self) -> i64 {
        Connection::open(&self.db_path)
            .unwrap()
            .query_row(
                "SELECT data_revision FROM app_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
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

fn main_rollout_bytes(thread_id: &str) -> Vec<u8> {
    records_to_bytes(&[
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": thread_id,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": "/work/main",
                "agent_role": "main"
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {
                "turn_id": "00000000-0000-7000-8000-000000000010",
                "cwd": "/work/main",
                "model": "rollout-model"
            }
        }),
        json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "sentinel": "SPEC03_BODY_SENTINEL"}
        }),
        json!({
            "type": "response_item",
            "payload": {"text": "SPEC03_BODY_SENTINEL"}
        }),
    ])
}

fn records_to_bytes(records: &[serde_json::Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record).expect("serialize rollout record"));
        bytes.push(b'\n');
    }
    bytes
}

fn write_state_main(home: &Path, rollout_path: &Path, thread_id: &str, title: &str) {
    let connection = create_state_schema(home);
    connection
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms,
                archived, cwd, title, name, model, agent_role
             ) VALUES (?1, ?2, 1700000000000, 1700000000100,
                       0, '/state/main', ?3, NULL, 'state-model', 'main')",
            params![thread_id, rollout_path.to_str().unwrap(), title],
        )
        .expect("insert state thread");
}

fn create_state_schema(home: &Path) -> Connection {
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
}

fn write_session_index(home: &Path, thread_id: &str, title: &str) {
    let record = json!({
        "id": thread_id,
        "thread_name": title,
        "updated_at": "2026-08-08T01:02:05Z",
        "preview": "SESSION_INDEX_BODY_SENTINEL"
    });
    fs::write(
        home.join("session_index.jsonl"),
        records_to_bytes(&[record]),
    )
    .expect("write session index");
}

fn observe_rollout_only(ledger: &Ledger, rollout_path: &Path) -> i64 {
    let file = fs::File::open(rollout_path).expect("open rollout");
    let metadata = file.metadata().expect("stat rollout");
    let identity = file_identity::identity_from_file(&file).expect("rollout identity");
    let mtime_ns = file_identity::modified_ns(&metadata).expect("mtime fits i64 nanoseconds");
    let observation = SourceObservation::new(
        rollout_path.to_str().unwrap(),
        SourceArea::Sessions,
        i64::try_from(identity.device_id).expect("device id fits i64"),
        i64::try_from(identity.inode).expect("inode fits i64"),
        i64::try_from(metadata.len()).expect("file size fits i64"),
        mtime_ns,
        1,
    )
    .unwrap();
    ledger
        .record_source_observations(
            SourceObservationBatch::new(
                vec![observation],
                SourceRegionStatus::Complete,
                SourceRegionStatus::Complete,
            )
            .unwrap(),
        )
        .unwrap()
        .results[0]
        .source_file_id
}

fn parse_entire_rollout_without_committing(source_file_id: i64, thread_id: &str, bytes: &[u8]) {
    let mut offset = 0_u64;
    let lines = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(|slice| {
            let line = CompleteRolloutLine::new(offset, slice.to_vec()).expect("complete line");
            offset = line.end_offset();
            line
        })
        .collect::<Vec<_>>();
    let result = RolloutMetadataParser::parse_chunk(
        RolloutParseContext {
            source_file_id,
            chunk_start_offset: 0,
            candidates: OwningThreadCandidates {
                state_rollout: None,
                filename: Some(OwningThreadCandidate {
                    thread_id: thread_id.to_owned(),
                    confidence: OwningCandidateConfidence::Confirmed,
                }),
            },
            resume_state: ResumeState::AwaitOwningMeta,
            existing_fact: None,
        },
        lines,
    );
    assert!(!result.needs_rebuild);
    assert!(result.fact.is_some());
    assert_eq!(result.last_processed_offset, bytes.len() as u64);
}

fn uuid7(timestamp_ms: u64, suffix: u8) -> String {
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes[..6].iter_mut().enumerate() {
        *byte = ((timestamp_ms >> (8 * (5 - index))) & 0xff) as u8;
    }
    bytes[6] = 0x70;
    bytes[8] = 0x80;
    bytes[15] = suffix;
    Uuid::from_bytes(bytes).to_string()
}

#[cfg(unix)]
#[test]
fn t_s03_009_real_ledger_crash_windows_resume_from_last_committed_offset() {
    // Crash after source observation but before parse: only the source row and
    // pending offset=0 checkpoint survive. Reopen must parse from zero.
    {
        let fixture = ScannerFixture::main("crash-after-observation");
        let ledger = fixture.open_ledger();
        let source_id = observe_rollout_only(&ledger, &fixture.rollout_path);
        let (_, offset, status, thread_id) = fixture.source_row();
        assert_eq!(offset, 0);
        assert_eq!(status, "pending");
        assert!(thread_id.is_none());
        assert_eq!(source_id, fixture.source_row().0);
        drop(ledger);

        let reopened = fixture.open_ledger();
        let handle = fixture.start(Arc::clone(&reopened));
        fixture.wait_for_startup(&reopened);
        assert_eq!(
            reopened.app_state().unwrap().scan.last_finished_scan_result,
            Some(ScanResult::Completed)
        );
        let (_, committed, status, thread_id) = fixture.source_row();
        assert_eq!(
            committed,
            fs::metadata(&fixture.rollout_path).unwrap().len() as i64
        );
        assert_eq!(status, "ready");
        assert_eq!(thread_id.as_deref(), Some(OWNER_ID));
        handle.shutdown().unwrap();
    }

    // Crash after parse but before commit: parsing has no durable side effect,
    // so the database must still expose the same old committed offset on reopen.
    {
        let fixture = ScannerFixture::main("crash-after-parse");
        let ledger = fixture.open_ledger();
        let source_id = observe_rollout_only(&ledger, &fixture.rollout_path);
        let bytes = fs::read(&fixture.rollout_path).unwrap();
        parse_entire_rollout_without_committing(source_id, OWNER_ID, &bytes);
        let connection = Connection::open(&fixture.db_path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT committed_offset FROM source_checkpoints
                     WHERE source_file_id = ?1 AND consumer_kind = 'metadata'",
                    [source_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM rollout_metadata_facts WHERE source_file_id = ?1",
                    [source_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        drop(connection);
        drop(ledger);

        let reopened = fixture.open_ledger();
        let handle = fixture.start(Arc::clone(&reopened));
        fixture.wait_for_startup(&reopened);
        let (_, committed, status, _) = fixture.source_row();
        assert_eq!(committed, bytes.len() as i64);
        assert_eq!(status, "ready");
        handle.shutdown().unwrap();
    }

    // Crash after metadata commit: reopen sees the committed offset and the
    // unchanged startup scan must not duplicate the normalized data revision.
    {
        let fixture = ScannerFixture::main("crash-after-commit");
        let ledger = fixture.open_ledger();
        let handle = fixture.start(Arc::clone(&ledger));
        fixture.wait_for_startup(&ledger);
        let (_, committed_before, status_before, _) = fixture.source_row();
        let revision_before = fixture.data_revision();
        assert!(committed_before > 0);
        assert_eq!(status_before, "ready");
        let previous_finished = ledger
            .app_state()
            .unwrap()
            .scan
            .last_finished_scan_id
            .clone();
        handle.shutdown().unwrap();
        drop(ledger);

        let reopened = fixture.open_ledger();
        let reopened_handle = fixture.start(Arc::clone(&reopened));
        fixture.wait_for_new_startup(&reopened, previous_finished.as_deref());
        let (_, committed_after, status_after, _) = fixture.source_row();
        assert_eq!(committed_after, committed_before);
        assert_eq!(status_after, "ready");
        assert_eq!(fixture.data_revision(), revision_before);
        reopened_handle.shutdown().unwrap();
    }
}

#[test]
fn t_s03_016_real_scanner_preserves_child_fact_across_parent_replay_until_owning_live() {
    let parent = uuid7(1_000, 1);
    let child = uuid7(2_000, 2);
    let parent_turn = uuid7(1_500, 3);
    let child_turn = uuid7(2_100, 4);
    let fixture = ScannerFixture::with_rollout("fork-replay", &child, &[]);
    let child_cwd = fixture._root.path().join("child");
    let parent_cwd = fixture._root.path().join("parent");
    let parent_turn_cwd = fixture._root.path().join("parent-turn");
    let child_turn_cwd = fixture._root.path().join("child-turn");
    let rollout = records_to_bytes(&[
        json!({
            "type": "session_meta",
            "payload": {
                "id": child,
                "cwd": child_cwd.to_str().unwrap(),
                "source": {"subagent": {"thread_spawn": {
                    "parent_thread_id": parent,
                    "depth": 1
                }}}
            }
        }),
        json!({
            "type": "session_meta",
            "payload": {"id": parent, "cwd": parent_cwd.to_str().unwrap()}
        }),
        json!({
            "type": "turn_context",
            "payload": {
                "turn_id": parent_turn,
                "cwd": parent_turn_cwd.to_str().unwrap(),
                "model": "parent-model"
            }
        }),
        json!({"type": "event_msg", "payload": {"type": "token_count", "total": 111}}),
        json!({
            "type": "turn_context",
            "payload": {
                "turn_id": child_turn,
                "cwd": child_turn_cwd.to_str().unwrap(),
                "model": "child-model"
            }
        }),
    ]);
    fs::write(&fixture.rollout_path, rollout).expect("write rollout fixture");

    let state = create_state_schema(&fixture.home);
    state
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms, archived,
                cwd, title, name, model, agent_role
             ) VALUES (?1, NULL, 1000, 1500, 0, ?2, 'Parent', NULL, 'parent-state', 'main')",
            params![parent, parent_cwd.to_str().unwrap()],
        )
        .unwrap();
    state
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms, archived,
                cwd, title, name, model, agent_role
             ) VALUES (?1, ?2, 2000, 2100, 0, ?3, 'Child', NULL, 'child-state', 'subagent')",
            params![
                child,
                fixture.rollout_path.to_str().unwrap(),
                child_cwd.to_str().unwrap()
            ],
        )
        .unwrap();
    state
        .execute(
            "INSERT INTO thread_spawn_edges (
                parent_thread_id, child_thread_id, status, observed_at_ms
             ) VALUES (?1, ?2, 'spawned', 2000)",
            params![parent, child],
        )
        .unwrap();
    drop(state);
    write_session_index(&fixture.home, &child, "Child index title");

    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let fact: MetadataFactRow = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT owning_thread_id, cwd, latest_context_model,
                    parent_thread_id_hint, replay_start_offset,
                    owning_records_start_offset, continuation_state,
                    ownership_confidence
             FROM rollout_metadata_facts",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(fact.0, child);
    assert_eq!(fact.1.as_deref(), Some(child_cwd.to_str().unwrap()));
    assert_eq!(fact.2.as_deref(), Some("child-model"));
    assert_eq!(fact.3.as_deref(), Some(parent.as_str()));
    assert!(fact.4.is_some(), "parent replay start must be persisted");
    assert!(fact.5.is_some(), "owning-live boundary must be persisted");
    assert!(fact.5.unwrap() > fact.4.unwrap());
    assert_eq!(fact.6, "owning_live");
    assert_eq!(fact.7, "confirmed");
    handle.shutdown().unwrap();
}

#[test]
fn t_s03_017_missing_and_stale_safe_facts_force_real_worker_rebuild_from_zero() {
    let fixture = ScannerFixture::main("safe-fact-rebuild");
    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    let (source_id, eof, status, _) = fixture.source_row();
    assert!(eof > 0);
    assert_eq!(status, "ready");

    // Missing fact: ready/nonzero checkpoint cannot Skip. A successful scan
    // must rebuild from zero and recreate the fact.
    Connection::open(&fixture.db_path)
        .unwrap()
        .execute(
            "DELETE FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_id],
        )
        .unwrap();
    fixture.request_and_wait(&handle, &ledger);
    let repaired: (i64, i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT file_generation, resolved_through_offset, latest_context_model
             FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(repaired.0, 1);
    assert_eq!(repaired.1, eof);
    assert_eq!(repaired.2, "rollout-model");

    // Stale generation: corrupt only the durable fact. The source generation
    // remains 1, so the next worker round must reject the stale fact and
    // rebuild it from zero rather than resume/skip it.
    Connection::open(&fixture.db_path)
        .unwrap()
        .execute(
            "UPDATE rollout_metadata_facts
             SET file_generation = 999, latest_context_model = 'STALE_GENERATION_MARKER'
             WHERE source_file_id = ?1",
            [source_id],
        )
        .unwrap();
    fixture.request_and_wait(&handle, &ledger);
    let repaired: (i64, i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT file_generation, resolved_through_offset, latest_context_model
             FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(repaired.0, 1);
    assert_eq!(repaired.1, eof);
    assert_eq!(repaired.2, "rollout-model");

    // Stale offset: the checkpoint remains at EOF but the fact claims an older
    // seam. Matching must fail and a from-zero rebuild must restore the seam.
    Connection::open(&fixture.db_path)
        .unwrap()
        .execute(
            "UPDATE rollout_metadata_facts
             SET resolved_through_offset = ?2,
                 latest_context_model = 'STALE_OFFSET_MARKER'
             WHERE source_file_id = ?1",
            params![source_id, eof - 1],
        )
        .unwrap();
    fixture.request_and_wait(&handle, &ledger);
    let repaired: (i64, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT resolved_through_offset, latest_context_model
             FROM rollout_metadata_facts WHERE source_file_id = ?1",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(repaired.0, eof);
    assert_eq!(repaired.1, "rollout-model");
    assert_eq!(fixture.source_row().1, eof);
    handle.shutdown().unwrap();
}

#[test]
fn t_s03_019_state_unavailable_never_infers_main_without_explicit_evidence() {
    let rollout = records_to_bytes(&[
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": OWNER_ID,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": "/work/unknown-role"
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {
                "turn_id": "00000000-0000-7000-8000-000000000010",
                "cwd": "/work/unknown-role",
                "model": "unknown-role-model"
            }
        }),
    ]);
    let fixture = ScannerFixture::with_rollout("state-unavailable", OWNER_ID, &rollout);
    // Deliberately do not create state_5.sqlite. Session-index title gives the
    // resolver useful metadata while relationship completeness remains absent.
    write_session_index(&fixture.home, OWNER_ID, "Index-only title");

    let ledger = fixture.open_ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    fixture.wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Failed)
    );
    assert_eq!(
        ledger
            .app_state()
            .unwrap()
            .scan
            .last_scan_error_code
            .as_deref(),
        Some("STATE_SOURCE_UNAVAILABLE")
    );

    let connection = Connection::open(&fixture.db_path).unwrap();
    let projection = connection
        .query_row(
            "SELECT agent_role, root_session_id, title
             FROM threads WHERE thread_id = ?1",
            [OWNER_ID],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .unwrap();
    if let Some((agent_role, root_session_id, _title)) = projection {
        assert_eq!(agent_role, "unknown");
        assert!(root_session_id.is_none());
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM threads
                 WHERE thread_id = ?1
                   AND (agent_role = 'main' OR root_session_id = ?1)",
                [OWNER_ID],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "an unavailable relationship source must not manufacture a Main/root Session"
    );
    handle.shutdown().unwrap();
}
