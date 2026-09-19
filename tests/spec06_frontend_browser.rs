use std::{
    fs,
    io::{self, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::serve;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    time::{sleep, timeout},
};
use usagi::{
    api::{AppContext, QueryApi},
    codex::quota::CodexQuotaService,
    domain::ScanResult,
    platform::browser::SystemBrowser,
    platform::file_identity,
    scanner::{CodexMetadata, ScanConfig, ScanCoordinator, ScanHandle, ScanTrigger},
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
    usage::{CompletionStatus, USAGE_PARSER_VERSION, UsageLedger},
};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir();
        for attempt in 0..8 {
            let path = directory.join(format!(
                "usagi-spec06-browser-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate an exclusive browser fixture directory",
        ))
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

fn run_playwright(
    frontend_dir: &Path,
    base_url: &str,
    production: bool,
    control_file: &Path,
) -> Result<(), String> {
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let mut command = Command::new(npm);
    command
        .current_dir(frontend_dir)
        .args(["run", "test:browser"])
        .env("AXUM_BASE_URL", base_url)
        .env("BROWSER_CONTROL_FILE", control_file)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if production {
        command.env("FRONTEND_BASE_URL", base_url);
    } else {
        command.env_remove("FRONTEND_BASE_URL");
    }
    if let Ok(grep) = std::env::var("BROWSER_TEST_GREP") {
        command.args(["--", "--grep"]).arg(grep);
    }
    let status = command
        .status()
        .map_err(|error| format!("could not start Playwright: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Playwright exited with {status}"))
    }
}

fn clear_browser_artifacts(frontend_dir: &Path) {
    let _ = fs::remove_dir_all(frontend_dir.join("test-results"));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureServerCommand {
    Restart,
    Shutdown,
}

async fn bind_fixture_listener() -> TcpListener {
    for _ in 0..200 {
        match TcpListener::bind(("127.0.0.1", 3210)).await {
            Ok(listener) => return listener,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!("Axum fixture could not bind: {error}"),
        }
    }
    panic!("Axum fixture port 3210 remained unavailable");
}

async fn serve_fixture(
    ledger: Arc<Ledger>,
    scanner: ScanHandle,
    static_dir: PathBuf,
    mut commands: UnboundedReceiver<FixtureServerCommand>,
) {
    let mut listener = bind_fixture_listener().await;
    loop {
        let app = QueryApi::router(
            AppContext {
                ledger: Arc::clone(&ledger),
                scanner: scanner.clone(),
                codex_quota_service: CodexQuotaService::unavailable(ledger.codex_home()),
                update_service: UpdateService::unavailable(),
                browser_opener: Arc::new(SystemBrowser),
            },
            static_dir.clone(),
        )
        .expect("real QueryApi router");
        let command = {
            let server = serve(listener, app).into_future();
            tokio::pin!(server);
            tokio::select! {
                result = &mut server => {
                    result.expect("Axum fixture server");
                    return;
                }
                command = commands.recv() => command,
            }
        };
        match command {
            Some(FixtureServerCommand::Restart) => {
                listener = bind_fixture_listener().await;
            }
            Some(FixtureServerCommand::Shutdown) | None => return,
        }
    }
}

async fn watch_fixture_control(
    control_file: PathBuf,
    revision_rollout: PathBuf,
    state_index: PathBuf,
    ledger: Arc<Ledger>,
    scanner: ScanHandle,
    commands: UnboundedSender<FixtureServerCommand>,
) {
    let mut revision_serial = 0_u64;
    loop {
        sleep(Duration::from_millis(20)).await;
        let command = match fs::read_to_string(&control_file) {
            Ok(value) => value.trim().to_owned(),
            Err(_) => continue,
        };
        if command.is_empty() {
            continue;
        }
        let _ = fs::write(&control_file, "");
        match command.as_str() {
            "restart" => {
                if commands.send(FixtureServerCommand::Restart).is_err() {
                    return;
                }
            }
            "revision" => {
                revision_serial += 1;
                let state = match Connection::open(&state_index) {
                    Ok(state) => state,
                    Err(_) => continue,
                };
                let _ = state.execute(
                    "UPDATE threads SET title=?1 WHERE id=?2",
                    params![format!("Main Revision {revision_serial}"), INCIDENT_ROOT],
                );
                drop(state);
                let record = json!({
                    "type": "session_meta",
                    "timestamp": format!("2026-08-08T01:01:{revision_serial:02}Z"),
                    "payload": {
                        "id": INCIDENT_ROOT,
                        "timestamp": "2026-08-07T01:00:00Z",
                    }
                });
                let mut file = match fs::OpenOptions::new().append(true).open(&revision_rollout) {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                let bytes = match serde_json::to_vec(&record) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                };
                if file.write_all(&bytes).is_err() || file.write_all(b"\n").is_err() {
                    continue;
                }
                let _ = scanner.request(ScanTrigger::Manual);
                for _ in 0..500 {
                    sleep(Duration::from_millis(20)).await;
                    let state = ledger.app_state().expect("read browser revision state");
                    if state.scan.active_scan_id.is_none() {
                        break;
                    }
                }
            }
            "shutdown" => {
                let _ = commands.send(FixtureServerCommand::Shutdown);
                return;
            }
            _ => {}
        }
    }
}

async fn wait_for_port(port: u16) {
    for _ in 0..240 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Axum fixture did not bind loopback port {port}");
}

const INCIDENT_ROOT: &str = "00000000-03e8-7000-8000-000000000001";
const INCIDENT_LEGACY: &str = "00000000-03e8-7000-8000-000000000004";
const INCIDENT_CHILD: &str = "00000000-07d0-7000-8000-000000000002";
const SPECIAL_PROJECTLESS: &str = "00000000-03e8-7000-8000-000000000064";
const SPECIAL_UNKNOWN: &str = "00000000-03e8-7000-8000-000000000065";
const EXTRA_ROLLOUTS: usize = 200;

fn records_to_bytes(records: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        bytes.extend(serde_json::to_vec(record).expect("serialize browser fixture record"));
        bytes.push(b'\n');
    }
    bytes
}

fn guardian_records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {
                "id": INCIDENT_CHILD,
                "cwd": "/work/child",
                "parent_thread_id": INCIDENT_ROOT,
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
        json!({
            "timestamp": "2026-08-08T01:00:02Z",
            "type": "event_msg",
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
    ]
}

fn main_records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {
                "id": INCIDENT_ROOT,
                "cwd": "/work/main",
                "agent_role": "main",
                "source": "main",
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {
                "turn_id": "00000000-0bb8-7000-8000-000000000010",
                "model": "main-model",
            }
        }),
        json!({
            "timestamp": "2026-08-08T01:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 1,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 1
                    },
                    "last_token_usage": {
                        "input_tokens": 1,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 1
                    }
                }
            }
        }),
    ]
}

