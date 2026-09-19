use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use usagi::{
    domain::{ScanResult, ScanTrigger},
    range::ResolvedDay,
    scanner::{CodexMetadata, RequestDisposition, ScanConfig, ScanCoordinator},
    storage::{Ledger, LedgerOptions},
    usage::{USAGE_PARSER_VERSION, UsageFilter, UsageLedger, analytics::skills_usage_snapshot},
};

const ROOT: &str = "00000000-03e8-7000-8000-000000000001";
const CHILD: &str = "00000000-07d0-7000-8000-000000000002";
const PRE_TURN: &str = "00000000-0898-7000-8000-000000000003";
const CHILD_TURN: &str = "00000000-0bb8-7000-8000-000000000003";
const ROOT_TURN: &str = "00000000-05dc-7000-8000-000000000004";

static TEMP_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let sequence = TEMP_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "usagi-mu04-b03-{label}-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create fixture root");
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
    main_rollout: PathBuf,
    rollout: PathBuf,
}

struct SkillFixture {
    _root: TempRoot,
    home: PathBuf,
    db: PathBuf,
    rollout: PathBuf,
}

impl SkillFixture {
    fn new(skill_name: &str) -> Self {
        let root = TempRoot::new("s07-skills");
        let home = root.path().join("codex");
        fs::create_dir_all(home.join("sessions")).expect("create sessions directory");
        fs::create_dir_all(home.join("archived_sessions"))
            .expect("create archived sessions directory");
        let rollout = home.join(format!("sessions/rollout-{ROOT}.jsonl"));
        fs::write(&rollout, records_to_bytes(&skill_records(skill_name)))
            .expect("write skill rollout fixture");
        write_skill_state(&home, &rollout);
        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            rollout,
        }
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(
            Ledger::open(LedgerOptions::new(&self.db, &self.home)).expect("open skill fixture"),
        )
    }

    fn scanner(&self, ledger: Arc<Ledger>) -> usagi::scanner::ScanHandle {
        ScanCoordinator::start(
            ScanConfig::new(self.home.clone()),
            ledger,
            CodexMetadata::from_home(self.home.clone()),
        )
        .expect("start skill scanner")
    }
}

impl Fixture {
    fn new() -> Self {
        let root = TempRoot::new("history");
        let home = root.path().join("codex");
        fs::create_dir_all(home.join("sessions")).expect("create sessions directory");
        fs::create_dir_all(home.join("archived_sessions"))
            .expect("create archived sessions directory");

        let main_rollout = home.join(format!("sessions/rollout-{ROOT}.jsonl"));
        fs::write(&main_rollout, records_to_bytes(&main_records()))
            .expect("write main rollout fixture");
        let records = records();
        let bytes = records_to_bytes(&records);
        let rollout = home.join(format!("sessions/rollout-{CHILD}.jsonl"));
        fs::write(&rollout, &bytes).expect("write rollout fixture");
        write_state(&home, &main_rollout, &rollout);
        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            main_rollout,
            rollout,
        }
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(
            Ledger::open(LedgerOptions::new(&self.db, &self.home)).expect("open fixture ledger"),
        )
    }

    fn scanner(&self, ledger: Arc<Ledger>) -> usagi::scanner::ScanHandle {
        ScanCoordinator::start(
            ScanConfig::new(self.home.clone()),
            ledger,
            CodexMetadata::from_home(self.home.clone()),
        )
        .expect("start scanner")
    }
}

