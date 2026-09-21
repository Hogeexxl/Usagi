use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde_json::json;
use usagi::{
    codex::{CodexAdapter, CodexConfig},
    domain::{ScanResult, ScanTrigger},
    ingestion::RequestDisposition,
    ingestion::{IngestionConfig, IngestionCoordinator},
    source::SourceRegistry,
    storage::{Ledger, LedgerOptions},
};
use uuid::Uuid;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-replayed-ancestor-{label}-{}-{stamp}",
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

fn records_to_bytes(records: &[serde_json::Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record).unwrap());
        bytes.push(b'\n');
    }
    bytes
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

fn write_rollout(path: &Path, child: &str, parent: &str, parent_turn: &str, cwd: &Path) {
    let records = [
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": child,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": cwd.to_str().unwrap(),
                "agent_role": "main"
            }
        }),
        json!({
            "type": "session_meta",
            "payload": {"id": parent}
        }),
        json!({
            "type": "turn_context",
            "payload": {"turn_id": parent_turn, "model": "parent-model"}
        }),
        json!({
            "type": "event_msg",
            "payload": {"type": "token_count"}
        }),
    ];
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(&record).unwrap());
        bytes.push(b'\n');
    }
    fs::write(path, bytes).expect("write replay-tail rollout");
}

fn write_state(home: &Path, rollout_path: &Path, child: &str, cwd: &Path) {
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
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms,
                archived, cwd, title, name, model, agent_role
             ) VALUES (?1, ?2, 1700000000000, 1700000000100,
                       0, ?3, 'Replay tail', NULL, 'state-model', 'main')",
            params![child, rollout_path.to_str().unwrap(), cwd.to_str().unwrap()],
        )
        .unwrap();
}

fn write_session_index(home: &Path, child: &str) {
    let value = json!({
        "id": child,
        "thread_name": "Replay tail",
        "updated_at": "2026-08-08T01:02:05Z"
    });
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    fs::write(home.join("session_index.jsonl"), bytes).unwrap();
}

fn write_subagent_state(home: &Path, rollout_path: &Path, child: &str, parent: &str) {
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
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads (
                id, rollout_path, created_at_ms, updated_at_ms,
                archived, cwd, title, name, model, agent_role
             ) VALUES (?1, NULL, 1, 2, 0, '/work/root', 'Root', NULL, 'root-model', 'main'),
                      (?2, ?3, 3, 4, 0, '/work/child', 'Child', NULL, 'child-model', 'subagent')",
            params![parent, child, rollout_path.to_str().unwrap()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO thread_spawn_edges(parent_thread_id, child_thread_id, status, observed_at_ms)
             VALUES (?1, ?2, 'spawned', 3)",
            params![parent, child],
        )
        .unwrap();
    drop(connection);
    let values = [parent, child]
        .into_iter()
        .map(|id| json!({"id": id, "thread_name": format!("name-{id}")}))
        .collect::<Vec<_>>();
    fs::write(home.join("session_index.jsonl"), records_to_bytes(&values)).unwrap();
}