fn legacy_records() -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {
                "id": INCIDENT_LEGACY,
                "cwd": "/work/legacy",
                "parent_thread_id": INCIDENT_ROOT,
                "forked_from_id": INCIDENT_ROOT,
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": INCIDENT_ROOT, "depth": 1}}},
            }
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {
                "turn_id": "00000000-0bb8-7000-8000-000000000011",
                "model": "legacy-model",
            }
        }),
        json!({
            "timestamp": "2026-08-08T01:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 1,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 1
                    },
                    "last_token_usage": {
                        "input_tokens": 1,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 1
                    }
                }
            }
        }),
    ]
}

fn extra_records(id: &str, turn_id: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T01:00:00Z",
            "payload": {"id": id, "cwd": "/work/extra", "agent_role": "main"}
        }),
        json!({
            "type": "turn_context",
            "timestamp": "2026-08-08T01:00:01Z",
            "payload": {"turn_id": turn_id, "model": "extra-model"}
        }),
        json!({
            "timestamp": "2026-08-08T01:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 0,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 0
                    },
                    "last_token_usage": {
                        "input_tokens": 0,
                        "cached_input_tokens": 0,
                        "cache_write_input_tokens": 0,
                        "output_tokens": 0,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 0
                    }
                }
            }
        }),
    ]
}