fn records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {
                "id": CHILD,
                "cwd": "/work/child",
                "parent_thread_id": ROOT,
                "source": {"subagent": {"other": "guardian"}}
            }
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {"type": "turn_started", "turn_id": PRE_TURN}
        }),
        token(5, 5, "2026-08-08T01:00:01Z", "B03_PRE_CONTEXT"),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {"type": "turn_complete", "turn_id": PRE_TURN}
        }),
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:02Z",
            "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:03Z",
            "payload": {"turn_id": ROOT_TURN, "model": "replayed-root", "effort": "low"}
        }),
        token(100, 100, "2026-08-08T01:00:04Z", "B03_REPLAY_TOKEN"),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:05Z",
            "payload": {"turn_id": CHILD_TURN, "model": "gpt-5.6-sol", "effort": "medium"}
        }),
        token(12, 7, "2026-08-08T01:00:06Z", "B03_POST_CONTEXT"),
    ]
}

fn skill_records(skill_name: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {"id": ROOT, "cwd": "/work/skill", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {"turn_id": "s07-skill-turn", "model": "skill-model"}
        }),
        token(10, 10, "2026-08-08T01:00:02Z", "S07_SKILL_TOKEN"),
        json!({
            "type": "response_item",
            "timestamp": "2026-08-08T01:00:03Z",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "arguments": json!({
                    "cmd": format!("cat /tmp/.codex/skills/{skill_name}/SKILL.md")
                }).to_string()
            }
        }),
    ]
}

fn main_records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T00:59:00Z",
            "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
        }),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T00:59:01Z",
            "payload": {"type": "turn_started", "turn_id": PRE_TURN}
        }),
        token(5, 5, "2026-08-08T00:59:02Z", "B03_TRUE_UNRESOLVED"),
        json!({
            "type": "event_msg",
            "timestamp": "2026-08-08T00:59:03Z",
            "payload": {"type": "turn_complete", "turn_id": PRE_TURN}
        }),
    ]
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

fn records_to_bytes(records: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record).expect("serialize fixture record"));
        bytes.push(b'\n');
    }
    bytes
}

fn write_state(home: &Path, main_rollout: &Path, child_rollout: &Path) {
    let connection = Connection::open(home.join("state_5.sqlite")).expect("open state fixture");
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
        .expect("create state fixture tables");
    connection
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,1,2,0,'/work/root','Root',NULL,'root-model','main')",
            params![ROOT, main_rollout.to_str().expect("main rollout path")],
        )
        .expect("insert root state");
    connection
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,3,4,0,'/work/child','Child',NULL,'child-model','subagent')",
            params![CHILD, child_rollout.to_str().expect("child rollout path")],
        )
        .expect("insert child state");
    connection
        .execute(
            "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
             VALUES (?1,?2,'spawned',3)",
            params![ROOT, CHILD],
        )
        .expect("insert spawn edge");

    let mut index = Vec::new();
    for id in [ROOT, CHILD] {
        index.extend(
            serde_json::to_vec(&json!({"id": id, "thread_name": format!("name-{id}")}))
                .expect("serialize session index"),
        );
        index.push(b'\n');
    }
    fs::write(home.join("session_index.jsonl"), index).expect("write session index");
}

fn write_skill_state(home: &Path, rollout: &Path) {
    let connection = Connection::open(home.join("state_5.sqlite")).expect("open state fixture");
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
        .expect("create skill state tables");
    connection
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,1,2,0,'/work/skill','Skill',NULL,'skill-model','main')",
            params![ROOT, rollout.to_str().expect("skill rollout path")],
        )
        .expect("insert skill state thread");
    let mut index = serde_json::to_vec(&json!({"id": ROOT, "thread_name": format!("name-{ROOT}")}))
        .expect("serialize skill session index");
    index.push(b'\n');
    fs::write(home.join("session_index.jsonl"), index).expect("write skill session index");
}

fn wait_scan(ledger: &Ledger, wanted: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let scan = ledger.app_state().expect("read scanner state").scan;
        let finished = wanted.is_none_or(|id| scan.last_finished_scan_id.as_deref() == Some(id));
        if finished && scan.active_scan_id.is_none() && scan.last_finished_scan_id.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "scan timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn request_and_wait(handle: &usagi::scanner::ScanHandle, ledger: &Ledger) {
    let scan_id = loop {
        match handle.request(ScanTrigger::Manual) {
            Ok(RequestDisposition::Started { scan_id, .. }) => break scan_id,
            Ok(RequestDisposition::Coalesced {
                followup_scan_id, ..
            }) => break followup_scan_id,
            Err(usagi::scanner::ScanRequestError::Recovering) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("request scanner round failed: {error:?}"),
        }
    };
    wait_scan(ledger, Some(&scan_id));
}

