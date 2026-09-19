use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use futures_util::StreamExt;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use usagi::{
    api::{AppContext, QueryApi, listen_address},
    codex::quota::CodexQuotaService,
    domain::{ScanCompletedEvent, ScanFailedEvent, ScanStartEvent, ScanTrigger},
    platform::browser::SystemBrowser,
    scanner::{CodexMetadata, RequestDisposition, ScanConfig, ScanCoordinator, ScanHandle},
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
    usage::{SummaryQuery, TimeRange, UsageFilter, UsageLedger},
};
use uuid::Uuid;

const ROOT_A: &str = "00000000-03e8-7000-8000-000000000001";
const ROOT_B: &str = "00000000-03e8-7000-8000-000000000002";
const GATE_ROOT: &str = "00000000-03e8-7000-8000-000000000101";
const GATE_CHILD: &str = "00000000-03e8-7000-8000-000000000102";
const GATE_GRANDCHILD: &str = "00000000-03e8-7000-8000-000000000103";
const GATE_OTHER: &str = "00000000-03e8-7000-8000-000000000104";
const GATE_PROJECTLESS: &str = "00000000-03e8-7000-8000-000000000105";
const GATE_UNKNOWN: &str = "00000000-03e8-7000-8000-000000000106";
const F01_REAL_STRUCTURE_FIXTURE: &str =
    include_str!("../frontend/src/test-fixtures/t_mu03_f01_real_structure.json");

fn empty_summary_query(range: TimeRange) -> SummaryQuery {
    SummaryQuery::new(range, UsageFilter::default())
}

fn f01_fixture() -> Value {
    serde_json::from_str(F01_REAL_STRUCTURE_FIXTURE).expect("valid F01 shared fixture")
}

struct TempRoot(PathBuf);
impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec05-{label}-{}-{stamp}",
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
    static_dir: PathBuf,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let static_dir = root.path().join("static");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("archived_sessions")).unwrap();
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(
            static_dir.join("index.html"),
            "<html>USAGI_STATIC_SENTINEL</html>",
        )
        .unwrap();
        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            static_dir,
        }
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(Ledger::open(LedgerOptions::new(&self.db, &self.home)).unwrap())
    }

    fn start(&self, ledger: Arc<Ledger>) -> ScanHandle {
        ScanCoordinator::start(
            ScanConfig::new(self.home.clone()).with_interval(std::time::Duration::from_secs(3_600)),
            ledger,
            CodexMetadata::from_home(self.home.clone()),
        )
        .unwrap()
    }

    fn router(&self, ledger: Arc<Ledger>, scanner: ScanHandle) -> Router {
        let codex_quota_service = CodexQuotaService::unavailable(ledger.codex_home());
        QueryApi::router(
            AppContext {
                ledger,
                scanner,
                codex_quota_service,
                update_service: UpdateService::unavailable(),
                browser_opener: Arc::new(SystemBrowser),
            },
            self.static_dir.clone(),
        )
        .unwrap()
    }

    fn seed_two_roots(&self) {
        let now_ms: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .try_into()
            .unwrap();
        let ts_a = chrono::DateTime::from_timestamp_millis(now_ms - 2_000)
            .unwrap()
            .to_rfc3339();
        let ts_b = chrono::DateTime::from_timestamp_millis(now_ms - 1_000)
            .unwrap()
            .to_rfc3339();
        let path_a = self.home.join("sessions").join("rollout-a.jsonl");
        let path_b = self.home.join("sessions").join("rollout-b.jsonl");
        fs::write(
            &path_a,
            records(&root_records(ROOT_A, "turn-a", "model-a", 10, &ts_a)),
        )
        .unwrap();
        fs::write(
            &path_b,
            records(&root_records(ROOT_B, "turn-b", "unknown", 20, &ts_b)),
        )
        .unwrap();
        let state = Connection::open(self.home.join("state_5.sqlite")).unwrap();
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
            .unwrap();
        for (id, path, title, model) in [
            (ROOT_A, &path_a, "Root A", "model-a"),
            (ROOT_B, &path_b, "Root B", "unknown"),
        ] {
            state.execute(
                "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
                 VALUES (?1,?2,1,2,0,'/work',?3,NULL,?4,'main')",
                params![id, path.to_str().unwrap(), title, model],
            ).unwrap();
        }
        fs::write(
            self.home.join("session_index.jsonl"),
            format!("{{\"id\":\"{ROOT_A}\",\"thread_name\":\"Root A\"}}\n{{\"id\":\"{ROOT_B}\",\"thread_name\":\"Root B\"}}\n"),
        ).unwrap();
    }

    fn seed_many_roots(&self, count: usize) {
        assert!(count > 0);
        let now_ms: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .try_into()
            .unwrap();
        let state = Connection::open(self.home.join("state_5.sqlite")).unwrap();
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
            .unwrap();
        let mut index = String::new();
        for number in 0..count {
            let id = format!("00000000-03e8-7000-8000-{number:012x}");
            let title = format!("Session {number:02}");
            let model = if number % 2 == 0 {
                "model-a"
            } else {
                "model-b"
            };
            let timestamp = chrono::DateTime::from_timestamp_millis(
                now_ms - (count.saturating_sub(number) as i64) * 1_000,
            )
            .unwrap()
            .to_rfc3339();
            let path = self
                .home
                .join("sessions")
                .join(format!("rollout-{number:02}.jsonl"));
            fs::write(
                &path,
                records(&root_records(
                    &id,
                    &format!("turn-{number:02}"),
                    model,
                    (number + 1) as i64,
                    &timestamp,
                )),
            )
            .unwrap();
            state
                .execute(
                    "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
                     VALUES (?1,?2,?3,?4,0,'/work',?5,NULL,?6,'main')",
                    params![
                        id,
                        path.to_str().unwrap(),
                        now_ms - (count.saturating_sub(number) as i64) * 1_000,
                        now_ms - (count.saturating_sub(number) as i64) * 1_000,
                        title,
                        model,
                    ],
                )
                .unwrap();
            index.push_str(&format!(
                "{{\"id\":\"{id}\",\"thread_name\":\"{title}\"}}\n"
            ));
        }
        fs::write(self.home.join("session_index.jsonl"), index).unwrap();
    }

    fn seed_gate_a_tree(&self) {
        let main_path = self.home.join("sessions").join("rollout-gate-main.jsonl");
        let child_path = self.home.join("sessions").join("rollout-gate-child.jsonl");
        let grandchild_path = self
            .home
            .join("sessions")
            .join("rollout-gate-grandchild.jsonl");
        let other_path = self.home.join("sessions").join("rollout-gate-other.jsonl");
        let projectless_path = self
            .home
            .join("sessions")
            .join("rollout-gate-projectless.jsonl");
        let unknown_path = self
            .home
            .join("sessions")
            .join("rollout-gate-unknown.jsonl");
        fs::write(
            &main_path,
            records(&[
                json!({"type":"session_meta","timestamp":"2026-08-08T01:00:00Z","payload":{"id":GATE_ROOT,"cwd":"/project/a","agent_role":"main"}}),
                json!({"type":"turn_context","timestamp":"2026-08-08T01:00:01Z","payload":{"turn_id":"gate-main-a","model":"main-model-a"}}),
                token_record(100, "2026-08-08T01:00:02Z"),
                json!({"type":"turn_context","timestamp":"2026-08-08T01:00:03Z","payload":{"turn_id":"gate-main-b","model":"main-model-b"}}),
                reasoning_token_record(20, 5, "2026-08-08T01:00:04Z"),
                token_record(999, "2025-01-01T00:00:00Z"),
            ]),
        )
        .unwrap();
        fs::write(
            &child_path,
            records(&[
                json!({"type":"session_meta","timestamp":"2026-08-08T01:00:05Z","payload":{"id":GATE_CHILD,"cwd":"/project/a","parent_thread_id":GATE_ROOT,"source":{"subagent":{"other":"gate"}}}}),
                json!({"type":"turn_context","timestamp":"2026-08-08T01:00:06Z","payload":{"turn_id":"gate-child","model":"deep-model"}}),
                reasoning_token_record(30, 3, "2026-08-08T01:00:07Z"),
            ]),
        )
        .unwrap();
        fs::write(
            &grandchild_path,
            records(&[
                json!({"type":"session_meta","timestamp":"2026-08-08T01:00:08Z","payload":{"id":GATE_GRANDCHILD,"cwd":"/project/a","parent_thread_id":GATE_CHILD,"source":{"subagent":{"other":"gate"}}}}),
                json!({"type":"turn_context","timestamp":"2026-08-08T01:00:09Z","payload":{"turn_id":"gate-grandchild","model":"grand-model"}}),
                token_record(40, "2026-08-08T01:00:10Z"),
            ]),
        )
        .unwrap();
        fs::write(
            &other_path,
            records(&root_records(
                GATE_OTHER,
                "gate-other",
                "other-model",
                7,
                "2026-08-08T01:00:11Z",
            )),
        )
        .unwrap();
        fs::write(
            &projectless_path,
            records(&root_records(
                GATE_PROJECTLESS,
                "gate-projectless",
                "projectless-model",
                8,
                "2026-08-08T01:00:12Z",
            )),
        )
        .unwrap();
        fs::write(
            &unknown_path,
            records(&root_records(
                GATE_UNKNOWN,
                "gate-unknown",
                "unknown-model",
                9,
                "2026-08-08T01:00:13Z",
            )),
        )
        .unwrap();

        let state = Connection::open(self.home.join("state_5.sqlite")).unwrap();
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
            .unwrap();
        for (id, path, title, model, role) in [
            (GATE_ROOT, &main_path, "Gate root", "main-model-a", "main"),
            (
                GATE_CHILD,
                &child_path,
                "Gate child",
                "deep-model",
                "subagent",
            ),
            (
                GATE_GRANDCHILD,
                &grandchild_path,
                "Gate grandchild",
                "grand-model",
                "subagent",
            ),
            (GATE_OTHER, &other_path, "Gate other", "other-model", "main"),
            (
                GATE_PROJECTLESS,
                &projectless_path,
                "Gate projectless",
                "projectless-model",
                "main",
            ),
            (
                GATE_UNKNOWN,
                &unknown_path,
                "Gate unknown",
                "unknown-model",
                "main",
            ),
        ] {
            state
                .execute(
                    "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
                     VALUES (?1,?2,1,2,0,'/project/a',?3,NULL,?4,?5)",
                    params![id, path.to_str().unwrap(), title, model, role],
                )
                .unwrap();
        }
        state
            .execute(
                "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms) VALUES (?1,?2,'spawned',1), (?2,?3,'spawned',2)",
                params![GATE_ROOT, GATE_CHILD, GATE_GRANDCHILD],
            )
            .unwrap();
        fs::write(
            self.home.join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{GATE_ROOT}\",\"thread_name\":\"Gate root\"}}\n{{\"id\":\"{GATE_CHILD}\",\"thread_name\":\"Gate child\"}}\n{{\"id\":\"{GATE_GRANDCHILD}\",\"thread_name\":\"Gate grandchild\"}}\n{{\"id\":\"{GATE_OTHER}\",\"thread_name\":\"Gate other\"}}\n{{\"id\":\"{GATE_PROJECTLESS}\",\"thread_name\":\"Gate projectless\"}}\n{{\"id\":\"{GATE_UNKNOWN}\",\"thread_name\":\"Gate unknown\"}}\n"
            ),
        )
        .unwrap();
    }

    fn mark_gate_projects(&self) {
        let db = Connection::open(&self.db).unwrap();
        db.execute(
            "UPDATE threads SET project_kind='project', project_path='/project/a', project_name='Project A' WHERE thread_id=?1",
            [GATE_ROOT],
        )
        .unwrap();
        db.execute(
            "UPDATE threads SET project_kind='project', project_path='/project/b', project_name='Project B' WHERE thread_id=?1",
            [GATE_OTHER],
        )
        .unwrap();
        db.execute(
            "UPDATE threads SET project_kind='projectless', project_path=NULL, project_name=NULL WHERE thread_id=?1",
            [GATE_PROJECTLESS],
        )
        .unwrap();
        db.execute(
            "UPDATE threads SET project_kind='unknown', project_path=NULL, project_name=NULL WHERE thread_id=?1",
            [GATE_UNKNOWN],
        )
        .unwrap();
    }
}