fn write_guardian_state(codex_home: &Path, rollouts: &[(String, PathBuf)]) {
    let state = Connection::open(codex_home.join("state_5.sqlite")).expect("open browser state");
    state
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
        .expect("create browser state tables");
    let main_path = rollouts
        .iter()
        .find(|(id, _)| id == INCIDENT_ROOT)
        .map(|(_, path)| path)
        .expect("main rollout state path");
    let legacy_path = rollouts
        .iter()
        .find(|(id, _)| id == INCIDENT_LEGACY)
        .map(|(_, path)| path)
        .expect("legacy rollout state path");
    let guardian_path = rollouts
        .iter()
        .find(|(id, _)| id == INCIDENT_CHILD)
        .map(|(_, path)| path)
        .expect("guardian rollout state path");
    state
        .execute(
            "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
             VALUES (?1,?2,1,2,0,'/work/main','Main',NULL,'main-model','main'),
                    (?3,?4,3,4,0,'/work/legacy','Legacy',NULL,'legacy-model','subagent'),
                    (?5,?6,5,6,0,'/work/child','Guardian',NULL,'guardian-model','subagent')",
            params![
                INCIDENT_ROOT,
                main_path.to_str().expect("browser main rollout path"),
                INCIDENT_LEGACY,
                legacy_path.to_str().expect("browser legacy rollout path"),
                INCIDENT_CHILD,
                guardian_path.to_str().expect("browser guardian rollout path"),
            ],
        )
        .expect("write browser core state");
    state
        .execute(
            "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
             VALUES (?1,?2,'spawned',3)",
            params![INCIDENT_ROOT, INCIDENT_LEGACY],
        )
        .expect("write browser legacy edge");
    for (number, (id, path)) in rollouts.iter().enumerate() {
        if id == INCIDENT_ROOT || id == INCIDENT_LEGACY || id == INCIDENT_CHILD {
            continue;
        }
        state
            .execute(
                "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
                 VALUES (?1,?2,?3,?3,0,'/work/extra',?4,NULL,'extra-model','main')",
                params![id, path.to_str().expect("browser extra rollout path"), 10 + number as i64, format!("Extra {number:02}")],
            )
            .expect("write browser extra state");
    }
    drop(state);
    let mut index = String::new();
    for (id, _) in rollouts {
        index.push_str(&format!(
            "{{\"id\":\"{id}\",\"thread_name\":\"name-{id}\"}}\n"
        ));
    }
    fs::write(codex_home.join("session_index.jsonl"), index).expect("write browser session index");
}

fn seed_v3_guardian_database(database: &Path, rollouts: &[(i64, String, PathBuf)]) {
    let connection = Connection::open(database).expect("open browser v3 database");
    connection
        .pragma_update(None, "foreign_keys", true)
        .expect("enable browser foreign keys");
    connection
        .execute_batch(include_str!("../src/storage/schema/0001_initial.sql"))
        .expect("create browser v1 schema");
    connection
        .execute_batch(include_str!("../src/storage/schema/0002_usage_ledger.sql"))
        .expect("create browser v2 schema");
    connection
        .execute_batch(include_str!(
            "../src/storage/schema/0003_normalized_token_usage.sql"
        ))
        .expect("create browser v3 schema");
    connection
        .execute(
            "INSERT INTO threads(
                thread_id,parent_thread_id,root_session_id,agent_role,archived,
                metadata_quality_status,metadata_resolved_at_ms
             ) VALUES (?1,NULL,?1,'main',0,'complete',1),
                      (?2,NULL,NULL,'unknown',0,'partial',1),
                      (?3,NULL,NULL,'unknown',0,'partial',1)",
            params![INCIDENT_ROOT, INCIDENT_LEGACY, INCIDENT_CHILD],
        )
        .expect("seed browser core thread rows");
    for (source_id, thread_id, path) in rollouts {
        let file = fs::File::open(path).expect("open browser rollout");
        let metadata = file.metadata().expect("stat browser rollout");
        let identity = file_identity::identity_from_file(&file).expect("browser identity");
        let device_id = i64::try_from(identity.device_id).expect("browser device id");
        let inode = i64::try_from(identity.inode).expect("browser inode");
        let observed_mtime_ns =
            file_identity::modified_ns(&metadata).expect("browser modification time");
        let observed_size = i64::try_from(metadata.len()).expect("browser rollout size");
        if thread_id != INCIDENT_ROOT && thread_id != INCIDENT_LEGACY && thread_id != INCIDENT_CHILD
        {
            connection
                .execute(
                    "INSERT INTO threads(
                        thread_id,parent_thread_id,root_session_id,agent_role,archived,
                        metadata_quality_status,metadata_resolved_at_ms
                     ) VALUES (?1,NULL,?1,'main',0,'complete',1)",
                    [thread_id],
                )
                .expect("seed browser extra thread row");
        }
        connection
            .execute(
                "INSERT INTO source_files(
                    source_file_id,thread_id,current_path,source_area,device_id,inode,
                    file_generation,observed_size,observed_mtime_ns,file_status,last_seen_at_ms
                 ) VALUES (?1,?2,?3,'sessions',?4,?5,1,?6,?7,'present',1)",
                params![
                    source_id,
                    thread_id,
                    path.to_str().expect("browser rollout path"),
                    device_id,
                    inode,
                    observed_size,
                    observed_mtime_ns
                ],
            )
            .expect("seed browser source");
        connection
            .execute(
                "INSERT INTO source_checkpoints(
                    source_file_id,consumer_kind,parser_version,committed_offset,guard_hash,
                    processing_status,last_successful_scan_at_ms,last_error_code
                 ) VALUES (?1,'metadata',1,?2,zeroblob(32),'ready',1,NULL)",
                params![source_id, observed_size],
            )
            .expect("seed browser stale checkpoint");
        let cwd = if thread_id == INCIDENT_ROOT {
            "/work/main"
        } else if thread_id == INCIDENT_LEGACY {
            "/work/legacy"
        } else if thread_id == INCIDENT_CHILD {
            "/work/child"
        } else {
            "/work/extra"
        };
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
                    ?1,1,1,?2,?3,'owning_live',
                    ?4,'session_meta',0,1,
                    NULL,NULL,
                    NULL,NULL,NULL,
                    NULL,NULL,NULL,
                    NULL,0,'confirmed','complete',1
                 )",
                params![source_id, observed_size, thread_id, cwd],
            )
            .expect("seed browser stale metadata fact");
    }
    connection
        .execute_batch("PRAGMA user_version = 3;")
        .expect("mark browser fixture v3");
}