fn wait_for_startup(ledger: &Ledger) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = ledger.app_state().expect("read scan state").scan;
        if state.last_finished_scan_id.is_some() && state.active_scan_id.is_none() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for startup scan"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_scan(ledger: &Ledger, wanted: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let scan = ledger.app_state().unwrap().scan;
        let done = match wanted {
            Some(id) => scan.last_finished_scan_id.as_deref() == Some(id),
            None => scan.last_finished_scan_id.is_some(),
        } && scan.active_scan_id.is_none();
        if done {
            return;
        }
        assert!(Instant::now() < deadline, "scan timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn startup_replay_tail_scan_completes_and_activates_usage() {
    let root = TempRoot::new("scanner-e2e");
    let home = root.path().join("codex");
    let sessions = home.join("sessions");
    let archived = home.join("archived_sessions");
    let cwd = root.path().join("project");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    let parent = uuid7(1_000, 1);
    let parent_turn = uuid7(1_500, 2);
    let child = uuid7(2_000, 3);
    let rollout_path = sessions.join(format!("rollout-{child}.jsonl"));
    write_rollout(&rollout_path, &child, &parent, &parent_turn, &cwd);
    write_state(&home, &rollout_path, &child, &cwd);
    write_session_index(&home, &child);

    let db_path = root.path().join("mu.sqlite3");
    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    registry
        .register(CodexAdapter::new(CodexConfig::from_home(home.clone())))
        .unwrap();
    let handle =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .expect("start scanner");
    wait_for_startup(&ledger);

    let scan = ledger.app_state().unwrap().scan;
    assert_eq!(scan.last_finished_scan_result, Some(ScanResult::Completed));
    assert_eq!(scan.last_scan_error_code, None);

    let connection = Connection::open(&db_path).unwrap();
    let metadata: (String, i64, String) = connection
        .query_row(
            "SELECT f.continuation_state, sc.committed_offset, sc.processing_status
             FROM codex_source_files sf
             JOIN codex_rollout_metadata_facts f USING (source_file_id)
             JOIN codex_source_checkpoints sc USING (source_file_id)
             WHERE sf.current_path=?1 AND sc.consumer_kind='metadata'",
            [rollout_path.canonicalize().unwrap().to_str().unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(metadata.0, "replayed_ancestor");
    assert_eq!(
        metadata.1,
        fs::metadata(&rollout_path).unwrap().len() as i64
    );
    assert_eq!(metadata.2, "ready");

    let epochs: (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT active_epoch, build_epoch, active_parser_version
             FROM source_usage_epochs WHERE source='codex'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(
        epochs.0 > 0,
        "replay-tail source must not keep active epoch at zero"
    );
    assert_eq!(epochs.1, None);
    assert_eq!(epochs.2, usagi::codex::normalization::USAGE_PARSER_VERSION);

    handle.shutdown().unwrap();
}

#[test]
fn rebuilt_subagent_same_envelope_timestamp_models_keep_relationship_and_usage_root() {
    let root = TempRoot::new("legacy-compacted-model-order");
    let home = root.path().join("codex");
    let sessions = home.join("sessions");
    let archived = home.join("archived_sessions");
    let cwd = root.path().join("project");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    let parent = uuid7(1_000, 1);
    let child = uuid7(2_000, 2);
    let initial_turn = uuid7(2_100, 3);
    let rollout_path = sessions.join(format!("rollout-{child}.jsonl"));
    let initial_records = vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": child,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": cwd.to_str().unwrap(),
                "parent_thread_id": parent,
                "source": {"subagent": {"other": "legacy"}}
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {"turn_id": initial_turn, "model": "gpt-5.6-sol"}
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T01:02:05Z",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 7,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 7
                    },
                    "last_token_usage": {
                        "input_tokens": 7,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 7
                    }
                }
            }
        }),
    ];
    fs::write(&rollout_path, records_to_bytes(&initial_records)).unwrap();
    write_subagent_state(&home, &rollout_path, &child, &parent);

    let db_path = root.path().join("mu.sqlite3");
    let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());
    let mut registry = SourceRegistry::new();
    registry
        .register(CodexAdapter::new(CodexConfig::from_home(home.clone())))
        .unwrap();
    let handle =
        IngestionCoordinator::start(IngestionConfig::default(), Arc::clone(&ledger), registry)
            .unwrap();
    wait_for_startup(&ledger);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let before = Connection::open(&db_path).unwrap();
    let relationship_before: (Option<String>, Option<String>, String) = before
        .query_row(
            "SELECT parent_thread_id,root_session_id,agent_role
             FROM threads WHERE thread_id=?1",
            [child.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        relationship_before,
        (
            Some(parent.clone()),
            Some(parent.clone()),
            "subagent".to_owned(),
        )
    );
    let usage_root_before: String = before
        .query_row(
            "SELECT root_session_id FROM usage_events
             WHERE source='codex' AND thread_id=?1
             ORDER BY occurred_at_ms LIMIT 1",
            [child.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usage_root_before, parent);
    drop(before);

    let first_turn = uuid7(3_000, 4);
    let later_turn = uuid7(3_100, 5);
    let rebuilt_records = vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:02:03Z",
            "payload": {
                "id": child,
                "timestamp": "2026-08-08T01:02:03Z",
                "cwd": cwd.to_str().unwrap(),
                "parent_thread_id": parent,
                "source": {"subagent": {"other": "legacy"}}
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {"turn_id": first_turn, "model": "gpt-5.6-luna"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:02:04Z",
            "payload": {"turn_id": later_turn, "model": "gpt-5.6-sol"}
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T01:02:05Z",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 7,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 7
                    },
                    "last_token_usage": {
                        "input_tokens": 7,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 7
                    }
                }
            }
        }),
    ];
    let replacement = root.path().join("rebuilt-rollout.jsonl");
    fs::write(&replacement, records_to_bytes(&rebuilt_records)).unwrap();
    fs::rename(&replacement, &rollout_path).unwrap();

    let scan_id = match handle.request(ScanTrigger::Manual).unwrap() {
        RequestDisposition::Started { scan_id, .. }
        | RequestDisposition::Coalesced {
            followup_scan_id: scan_id,
            ..
        } => scan_id,
    };
    wait_for_scan(&ledger, Some(&scan_id));
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed),
        "scanner failed with {:?}",
        ledger.app_state().unwrap().scan.last_scan_error_code
    );

    let after = Connection::open(&db_path).unwrap();
    let metadata_checkpoint: (i64, String) = after
        .query_row(
            "SELECT parser_version,processing_status FROM codex_source_checkpoints
             WHERE source_file_id=1 AND consumer_kind='metadata'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        metadata_checkpoint,
        (usagi::codex::METADATA_PARSER_VERSION, "ready".to_owned(),)
    );
    let relationship_after: (Option<String>, Option<String>, String) = after
        .query_row(
            "SELECT parent_thread_id,root_session_id,agent_role
             FROM threads WHERE thread_id=?1",
            [child.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(relationship_after, relationship_before);
    let usage_root_after: String = after
        .query_row(
            "SELECT root_session_id FROM usage_events
             WHERE source='codex' AND thread_id=?1
             ORDER BY occurred_at_ms LIMIT 1",
            [child.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(usage_root_after, usage_root_before);
    handle.shutdown().unwrap();
}