fn source_id(connection: &Connection, thread_id: &str) -> i64 {
    connection
        .query_row(
            "SELECT source_file_id FROM source_files WHERE thread_id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .expect("read source id")
}

fn active_events(
    connection: &Connection,
    epoch: i64,
) -> Vec<(i64, i64, String, i64, Option<String>)> {
    let mut statement = connection
        .prepare(
            "SELECT source_file_id,source_start_offset,model,total_tokens,reasoning_effort
             FROM usage_events WHERE ledger_epoch=?1 ORDER BY source_file_id,source_start_offset",
        )
        .expect("prepare active event query");
    statement
        .query_map([epoch], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query active events")
        .map(|row| row.expect("read active event"))
        .collect()
}

fn seed_parser9_skill_fixture(fixture: &SkillFixture) -> (Arc<Ledger>, i64, i64) {
    let ledger = fixture.ledger();
    let scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial skill scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    scanner.shutdown().expect("stop initial skill scanner");

    let connection = Connection::open(&fixture.db).expect("open seeded skill database");
    let (active_epoch, parser_version): (i64, i64) = connection
        .query_row(
            "SELECT usage_active_epoch,usage_parser_version FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read initial skill epoch");
    assert!(active_epoch > 0);
    assert_eq!(parser_version, USAGE_PARSER_VERSION);
    let source_file_id = source_id(&connection, ROOT);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM skill_usage_events
                 WHERE ledger_epoch=?1 AND source_file_id=?2",
                params![active_epoch, source_file_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("count initial skill event"),
        1
    );

    // Fixture seed: this is the parser-9 active state that predates the
    // parser-11 rebuild exercised by the tests below.
    connection
        .execute("UPDATE app_meta SET usage_parser_version=9 WHERE id=1", [])
        .expect("seed parser-9 active metadata");
    connection
        .execute(
            "UPDATE source_checkpoints SET parser_version=9
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [source_file_id],
        )
        .expect("seed parser-9 usage checkpoint");
    connection
        .execute(
            "UPDATE usage_source_states SET usage_parser_version=9
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![active_epoch, source_file_id],
        )
        .expect("seed parser-9 usage source state");
    drop(connection);
    (ledger, active_epoch, source_file_id)
}

fn all_time_skill_day() -> ResolvedDay {
    ResolvedDay {
        date: "fixture".to_owned(),
        start_ms: 0,
        end_ms: i64::MAX,
    }
}

fn skill_names_for_epoch(connection: &Connection, epoch: i64, source_file_id: i64) -> Vec<String> {
    let mut statement = connection
        .prepare(
            "SELECT skill_name FROM skill_usage_events
             WHERE ledger_epoch=?1 AND source_file_id=?2
             ORDER BY skill_name",
        )
        .expect("prepare skill event names");
    statement
        .query_map(params![epoch, source_file_id], |row| row.get(0))
        .expect("query skill event names")
        .map(|row| row.expect("read skill event name"))
        .collect()
}

#[test]
fn t_mu04_b03_parser_v5_shadow_rebuild_repairs_historical_owning_context() {
    let fixture = Fixture::new();
    let ledger = fixture.ledger();
    let first_scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    first_scanner.shutdown().expect("stop initial scanner");

    let connection = Connection::open(&fixture.db).expect("open fixture database");
    let initial_epoch: i64 = connection
        .query_row(
            "SELECT usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read initial epoch");
    assert_eq!(initial_epoch, 1);
    let main_source_id = source_id(&connection, ROOT);
    let child_source_id = source_id(&connection, CHILD);
    let initial_events = active_events(&connection, initial_epoch);
    assert_eq!(
        initial_events.len(),
        2,
        "initial events: {initial_events:?}"
    );
    let main_initial = initial_events
        .iter()
        .find(|row| row.0 == main_source_id)
        .expect("main unresolved event");
    let child_initial = initial_events
        .iter()
        .find(|row| row.0 == child_source_id)
        .expect("child owning event");
    assert_eq!((main_initial.2.as_str(), main_initial.3), ("unknown", 5));
    assert_eq!(
        (
            child_initial.2.as_str(),
            child_initial.3,
            child_initial.4.as_deref()
        ),
        ("gpt-5.6-sol", 7, Some("medium"))
    );
    let raw_before = fs::read(&fixture.rollout).expect("read raw fixture");
    let main_raw_before = fs::read(&fixture.main_rollout).expect("read main fixture");

    // Reconstruct the parser-v4 active epoch that predates the ownership-boundary
    // fix: the genuine pre-context token stays unknown, while the later event
    // has been persisted with the same unknown model and lost effort.
    connection
        .execute("UPDATE app_meta SET usage_parser_version=4 WHERE id=1", [])
        .expect("mark active parser v4");
    connection
        .execute(
            "UPDATE source_checkpoints SET parser_version=4
             WHERE source_file_id=?1 AND consumer_kind='usage'",
            [child_source_id],
        )
        .expect("mark usage checkpoint parser v4");
    connection
        .execute(
            "UPDATE usage_source_states
             SET usage_parser_version=4,canonical_algorithm_version=4,
                 active_model=NULL,active_model_offset=NULL,
                 active_reasoning_effort=NULL,active_reasoning_effort_offset=NULL
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![initial_epoch, child_source_id],
        )
        .expect("remove historical active context");
    connection
        .execute(
            "UPDATE usage_events SET model='unknown',reasoning_effort=NULL
             WHERE ledger_epoch=?1 AND source_file_id=?2 AND model='gpt-5.6-sol'",
            params![initial_epoch, child_source_id],
        )
        .expect("persist historical unknown event");
    let mut expected_old = vec![
        (
            main_source_id,
            main_initial.1,
            "unknown".to_owned(),
            5,
            None,
        ),
        (
            child_source_id,
            child_initial.1,
            "unknown".to_owned(),
            7,
            None,
        ),
    ];
    expected_old.sort_by_key(|row| row.0);
    assert_eq!(active_events(&connection, initial_epoch), expected_old);
    drop(connection);

    let usage = usagi::usage::UsageLedger::new(&ledger);
    let build = usage
        .begin_rebuild(USAGE_PARSER_VERSION, [main_source_id, child_source_id], 10)
        .expect("begin parser-v5 shadow rebuild");
    assert_eq!(build.active_epoch, initial_epoch);
    assert_eq!(build.target_parser_version, USAGE_PARSER_VERSION);
    assert_eq!(build.build_epoch, initial_epoch + 1);
    assert_eq!(build.members.len(), 2);

    // The old active epoch remains the read source until the new build proves
    // every source complete and activation swaps the epoch atomically.
    let connection = Connection::open(&fixture.db).expect("reopen fixture database");
    assert_eq!(active_events(&connection, initial_epoch), expected_old);
    assert_eq!(
        connection
            .query_row(
                "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version
                 FROM app_meta WHERE id=1",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get(2)?
                )),
            )
            .expect("read pre-activation epoch"),
        (initial_epoch, Some(build.build_epoch), 4)
    );
    drop(connection);

    let second_scanner = fixture.scanner(Arc::clone(&ledger));
    request_and_wait(&second_scanner, &ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("read rebuild scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open rebuilt database");
    let (active_epoch, build_epoch, parser_version): (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read rebuilt epoch");
    assert_eq!(
        active_epoch, build.build_epoch,
        "epoch={active_epoch} build={build_epoch:?} parser={parser_version}"
    );
    assert_eq!(build_epoch, None);
    assert_eq!(parser_version, USAGE_PARSER_VERSION);
    assert_eq!(active_events(&connection, active_epoch).len(), 2);
    assert_eq!(
        active_events(&connection, active_epoch)
            .iter()
            .map(|row| row.3)
            .sum::<i64>(),
        12
    );
    let mut expected_new = vec![
        (main_source_id, "unknown".to_owned(), 5, None),
        (
            child_source_id,
            "gpt-5.6-sol".to_owned(),
            7,
            Some("medium".to_owned()),
        ),
    ];
    expected_new.sort_by_key(|row| row.0);
    assert_eq!(
        active_events(&connection, active_epoch)
            .into_iter()
            .map(|(source, _, model, total, effort)| (source, model, total, effort))
            .collect::<Vec<_>>(),
        expected_new
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=?1",
                [active_epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rebuilt occurrences"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(DISTINCT event_id) FROM usage_event_occurrences
                 WHERE ledger_epoch=?1",
                [active_epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rebuilt event IDs"),
        2
    );
    let state: (i64, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT usage_parser_version,active_model,active_reasoning_effort
             FROM usage_source_states WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![active_epoch, child_source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read rebuilt source state");
    assert_eq!(
        state,
        (
            USAGE_PARSER_VERSION,
            Some("gpt-5.6-sol".to_owned()),
            Some("medium".to_owned())
        )
    );
    assert_eq!(
        fs::read(&fixture.rollout).expect("read raw fixture"),
        raw_before
    );
    assert_eq!(
        fs::read(&fixture.main_rollout).expect("read main fixture"),
        main_raw_before
    );
    drop(connection);
    second_scanner.shutdown().expect("stop rebuild scanner");
}

#[test]
fn t_s07_003_rebuild_activation_keeps_parser9_active_until_parser11_completes() {
    assert_eq!(USAGE_PARSER_VERSION, 11);
    let fixture = SkillFixture::new("legacy-skill");
    let (ledger, active_before, source_file_id) = seed_parser9_skill_fixture(&fixture);
    let usage = UsageLedger::new(&ledger);
    let build = usage
        .begin_rebuild(USAGE_PARSER_VERSION, [source_file_id], 10)
        .expect("begin parser-11 rebuild");
    assert_eq!(build.active_epoch, active_before);
    assert_eq!(build.target_parser_version, 11);
    assert_eq!(build.build_epoch, active_before + 1);

    let connection = Connection::open(&fixture.db).expect("open parser-9 rebuild database");
    let before: (i64, Option<i64>, i64, Option<i64>) = connection
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,
                    usage_parser_version,usage_build_parser_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read parser-9 rebuild state");
    assert_eq!(
        before,
        (active_before, Some(build.build_epoch), 9, Some(11))
    );
    drop(connection);

    let before_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read parser-9 Skills readiness");
    assert!(!before_snapshot.value.ready);
    assert_eq!(before_snapshot.value.days[0].total, 0);

    let scanner = fixture.scanner(Arc::clone(&ledger));
    request_and_wait(&scanner, &ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("read parser-11 rebuild state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open activated parser-11 database");
    let after: (i64, Option<i64>, i64, Option<i64>) = connection
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,
                    usage_parser_version,usage_build_parser_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read activated parser-11 state");
    assert_eq!(after, (build.build_epoch, None, 11, None));
    drop(connection);

    let after_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read parser-11 Skills aggregate");
    assert!(after_snapshot.value.ready);
    assert_eq!(after_snapshot.value.days[0].total, 1);
    assert_eq!(
        after_snapshot.value.days[0].skills[0].skill_name,
        "legacy-skill"
    );
    scanner.shutdown().expect("stop parser-11 scanner");
}

#[test]
fn t_s07_004_skill_event_source_replace_clears_old_rows_before_activation() {
    assert_eq!(USAGE_PARSER_VERSION, 11);
    let fixture = SkillFixture::new("legacy-skill");
    let (ledger, active_before, source_file_id) = seed_parser9_skill_fixture(&fixture);
    let replacement_rollout = fixture.rollout.with_extension("replacement.jsonl");
    fs::write(
        &replacement_rollout,
        records_to_bytes(&skill_records("replacement-skill")),
    )
    .expect("write parser-11 replacement rollout");
    fs::rename(&replacement_rollout, &fixture.rollout)
        .expect("atomically replace parser-11 rollout");

    let usage = UsageLedger::new(&ledger);
    let build = usage
        .begin_rebuild(USAGE_PARSER_VERSION, [source_file_id], 10)
        .expect("begin parser-11 skill replacement");
    assert_eq!(build.active_epoch, active_before);
    assert_eq!(build.target_parser_version, 11);

    let connection = Connection::open(&fixture.db).expect("open skill replacement database");
    let old_row: (
        i64,
        i64,
        i64,
        i64,
        String,
        String,
        Option<String>,
        String,
        i64,
    ) = connection
        .query_row(
            "SELECT file_generation,source_start_offset,source_end_offset,occurred_at_ms,
                    thread_id,root_session_id,model,skill_name,created_at_ms
             FROM skill_usage_events
             WHERE ledger_epoch=?1 AND source_file_id=?2",
            params![active_before, source_file_id],
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
                    row.get(8)?,
                ))
            },
        )
        .expect("read parser-9 skill row");
    assert_eq!(old_row.7, "legacy-skill");
    connection
        .execute(
            "INSERT INTO skill_usage_events(
                ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
                occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                build.build_epoch,
                source_file_id,
                old_row.0,
                old_row.1,
                old_row.2,
                old_row.3,
                old_row.4,
                old_row.5,
                old_row.6,
                old_row.7,
                old_row.8,
            ],
        )
        .expect("seed stale build skill row");
    assert_eq!(
        skill_names_for_epoch(&connection, build.build_epoch, source_file_id),
        vec!["legacy-skill"]
    );
    drop(connection);

    usage
        .replace_build_sources(USAGE_PARSER_VERSION, [source_file_id], [source_file_id], 11)
        .expect("replace parser-11 source build");
    let connection = Connection::open(&fixture.db).expect("reopen replaced skill database");
    assert!(skill_names_for_epoch(&connection, build.build_epoch, source_file_id).is_empty());
    assert_eq!(
        skill_names_for_epoch(&connection, active_before, source_file_id),
        vec!["legacy-skill"]
    );
    drop(connection);

    let before_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read pre-activation Skills readiness");
    assert!(!before_snapshot.value.ready);
    assert_eq!(before_snapshot.value.days[0].total, 0);

    let scanner = fixture.scanner(Arc::clone(&ledger));
    request_and_wait(&scanner, &ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("read replacement scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open activated replacement database");
    let (active_epoch, build_epoch, parser_version): (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read activated replacement state");
    assert_eq!(active_epoch, build.build_epoch);
    assert_eq!(build_epoch, None);
    assert_eq!(parser_version, 11);
    assert_eq!(
        skill_names_for_epoch(&connection, active_epoch, source_file_id),
        vec!["replacement-skill"]
    );
    drop(connection);

    let after_snapshot = skills_usage_snapshot(
        ledger.as_ref(),
        &[all_time_skill_day()],
        &UsageFilter::default(),
    )
    .expect("read activated replacement Skills aggregate");
    assert!(after_snapshot.value.ready);
    assert_eq!(after_snapshot.value.days[0].total, 1);
    assert_eq!(
        after_snapshot.value.days[0].skills[0].skill_name,
        "replacement-skill"
    );
    scanner.shutdown().expect("stop replacement scanner");
}