fn wait_for_scan(ledger: &Ledger) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let state = ledger.app_state().expect("read browser scan state");
        if state.scan.last_finished_scan_id.is_some() && state.scan.active_scan_id.is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "browser fixture scan timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs the real Vite/Axum browser gate"]
async fn spec06_real_axum_browser_gate() {
    let root = TempRoot::new().expect("temporary fixture root");
    let codex_home = root.path().join("codex");
    let static_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/dist");
    fs::create_dir_all(codex_home.join("sessions")).expect("temporary sessions directory");
    fs::create_dir_all(codex_home.join("archived_sessions"))
        .expect("temporary archived sessions directory");
    assert!(
        static_dir.join("index.html").is_file(),
        "frontend/dist must be built before the real browser gate"
    );

    let database = root.path().join("mu.sqlite3");
    let main_rollout = codex_home.join("sessions").join("rollout-main.jsonl");
    let legacy_rollout = codex_home.join("sessions").join("rollout-legacy.jsonl");
    let guardian_rollout = codex_home
        .join("sessions")
        .join(format!("rollout-{INCIDENT_CHILD}.jsonl"));
    let mut rollouts = vec![
        (INCIDENT_ROOT.to_owned(), main_rollout.clone()),
        (INCIDENT_LEGACY.to_owned(), legacy_rollout.clone()),
        (INCIDENT_CHILD.to_owned(), guardian_rollout.clone()),
    ];
    fs::write(&main_rollout, records_to_bytes(&main_records()))
        .expect("write browser Main rollout");
    fs::write(&legacy_rollout, records_to_bytes(&legacy_records()))
        .expect("write browser Legacy rollout");
    fs::write(&guardian_rollout, records_to_bytes(&guardian_records()))
        .expect("write browser Guardian rollout");
    for number in 0..EXTRA_ROLLOUTS {
        let id = format!("00000000-03e8-7000-8000-{:012x}", number + 100);
        let turn_id = format!("00000000-0bb8-7000-8000-{:012x}", number + 1_000);
        let path = codex_home
            .join("sessions")
            .join(format!("rollout-extra-{number:02}.jsonl"));
        fs::write(&path, records_to_bytes(&extra_records(&id, &turn_id)))
            .expect("write browser extra rollout");
        rollouts.push((id, path));
    }
    write_guardian_state(&codex_home, &rollouts);
    fs::write(
        codex_home.join(".codex-global-state.json"),
        serde_json::to_vec(&json!({
            "projectless-thread-ids": [SPECIAL_PROJECTLESS, SPECIAL_UNKNOWN],
            "thread-project-assignments": {"00000000-03e8-7000-8000-000000000065": {}}
        }))
        .expect("serialize browser global state"),
    )
    .expect("write browser global state");
    let sources = rollouts
        .iter()
        .enumerate()
        .map(|(index, (id, path))| ((index + 1) as i64, id.clone(), path.clone()))
        .collect::<Vec<_>>();
    seed_v3_guardian_database(&database, &sources);
    let ledger = Arc::new(
        Ledger::open(LedgerOptions::new(&database, &codex_home)).expect("temporary ledger"),
    );
    assert_eq!(ledger.schema_version().expect("browser schema version"), 10);
    let source_ids = sources
        .iter()
        .map(|(source_id, _, _)| *source_id)
        .collect::<Vec<_>>();
    let build = UsageLedger::new(&ledger)
        .begin_rebuild(USAGE_PARSER_VERSION, source_ids, 10)
        .expect("begin blocked browser build");
    assert_eq!(build.members.len(), 3 + EXTRA_ROLLOUTS);
    assert!(
        build
            .members
            .iter()
            .any(|member| member.completion_status == CompletionStatus::Blocked)
    );
    let state_index = codex_home.join("state_5.sqlite");
    let scanner: ScanHandle = ScanCoordinator::start(
        ScanConfig::new(codex_home.clone()),
        Arc::clone(&ledger),
        CodexMetadata::from_home(codex_home),
    )
    .expect("temporary scanner");
    wait_for_scan(&ledger);
    assert_eq!(
        ledger
            .app_state()
            .expect("browser final scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    let connection = Connection::open(&database).expect("open browser final database");
    let active: (i64, Option<i64>, i64) = connection
        .query_row(
            "SELECT usage_active_epoch,usage_build_epoch,usage_parser_version FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read browser active epoch");
    assert_eq!(active, (1, None, USAGE_PARSER_VERSION));
    let metadata: (i64, i64, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT c.parser_version,f.metadata_parser_version,
                    f.parent_thread_id_hint,f.parent_hint_provenance
             FROM source_checkpoints c JOIN rollout_metadata_facts f
               ON f.source_file_id=c.source_file_id
             WHERE c.source_file_id=3 AND c.consumer_kind='metadata'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read browser metadata replay result");
    assert_eq!(
        metadata,
        (
            usagi::codex::METADATA_PARSER_VERSION,
            usagi::codex::METADATA_PARSER_VERSION,
            Some(INCIDENT_ROOT.to_owned()),
            Some("session_meta_parent".to_owned())
        )
    );
    let main_parent: Option<String> = connection
        .query_row(
            "SELECT parent_thread_id_hint FROM rollout_metadata_facts WHERE source_file_id=1",
            [],
            |row| row.get(0),
        )
        .expect("read browser Main parent fact");
    assert_eq!(main_parent, None);
    let legacy_fact: (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT parent_thread_id_hint,parent_hint_provenance
             FROM rollout_metadata_facts WHERE source_file_id=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read browser Legacy parent fact");
    assert_eq!(
        legacy_fact,
        (
            Some(INCIDENT_ROOT.to_owned()),
            Some("session_meta_parent".to_owned())
        )
    );
    let legacy_root: (Option<String>, Option<String>) = connection
        .query_row(
            "SELECT parent_thread_id,root_session_id FROM threads WHERE thread_id=?1",
            [INCIDENT_LEGACY],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read browser Legacy resolved root");
    assert_eq!(
        legacy_root,
        (
            Some(INCIDENT_ROOT.to_owned()),
            Some(INCIDENT_ROOT.to_owned())
        )
    );
    let state = Connection::open(&state_index).expect("open browser state for edge assertion");
    let guardian_edges: i64 = state
        .query_row(
            "SELECT COUNT(*) FROM thread_spawn_edges WHERE child_thread_id=?1",
            [INCIDENT_CHILD],
            |row| row.get(0),
        )
        .expect("read browser Guardian state edges");
    assert_eq!(guardian_edges, 0);
    drop(state);
    drop(connection);
    let control_file = root.path().join("browser-control");
    fs::write(&control_file, "").expect("create browser control file");
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let mut server = tokio::spawn(serve_fixture(
        Arc::clone(&ledger),
        scanner.clone(),
        static_dir,
        command_rx,
    ));
    let watcher = tokio::spawn(watch_fixture_control(
        control_file.clone(),
        main_rollout,
        state_index,
        Arc::clone(&ledger),
        scanner.clone(),
        command_tx.clone(),
    ));
    wait_for_port(3210).await;

    let frontend_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend");
    let base_url = "http://127.0.0.1:3210";
    let browser_result = if std::env::var_os("BROWSER_TEST_GREP").is_some() {
        run_playwright(&frontend_dir, base_url, false, &control_file)
    } else {
        run_playwright(&frontend_dir, base_url, false, &control_file)
            .and_then(|_| run_playwright(&frontend_dir, base_url, true, &control_file))
    };
    clear_browser_artifacts(&frontend_dir);

    let _ = command_tx.send(FixtureServerCommand::Shutdown);
    match timeout(Duration::from_secs(2), &mut server).await {
        Ok(result) => result.expect("Axum server task"),
        Err(_) => {
            server.abort();
            let _ = server.await;
        }
    }
    watcher.abort();
    let _ = watcher.await;
    scanner.shutdown().expect("scanner shutdown");
    drop(ledger);

    if let Err(error) = browser_result {
        panic!("real browser gate failed: {error}");
    }
}
