use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use usagi::{
    api::{AppContext, QueryApi},
    codex::quota::CodexQuotaService,
    domain::{ScanResult, ScanTrigger},
    platform::browser::SystemBrowser,
    platform::file_identity,
    scanner::{CodexMetadata, RequestDisposition, ScanConfig, ScanCoordinator},
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
    usage::{
        CompletionStatus, SessionPageRequest, SummaryQuery, TimeRange, UsageFilter, UsageLedger,
    },
};

const ROOT: &str = "00000000-03e8-7000-8000-000000000001";
const CHILD: &str = "00000000-07d0-7000-8000-000000000002";

fn empty_summary_query(range: TimeRange) -> SummaryQuery {
    SummaryQuery::new(range, UsageFilter::default())
}

struct TempRoot(PathBuf);
impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec04-{label}-{}-{stamp}",
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

struct Fixture {
    _root: TempRoot,
    home: PathBuf,
    db: PathBuf,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("archived_sessions")).unwrap();
        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
        }
    }

    fn rollout(&self, area: &str, name: &str, records: &[Value]) -> PathBuf {
        let path = self.home.join(area).join(name);
        fs::write(&path, records_to_bytes(records)).unwrap();
        path
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(Ledger::open(LedgerOptions::new(&self.db, &self.home)).unwrap())
    }

    fn start(&self, ledger: Arc<Ledger>) -> usagi::scanner::ScanHandle {
        ScanCoordinator::start(
            ScanConfig::new(self.home.clone()),
            ledger,
            CodexMetadata::from_home(self.home.clone()),
        )
        .unwrap()
    }
}

fn records_to_bytes(records: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

fn token(total: i64, last: i64, at: &str, marker: &str) -> Value {
    json!({
        "timestamp": at,
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "marker": marker,
            "info": {
                "total_token_usage": {
                    "input_tokens": total,
                    "cached_input_tokens": 0,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 0,
                    "reasoning_output_tokens": 0,
                    "total_tokens": total
                },
                "last_token_usage": {
                    "input_tokens": last,
                    "cached_input_tokens": 0,
                    "cache_write_input_tokens": 0,
                    "output_tokens": 0,
                    "reasoning_output_tokens": 0,
                    "total_tokens": last
                }
            }
        }
    })
}

fn main_records(marker: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {"id": ROOT, "cwd": "/work/main", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {"turn_id": "turn-main", "model": "model-main"}
        }),
        token(10, 10, "2026-08-08T01:00:02Z", marker),
        json!({"type":"event_msg","payload":{"type":"rate_limits","body":"RATE_LIMIT_BODY_SENTINEL"}}),
        json!({"type":"response_item","payload":{"text":"RESPONSE_BODY_SENTINEL"}}),
    ]
}

fn guardian_records(total_tokens: i64) -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {
                "id": CHILD,
                "cwd": "/work/child",
                "parent_thread_id": ROOT,
                "source": {"subagent": {"other": "guardian"}},
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {
                "turn_id": "00000000-0bb8-7000-8000-000000000003",
                "model": "guardian-model",
            }
        }),
        token(
            total_tokens,
            total_tokens,
            "2026-08-08T01:00:02Z",
            "S7_GUARDIAN_TOKEN",
        ),
    ]
}

fn create_state(home: &Path) -> Connection {
    let connection = Connection::open(home.join("state_5.sqlite")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT NOT NULL, rollout_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER,
                archived INTEGER, cwd TEXT, title TEXT, name TEXT, model TEXT, agent_role TEXT
             );
             CREATE TABLE thread_spawn_edges (
                parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL,
                status TEXT, observed_at_ms INTEGER
             );",
        )
        .unwrap();
    connection
}

fn write_main_state(home: &Path, rollout: &Path) {
    let connection = create_state(home);
    connection.execute(
        "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
         VALUES (?1,?2,1,2,0,'/work/main','Main',NULL,'model-main','main')",
        params![ROOT, rollout.to_str().unwrap()],
    ).unwrap();
    drop(connection);
    write_session_index(home, &[ROOT]);
}

fn write_child_state(home: &Path, rollout: &Path) {
    let connection = create_state(home);
    connection.execute(
        "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
         VALUES (?1,NULL,1,2,0,'/root','Root',NULL,'root-model','main')",
        [ROOT],
    ).unwrap();
    connection.execute(
        "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
         VALUES (?1,?2,3,4,0,'/child','Child',NULL,'child-model','subagent')",
        params![CHILD, rollout.to_str().unwrap()],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
         VALUES (?1,?2,'spawned',3)",
            params![ROOT, CHILD],
        )
        .unwrap();
    drop(connection);
    write_session_index(home, &[ROOT, CHILD]);
}

fn write_guardian_state(home: &Path, rollout: &Path) {
    let connection = create_state(home);
    connection
        .execute(
            "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
             VALUES (?1,NULL,1,2,0,'/work/root','Root',NULL,'root-model','main')",
            [ROOT],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
             VALUES (?1,?2,3,4,0,'/work/child','Child',NULL,'guardian-model','subagent')",
            params![CHILD, rollout.to_str().unwrap()],
        )
        .unwrap();
    // Deliberately leave thread_spawn_edges empty: Guardian direct-parent
    // provenance must be sufficient to repair the blocked relationship.
    drop(connection);
    write_session_index(home, &[ROOT, CHILD]);
}

fn write_session_index(home: &Path, ids: &[&str]) {
    let mut bytes = Vec::new();
    for id in ids {
        let value = json!({"id": id, "thread_name": format!("name-{id}")});
        bytes.extend(serde_json::to_vec(&value).unwrap());
        bytes.push(b'\n');
    }
    fs::write(home.join("session_index.jsonl"), bytes).unwrap();
}