fn root_records(id: &str, turn: &str, model: &str, total: i64, timestamp: &str) -> Vec<Value> {
    vec![
        json!({"type":"session_meta","timestamp":timestamp,"payload":{"id":id,"cwd":"/work","agent_role":"main"}}),
        json!({"type":"turn_context","timestamp":timestamp,"payload":{"turn_id":turn,"model":model}}),
        json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":total,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":total},
            "last_token_usage":{"input_tokens":total,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":total}
        }}}),
    ]
}
fn records(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in values {
        out.extend(serde_json::to_vec(value).unwrap());
        out.push(b'\n');
    }
    out
}

fn token_record(total: i64, timestamp: &str) -> Value {
    json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{
        "total_token_usage":{"input_tokens":total,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":total},
        "last_token_usage":{"input_tokens":total,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":total}
    }}})
}

fn reasoning_token_record(input: i64, reasoning: i64, timestamp: &str) -> Value {
    let total = input + reasoning;
    json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{
        "total_token_usage":{"input_tokens":input,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":reasoning,"reasoning_output_tokens":reasoning,"total_tokens":total},
        "last_token_usage":{"input_tokens":input,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":reasoning,"reasoning_output_tokens":reasoning,"total_tokens":total}
    }}})
}

fn wait_scan(ledger: &Ledger, wanted: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = ledger.app_state().unwrap();
        let done = match wanted {
            Some(id) => state.last_finished_scan_id.as_deref() == Some(id),
            None => state.last_finished_scan_id.is_some(),
        } && state.active_scan_id.is_none();
        if done {
            return;
        }
        assert!(Instant::now() < deadline, "scan timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn request_and_wait(scanner: &ScanHandle, ledger: &Ledger) -> String {
    let scan_id = match scanner.request(ScanTrigger::Manual).unwrap() {
        RequestDisposition::Started { scan_id, .. } => scan_id,
        RequestDisposition::Coalesced {
            followup_scan_id, ..
        } => followup_scan_id,
    };
    wait_scan(ledger, Some(&scan_id));
    scan_id
}

fn append_root_a_token(home: &Path, total: i64, last: i64) {
    append_root_token(home, "rollout-a.jsonl", total, last);
}

fn append_root_token(home: &Path, filename: &str, total: i64, last: i64) {
    let path = home.join("sessions").join(filename);
    let now_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    let timestamp = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap()
        .to_rfc3339();
    let record = json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{
        "total_token_usage":{"input_tokens":total,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":total},
        "last_token_usage":{"input_tokens":last,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":last}
    }}});
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend(records(&[record]));
    fs::write(path, bytes).unwrap();
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    extra: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:3210");
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

fn assert_canonical_usage(value: &Value) {
    for field in [
        "input_tokens",
        "cached_tokens",
        "cache_write_tokens",
        "uncached_input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "other_output_tokens",
        "total_tokens",
        "cache_hit_rate",
        "estimated_cost",
    ] {
        assert!(
            value.get(field).is_some(),
            "missing canonical field {field}"
        );
    }
    for field in [
        "cached_input_tokens",
        "cache_write_input_tokens",
        "cache_write_status",
        "reasoning_output_tokens",
        "cache_tokens",
        "reported_total_tokens",
        "derived_total_tokens",
    ] {
        assert!(value.get(field).is_none(), "legacy field leaked: {field}");
    }
}

#[tokio::test]
async fn t_s05_017_018_http_guard_no_store_sse_and_static_fallback_are_exact() {
    assert_eq!(listen_address().to_string(), "127.0.0.1:3210");
    assert!(!listen_address().ip().is_unspecified());
    let fixture = Fixture::new("http-security");
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let health = call(&app, Method::GET, "/api/health", &[]).await;
    assert_eq!(health.status(), StatusCode::NO_CONTENT);
    assert_eq!(health.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        health
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );

    let missing_host = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_host.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(missing_host).await["error"]["code"],
        "FORBIDDEN_HOST"
    );

    for extra in [
        vec![("host", "evil.example:3210")],
        vec![("origin", "http://evil.example:3210")],
        vec![("sec-fetch-site", "cross-site")],
        vec![("origin", "null")],
    ] {
        let mut builder = Request::builder().uri("/api/health");
        if !extra.iter().any(|(n, _)| *n == "host") {
            builder = builder.header(header::HOST, "127.0.0.1:3210");
        }
        for (n, v) in extra {
            builder = builder.header(n, v);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        call(
            &app,
            Method::GET,
            "/api/health",
            &[("origin", "http://127.0.0.1:3210")]
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &app,
            Method::GET,
            "/api/health",
            &[("origin", "http://localhost:3210")]
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );

    let unknown = call(&app, Method::GET, "/api/does-not-exist", &[]).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(unknown.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(json_body(unknown).await["error"]["code"], "NOT_FOUND");

    let page = call(&app, Method::GET, "/dashboard/deep/link", &[]).await;
    assert_eq!(page.status(), StatusCode::OK);
    let page_body = String::from_utf8(
        to_bytes(page.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(page_body.contains("USAGI_STATIC_SENTINEL"));

    let forbidden_page = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .header(header::HOST, "example.com:3210")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden_page.status(), StatusCode::FORBIDDEN);

    let events = call(&app, Method::GET, "/api/events", &[]).await;
    assert_eq!(events.status(), StatusCode::OK);
    assert!(
        events.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert_eq!(events.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(events.headers()["x-accel-buffering"], "no");
    let mut stream = events.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first = String::from_utf8(first.to_vec()).unwrap();
    assert!(first.contains("event: revision"));
    assert!(first.contains("data_revision"));
    drop(stream);

    let refresh = call(&app, Method::POST, "/api/refresh", &[]).await;
    assert_eq!(refresh.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(refresh).await["error"]["code"], "FORBIDDEN");
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s05_003_004_005_006_007_008_019_020_real_http_queries_and_cursor_snapshot_contract() {
    let fixture = Fixture::new("query-contract");
    fixture.seed_two_roots();
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let revision_response = call(&app, Method::GET, "/api/revision", &[]).await;
    assert_eq!(revision_response.status(), StatusCode::OK);
    let revision = json_body(revision_response).await;
    assert!(revision["data_revision"].as_i64().unwrap() > 0);

    let status_response = call(&app, Method::GET, "/api/status", &[]).await;
    assert_eq!(status_response.status(), StatusCode::OK);
    let status = json_body(status_response).await;
    assert_eq!(status["scan_state"], "idle");
    assert_eq!(status["source_binding_status"], "ready");
    assert!(status["last_finished_scan_id"].as_str().is_some());

    for (uri, code) in [
        ("/api/usage/summary", "INVALID_RANGE"),
        ("/api/usage/summary?range=quarter", "INVALID_RANGE"),
        ("/api/usage/sessions?range=year&limit=0", "INVALID_FILTER"),
        (
            "/api/usage/sessions?range=year&cursor=bad",
            "INVALID_FILTER",
        ),
        ("/api/status?target_scan_id=not-a-uuid", "INVALID_SCAN_ID"),
    ] {
        let response = call(&app, Method::GET, uri, &[]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["error"]["code"], code);
    }
    let missing_scan = call(
        &app,
        Method::GET,
        &format!("/api/status?target_scan_id={}", Uuid::max()),
        &[],
    )
    .await;
    assert_eq!(missing_scan.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(missing_scan).await["error"]["code"],
        "SCAN_NOT_FOUND"
    );

    let summary_response = call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await;
    assert_eq!(summary_response.status(), StatusCode::OK);
    assert_eq!(
        summary_response.headers()[header::CACHE_CONTROL],
        "no-store"
    );
    let summary = json_body(summary_response).await;
    assert_canonical_usage(&summary["usage"]);
    assert_eq!(summary["usage"]["total_tokens"], 30);
    assert_eq!(summary["usage"]["session_count"], 2);
    assert!(summary["usage"]["estimated_cost"].is_null());
    assert_eq!(summary["data_revision"], revision["data_revision"]);

    let frozen = UsageLedger::new(&ledger)
        .summary_snapshot(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    let (db_revision, db_epoch): (i64, i64) = Connection::open(&fixture.db)
        .unwrap()
        .query_row(
            "SELECT data_revision,usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(frozen.data_revision, db_revision);
    assert_eq!(frozen.active_epoch, db_epoch);
    assert_eq!(frozen.value.totals.total_tokens, 30);

    let page1_response = call(&app, Method::GET, "/api/usage/sessions?range=year", &[]).await;
    assert_eq!(page1_response.status(), StatusCode::OK);
    let page1 = json_body(page1_response).await;
    assert_eq!(page1["items"].as_array().unwrap().len(), 2);
    assert_eq!(page1["total_items"], 2);
    assert_eq!(page1["sort_index"].as_array().unwrap().len(), 2);
    assert_eq!(page1["data_revision"], summary["data_revision"]);
    assert_eq!(page1["items"][0]["root_session_id"], ROOT_B);
    for scope in ["inclusive_usage", "self_usage", "subagent_usage"] {
        assert_canonical_usage(&page1["items"][0][scope]);
    }
    assert!(page1.get("next_cursor").is_none());
    let expected_revision = page1["data_revision"].as_i64().unwrap();

    append_root_a_token(&fixture.home, 15, 5);
    request_and_wait(&scanner, &ledger);
    let stale_revision = call(
        &app,
        Method::GET,
        &format!(
            "/api/usage/session-rows?range=year&expected_data_revision={expected_revision}&root_session_id={ROOT_A}"
        ),
        &[],
    )
    .await;
    assert_eq!(stale_revision.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(stale_revision).await["error"]["code"],
        "STALE_DATA_REVISION"
    );

    let models =
        json_body(call(&app, Method::GET, "/api/usage/models?range=year", &[]).await).await;
    for row in models["items"].as_array().unwrap() {
        assert_canonical_usage(&row["usage"]);
    }
    let refreshed_summary =
        json_body(call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await).await;
    assert_eq!(models["data_revision"], refreshed_summary["data_revision"]);
    assert_eq!(models["items"][0]["model"], "unknown");
    assert!(
        models["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["model"] == "unknown")
    );

    // An in-progress shadow build must not replace the active stable epoch.
    let before = UsageLedger::new(&ledger)
        .summary(empty_summary_query(TimeRange::new(0, i64::MAX).unwrap()))
        .unwrap();
    let source_ids = {
        let db = Connection::open(&fixture.db).unwrap();
        let mut stmt = db
            .prepare("SELECT source_file_id FROM source_files ORDER BY source_file_id")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    };
    UsageLedger::new(&ledger)
        .begin_rebuild(usagi::usage::USAGE_PARSER_VERSION, source_ids, 100)
        .unwrap();
    let during =
        json_body(call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await).await;
    assert_eq!(during["usage"]["total_tokens"], before.totals.total_tokens);

    // The lifecycle projection must never hide the last committed active epoch.
    // Exercise both running and failed states without letting a scanner write
    // new usage, so the API can only return the already-stable snapshot.
    let lifecycle_id = "20000000-0000-4000-8000-000000000020";
    ledger
        .mark_scan_started(ScanStartEvent::new(lifecycle_id, ScanTrigger::Manual, 200).unwrap())
        .unwrap();
    let while_running =
        json_body(call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await).await;
    assert_eq!(
        while_running["usage"]["total_tokens"],
        before.totals.total_tokens
    );
    ledger
        .mark_scan_failed(ScanFailedEvent::new(lifecycle_id, 201, "SCAN_INTERRUPTED").unwrap())
        .unwrap();
    let while_failed =
        json_body(call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await).await;
    assert_eq!(
        while_failed["usage"]["total_tokens"],
        before.totals.total_tokens
    );

    scanner.shutdown().unwrap();

    // Fresh active epoch 0 is a valid empty API result.
    let empty_fixture = Fixture::new("active-zero");
    let empty_ledger = empty_fixture.ledger();
    let empty_scanner = empty_fixture.start(Arc::clone(&empty_ledger));
    wait_scan(&empty_ledger, None);
    let empty_app = empty_fixture.router(Arc::clone(&empty_ledger), empty_scanner.clone());
    let empty = json_body(
        call(
            &empty_app,
            Method::GET,
            "/api/usage/summary?range=year",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(empty["usage"]["total_tokens"], 0);
    assert_eq!(empty["usage"]["cache_write_tokens"], 0);
    assert!(empty["usage"].get("cache_write_input_tokens").is_none());
    assert!(empty["usage"].get("cache_write_status").is_none());
    empty_scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s06_028_real_http_session_pagination_revision_and_restart_contract() {
    let fixture = Fixture::new("sessions-50-plus-1");
    fixture.seed_many_roots(51);
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let first =
        json_body(call(&app, Method::GET, "/api/usage/sessions?range=year", &[]).await).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 51);
    assert_eq!(first["total_items"], 51);
    assert_eq!(first["sort_index"].as_array().unwrap().len(), 51);
    assert!(first.get("next_cursor").is_none());
    assert!(first["data_revision"].as_i64().unwrap() > 0);
    assert!(
        first["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["inclusive_usage"]["estimated_cost"].is_null())
    );

    append_root_token(&fixture.home, "rollout-00.jsonl", 52, 2);
    request_and_wait(&scanner, &ledger);
    let expected_revision = first["data_revision"].as_i64().unwrap();
    let stale = call(
        &app,
        Method::GET,
        &format!(
            "/api/usage/session-rows?range=year&expected_data_revision={expected_revision}&root_session_id=00000000-03e8-7000-8000-000000000000"
        ),
        &[],
    )
    .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(stale).await["error"]["code"],
        "STALE_DATA_REVISION"
    );

    scanner.shutdown().unwrap();
    let reopened_ledger = fixture.ledger();
    let reopened_scanner = fixture.start(Arc::clone(&reopened_ledger));
    let reopened_app = fixture.router(Arc::clone(&reopened_ledger), reopened_scanner.clone());
    let invalid = call(
        &reopened_app,
        Method::GET,
        "/api/usage/sessions?range=year&cursor=legacy",
        &[],
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(invalid).await["error"]["code"], "INVALID_FILTER");
    reopened_scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s01_001_gate_a_session_qualification_and_full_range_matrix() {
    let fixture = Fixture::new("gate-a-s1");
    fixture.seed_gate_a_tree();
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    fixture.mark_gate_projects();
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let all = json_body(call(&app, Method::GET, "/api/usage/sessions?range=year", &[]).await).await;
    let all_ids = all["sort_index"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["root_session_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        all_ids,
        vec![GATE_ROOT, GATE_OTHER, GATE_PROJECTLESS, GATE_UNKNOWN]
    );
    assert_eq!(all["items"].as_array().unwrap().len(), 4);
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["root_session_id"] != GATE_CHILD
                && item["root_session_id"] != GATE_GRANDCHILD)
    );

    let deep = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&model=deep-model",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(deep["total_items"], 1);
    let row = &deep["items"][0];
    assert_eq!(row["root_session_id"], GATE_ROOT);
    assert_eq!(
        row["models_used"],
        json!(["main-model-a", "main-model-b", "deep-model", "grand-model"])
    );
    assert_eq!(row["self_usage"]["total_tokens"], 125);
    assert_eq!(row["subagent_usage"]["total_tokens"], 73);
    // The 2025 event in the rollout is outside the requested year range.
    assert_eq!(row["inclusive_usage"]["total_tokens"], 198);

    let project = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&project_path=%2Fproject%2Fa",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(project["total_items"], 1);
    assert_eq!(project["items"][0]["root_session_id"], GATE_ROOT);
    let projectless = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&include_projectless=1",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(projectless["total_items"], 1);
    assert_eq!(projectless["items"][0]["root_session_id"], GATE_PROJECTLESS);
    let unknown = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&include_unknown_project=1",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(unknown["total_items"], 1);
    assert_eq!(unknown["items"][0]["root_session_id"], GATE_UNKNOWN);

    let multi_model = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&model=deep-model&model=main-model-b",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(multi_model["total_items"], 1);
    assert_eq!(multi_model["items"][0]["root_session_id"], GATE_ROOT);

    let and_filter = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&model=main-model-a&project_path=%2Fproject%2Fa",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(and_filter["total_items"], 1);
    assert_eq!(and_filter["items"][0]["root_session_id"], GATE_ROOT);

    let disjoint = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&model=other-model&project_path=%2Fproject%2Fa",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(disjoint["total_items"], 0);
    let matching = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&model=other-model&project_path=%2Fproject%2Fb",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(matching["total_items"], 1);
    assert_eq!(matching["items"][0]["root_session_id"], GATE_OTHER);
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s02_001_gate_a_full_sort_index_snapshot_seed_matrix() {
    let fixture = Fixture::new("gate-a-s2");
    fixture.seed_many_roots(61);
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let db = Connection::open(&fixture.db).unwrap();
    db.execute(
        "UPDATE threads SET project_kind='project', project_path='/project/sort', project_name='Project Sort' WHERE thread_id=?1",
        ["00000000-03e8-7000-8000-000000000000"],
    )
    .unwrap();
    drop(db);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());
    let response = call(
        &app,
        Method::GET,
        "/api/usage/sessions?range=year&seed_sort_by=total_tokens&seed_sort_order=asc",
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = json_body(response).await;
    let sort_index = snapshot["sort_index"].as_array().unwrap();
    let items = snapshot["items"].as_array().unwrap();
    assert_eq!(snapshot["total_items"], 61);
    assert_eq!(sort_index.len(), 61);
    assert!(items.len() <= 60);
    let ids = sort_index
        .iter()
        .map(|item| item["root_session_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 61);
    assert!(
        items
            .iter()
            .all(|item| ids.contains(item["root_session_id"].as_str().unwrap()))
    );
    for item in items {
        let id = item["root_session_id"].as_str().unwrap();
        let index = sort_index
            .iter()
            .find(|candidate| candidate["root_session_id"] == id)
            .unwrap();
        assert_eq!(index["last_activity_at_ms"], item["last_activity_at_ms"]);
        assert_eq!(index["total_tokens"], item["self_usage"]["total_tokens"]);
        assert_eq!(
            index["combined_total_tokens"],
            item["inclusive_usage"]["total_tokens"]
        );
        assert_eq!(
            index["cache_hit_rate"],
            item["inclusive_usage"]["cache_hit_rate"]
        );
        assert_eq!(index["model_sort_key"], item["models_used"][0]);
    }
    assert!(items.windows(2).all(|pair| {
        pair[0]["self_usage"]["total_tokens"].as_i64().unwrap()
            <= pair[1]["self_usage"]["total_tokens"].as_i64().unwrap()
    }));

    let project_sorted = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&seed_sort_by=project&seed_sort_order=desc",
            &[],
        )
        .await,
    )
    .await;
    let project_ids = project_sorted["sort_index"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["root_session_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(project_ids, ids);
    let project_item = project_sorted["sort_index"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["root_session_id"] == "00000000-03e8-7000-8000-000000000000")
        .unwrap();
    assert_eq!(project_item["project_sort_key"], "Project Sort");
    assert!(
        project_sorted["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| { project_ids.contains(item["root_session_id"].as_str().unwrap()) })
    );
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s03_001_gate_a_batch_detail_revision_and_cursor_replacement_matrix() {
    let fixture = Fixture::new("gate-a-s3");
    fixture.seed_gate_a_tree();
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    fixture.mark_gate_projects();
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());
    let snapshot = json_body(
        call(
            &app,
            Method::GET,
            "/api/usage/sessions?range=year&model=deep-model",
            &[],
        )
        .await,
    )
    .await;
    let revision = snapshot["data_revision"].as_i64().unwrap();
    assert!(snapshot.get("next_cursor").is_none());

    let rows = json_body(
        call(
            &app,
            Method::GET,
            &format!(
                "/api/usage/session-rows?range=year&model=deep-model&expected_data_revision={revision}&root_session_id={GATE_ROOT}&root_session_id={GATE_ROOT}"
            ),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(rows["items"].as_array().unwrap().len(), 1);
    assert_eq!(rows["items"][0]["inclusive_usage"]["total_tokens"], 198);

    let db = Connection::open(&fixture.db).unwrap();
    db.execute(
        "UPDATE usage_events SET cache_write_tokens=NULL WHERE root_session_id=?1 AND model='main-model-b'",
        [GATE_ROOT],
    )
    .unwrap();
    drop(db);
    let detail = json_body(
        call(
            &app,
            Method::GET,
            &format!(
                "/api/usage/sessions/{GATE_ROOT}/detail?range=year&model=deep-model&expected_data_revision={revision}"
            ),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(detail["root_session_id"], GATE_ROOT);
    assert_eq!(
        detail["last_activity_at_ms"],
        snapshot["items"][0]["last_activity_at_ms"]
    );
    assert_eq!(detail["main"]["model_usage"].as_array().unwrap().len(), 2);
    assert_eq!(detail["main"]["model_usage"][0]["model"], "main-model-a");
    assert_eq!(detail["main"]["model_usage"][1]["model"], "main-model-b");
    assert_eq!(
        detail["main"]["model_usage"][0]["usage"]["total_tokens"],
        100
    );
    assert_eq!(
        detail["main"]["model_usage"][1]["usage"]["total_tokens"],
        25
    );
    assert_eq!(detail["main"]["self_usage"]["total_tokens"], 125);
    assert_eq!(detail["main"]["inclusive_usage"]["total_tokens"], 198);
    assert_eq!(
        detail["main"]["model_usage"][1]["usage"]["reasoning_tokens"],
        5
    );
    assert!(detail["main"]["model_usage"][1]["usage"]["cache_write_tokens"].is_null());
    assert!(
        detail["main"]["model_usage"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model["usage"]["estimated_cost"].is_null())
    );
    assert_eq!(detail["subagents"].as_array().unwrap().len(), 2);
    let child = detail["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["thread_id"] == GATE_CHILD)
        .unwrap();
    let grandchild = detail["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["thread_id"] == GATE_GRANDCHILD)
        .unwrap();
    assert_eq!(child["parent_thread_id"], GATE_ROOT);
    assert_eq!(grandchild["parent_thread_id"], GATE_CHILD);
    let child_model_usage = child["model_usage"].as_array().unwrap();
    let grandchild_model_usage = grandchild["model_usage"].as_array().unwrap();
    assert_eq!(child_model_usage.len(), 1);
    assert_eq!(grandchild_model_usage.len(), 1);
    assert_eq!(child_model_usage[0]["model"], "deep-model");
    assert_eq!(grandchild_model_usage[0]["model"], "grand-model");
    assert_eq!(child_model_usage[0]["usage"]["total_tokens"], 33);
    assert_eq!(child_model_usage[0]["usage"]["reasoning_tokens"], 3);
    assert!(child_model_usage[0]["usage"]["estimated_cost"].is_null());
    assert_eq!(grandchild_model_usage[0]["usage"]["total_tokens"], 40);
    assert!(grandchild_model_usage[0]["usage"]["estimated_cost"].is_null());

    // A root can be eligible solely because a descendant has usage in-range.
    // Detail still returns the descendant aggregate with an empty Main block.
    let db = Connection::open(&fixture.db).unwrap();
    db.execute(
        "DELETE FROM usage_event_occurrences
         WHERE (ledger_epoch, event_id) IN (
             SELECT ledger_epoch, event_id FROM usage_events
             WHERE root_session_id=?1 AND thread_id=?1
         )",
        [GATE_ROOT],
    )
    .unwrap();
    db.execute(
        "DELETE FROM usage_events WHERE root_session_id=?1 AND thread_id=?1",
        [GATE_ROOT],
    )
    .unwrap();
    drop(db);
    let descendant_only = json_body(
        call(
            &app,
            Method::GET,
            &format!(
                "/api/usage/sessions/{GATE_ROOT}/detail?range=year&model=deep-model&expected_data_revision={revision}"
            ),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(descendant_only["main"]["self_usage"]["total_tokens"], 0);
    assert!(
        descendant_only["main"]["models_used"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        descendant_only["main"]["model_usage"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        descendant_only["main"]["inclusive_usage"]["total_tokens"],
        73
    );
    assert_eq!(descendant_only["main"]["subagent_count"], 2);
    assert_eq!(descendant_only["subagents"].as_array().unwrap().len(), 2);

    let too_many = (0..61)
        .map(|index| format!("root_session_id=invalid-{index}"))
        .collect::<Vec<_>>()
        .join("&");
    let too_many_response = call(
        &app,
        Method::GET,
        &format!("/api/usage/session-rows?range=year&{too_many}"),
        &[],
    )
    .await;
    assert_eq!(too_many_response.status(), StatusCode::BAD_REQUEST);
    // The root fixture has its own file name; append directly to the gate root
    // rollout so the revision changes without changing the requested scope.
    let gate_path = fixture
        .home
        .join("sessions")
        .join("rollout-gate-main.jsonl");
    let mut bytes = fs::read(&gate_path).unwrap();
    bytes.extend(records(&[token_record(140, "2026-08-08T01:01:00Z")]));
    fs::write(gate_path, bytes).unwrap();
    request_and_wait(&scanner, &ledger);
    let stale = call(
        &app,
        Method::GET,
        &format!(
            "/api/usage/session-rows?range=year&expected_data_revision={revision}&root_session_id={GATE_ROOT}"
        ),
        &[],
    )
    .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(stale).await["error"]["code"],
        "STALE_DATA_REVISION"
    );
    let stale_detail = call(
        &app,
        Method::GET,
        &format!(
            "/api/usage/sessions/{GATE_ROOT}/detail?range=year&expected_data_revision={revision}"
        ),
        &[],
    )
    .await;
    assert_eq!(stale_detail.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(stale_detail).await["error"]["code"],
        "STALE_DATA_REVISION"
    );
    scanner.shutdown().unwrap();

    let batch_fixture = Fixture::new("gate-a-s3-batch");
    batch_fixture.seed_many_roots(61);
    let batch_ledger = batch_fixture.ledger();
    let batch_scanner = batch_fixture.start(Arc::clone(&batch_ledger));
    wait_scan(&batch_ledger, None);
    let batch_app = batch_fixture.router(Arc::clone(&batch_ledger), batch_scanner.clone());
    let batch_revision = json_body(
        call(
            &batch_app,
            Method::GET,
            "/api/usage/sessions?range=year",
            &[],
        )
        .await,
    )
    .await["data_revision"]
        .as_i64()
        .unwrap();
    let batch_ids = (0..60)
        .map(|number| format!("00000000-03e8-7000-8000-{number:012x}"))
        .collect::<Vec<_>>();
    let query = batch_ids
        .iter()
        .map(|id| format!("root_session_id={id}"))
        .chain(std::iter::once(format!(
            "expected_data_revision={batch_revision}"
        )))
        .collect::<Vec<_>>()
        .join("&");
    let batch_rows = json_body(
        call(
            &batch_app,
            Method::GET,
            &format!("/api/usage/session-rows?range=year&{query}"),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(batch_rows["items"].as_array().unwrap().len(), 60);
    assert_eq!(batch_rows["items"][0]["root_session_id"], batch_ids[0]);
    assert_eq!(batch_rows["items"][59]["root_session_id"], batch_ids[59]);
    let batch_ids_61 = (0..61)
        .map(|number| format!("00000000-03e8-7000-8000-{number:012x}"))
        .collect::<Vec<_>>();
    let query_61 = batch_ids_61
        .iter()
        .map(|id| format!("root_session_id={id}"))
        .chain(std::iter::once(format!(
            "expected_data_revision={batch_revision}"
        )))
        .collect::<Vec<_>>()
        .join("&");
    let too_many_valid = call(
        &batch_app,
        Method::GET,
        &format!("/api/usage/session-rows?range=year&{query_61}"),
        &[],
    )
    .await;
    assert_eq!(too_many_valid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(too_many_valid).await["error"]["code"],
        "INVALID_SESSION_IDS"
    );
    let invalid_id = call(
        &batch_app,
        Method::GET,
        "/api/usage/session-rows?range=year&root_session_id=not-a-session",
        &[],
    )
    .await;
    assert_eq!(invalid_id.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid_id).await["error"]["code"],
        "INVALID_SESSION_IDS"
    );
    batch_scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_mu03_a03_metadata_v2_to_v3_rebuild_persists_canonical_title_for_detail_api() {
    let fixture = Fixture::new("mu03-a03-metadata-rebuild");
    fixture.seed_gate_a_tree();

    // Start with a metadata-v2-shaped state: the subagent has no title and
    // session_index has no usable title candidate.  The first scan therefore
    // persists a NULL canonical title.
    let state = Connection::open(fixture.home.join("state_5.sqlite")).unwrap();
    state
        .execute(
            "UPDATE threads SET title=NULL,name=NULL WHERE id=?1",
            [GATE_CHILD],
        )
        .unwrap();
    drop(state);
    let index = fs::read_to_string(fixture.home.join("session_index.jsonl")).unwrap();
    let index = index
        .lines()
        .filter(|line| !line.contains(GATE_CHILD))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        fixture.home.join("session_index.jsonl"),
        format!("{index}\n"),
    )
    .unwrap();

    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let db = Connection::open(&fixture.db).unwrap();
    let initial_title: Option<String> = db
        .query_row(
            "SELECT title FROM threads WHERE thread_id=?1",
            [GATE_CHILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(initial_title, None);
    drop(db);

    // Emulate an old metadata checkpoint/fact, then provide the v3 rollout
    // agent_path.  The next scan must rebuild metadata only and persist the
    // derived title in threads; the Detail API reads that canonical title.
    let child_path = fixture
        .home
        .join("sessions")
        .join("rollout-gate-child.jsonl");
    fs::write(
        &child_path,
        records(&[
            json!({
                "type":"session_meta",
                "timestamp":"2026-08-08T01:00:05Z",
                "payload":{
                    "id":GATE_CHILD,
                    "cwd":"/project/a",
                    "parent_thread_id":GATE_ROOT,
                    "agent_path":"/root/gate_b_rereview",
                    "source":{"subagent":{"other":"gate"}}
                }
            }),
            json!({
                "type":"turn_context",
                "timestamp":"2026-08-08T01:00:06Z",
                "payload":{"turn_id":"gate-child-v3","model":"deep-model"}
            }),
            reasoning_token_record(30, 3, "2026-08-08T01:00:07Z"),
        ]),
    )
    .unwrap();
    let db = Connection::open(&fixture.db).unwrap();
    db.execute(
        "UPDATE source_checkpoints SET parser_version=2
         WHERE consumer_kind='metadata' AND source_file_id=(
             SELECT source_file_id FROM source_files WHERE current_path=?1
         )",
        [child_path.to_str().unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE rollout_metadata_facts SET metadata_parser_version=2
         WHERE source_file_id=(SELECT source_file_id FROM source_files WHERE current_path=?1)",
        [child_path.to_str().unwrap()],
    )
    .unwrap();
    drop(db);

    request_and_wait(&scanner, &ledger);
    let db = Connection::open(&fixture.db).unwrap();
    let rebuilt_title: Option<String> = db
        .query_row(
            "SELECT title FROM threads WHERE thread_id=?1",
            [GATE_CHILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rebuilt_title.as_deref(), Some("Gate b rereview"));
    drop(db);

    let app = fixture.router(Arc::clone(&ledger), scanner.clone());
    let detail = json_body(
        call(
            &app,
            Method::GET,
            &format!("/api/usage/sessions/{GATE_ROOT}/detail?range=year"),
            &[],
        )
        .await,
    )
    .await;
    let child = detail["subagents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["thread_id"] == GATE_CHILD)
        .unwrap();
    assert_eq!(child["title"], "Gate b rereview");
    assert!(child.get("agent_path").is_none());
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_mu03_f01_real_structure_cost_effort_closes_db_aggregate_detail_chain() {
    let shared = f01_fixture();
    let root_id = shared["root_session_id"].as_str().unwrap();
    let child_id = shared["child_session_id"].as_str().unwrap();
    let fixture = Fixture::new("mu03-f01-real-structure");
    let main_path = fixture.home.join("sessions").join("rollout-f01-main.jsonl");
    let child_path = fixture
        .home
        .join("sessions")
        .join("rollout-f01-child.jsonl");
    fs::write(
        &main_path,
        records(shared["main_rollout"].as_array().unwrap()),
    )
    .unwrap();
    fs::write(
        &child_path,
        records(shared["subagent_rollout"].as_array().unwrap()),
    )
    .unwrap();
    let state = Connection::open(fixture.home.join("state_5.sqlite")).unwrap();
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
        .unwrap();
    for thread in shared["state_threads"].as_array().unwrap() {
        let id = thread["id"].as_str().unwrap();
        let rollout_path = match thread["rollout"].as_str().unwrap() {
            "main" => &main_path,
            "subagent" => &child_path,
            value => panic!("unexpected fixture rollout {value}"),
        };
        state
            .execute(
                "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
                 VALUES (?1,?2,?3,?4,0,'/project/a',?5,NULL,?6,?7)",
                params![
                    id,
                    rollout_path.to_str().unwrap(),
                    thread["created_at_ms"].as_i64().unwrap(),
                    thread["updated_at_ms"].as_i64().unwrap(),
                    thread["title"].as_str(),
                    thread["model"].as_str().unwrap(),
                    thread["agent_role"].as_str().unwrap(),
                ],
            )
            .unwrap();
    }
    for edge in shared["state_spawn_edges"].as_array().unwrap() {
        state
            .execute(
                "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
                 VALUES (?1,?2,?3,?4)",
                params![
                    edge["parent_thread_id"].as_str().unwrap(),
                    edge["child_thread_id"].as_str().unwrap(),
                    edge["status"].as_str().unwrap(),
                    edge["observed_at_ms"].as_i64().unwrap(),
                ],
            )
            .unwrap();
    }
    drop(state);
    fs::write(
        fixture.home.join("session_index.jsonl"),
        records(shared["session_index"].as_array().unwrap()),
    )
    .unwrap();

    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let db = Connection::open(&fixture.db).unwrap();
    let event_rows: Vec<(String, Option<String>, Option<i64>)> = db
        .prepare(
            "SELECT model,reasoning_effort,estimated_cost_nanos_usd
             FROM usage_events WHERE ledger_epoch=(SELECT usage_active_epoch FROM app_meta WHERE id=1)
             ORDER BY thread_id,occurred_at_ms",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(event_rows.len(), 4);
    assert!(
        event_rows
            .iter()
            .all(|(_, effort, cost)| effort.is_some() && cost.is_some())
    );
    assert!(
        event_rows
            .iter()
            .any(|(model, effort, _)| model == "gpt-5.6-sol" && effort.as_deref() == Some("high"))
    );
    assert!(event_rows
        .iter()
        .any(|(model, effort, _)| model == "gpt-5.6-sol" && effort.as_deref() == Some("medium")));
    assert!(
        event_rows
            .iter()
            .any(|(model, effort, _)| model == "gpt-5.6-terra" && effort.as_deref() == Some("max"))
    );
    assert!(
        event_rows
            .iter()
            .any(|(model, effort, _)| model == "gpt-5.6-luna" && effort.as_deref() == Some("high"))
    );
    drop(db);

    let range = TimeRange::new(0, i64::MAX).unwrap();
    let aggregate = UsageLedger::new(&ledger)
        .session_detail_snapshot(range, UsageFilter::default(), None, root_id.to_owned())
        .unwrap()
        .value;
    assert_eq!(aggregate.main.model_usage.len(), 3);
    assert_eq!(aggregate.subagents.len(), 1);
    assert!(
        aggregate
            .main
            .model_usage
            .iter()
            .all(|block| block.usage.estimated_cost_nanos_usd.is_some())
    );
    assert_eq!(
        aggregate.subagents[0].title.as_deref(),
        Some("Gate b rereview")
    );
    assert_eq!(aggregate.subagents[0].model_usage.len(), 1);
    assert_eq!(aggregate.subagents[0].model_usage[0].model, "gpt-5.6-luna");
    assert_eq!(
        aggregate.subagents[0].model_usage[0]
            .reasoning_effort
            .as_deref(),
        Some("high")
    );
    assert!(
        aggregate.subagents[0].model_usage[0]
            .usage
            .estimated_cost_nanos_usd
            .is_some()
    );

    let app = fixture.router(Arc::clone(&ledger), scanner.clone());
    let detail = json_body(
        call(
            &app,
            Method::GET,
            &format!("/api/usage/sessions/{root_id}/detail?range=year"),
            &[],
        )
        .await,
    )
    .await;
    let expected_detail = &shared["api_detail"];
    assert_eq!(
        detail["root_session_id"],
        expected_detail["root_session_id"]
    );
    assert_eq!(detail["main"]["title"], expected_detail["main"]["title"]);
    let blocks = detail["main"]["model_usage"].as_array().unwrap();
    let expected_blocks = expected_detail["main"]["model_usage"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks.len(), expected_blocks.len());
    for (block, expected) in blocks.iter().zip(expected_blocks) {
        assert_eq!(block["model"], expected["model"]);
        assert_eq!(block["reasoning_effort"], expected["reasoning_effort"]);
        assert_eq!(
            block["usage"]["estimated_cost"],
            expected["usage"]["estimated_cost"]
        );
        assert!(block["usage"]["estimated_cost"].is_number());
    }
    let subagent = &detail["subagents"].as_array().unwrap()[0];
    let expected_subagent = &expected_detail["subagents"].as_array().unwrap()[0];
    assert_eq!(subagent["thread_id"], child_id);
    assert_eq!(subagent["title"], expected_subagent["title"]);
    let subagent_blocks = subagent["model_usage"].as_array().unwrap();
    let expected_subagent_blocks = expected_subagent["model_usage"].as_array().unwrap();
    assert_eq!(subagent_blocks.len(), expected_subagent_blocks.len());
    for (block, expected) in subagent_blocks.iter().zip(expected_subagent_blocks) {
        assert_eq!(block["model"], expected["model"]);
        assert_eq!(block["reasoning_effort"], expected["reasoning_effort"]);
        assert_eq!(
            block["usage"]["estimated_cost"],
            expected["usage"]["estimated_cost"]
        );
        assert!(block["usage"]["estimated_cost"].is_number());
    }
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s05_009_010_012_014_015_refresh_target_and_revision_watch_use_durable_anchors() {
    let fixture = Fixture::new("refresh-revision");
    fixture.seed_two_roots();
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let mut receiver = ledger.subscribe_revisions();
    let before = *receiver.borrow_and_update();
    let response = call(
        &app,
        Method::POST,
        "/api/refresh",
        &[("x-usagi-request", "1")],
    )
    .await;
    assert!(matches!(
        response.status(),
        StatusCode::ACCEPTED | StatusCode::OK
    ));
    let body = json_body(response).await;
    let target = body["scan_id"].as_str().unwrap().to_owned();
    assert!(Uuid::parse_str(&target).is_ok());
    assert!(body["status_revision"].as_i64().unwrap() > before.status_revision);
    tokio::time::timeout(Duration::from_secs(2), receiver.changed())
        .await
        .unwrap()
        .unwrap();
    let changed = *receiver.borrow_and_update();
    assert!(changed.status_revision > before.status_revision);

    wait_scan(&ledger, Some(&target));
    let target_status = json_body(
        call(
            &app,
            Method::GET,
            &format!("/api/status?target_scan_id={target}"),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(target_status["target_scan"]["scan_id"], target);
    assert_eq!(target_status["target_scan"]["state"], "completed");

    // Later scans cannot erase the old target row.
    let later = match scanner.request(ScanTrigger::Manual).unwrap() {
        RequestDisposition::Started { scan_id, .. } => scan_id,
        RequestDisposition::Coalesced {
            followup_scan_id, ..
        } => followup_scan_id,
    };
    wait_scan(&ledger, Some(&later));
    let old_again = json_body(
        call(
            &app,
            Method::GET,
            &format!("/api/status?target_scan_id={target}"),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(old_again["target_scan"]["scan_id"], target);
    assert_eq!(old_again["target_scan"]["state"], "completed");

    // Failed lifecycle mutation does not create a false revision publication.
    let direct_fixture = Fixture::new("failed-publish");
    let direct = direct_fixture.ledger();
    let mut rx = direct.subscribe_revisions();
    let id = "10000000-0000-4000-8000-000000000001".to_owned();
    direct
        .mark_scan_started(ScanStartEvent::new(&id, ScanTrigger::Manual, 1).unwrap())
        .unwrap();
    rx.changed().await.unwrap();
    let committed = *rx.borrow_and_update();
    let other = "10000000-0000-4000-8000-000000000002".to_owned();
    assert!(
        direct
            .mark_scan_started(ScanStartEvent::new(&other, ScanTrigger::Manual, 2).unwrap())
            .is_err()
    );
    assert!(!rx.has_changed().unwrap());
    drop(rx);
    direct
        .mark_scan_completed(ScanCompletedEvent::new(&id, 3).unwrap())
        .unwrap();
    let reconnected = direct.subscribe_revisions();
    assert!(reconnected.borrow().status_revision > committed.status_revision);

    let polled = json_body(call(&app, Method::GET, "/api/revision", &[]).await).await;
    let db_state = ledger.app_state().unwrap();
    assert_eq!(polled["data_revision"], db_state.data_revision);
    assert_eq!(polled["status_revision"], db_state.status_revision);
    scanner.shutdown().unwrap();

    // A process-local SSE/watch disappears on restart, but the immutable
    // scan_runs target row remains the recovery anchor in SQLite.
    drop(app);
    drop(scanner);
    drop(ledger);
    let reopened = fixture.ledger();
    let reopened_scanner = fixture.start(Arc::clone(&reopened));
    wait_scan(&reopened, None);
    let reopened_app = fixture.router(Arc::clone(&reopened), reopened_scanner.clone());
    let persisted_target = json_body(
        call(
            &reopened_app,
            Method::GET,
            &format!("/api/status?target_scan_id={target}"),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(persisted_target["target_scan"]["scan_id"], target);
    assert_eq!(persisted_target["target_scan"]["state"], "completed");
    // The restarted coordinator can publish a later status revision between
    // the HTTP read and the direct SQLite read. Stop it before asserting exact
    // equality so this test verifies the durable restart anchor rather than a
    // scheduler race between two individually valid snapshots.
    reopened_scanner.shutdown().unwrap();
    let reopened_revision =
        json_body(call(&reopened_app, Method::GET, "/api/revision", &[]).await).await;
    assert_eq!(
        reopened_revision["status_revision"],
        reopened.app_state().unwrap().status_revision
    );
}

#[tokio::test]
async fn t_s05_013_source_changed_refresh_is_rejected_before_scanner_request() {
    let fixture = Fixture::new("source-changed-refresh");
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let other_home = fixture._root.path().join("different-codex-home");
    fs::create_dir_all(&other_home).unwrap();
    let _mismatch = Ledger::open(LedgerOptions::new(&fixture.db, &other_home)).unwrap();
    let before = ledger.app_state().unwrap();
    assert_eq!(before.scan.source_binding_status.as_str(), "source_changed");
    assert!(before.scan.active_scan_id.is_none());
    assert!(before.scan.followup_scan_id.is_none());

    let response = call(
        &app,
        Method::POST,
        "/api/refresh",
        &[("x-usagi-request", "1")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(response).await["error"]["code"], "SOURCE_CHANGED");

    // The HTTP rejection happens before ScanHandle::request: there is no new
    // active or queued lifecycle row and no status revision is consumed.
    let after = ledger.app_state().unwrap();
    assert_eq!(after.scan.status_revision, before.scan.status_revision);
    assert_eq!(after.scan.active_scan_id, before.scan.active_scan_id);
    assert_eq!(after.scan.followup_scan_id, before.scan.followup_scan_id);
    scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s05_021_concurrent_usage_queries_and_real_scan_never_expose_partial_snapshot() {
    let fixture = Fixture::new("query-scan-concurrency");
    fixture.seed_two_roots();
    let ledger = fixture.ledger();
    let scanner = fixture.start(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    let app = fixture.router(Arc::clone(&ledger), scanner.clone());

    let before =
        json_body(call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await).await;
    let before_revision = before["data_revision"].as_i64().unwrap();
    assert_eq!(before["usage"]["total_tokens"], 30);

    let path = fixture.home.join("sessions").join("rollout-a.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    let ignored = b"{\"type\":\"response_item\",\"payload\":{\"text\":\"concurrency-fixture\"}}\n";
    for _ in 0..50_000 {
        bytes.extend_from_slice(ignored);
    }
    fs::write(&path, bytes).unwrap();
    append_root_a_token(&fixture.home, 15, 5);

    let target = match scanner.request(ScanTrigger::Manual).unwrap() {
        RequestDisposition::Started { scan_id, .. } => scan_id,
        RequestDisposition::Coalesced {
            followup_scan_id, ..
        } => followup_scan_id,
    };

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..96 {
        let app = app.clone();
        tasks.spawn(async move {
            let response = call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            (
                body["data_revision"].as_i64().unwrap(),
                body["usage"]["total_tokens"].as_i64().unwrap(),
            )
        });
    }
    let mut observed = Vec::new();
    while let Some(result) = tasks.join_next().await {
        observed.push(result.unwrap());
    }
    wait_scan(&ledger, Some(&target));
    let after =
        json_body(call(&app, Method::GET, "/api/usage/summary?range=year", &[]).await).await;
    let after_revision = after["data_revision"].as_i64().unwrap();
    assert_eq!(after["usage"]["total_tokens"], 35);
    assert!(after_revision > before_revision);

    for pair in observed {
        assert!(
            pair == (before_revision, 30) || pair == (after_revision, 35),
            "query exposed a partial/mismatched snapshot: {pair:?}; before={before_revision}, after={after_revision}"
        );
    }
    scanner.shutdown().unwrap();
}