fn seed_v3_guardian_database(fixture: &Fixture, rollout: &Path) {
    let file = fs::File::open(rollout).unwrap();
    let metadata = file.metadata().unwrap();
    let identity = file_identity::identity_from_file(&file).unwrap();
    let device_id = i64::try_from(identity.device_id).unwrap();
    let inode = i64::try_from(identity.inode).unwrap();
    let observed_mtime_ns = file_identity::modified_ns(&metadata).unwrap();
    let observed_size = i64::try_from(metadata.len()).unwrap();
    let path = rollout.to_str().unwrap();
    let connection = Connection::open(&fixture.db).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    connection
        .execute_batch(include_str!("../src/storage/schema/0001_initial.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!("../src/storage/schema/0002_usage_ledger.sql"))
        .unwrap();
    connection
        .execute_batch(include_str!(
            "../src/storage/schema/0003_normalized_token_usage.sql"
        ))
        .unwrap();
    connection
        .execute(
            "INSERT INTO threads(
                thread_id,parent_thread_id,root_session_id,agent_role,archived,
                metadata_quality_status,metadata_resolved_at_ms
             ) VALUES (?1,NULL,?1,'main',0,'complete',1),
                      (?2,NULL,NULL,'unknown',0,'partial',1)",
            params![ROOT, CHILD],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_files(
                source_file_id,thread_id,current_path,source_area,device_id,inode,
                file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
             ) VALUES (1,?1,?2,'sessions',?3,?4,1,?5,?6,'present',1)",
            params![
                CHILD,
                path,
                device_id,
                inode,
                observed_size,
                observed_mtime_ns
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_checkpoints(
                source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                processing_status,last_successful_scan_at_ms,last_error_code
             ) VALUES (1,'metadata',1,?1,zeroblob(32),'ready',1,NULL)",
            [observed_size],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rollout_metadata_facts(
                source_file_id,file_generation,metadata_parser_version,
                resolved_through_offset,owning_thread_id,continuation_state,
                cwd,cwd_provenance,cwd_record_offset,created_at_ms,
                latest_context_model,latest_context_at_ms,
                parent_thread_id_hint,parent_hint_provenance,parent_hint_record_offset,
                agent_role_hint,agent_role_provenance,agent_role_record_offset,
                replay_start_offset,owning_records_start_offset,
                ownership_confidence,fact_quality_status,updated_at_ms
             ) VALUES (
                1,1,1,?1,?2,'owning_live',
                '/work/child','session_meta',0,1,
                NULL,NULL,
                NULL,NULL,NULL,
                NULL,NULL,NULL,
                NULL,0,'confirmed','complete',1
             )",
            params![observed_size, CHILD],
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA user_version = 3;")
        .unwrap();
}

fn wait_scan(ledger: &Ledger, wanted: Option<&str>) {
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

fn request_and_wait(handle: &usagi::scanner::ScanHandle, ledger: &Ledger) {
    let id = match handle.request(ScanTrigger::Manual).unwrap() {
        RequestDisposition::Started { scan_id, .. } => scan_id,
        RequestDisposition::Coalesced {
            followup_scan_id, ..
        } => followup_scan_id,
    };
    wait_scan(ledger, Some(&id));
}

#[test]
fn t_s04_053_full_incident_replays_guardian_repairs_blocked_build_and_activates_atomically() {
    let fixture = Fixture::new("s7-full-incident");
    let records = guardian_records(9);
    let rollout = fixture.rollout("sessions", &format!("rollout-{CHILD}.jsonl"), &records);
    let bytes = records_to_bytes(&records);
    write_guardian_state(&fixture.home, &rollout);
    seed_v3_guardian_database(&fixture, &rollout);

    // Opening this fixture is the only migration step in the chain: the
    // runtime must migrate the incident-shaped v3 database before any S7
    // rebuild work is started.
    let ledger = fixture.ledger();
    let user_version: i64 = Connection::open(&fixture.db)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 10);
    let source_id = 1_i64;
    let db = Connection::open(&fixture.db).unwrap();
    let old_metadata: (i64, i64, Option<String>, Option<String>, Option<i64>) = db
        .query_row(
            "SELECT c.parser_version,f.metadata_parser_version,
                    f.parent_thread_id_hint,f.parent_hint_provenance,
                    f.parent_hint_record_offset
             FROM source_checkpoints c JOIN rollout_metadata_facts f
               ON f.source_file_id=c.source_file_id
             WHERE c.source_file_id=?1 AND c.consumer_kind='metadata'",
            [source_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(old_metadata, (1, 1, None, None, None));
    drop(db);

    let usage = UsageLedger::new(&ledger);
    let build = usage
        .begin_rebuild(usagi::usage::USAGE_PARSER_VERSION, [source_id], 10)
        .unwrap();
    assert_eq!(build.active_epoch, 0);
    assert_eq!(build.build_epoch, 1);
    assert_eq!(
        build.target_parser_version,
        usagi::usage::USAGE_PARSER_VERSION
    );
    assert_eq!(build.members.len(), 1);
    assert_eq!(
        build.members[0].completion_status,
        CompletionStatus::Blocked
    );
    let expected_root: Option<String> = Connection::open(&fixture.db)
        .unwrap()
        .query_row(
            "SELECT expected_root_session_id FROM usage_build_sources
             WHERE build_epoch=?1 AND source_file_id=?2",
            params![build.build_epoch, source_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(expected_root.is_none());

    // The incomplete manifest cannot be activated by a caller; activation is
    // reached only after metadata reconciliation and a complete usage proof.
    assert!(
        usage
            .activate_rebuild(build.build_epoch, &[source_id])
            .is_err()
    );
    let db = Connection::open(&fixture.db).unwrap();
    let before_scan: (i64, Option<i64>, i64) = db
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(before_scan, (0, Some(1), 0));
    let usage_checkpoint_before: (i64, i64, String) = db
        .query_row(
            "SELECT parser_version,committed_offset,processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        usage_checkpoint_before,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            0,
            "rebuild_required".to_owned()
        )
    );
    drop(db);

    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed),
        "scanner failed with {:?}",
        ledger.app_state().unwrap().scan.last_scan_error_code
    );

    let db = Connection::open(&fixture.db).unwrap();
    let after_scan: (i64, Option<i64>, i64, i64) = db
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version,data_revision
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after_scan.0, 1);
    assert!(after_scan.1.is_none());
    assert_eq!(after_scan.2, usagi::usage::USAGE_PARSER_VERSION);
    assert!(after_scan.3 > 0);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM usage_build_sources WHERE build_epoch=1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    let metadata_checkpoint: (i64, i64, String) = db
        .query_row(
            "SELECT parser_version,committed_offset,processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='metadata'",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        metadata_checkpoint,
        (
            usagi::codex::METADATA_PARSER_VERSION,
            bytes.len() as i64,
            "ready".to_owned()
        )
    );
    let fact_after: (i64, Option<String>, Option<String>, Option<i64>) = db
        .query_row(
            "SELECT metadata_parser_version,parent_thread_id_hint,
                    parent_hint_provenance,parent_hint_record_offset
             FROM rollout_metadata_facts WHERE source_file_id=?1",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        fact_after,
        (
            usagi::codex::METADATA_PARSER_VERSION,
            Some(ROOT.to_owned()),
            Some("session_meta_parent".to_owned()),
            Some(0),
        )
    );
    let thread_after: (Option<String>, Option<String>, String) = db
        .query_row(
            "SELECT parent_thread_id,root_session_id,agent_role
             FROM threads WHERE thread_id=?1",
            [CHILD],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        thread_after,
        (
            Some(ROOT.to_owned()),
            Some(ROOT.to_owned()),
            "subagent".to_owned()
        )
    );
    let usage_checkpoint_after: (i64, i64, String) = db
        .query_row(
            "SELECT parser_version,committed_offset,processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        usage_checkpoint_after,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            bytes.len() as i64,
            "ready".to_owned()
        )
    );
    let usage_state: (i64, i64, String, String, String) = db
        .query_row(
            "SELECT usage_parser_version,resolved_through_offset,
                    owning_thread_id,root_session_id,raw_tail_status
             FROM usage_source_states
             WHERE ledger_epoch=1 AND source_file_id=?1",
            [source_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        usage_state,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            bytes.len() as i64,
            CHILD.to_owned(),
            ROOT.to_owned(),
            "none".to_owned()
        )
    );
    let usage_event: (String, String, i64, String) = db
        .query_row(
            "SELECT thread_id,root_session_id,total_tokens,model
             FROM usage_events WHERE ledger_epoch=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        usage_event,
        (
            CHILD.to_owned(),
            ROOT.to_owned(),
            9,
            "guardian-model".to_owned()
        )
    );
    drop(db);

    // The read seams all use the newly active epoch and agree on totals.
    let range = TimeRange::new(0, i64::MAX).unwrap();
    let summary = usage.summary(empty_summary_query(range)).unwrap();
    assert_eq!(summary.totals.total_tokens, 9);
    assert_eq!(summary.session_count, 1);
    let models = usage.models(range).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model, "guardian-model");
    assert_eq!(models[0].totals.total_tokens, 9);
    let sessions = usage.sessions(range, SessionPageRequest::new(10)).unwrap();
    assert_eq!(sessions.rows.len(), 1);
    assert_eq!(sessions.rows[0].root_session_id, ROOT);
    assert_eq!(sessions.rows[0].inclusive_usage.total_tokens, 9);
    handle.shutdown().unwrap();
}

#[test]
fn t_mu03_f02_v5_upgrade_rebuilds_metadata_usage_and_cost_without_loss() {
    let fixture = Fixture::new("mu03-f02-upgrade");
    let mut records = guardian_records(9);
    records[1]["payload"]["model"] = json!("gpt-5.6-luna");
    records[1]["payload"]["effort"] = json!("high");
    let rollout = fixture.rollout("sessions", &format!("rollout-{CHILD}.jsonl"), &records);
    write_guardian_state(&fixture.home, &rollout);
    seed_v3_guardian_database(&fixture, &rollout);

    let bytes = records_to_bytes(&records);
    let db = Connection::open(&fixture.db).unwrap();
    // Start from the legacy schema-v5 boundary so the runtime exercises only
    // the v6/v7 migrations here while preserving old parser state.
    db.execute_batch(include_str!(
        "../src/storage/schema/0004_metadata_parent_v2_cleanup.sql"
    ))
    .unwrap();
    db.execute_batch(include_str!("../src/storage/schema/0005_project_kind.sql"))
        .unwrap();
    db.pragma_update(None, "user_version", 5_i64).unwrap();
    db.execute(
        "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=3 WHERE id=1",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE source_checkpoints SET parser_version=2
         WHERE source_file_id=1 AND consumer_kind='metadata'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE rollout_metadata_facts SET metadata_parser_version=2
         WHERE source_file_id=1",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO source_checkpoints(
            source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
            processing_status,last_successful_scan_at_ms,last_error_code
         ) VALUES (1,'usage',3,0,NULL,'pending',NULL,NULL)",
        [],
    )
    .unwrap();
    let event_id = "f".repeat(64);
    db.execute(
        "INSERT INTO usage_events(
            ledger_epoch,event_id,event_kind,occurred_at_ms,thread_id,root_session_id,
            turn_key,model,input_tokens,cached_tokens,cache_write_tokens,output_tokens,
            reasoning_tokens,total_tokens,quality_status,source_file_id,file_generation,
            source_start_offset,source_end_offset,created_at_ms
         ) VALUES (1,?1,'normal',10,?2,?3,'turn-old','gpt-5.6-luna',9,0,0,0,0,9,
                   'complete',1,1,0,?4,10)",
        params![event_id, CHILD, ROOT, i64::try_from(bytes.len()).unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO usage_event_occurrences(
            ledger_epoch,source_file_id,file_generation,source_start_offset,
            source_end_offset,event_id,created_at_ms
         ) VALUES (1,1,1,0,?1,?2,10)",
        params![i64::try_from(bytes.len()).unwrap(), event_id],
    )
    .unwrap();
    drop(db);

    // Opening a schema-v5 database performs every migration through the
    // current schema-v10 boundary, including the independent cost backfill,
    // before scanner metadata/usage rebuilds run.
    let ledger = fixture.ledger();
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        10
    );
    let backfilled_cost: Option<i64> = db
        .query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events WHERE ledger_epoch=1 AND event_id=?1",
            [&event_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        backfilled_cost.is_some(),
        "known legacy model must be repriced"
    );
    drop(db);

    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let db = Connection::open(&fixture.db).unwrap();
    let active_epoch: i64 = db
        .query_row(
            "SELECT usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let active: (i64, i64, Option<i64>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT count(*),COALESCE(SUM(total_tokens),0),
                    MIN(estimated_cost_nanos_usd),MIN(model),MIN(reasoning_effort)
             FROM usage_events WHERE ledger_epoch=?1",
            [active_epoch],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(active.0, 1);
    assert_eq!(active.1, 9, "usage rebuild must preserve historical tokens");
    assert!(active.2.is_some(), "rebuilt known model remains billable");
    assert_eq!(active.3.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(active.4.as_deref(), Some("high"));
    let metadata_parser: i64 = db
        .query_row(
            "SELECT parser_version FROM source_checkpoints
             WHERE source_file_id=1 AND consumer_kind='metadata'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let usage_parser: i64 = db
        .query_row(
            "SELECT parser_version FROM source_checkpoints
             WHERE source_file_id=1 AND consumer_kind='usage'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(metadata_parser, usagi::codex::METADATA_PARSER_VERSION);
    assert_eq!(usage_parser, usagi::usage::USAGE_PARSER_VERSION);
    drop(db);
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_mu03_s03_usage_v3_to_v5_rebuild_uses_rollout_effort_and_preserves_tokens() {
    let fixture = Fixture::new("mu03-s03-usage-rebuild");
    let mut historical_records = main_records("s03-history");
    historical_records[1]["payload"]["model"] = json!("gpt-5.6-sol");
    historical_records.extend([
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:03Z",
            "payload": {"turn_id": "turn-main-effort", "model": "gpt-5.6-sol", "effort": " HIGH "}
        }),
        token(20, 10, "2026-08-08T01:00:04Z", "s03-history-effort"),
    ]);
    let rollout = fixture.rollout(
        "sessions",
        &format!("rollout-{ROOT}.jsonl"),
        &historical_records,
    );
    let raw_before = fs::read(&rollout).unwrap();
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    // The same raw two-event history is retained while the active epoch is
    // marked as the legacy parser/canonical v3 baseline.
    let db = Connection::open(&fixture.db).unwrap();
    db.execute("UPDATE app_meta SET usage_parser_version=3 WHERE id=1", [])
        .unwrap();
    db.execute(
        "UPDATE source_checkpoints SET parser_version=3
         WHERE source_file_id=1 AND consumer_kind='usage'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE usage_source_states SET usage_parser_version=3,canonical_algorithm_version=3
         WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)
           AND source_file_id=1",
        [],
    )
    .unwrap();
    let baseline: (i64, i64, i64, i64, i64) = db
        .query_row(
            "SELECT count(*),COALESCE(SUM(total_tokens),0),
                    SUM(CASE WHEN reasoning_effort IS NULL THEN 1 ELSE 0 END),
                    SUM(CASE WHEN reasoning_effort='high' THEN 1 ELSE 0 END),
                    (SELECT usage_parser_version FROM app_meta WHERE id=1)
             FROM usage_events WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(baseline, (2, 20, 1, 1, 3));
    assert_eq!(fs::read(&rollout).unwrap(), raw_before);
    drop(db);

    // A stale state_5 effort is deliberately misleading; the rebuild must
    // read effort only from the unchanged rollout turn_context records.
    let state = Connection::open(fixture.home.join("state_5.sqlite")).unwrap();
    state
        .execute("ALTER TABLE threads ADD COLUMN reasoning_effort TEXT", [])
        .unwrap();
    state
        .execute(
            "UPDATE threads SET reasoning_effort='low' WHERE id=?1",
            [ROOT],
        )
        .unwrap();
    drop(state);
    let db = Connection::open(&fixture.db).unwrap();
    db.execute("UPDATE app_meta SET usage_parser_version=3 WHERE id=1", [])
        .unwrap();
    db.execute(
        "UPDATE source_checkpoints SET parser_version=3,committed_offset=0,
                guard_hash=NULL,processing_status='pending'
         WHERE source_file_id=1 AND consumer_kind='usage'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE usage_source_states SET usage_parser_version=3,canonical_algorithm_version=3
         WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)
           AND source_file_id=1",
        [],
    )
    .unwrap();
    drop(db);

    request_and_wait(&scanner, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    let active_epoch: i64 = db
        .query_row(
            "SELECT usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let after: (i64, i64, i64, i64, i64, i64) = db
        .query_row(
            "SELECT count(*),COALESCE(SUM(total_tokens),0),
                    SUM(CASE WHEN reasoning_effort IS NULL THEN 1 ELSE 0 END),
                    SUM(CASE WHEN reasoning_effort='high' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN reasoning_effort='low' THEN 1 ELSE 0 END),
                    (SELECT usage_parser_version FROM app_meta WHERE id=1)
             FROM usage_events WHERE ledger_epoch=?1",
            [active_epoch],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        after,
        (2, baseline.1, 1, 1, 0, usagi::usage::USAGE_PARSER_VERSION,)
    );
    assert_eq!(fs::read(&rollout).unwrap(), raw_before);

    let detail = UsageLedger::new(&ledger)
        .session_detail_snapshot(
            TimeRange::new(0, i64::MAX).unwrap(),
            UsageFilter::default(),
            None,
            ROOT.to_owned(),
        )
        .unwrap()
        .value;
    assert_eq!(detail.main.model_usage.len(), 2);
    assert_eq!(detail.main.self_usage.total_tokens, baseline.1);
    assert!(
        detail
            .main
            .model_usage
            .iter()
            .all(|block| block.usage.total_tokens == 10)
    );
    assert!(
        detail
            .main
            .model_usage
            .iter()
            .any(|block| block.reasoning_effort.is_none())
    );
    assert!(detail.main.model_usage.iter().any(|block| {
        block.reasoning_effort.as_deref() == Some("high") && block.usage.total_tokens == 10
    }));

    let static_dir = fixture._root.path().join("static");
    fs::create_dir_all(&static_dir).unwrap();
    fs::write(static_dir.join("index.html"), "<html>s03</html>").unwrap();
    let codex_quota_service = CodexQuotaService::unavailable(ledger.codex_home());
    let app = QueryApi::router(
        AppContext {
            ledger: Arc::clone(&ledger),
            scanner: scanner.clone(),
            codex_quota_service,
            update_service: UpdateService::unavailable(),
            browser_opener: Arc::new(SystemBrowser),
        },
        static_dir,
    )
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/usage/sessions/{ROOT}/detail?range=year"))
                .header("host", "127.0.0.1:3210")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let api_detail: Value = serde_json::from_slice(&body).unwrap();
    let api_blocks = api_detail["main"]["model_usage"].as_array().unwrap();
    assert_eq!(api_blocks.len(), 2);
    assert!(api_blocks.iter().all(|block| {
        block["usage"]["total_tokens"] == 10 && block["reasoning_effort"] != "low"
    }));
    assert!(
        api_blocks
            .iter()
            .any(|block| block["reasoning_effort"].is_null())
    );
    assert!(
        api_blocks
            .iter()
            .any(|block| block["reasoning_effort"] == "high")
    );
    scanner.shutdown().unwrap();
}

#[test]
fn t_mu03_s02_version_upgrades_remain_independent() {
    let fixture = Fixture::new("mu03-s02-independent-upgrades");
    let mut records = main_records("s02");
    records[1]["payload"]["model"] = json!("gpt-5.6-sol");
    let rollout = fixture.rollout("sessions", &format!("rollout-{ROOT}.jsonl"), &records);
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    let db = Connection::open(&fixture.db).unwrap();
    let initial: (i64, i64, i64, i64, i64) = db
        .query_row(
            "SELECT usage_active_epoch,usage_parser_version,cost_algorithm_version,
                    pricing_catalog_version,data_revision FROM app_meta WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(initial.1, usagi::usage::USAGE_PARSER_VERSION);
    assert_eq!((initial.2, initial.3), (1, 4));
    let initial_tokens: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM usage_events WHERE ledger_epoch=?1",
            [initial.0],
            |row| row.get(0),
        )
        .unwrap();
    drop(db);

    // Metadata v2 -> v3 replay changes only metadata facts/title; usage epoch
    // and event totals remain untouched.
    let db = Connection::open(&fixture.db).unwrap();
    db.execute(
        "UPDATE source_checkpoints SET parser_version=2
         WHERE source_file_id=1 AND consumer_kind='metadata'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE rollout_metadata_facts SET metadata_parser_version=2
         WHERE source_file_id=1",
        [],
    )
    .unwrap();
    drop(db);
    request_and_wait(&scanner, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    let metadata_only: (i64, i64, i64) = db
        .query_row(
            "SELECT usage_active_epoch,usage_parser_version,
                    (SELECT parser_version FROM source_checkpoints
                     WHERE source_file_id=1 AND consumer_kind='metadata')
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let tokens_after_metadata: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM usage_events WHERE ledger_epoch=?1",
            [metadata_only.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        metadata_only,
        (
            initial.0,
            usagi::usage::USAGE_PARSER_VERSION,
            usagi::codex::METADATA_PARSER_VERSION,
        )
    );
    assert_eq!(tokens_after_metadata, initial_tokens);
    drop(db);

    // Cost reprice is a derived-metric refresh and does not create a usage
    // rebuild or consume either consumer checkpoint.
    scanner.shutdown().unwrap();
    drop(ledger);
    let db = Connection::open(&fixture.db).unwrap();
    db.execute(
        "UPDATE app_meta SET cost_algorithm_version=0,pricing_catalog_version=0 WHERE id=1",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE usage_events SET estimated_cost_nanos_usd=NULL WHERE ledger_epoch=?1",
        [initial.0],
    )
    .unwrap();
    drop(db);
    let ledger = fixture.ledger();
    let db = Connection::open(&fixture.db).unwrap();
    let cost_only: (i64, i64, i64, Option<i64>) = db
        .query_row(
            "SELECT usage_active_epoch,cost_algorithm_version,pricing_catalog_version,
                    (SELECT estimated_cost_nanos_usd FROM usage_events
                     WHERE ledger_epoch=?1 LIMIT 1)
             FROM app_meta WHERE id=1",
            [initial.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(cost_only.0, initial.0);
    assert_eq!((cost_only.1, cost_only.2), (1, 4));
    assert!(cost_only.3.is_some());
    drop(db);

    // Finally force a usage parser/canonical v3 -> v5 shadow rebuild.  The
    // metadata checkpoint stays at v3 and cost versions remain current.
    let db = Connection::open(&fixture.db).unwrap();
    db.execute("UPDATE app_meta SET usage_parser_version=3 WHERE id=1", [])
        .unwrap();
    db.execute(
        "UPDATE source_checkpoints SET parser_version=3,committed_offset=0,
                guard_hash=NULL,processing_status='pending'
         WHERE source_file_id=1 AND consumer_kind='usage'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE usage_source_states SET usage_parser_version=3,canonical_algorithm_version=3
         WHERE ledger_epoch=?1 AND source_file_id=1",
        [initial.0],
    )
    .unwrap();
    drop(db);
    let scanner = fixture.start(Arc::clone(&ledger));
    let usage_scan = loop {
        match scanner.request(ScanTrigger::Manual) {
            Ok(RequestDisposition::Started { scan_id, .. }) => break scan_id,
            Ok(RequestDisposition::Coalesced {
                followup_scan_id, ..
            }) => break followup_scan_id,
            Err(usagi::scanner::ScanRequestError::Recovering) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("usage upgrade scan request failed: {error:?}"),
        }
    };
    wait_scan(&ledger, Some(&usage_scan));
    let db = Connection::open(&fixture.db).unwrap();
    let usage_only: (i64, i64, i64, i64, i64) = db
        .query_row(
            "SELECT usage_active_epoch,usage_parser_version,cost_algorithm_version,
                    pricing_catalog_version,
                    (SELECT parser_version FROM source_checkpoints
                     WHERE source_file_id=1 AND consumer_kind='metadata')
             FROM app_meta WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_ne!(usage_only.0, initial.0);
    assert_eq!(usage_only.1, usagi::usage::USAGE_PARSER_VERSION);
    assert_eq!((usage_only.2, usage_only.3), (1, 4));
    assert_eq!(usage_only.4, usagi::codex::METADATA_PARSER_VERSION);
    scanner.shutdown().unwrap();
}

#[test]
fn t_s04_010_026_035_046_047_real_scanner_builds_active_usage_and_dedupes_archive_copy() {
    let fixture = Fixture::new("main-e2e");
    let rollout = fixture.rollout(
        "sessions",
        "rollout-main.jsonl",
        &main_records("PRIVATE_USAGE_SENTINEL"),
    );
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger.app_state().unwrap().scan.last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let db = Connection::open(&fixture.db).unwrap();
    let app: (i64, Option<i64>, i64) = db.query_row(
        "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version FROM app_meta WHERE id=1",
        [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(app.0, 1);
    assert!(app.1.is_none());
    assert_eq!(app.2, usagi::usage::USAGE_PARSER_VERSION);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM usage_events WHERE ledger_epoch=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let revision_before: i64 = db
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    drop(db);

    // Unchanged scan must be idempotent.
    request_and_wait(&handle, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        revision_before
    );
    drop(db);

    // Archive copy has different provenance but identical canonical anchors.
    fs::copy(
        &rollout,
        fixture
            .home
            .join("archived_sessions/rollout-main-copy.jsonl"),
    )
    .unwrap();
    request_and_wait(&handle, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_event_occurrences", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let raw = fs::read(&fixture.db).unwrap();
    let rendered = String::from_utf8_lossy(&raw);
    for sentinel in [
        "PRIVATE_USAGE_SENTINEL",
        "RATE_LIMIT_BODY_SENTINEL",
        "RESPONSE_BODY_SENTINEL",
    ] {
        assert!(
            !rendered.contains(sentinel),
            "privacy sentinel leaked into SQLite"
        );
    }
    drop(db);

    let usage = UsageLedger::new(&ledger);
    let summary = usage
        .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    assert_eq!(summary.totals.total_tokens, 10);
    assert_eq!(summary.session_count, 1);
    let page = usage
        .sessions(
            TimeRange::new(0, i64::MAX).unwrap(),
            SessionPageRequest::new(10),
        )
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].root_session_id, ROOT);
    handle.shutdown().unwrap();
}

#[test]
fn t_s04_018_t_s02_020_real_scanner_excludes_parent_replay_and_counts_child_after_owning_live() {
    let fixture = Fixture::new("subagent-ownership");
    let records = vec![
        json!({"type":"session_meta","payload":{"id":CHILD,"cwd":"/child","source":{"subagent":{"thread_spawn":{"parent_thread_id":ROOT,"depth":1}}}}}),
        json!({"type":"session_meta","payload":{"id":ROOT,"cwd":"/root"}}),
        json!({"type":"turn_context","timestamp":"2026-08-08T02:00:00Z","payload":{"turn_id":"00000000-05dc-7000-8000-000000000003","model":"root-model"}}),
        token(
            100,
            100,
            "2026-08-08T02:00:01Z",
            "PARENT_REPLAY_TOKEN_SENTINEL",
        ),
        json!({"type":"turn_context","timestamp":"2026-08-08T02:00:02Z","payload":{"turn_id":"00000000-0834-7000-8000-000000000004","model":"child-model"}}),
        token(7, 7, "2026-08-08T02:00:03Z", "CHILD_TOKEN_SENTINEL"),
    ];
    let rollout = fixture.rollout("sessions", "rollout-child.jsonl", &records);
    write_child_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let scan = ledger.app_state().unwrap().scan;
    assert_eq!(
        scan.last_finished_scan_result,
        Some(ScanResult::Completed),
        "scanner failed with {:?}",
        scan.last_scan_error_code
    );

    let db = Connection::open(&fixture.db).unwrap();
    let rows: Vec<(String, String, i64)> = {
        let mut stmt = db.prepare("SELECT thread_id,root_session_id,total_tokens FROM usage_events ORDER BY occurred_at_ms").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(rows, vec![(CHILD.to_owned(), ROOT.to_owned(), 7)]);
    handle.shutdown().unwrap();
}

#[test]
fn t_s04_033_036_037_040_042_missing_source_carries_active_facts_and_reactivates_atomically() {
    let fixture = Fixture::new("carry-missing");
    let rollout = fixture.rollout(
        "sessions",
        "rollout-carry.jsonl",
        &main_records("CARRY_PRIVATE_SENTINEL"),
    );
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    let source_id: i64 = Connection::open(&fixture.db)
        .unwrap()
        .query_row(
            "SELECT source_file_id FROM source_files WHERE current_path=?1",
            [rollout.to_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let before = UsageLedger::new(&ledger)
        .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    let rev_before = ledger.app_state().unwrap().data_revision;

    let build = UsageLedger::new(&ledger)
        .begin_rebuild(usagi::usage::USAGE_PARSER_VERSION, [source_id], 100)
        .unwrap();
    assert_eq!(build.build_epoch, 2);
    let db = Connection::open(&fixture.db).unwrap();
    let active_during: i64 = db
        .query_row(
            "SELECT usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active_during, 1);
    drop(db);

    fs::remove_file(&rollout).unwrap();
    request_and_wait(&handle, &ledger);

    let db = Connection::open(&fixture.db).unwrap();
    let epoch: (i64, Option<i64>) = db
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch FROM app_meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(epoch, (2, None));
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=2 AND source_file_id=?1", [source_id], |r| r.get::<_, i64>(0)).unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_source_states WHERE ledger_epoch=2 AND source_file_id=?1 AND raw_tail_status<>'unverified'", [source_id], |r| r.get::<_, i64>(0)).unwrap(),
        1
    );
    drop(db);

    let after = UsageLedger::new(&ledger)
        .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    assert_eq!(after.totals, before.totals);
    assert_eq!(ledger.app_state().unwrap().data_revision, rev_before + 1);
    handle.shutdown().unwrap();
}

#[test]
fn t_s04_007_008_009_013_014_031_032_043_045_047_incremental_recovery_half_line_and_queries() {
    let fixture = Fixture::new("incremental-recovery");
    let rollout = fixture.rollout(
        "sessions",
        "rollout-incremental.jsonl",
        &main_records("BASE"),
    );
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    let source_id: i64 = Connection::open(&fixture.db)
        .unwrap()
        .query_row(
            "SELECT source_file_id FROM source_files WHERE current_path=?1",
            [rollout.to_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let offset_before: i64 = Connection::open(&fixture.db).unwrap()
        .query_row("SELECT committed_offset FROM source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'", [source_id], |r| r.get(0)).unwrap();

    // A truly missing last is recovered from the durable cumulative baseline.
    let recovered = json!({
        "timestamp":"2026-08-08T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":20,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":20}
        }}
    });
    let mut bytes = fs::read(&rollout).unwrap();
    bytes.extend(serde_json::to_vec(&recovered).unwrap());
    bytes.push(b'\n');
    fs::write(&rollout, &bytes).unwrap();
    request_and_wait(&handle, &ledger);
    let usage = UsageLedger::new(&ledger);
    assert_eq!(
        usage
            .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
            .unwrap()
            .totals
            .total_tokens,
        20
    );
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM usage_events WHERE event_kind='recovered'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let after_recovered: i64 = db.query_row("SELECT committed_offset FROM source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'", [source_id], |r| r.get(0)).unwrap();
    assert!(after_recovered > offset_before);
    drop(db);

    // An incomplete final line is durable tail proof only: checkpoint stays at
    // the half-line start and no event is emitted until the line is completed.
    let normal = token(25, 5, "2026-08-08T01:02:00Z", "HALF_LINE_PRIVATE_SENTINEL");
    let encoded = serde_json::to_vec(&normal).unwrap();
    let split = encoded.len() / 2;
    let mut bytes = fs::read(&rollout).unwrap();
    let half_start = bytes.len() as i64;
    bytes.extend_from_slice(&encoded[..split]);
    fs::write(&rollout, &bytes).unwrap();
    request_and_wait(&handle, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    let tail: (i64, String, Option<i64>) = db.query_row(
        "SELECT c.committed_offset,s.raw_tail_status,s.raw_tail_start_offset FROM source_checkpoints c JOIN usage_source_states s ON s.source_file_id=c.source_file_id AND s.ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1) WHERE c.source_file_id=?1 AND c.consumer_kind='usage'",
        [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(tail, (half_start, "half_line".to_owned(), Some(half_start)));
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(db);

    let mut bytes = fs::read(&rollout).unwrap();
    bytes.extend_from_slice(&encoded[split..]);
    bytes.push(b'\n');
    fs::write(&rollout, &bytes).unwrap();
    request_and_wait(&handle, &ledger);

    let summary = usage
        .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    assert_eq!(summary.totals.total_tokens, 25);
    assert_eq!(summary.totals.estimated_cost_nanos_usd, None);
    let sessions = usage
        .sessions(
            TimeRange::new(0, i64::MAX).unwrap(),
            SessionPageRequest::new(1),
        )
        .unwrap();
    assert_eq!(sessions.rows.len(), 1);
    assert_eq!(sessions.rows[0].inclusive_usage.total_tokens, 25);
    let models = usage.models(TimeRange::new(0, i64::MAX).unwrap()).unwrap();
    assert_eq!(
        models
            .iter()
            .map(|row| row.totals.total_tokens)
            .sum::<i64>(),
        25
    );
    let db = Connection::open(&fixture.db).unwrap();
    let final_tail: (i64, i64, String) = db.query_row(
        "SELECT c.committed_offset,s.observed_raw_size,s.raw_tail_status FROM source_checkpoints c JOIN usage_source_states s ON s.source_file_id=c.source_file_id AND s.ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1) WHERE c.source_file_id=?1 AND c.consumer_kind='usage'",
        [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(final_tail.0, final_tail.1);
    assert_eq!(final_tail.2, "none");
    handle.shutdown().unwrap();
}

#[test]
fn t_s04_019_root_unconfirmed_blocks_usage_then_parent_resolution_replays_once() {
    let fixture = Fixture::new("root-unconfirmed");
    let records = vec![
        json!({"type":"session_meta","payload":{"id":CHILD,"cwd":"/child","source":{"subagent":{"thread_spawn":{"parent_thread_id":ROOT,"depth":1}}}}}),
        json!({"type":"turn_context","timestamp":"2026-08-08T03:00:00Z","payload":{"turn_id":"child-turn","model":"child-model"}}),
        token(9, 9, "2026-08-08T03:00:01Z", "ROOT_BLOCK_SENTINEL"),
    ];
    let rollout = fixture.rollout("sessions", "rollout-child-unresolved.jsonl", &records);
    let connection = create_state(&fixture.home);
    connection.execute(
        "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role) VALUES (?1,?2,3,4,0,'/child','Child',NULL,'child-model','subagent')",
        params![CHILD, rollout.to_str().unwrap()],
    ).unwrap();
    drop(connection);
    write_session_index(&fixture.home, &[CHILD]);

    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let usage = UsageLedger::new(&ledger);
    assert_eq!(
        usage
            .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
            .unwrap()
            .totals
            .total_tokens,
        0
    );
    let db = Connection::open(&fixture.db).unwrap();
    let usage_offset: Option<i64> = db.query_row(
        "SELECT committed_offset FROM source_checkpoints c JOIN source_files s USING(source_file_id) WHERE c.consumer_kind='usage' AND s.current_path=?1",
        [rollout.to_str().unwrap()], |r| r.get(0)).optional().unwrap();
    assert!(usage_offset.is_none() || usage_offset == Some(0));
    drop(db);

    let state = Connection::open(fixture.home.join("state_5.sqlite")).unwrap();
    state.execute(
        "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role) VALUES (?1,NULL,1,5,0,'/root','Root',NULL,'root-model','main')",
        [ROOT],
    ).unwrap();
    state.execute(
        "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms) VALUES (?1,?2,'spawned',5)",
        params![ROOT, CHILD],
    ).unwrap();
    drop(state);
    write_session_index(&fixture.home, &[ROOT, CHILD]);
    request_and_wait(&handle, &ledger);

    let summary = usage
        .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    assert_eq!(summary.totals.total_tokens, 9);
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_event_occurrences", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    handle.shutdown().unwrap();
}

#[test]
fn t_s04_030_041_buildfrom_multibatch_and_localreplay_over_budget_promotes_to_shadow_build() {
    use usagi::domain::{CheckpointRebuildCommand, ConsumerKind};

    let fixture = Fixture::new("bounded-multibatch");
    let mut records = vec![
        json!({"type":"session_meta","timestamp":"2026-08-08T04:00:00Z","payload":{"id":ROOT,"cwd":"/work/main","agent_role":"main"}}),
        json!({"type":"turn_context","timestamp":"2026-08-08T04:00:01Z","payload":{"turn_id":"bulk-turn","model":"bulk-model"}}),
    ];
    for i in 1..=2050_i64 {
        records.push(token(i, 1, "2026-08-08T04:00:02Z", "BULK_PRIVATE_SENTINEL"));
    }
    let rollout = fixture.rollout("sessions", "rollout-bulk.jsonl", &records);
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let usage = UsageLedger::new(&ledger);
    assert_eq!(
        usage
            .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
            .unwrap()
            .totals
            .total_tokens,
        2050
    );

    let db = Connection::open(&fixture.db).unwrap();
    let source_id: i64 = db
        .query_row(
            "SELECT source_file_id FROM source_files WHERE current_path=?1",
            [rollout.to_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let epoch_before: i64 = db
        .query_row(
            "SELECT usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let revision_before: i64 = db
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    drop(db);

    ledger
        .require_checkpoint_rebuild(
            CheckpointRebuildCommand::new(ConsumerKind::Usage, vec![source_id]).unwrap(),
        )
        .unwrap();
    request_and_wait(&handle, &ledger);

    let db = Connection::open(&fixture.db).unwrap();
    let (active, build, revision, offset, raw): (i64, Option<i64>, i64, i64, i64) = db.query_row(
        "SELECT a.usage_active_epoch,a.usage_build_epoch,a.data_revision,c.committed_offset,s.observed_size FROM app_meta a,source_checkpoints c JOIN source_files s USING(source_file_id) WHERE a.id=1 AND c.consumer_kind='usage' AND c.source_file_id=?1",
        [source_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
    assert_eq!(
        active,
        epoch_before + 1,
        "over-budget LocalReplay must promote to a shadow build and activate it"
    );
    assert!(build.is_none());
    assert_eq!(
        revision,
        revision_before + 1,
        "failed LocalReplay attempt itself must not change query facts"
    );
    assert_eq!(offset, raw);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM usage_events WHERE ledger_epoch=?1",
            [active],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2050
    );
    assert_eq!(
        usage
            .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
            .unwrap()
            .totals
            .total_tokens,
        2050
    );
    handle.shutdown().unwrap();
}

#[test]
fn t_hf_re04_shadow_rebuild_cross_batch_none_to_single_completes() {
    const TURN: &str = "00000000-0bb8-7000-8000-000000000003";
    const BATCH_LINES: usize = 4096;

    let fixture = Fixture::new("hf-re04-cross-batch");
    let mut initial_records = vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T04:00:00Z",
            "payload": {"id": ROOT, "cwd": "/work/main", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T04:00:01Z",
            "payload": {"turn_id": TURN, "model": "hf-cross-model"}
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T04:00:01Z",
            "payload": {"type": "turn_started", "turn_id": TURN}
        }),
    ];
    // The usage reader's fixed line budget forces the effort-bearing context
    // below into the next real BuildFrom batch after this open Turn is saved.
    initial_records.extend(
        (0..(BATCH_LINES - 2))
            .map(|_| json!({"type":"response_item","payload":{"text":"ignored"}})),
    );
    let initial_bytes = records_to_bytes(&initial_records);
    let initial_offset = i64::try_from(initial_bytes.len()).unwrap();
    let rollout = fixture.rollout(
        "sessions",
        &format!("rollout-{ROOT}.jsonl"),
        &initial_records,
    );
    write_main_state(&fixture.home, &rollout);

    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let first_scan = ledger.app_state().unwrap().scan;
    assert_eq!(
        first_scan.last_finished_scan_result,
        Some(ScanResult::Completed),
        "initial scanner failed with {:?}",
        first_scan.last_scan_error_code
    );

    let db = Connection::open(&fixture.db).unwrap();
    let source_id: i64 = db
        .query_row(
            "SELECT source_file_id FROM source_files WHERE current_path=?1",
            [rollout.to_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let first_epoch: (i64, Option<i64>) = db
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(first_epoch.0 > 0);
    assert!(first_epoch.1.is_none());
    let first_checkpoint: (i64, i64, String) = db
        .query_row(
            "SELECT parser_version,committed_offset,processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        first_checkpoint,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            initial_offset,
            "ready".to_owned()
        )
    );
    let first_turn: (String, Option<String>, i64, i64, i64, String) = db
        .query_row(
            "SELECT reasoning_effort_state,single_reasoning_effort,
                    unresolved_reasoning_effort_seen,accounted_candidate_count,
                    state_through_offset,status
             FROM turns
             WHERE ledger_epoch=?1 AND source_file_id=?2 AND turn_key=?3",
            params![first_epoch.0, source_id, TURN],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        first_turn,
        (
            "none".to_owned(),
            None,
            0,
            0,
            initial_offset,
            "open".to_owned()
        )
    );
    drop(db);

    let appended_records = vec![
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T04:00:02Z",
            "payload": {
                "turn_id": TURN,
                "model": "hf-cross-model",
                "effort": "high"
            }
        }),
        token(10, 10, "2026-08-08T04:00:03Z", "HF_RE04_TOKEN"),
    ];
    let appended_bytes = records_to_bytes(&appended_records);
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    file.write_all(&appended_bytes).unwrap();
    drop(file);

    let usage = UsageLedger::new(&ledger);
    let build = usage
        .begin_rebuild(usagi::usage::USAGE_PARSER_VERSION, [source_id], 100)
        .unwrap();
    assert_eq!(build.active_epoch, first_epoch.0);
    assert!(build.build_epoch > build.active_epoch);
    assert_eq!(build.members.len(), 1);
    assert_eq!(
        build.members[0].completion_status,
        CompletionStatus::Pending
    );
    let required_boundary = build.members[0].required_through_offset;
    assert_eq!(required_boundary, u64::try_from(initial_offset).unwrap());

    let db = Connection::open(&fixture.db).unwrap();
    let rebuild_checkpoint: (i64, i64, String) = db
        .query_row(
            "SELECT parser_version,committed_offset,processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        rebuild_checkpoint,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            0,
            "rebuild_required".to_owned()
        )
    );
    drop(db);

    request_and_wait(&scanner, &ledger);
    let final_scan = ledger.app_state().unwrap().scan;
    assert_eq!(
        final_scan.last_finished_scan_result,
        Some(ScanResult::Completed),
        "shadow rebuild scanner failed with {:?}",
        final_scan.last_scan_error_code
    );

    let final_offset = initial_offset + i64::try_from(appended_bytes.len()).unwrap();
    let db = Connection::open(&fixture.db).unwrap();
    let final_epoch: (i64, Option<i64>) = db
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(final_epoch.0, build.build_epoch);
    assert!(final_epoch.1.is_none());
    // The completed member is consumed by atomic activation; active_epoch and
    // the final ready checkpoint are the durable rebuilt proof afterward.
    let final_checkpoint: (i64, i64, String) = db
        .query_row(
            "SELECT parser_version,committed_offset,processing_status
             FROM source_checkpoints
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        final_checkpoint,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            final_offset,
            "ready".to_owned()
        )
    );
    assert!(final_checkpoint.1 >= i64::try_from(required_boundary).unwrap());
    let final_state_offset: i64 = db
        .query_row(
            "SELECT resolved_through_offset FROM usage_source_states
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![final_epoch.0, source_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(final_state_offset, final_offset);
    let final_turn: (String, Option<String>, i64, i64, i64, String) = db
        .query_row(
            "SELECT reasoning_effort_state,single_reasoning_effort,
                    unresolved_reasoning_effort_seen,accounted_candidate_count,
                    state_through_offset,status
             FROM turns
             WHERE ledger_epoch=?1 AND source_file_id=?2 AND turn_key=?3",
            params![final_epoch.0, source_id, TURN],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        final_turn,
        (
            "single".to_owned(),
            Some("high".to_owned()),
            0,
            1,
            final_offset,
            "open".to_owned()
        )
    );
    let final_event: (i64, Option<String>, i64) = db
        .query_row(
            "SELECT count(*),MIN(reasoning_effort),COALESCE(SUM(total_tokens),0)
             FROM usage_events WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![final_epoch.0, source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(final_event, (1, Some("high".to_owned()), 10));
    drop(db);
    scanner.shutdown().unwrap();
}

#[test]
fn t_s04_019_t_s02_020_late_foreign_meta_discards_preceding_usage_and_starts_rebuild() {
    let fixture = Fixture::new("late-foreign-usage");
    let rollout = fixture.rollout(
        "sessions",
        "rollout-late-foreign.jsonl",
        &main_records("BASE"),
    );
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    let db = Connection::open(&fixture.db).unwrap();
    let source_id: i64 = db
        .query_row(
            "SELECT source_file_id FROM source_files WHERE current_path=?1",
            [rollout.to_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let old_offset: i64 = db.query_row(
        "SELECT committed_offset FROM source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'",
        [source_id], |r| r.get(0),
    ).unwrap();
    drop(db);

    let appended = vec![
        token(
            20,
            10,
            "2026-08-08T01:01:00Z",
            "MUST_ROLL_BACK_BEFORE_FOREIGN",
        ),
        json!({"type":"session_meta","timestamp":"2026-08-08T01:01:01Z","payload":{"id":CHILD,"cwd":"/foreign"}}),
        token(
            30,
            10,
            "2026-08-08T01:01:02Z",
            "MUST_NOT_COUNT_AFTER_FOREIGN",
        ),
    ];
    let mut bytes = fs::read(&rollout).unwrap();
    bytes.extend(records_to_bytes(&appended));
    fs::write(&rollout, bytes).unwrap();
    request_and_wait(&handle, &ledger);

    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM usage_events WHERE ledger_epoch=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1,
        "a token parsed earlier in the unstable chunk must not partially commit"
    );
    let build: (i64, Option<i64>, String, i64) = db.query_row(
        "SELECT
            (SELECT usage_active_epoch FROM app_meta WHERE id=1),
            (SELECT usage_build_epoch FROM app_meta WHERE id=1),
            (SELECT processing_status FROM source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage'),
            (SELECT committed_offset FROM source_checkpoints WHERE source_file_id=?1 AND consumer_kind='usage')",
        [source_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).unwrap();
    assert_eq!(
        build.0, 1,
        "old active ledger remains queryable during rebuild"
    );
    assert_eq!(build.1, Some(2));
    assert_eq!((build.2.as_str(), build.3), ("rebuild_required", 0));
    assert!(old_offset > 0);
    handle.shutdown().unwrap();
}

#[test]
fn t_s04_024_long_subagent_replay_prefix_persists_safe_resume_without_parent_usage() {
    let fixture = Fixture::new("long-replay");
    let mut records = Vec::with_capacity(5_010);
    records.push(json!({"type":"session_meta","payload":{"id":CHILD,"cwd":"/child","source":{"subagent":{"thread_spawn":{"parent_thread_id":ROOT,"depth":1}}}}}));
    records.push(json!({"type":"session_meta","payload":{"id":ROOT,"cwd":"/root"}}));
    records.push(json!({"type":"turn_context","timestamp":"2026-08-08T02:00:00Z","payload":{"turn_id":"00000000-05dc-7000-8000-000000000003","model":"root-model"}}));
    for index in 0..5_000_i64 {
        records.push(token(
            index + 1,
            1,
            "2026-08-08T02:00:01Z",
            "LONG_PARENT_REPLAY",
        ));
    }
    let rollout = fixture.rollout("sessions", "rollout-child-long.jsonl", &records);
    write_child_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    // The child's owning identity is already confirmed even though the fixed
    // view ends while replaying its ancestor. That replay tail is resumable:
    // it may advance durable usage progress, but replayed parent tokens must
    // never become usage events and it must not keep the active epoch at zero.
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let first_size = fs::metadata(&rollout).unwrap().len() as i64;
    let first: (i64, Option<i64>, i64, String, String, i64, String, String) = db
        .query_row(
            "SELECT
                a.usage_active_epoch,
                a.usage_build_epoch,
                c.committed_offset,
                c.processing_status,
                s.continuation_state,
                s.resolved_through_offset,
                s.owning_thread_id,
                s.root_session_id
             FROM app_meta a
             JOIN source_files f ON f.current_path=?1
             JOIN source_checkpoints c
               ON c.source_file_id=f.source_file_id AND c.consumer_kind='usage'
             JOIN usage_source_states s
               ON s.ledger_epoch=a.usage_active_epoch AND s.source_file_id=f.source_file_id
             WHERE a.id=1",
            [rollout.to_str().unwrap()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap();
    assert!(first.0 > 0, "replay tail must not block first activation");
    assert_eq!(first.1, None);
    assert_eq!(first.2, first_size);
    assert_eq!(first.3, "ready");
    assert_eq!(first.4, "replayed_ancestor");
    assert_eq!(first.5, first_size);
    assert_eq!(first.6, CHILD);
    assert_eq!(first.7, ROOT);
    let first_offset = first.2;
    drop(db);

    // When the child later reaches its own live records, resume from the saved
    // replay-tail checkpoint and count only the child request. Parent replay
    // history remains excluded.
    let mut append = records_to_bytes(&[
        json!({"type":"turn_context","timestamp":"2026-08-08T02:00:02Z","payload":{"turn_id":"00000000-0834-7000-8000-000000000004","model":"child-model"}}),
        token(7, 7, "2026-08-08T02:00:03Z", "CHILD_AFTER_LONG_REPLAY"),
    ]);
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    file.write_all(&append).unwrap();
    append.clear();
    drop(file);
    request_and_wait(&handle, &ledger);

    let db = Connection::open(&fixture.db).unwrap();
    let rows: Vec<(String, i64)> = {
        let mut statement = db
            .prepare(
                "SELECT thread_id,total_tokens FROM usage_events ORDER BY occurred_at_ms,event_id",
            )
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(rows, vec![(CHILD.to_owned(), 7)]);
    let final_size = fs::metadata(&rollout).unwrap().len() as i64;
    let resumed: (i64, String) = db
        .query_row(
            "SELECT c.committed_offset,s.continuation_state
             FROM app_meta a
             JOIN source_files f ON f.current_path=?1
             JOIN source_checkpoints c
               ON c.source_file_id=f.source_file_id AND c.consumer_kind='usage'
             JOIN usage_source_states s
               ON s.ledger_epoch=a.usage_active_epoch AND s.source_file_id=f.source_file_id
             WHERE a.id=1",
            [rollout.to_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(resumed.0 > first_offset);
    assert_eq!(resumed.0, final_size);
    assert_eq!(resumed.1, "owning_live");
    handle.shutdown().unwrap();
}

#[test]
fn t_dc_030_runtime_canonical_crud_duplicate_and_reopen() {
    let fixture = Fixture::new("canonical-runtime");
    let mut records = main_records("CANONICAL_RUNTIME");
    records[2]["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    records[2]["payload"]["info"]["last_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    let rollout = fixture.rollout("sessions", "rollout-canonical.jsonl", &records);
    write_main_state(&fixture.home, &rollout);
    let ledger = fixture.ledger();
    let handle = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);

    let db = Connection::open(&fixture.db).unwrap();
    let event: (i64, i64, Option<i64>, i64, i64, i64, String) = db
        .query_row(
            "SELECT input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status FROM usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .unwrap();
    assert_eq!(event, (10, 0, None, 0, 0, 10, "partial".to_owned()));
    let source_state: Option<i64> = db
        .query_row(
            "SELECT previous_total_cache_write_tokens FROM usage_source_states",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_state, None);
    drop(db);

    request_and_wait(&handle, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_events", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM usage_event_occurrences", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    drop(db);
    handle.shutdown().unwrap();
    drop(ledger);
    let reopened = fixture.ledger();
    assert_eq!(
        UsageLedger::new(&reopened)
            .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
            .unwrap()
            .totals
            .cache_write_tokens,
        None
    );
}
