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
    http::{Method, Request, StatusCode},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use usagi::{
    api::{AppContext, QueryApi},
    codex::quota::CodexQuotaService,
    domain::{ScanResult, ScanTrigger},
    platform::browser::SystemBrowser,
    scanner::{CodexMetadata, RequestDisposition, ScanConfig, ScanCoordinator, ScanHandle},
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
    usage::{SessionPageRequest, SummaryQuery, TimeRange, UsageFilter, UsageLedger},
};

const ROOT: &str = "00000000-03e8-7000-8000-000000000101";
const CHILD: &str = "00000000-07d0-7000-8000-000000000102";
const ROOT_TURN: &str = "00000000-05dc-7000-8000-000000000103";
const CHILD_REPLAY_TURN: &str = "00000000-05dc-7000-8000-000000000104";
const CHILD_SOL_TURN: &str = "00000000-0bb8-7000-8000-000000000105";
const CHILD_REVIEW_TURN: &str = "00000000-0fa0-7000-8000-000000000106";
const RESERVE_TURN: &str = "00000000-1388-7000-8000-000000000107";
const TARGET_PRICING_CATALOG_VERSION: i64 = 4;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-mu04-f-{label}-{}-{stamp}",
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
    static_dir: PathBuf,
}

impl Fixture {
    fn new(label: &str, records: FixtureRecords) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let static_dir = root.path().join("static");
        fs::create_dir_all(home.join("sessions")).expect("create sessions");
        fs::create_dir_all(home.join("archived_sessions")).expect("create archives");
        fs::create_dir_all(&static_dir).expect("create static directory");
        fs::write(static_dir.join("index.html"), "<html>fixture</html>").expect("write static");

        let main_rollout = home.join(format!("sessions/rollout-{ROOT}.jsonl"));
        let child_rollout = home.join(format!("sessions/rollout-{CHILD}.jsonl"));
        fs::write(&main_rollout, records_to_bytes(&records.main)).expect("write main rollout");
        fs::write(&child_rollout, records_to_bytes(&records.child)).expect("write child rollout");
        write_state(&home, &main_rollout, &child_rollout);

        Self {
            db: root.path().join("mu.sqlite3"),
            _root: root,
            home,
            static_dir,
        }
    }

    fn ledger(&self) -> Arc<Ledger> {
        Arc::new(
            Ledger::open(LedgerOptions::new(&self.db, &self.home)).expect("open fixture ledger"),
        )
    }

    fn scanner(&self, ledger: Arc<Ledger>) -> ScanHandle {
        ScanCoordinator::start(
            ScanConfig::new(self.home.clone()).with_interval(Duration::from_secs(3_600)),
            ledger,
            CodexMetadata::from_home(self.home.clone()),
        )
        .expect("start fixture scanner")
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
        .expect("build query router")
    }
}

struct FixtureRecords {
    main: Vec<Value>,
    child: Vec<Value>,
}

fn f01_records() -> FixtureRecords {
    FixtureRecords {
        // The first main token has no owning turn context and is deliberately
        // unresolved. The next token is known and contributes to a partial
        // subtotal without allowing the unknown row to erase it.
        main: vec![
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T02:00:00Z",
                "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
            }),
            token(5_000, 5_000, "2026-08-08T02:00:01Z"),
            turn_context(ROOT_TURN, "gpt-5.6-sol", "high", "2026-08-08T02:00:02Z"),
            token(15_000, 10_000, "2026-08-08T02:00:03Z"),
        ],
        // The root replay is discarded until the owning Sol/medium context.
        // The following alias remains stored as codex-auto-review while
        // pricing resolves it through the Luna alias catalog entry.
        child: vec![
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T02:00:04Z",
                "payload": {
                    "id": CHILD,
                    "cwd": "/work/child",
                    "parent_thread_id": ROOT,
                    "source": {"subagent": {"other": "fixture"}}
                }
            }),
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T02:00:05Z",
                "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
            }),
            turn_context(
                CHILD_REPLAY_TURN,
                "gpt-5.6-sol",
                "high",
                "2026-08-08T02:00:06Z",
            ),
            token(30_000, 30_000, "2026-08-08T02:00:07Z"),
            turn_context(
                CHILD_SOL_TURN,
                "gpt-5.6-sol",
                "medium",
                "2026-08-08T02:00:08Z",
            ),
            token(70_000, 70_000, "2026-08-08T02:00:09Z"),
            turn_context(
                CHILD_REVIEW_TURN,
                "codex-auto-review",
                "high",
                "2026-08-08T02:00:10Z",
            ),
            token(120_000, 50_000, "2026-08-08T02:00:11Z"),
        ],
    }
}

fn f02_records() -> FixtureRecords {
    FixtureRecords {
        main: vec![
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T03:00:00Z",
                "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
            }),
            turn_context(ROOT_TURN, "gpt-5.6-sol", "high", "2026-08-08T03:00:01Z"),
            token(80_000, 80_000, "2026-08-08T03:00:02Z"),
        ],
        child: vec![
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T03:00:03Z",
                "payload": {
                    "id": CHILD,
                    "cwd": "/work/child",
                    "parent_thread_id": ROOT,
                    "source": {"subagent": {"other": "fixture"}}
                }
            }),
            turn_context(
                CHILD_SOL_TURN,
                "codex-auto-review",
                "medium",
                "2026-08-08T03:00:04Z",
            ),
            token(40_000, 40_000, "2026-08-08T03:00:05Z"),
            turn_context(
                CHILD_REVIEW_TURN,
                "gpt-5.6-sol",
                "low",
                "2026-08-08T03:00:06Z",
            ),
            token(100_000, 60_000, "2026-08-08T03:00:07Z"),
        ],
    }
}

fn reserve_records() -> FixtureRecords {
    FixtureRecords {
        main: vec![
            json!({
                "type": "session_meta",
                "timestamp": "2026-08-08T04:00:00Z",
                "payload": {"id": ROOT, "cwd": "/work/root", "agent_role": "main"}
            }),
            turn_context(
                RESERVE_TURN,
                "gpt-reserve",
                "medium",
                "2026-08-08T04:00:01Z",
            ),
            token(40_000, 40_000, "2026-08-08T04:00:02Z"),
        ],
        child: vec![json!({
            "type": "session_meta",
            "timestamp": "2026-08-08T04:00:03Z",
            "payload": {
                "id": CHILD,
                "cwd": "/work/child",
                "parent_thread_id": ROOT,
                "source": {"subagent": {"other": "fixture"}}
            }
        })],
    }
}

fn turn_context(turn_id: &str, model: &str, effort: &str, timestamp: &str) -> Value {
    json!({
        "type": "turn_context",
        "timestamp": timestamp,
        "payload": {"turn_id": turn_id, "model": model, "effort": effort}
    })
}

fn token(total: i64, last: i64, timestamp: &str) -> Value {
    json!({
        "type": "event_msg",
        "timestamp": timestamp,
        "payload": {
            "type": "token_count",
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
    let state = Connection::open(home.join("state_5.sqlite")).expect("open state fixture");
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
        .expect("create state fixture tables");
    state
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,1,2,0,'/work/root','Root',NULL,'gpt-5.6-sol','main')",
            params![ROOT, main_rollout.to_str().expect("main path")],
        )
        .expect("insert root state");
    state
        .execute(
            "INSERT INTO threads(
                id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role
             ) VALUES (?1,?2,3,4,0,'/work/child','Child',NULL,'codex-auto-review','subagent')",
            params![CHILD, child_rollout.to_str().expect("child path")],
        )
        .expect("insert child state");
    state
        .execute(
            "INSERT INTO thread_spawn_edges(parent_thread_id,child_thread_id,status,observed_at_ms)
             VALUES (?1,?2,'spawned',3)",
            params![ROOT, CHILD],
        )
        .expect("insert spawn edge");
    drop(state);

    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{{\"id\":\"{ROOT}\",\"thread_name\":\"Root\"}}\n{{\"id\":\"{CHILD}\",\"thread_name\":\"Child\"}}\n"
        ),
    )
    .expect("write session index");
}

fn wait_scan(ledger: &Ledger, wanted: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = ledger.app_state().expect("read scanner state");
        let finished = wanted
            .is_none_or(|scan_id| state.scan.last_finished_scan_id.as_deref() == Some(scan_id));
        if finished
            && state.scan.active_scan_id.is_none()
            && state.scan.last_finished_scan_id.is_some()
        {
            return;
        }
        assert!(Instant::now() < deadline, "scan timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn request_and_wait(scanner: &ScanHandle, ledger: &Ledger) {
    let scan_id = loop {
        match scanner.request(ScanTrigger::Manual) {
            Ok(RequestDisposition::Started { scan_id, .. }) => break scan_id,
            Ok(RequestDisposition::Coalesced {
                followup_scan_id, ..
            }) => break followup_scan_id,
            Err(usagi::scanner::ScanRequestError::Recovering) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("manual scan request failed: {error:?}"),
        }
    };
    wait_scan(ledger, Some(&scan_id));
}

fn active_epoch(connection: &Connection) -> i64 {
    connection
        .query_row(
            "SELECT usage_active_epoch FROM app_meta WHERE id=1",
            [],
            |row| row.get(0),
        )
        .expect("read active epoch")
}

fn active_event_rows(
    connection: &Connection,
    epoch: i64,
) -> Vec<(String, Option<String>, i64, Option<i64>)> {
    let mut statement = connection
        .prepare(
            "SELECT model,reasoning_effort,total_tokens,estimated_cost_nanos_usd
             FROM usage_events WHERE ledger_epoch=?1
             ORDER BY source_file_id,source_start_offset",
        )
        .expect("prepare active event query");
    statement
        .query_map([epoch], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("query active events")
        .map(|row| row.expect("read active event"))
        .collect()
}

async fn call(app: &Router, method: Method, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "127.0.0.1:3210")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("send request")
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("read response body"),
    )
    .expect("decode response body")
}

#[tokio::test]
async fn t_mu04_f01_single_fixture_closes_scanner_db_aggregate_api_contract() {
    let fixture = Fixture::new("f01", f01_records());
    let ledger = fixture.ledger();
    let scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("open fixture database");
    let epoch = active_epoch(&connection);
    let events = active_event_rows(&connection, epoch);
    assert_eq!(events.len(), 4, "unexpected event rows: {events:?}");
    assert!(events.iter().any(|(model, effort, total, cost)| {
        model == "gpt-5.6-sol"
            && effort.as_deref() == Some("medium")
            && *total == 70_000
            && cost.is_some()
    }));
    let review = events
        .iter()
        .find(|(model, _, total, _)| model == "codex-auto-review" && *total == 50_000)
        .expect("auto-review event");
    assert_eq!(review.1.as_deref(), Some("high"));
    assert_eq!(review.3, Some(10_000_000));
    let unresolved = events
        .iter()
        .find(|(model, _, total, _cost)| model == "unknown" && *total == 5_000)
        .expect("true unresolved event");
    assert_eq!(unresolved.3, None);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=?1",
                [epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count event occurrences"),
        4
    );
    drop(connection);

    let range = TimeRange::new(0, i64::MAX).expect("valid range");
    let usage = UsageLedger::new(&ledger);
    let aggregate = usage
        .summary(SummaryQuery::new(range, UsageFilter::default()))
        .expect("aggregate summary");
    assert_eq!(aggregate.totals.total_tokens, 135_000);
    assert_eq!(aggregate.totals.estimated_cost_nanos_usd, Some(330_000_000));
    let session = usage
        .sessions(range, SessionPageRequest::new(10))
        .expect("aggregate sessions")
        .rows
        .into_iter()
        .find(|row| row.root_session_id == ROOT)
        .expect("root session row");
    assert_eq!(session.inclusive_usage.total_tokens, 135_000);
    assert_eq!(
        session.inclusive_usage.estimated_cost_nanos_usd,
        Some(330_000_000)
    );
    let detail = usage
        .session_detail_snapshot(range, UsageFilter::default(), None, ROOT.to_owned())
        .expect("aggregate detail")
        .value;
    assert_eq!(detail.main.inclusive_usage.total_tokens, 135_000);
    assert_eq!(
        detail.main.inclusive_usage.estimated_cost_nanos_usd,
        Some(330_000_000)
    );

    let app = fixture.router(Arc::clone(&ledger), scanner.clone());
    let summary_response = call(&app, Method::GET, "/api/usage/summary?range=year").await;
    assert_eq!(summary_response.status(), StatusCode::OK);
    let summary = json_body(summary_response).await;
    assert_eq!(summary["usage"]["total_tokens"], 135_000);
    assert_eq!(summary["usage"]["estimated_cost"], 0.33);
    assert_eq!(summary["usage"]["estimated_cost_status"], "partial");

    let sessions_response = call(&app, Method::GET, "/api/usage/sessions?range=year").await;
    assert_eq!(sessions_response.status(), StatusCode::OK);
    let sessions = json_body(sessions_response).await;
    let session_json = sessions["items"]
        .as_array()
        .expect("sessions items")
        .iter()
        .find(|item| item["root_session_id"] == ROOT)
        .expect("root session API item");
    assert_eq!(session_json["inclusive_usage"]["total_tokens"], 135_000);
    assert_eq!(session_json["inclusive_usage"]["estimated_cost"], 0.33);
    assert_eq!(
        session_json["inclusive_usage"]["estimated_cost_status"],
        "partial"
    );

    let detail_response = call(
        &app,
        Method::GET,
        &format!("/api/usage/sessions/{ROOT}/detail?range=year"),
    )
    .await;
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_json = json_body(detail_response).await;
    assert_eq!(
        detail_json["main"]["inclusive_usage"]["estimated_cost"],
        0.33
    );
    assert_eq!(
        detail_json["main"]["inclusive_usage"]["estimated_cost_status"],
        "partial"
    );
    let api_subagent = detail_json["subagents"]
        .as_array()
        .expect("subagent detail")
        .iter()
        .next()
        .unwrap_or_else(|| panic!("subagent detail missing: {detail_json:?}"));
    let model_usage = api_subagent["model_usage"]
        .as_array()
        .expect("subagent model usage");
    assert_eq!(model_usage.len(), 2);
    let sol = model_usage
        .iter()
        .find(|block| block["model"] == "gpt-5.6-sol" && block["reasoning_effort"] == "medium")
        .expect("gpt-5.6-sol medium model usage");
    assert_eq!(sol["usage"]["estimated_cost"], 0.28);
    assert_eq!(sol["usage"]["estimated_cost_status"], "complete");
    let review = model_usage
        .iter()
        .find(|block| block["model"] == "codex-auto-review" && block["reasoning_effort"] == "high")
        .expect("codex-auto-review high model usage");
    assert_eq!(review["usage"]["estimated_cost"], 0.01);
    assert_eq!(review["usage"]["estimated_cost_status"], "complete");
    scanner.shutdown().expect("stop F01 scanner");
}

#[test]
fn t_mu04_f02_parser4_pricing1_reprice_and_shadow_rebuild_stay_independent() {
    let fixture = Fixture::new("f02", f02_records());
    let ledger = fixture.ledger();
    let scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    scanner.shutdown().expect("stop initial F02 scanner");
    drop(ledger);

    let connection = Connection::open(&fixture.db).expect("open F02 database");
    let old_epoch = active_epoch(&connection);
    let old_event_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM usage_events WHERE ledger_epoch=?1",
            [old_epoch],
            |row| row.get(0),
        )
        .expect("count old active events");
    let old_token_total: i64 = connection
        .query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM usage_events WHERE ledger_epoch=?1",
            [old_epoch],
            |row| row.get(0),
        )
        .expect("sum old active tokens");
    assert_eq!((old_event_count, old_token_total), (3, 180_000));

    // Simulate a real parser-v4 active epoch with one lost context while the
    // alias row itself remains available to the independent pricing refresh.
    // The pricing value 1 is intentionally stale for this parser/price
    // independence scenario.
    connection
        .execute(
            "UPDATE app_meta SET usage_parser_version=4,
             cost_algorithm_version=1,pricing_catalog_version=1 WHERE id=1",
            [],
        )
        .expect("mark old app versions");
    connection
        .execute(
            "UPDATE source_checkpoints SET parser_version=4
             WHERE consumer_kind='usage'",
            [],
        )
        .expect("mark usage checkpoints parser4");
    connection
        .execute(
            "UPDATE usage_source_states
             SET usage_parser_version=4,canonical_algorithm_version=4
             WHERE ledger_epoch=?1",
            [old_epoch],
        )
        .expect("mark usage source states parser4");
    connection
        .execute(
            "UPDATE usage_events SET model='unknown',reasoning_effort=NULL
             WHERE ledger_epoch=?1 AND model='gpt-5.6-sol'",
            [old_epoch],
        )
        .expect("erase one historical context");
    let alias_event_id: String = connection
        .query_row(
            "SELECT event_id FROM usage_events
             WHERE ledger_epoch=?1 AND model='codex-auto-review'",
            [old_epoch],
            |row| row.get(0),
        )
        .expect("locate alias event");
    connection
        .execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=NULL
             WHERE ledger_epoch=?1 AND event_id=?2",
            params![old_epoch, alias_event_id],
        )
        .expect("clear legacy alias cost");
    drop(connection);

    // Opening the old database performs the catalog reprice only. It keeps the
    // parser-v4 active epoch readable until the scanner starts its own build.
    let reopened = fixture.ledger();
    let connection = Connection::open(&fixture.db).expect("reopen F02 database");
    let versions: (i64, i64, i64, i64, Option<i64>) = connection
        .query_row(
            "SELECT usage_parser_version,cost_algorithm_version,
                    pricing_catalog_version,usage_active_epoch,usage_build_epoch
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
        .expect("read reopened versions");
    assert_eq!(
        versions,
        (4, 1, TARGET_PRICING_CATALOG_VERSION, old_epoch, None)
    );
    let alias_cost: Option<i64> = connection
        .query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events
             WHERE ledger_epoch=?1 AND event_id=?2",
            params![old_epoch, alias_event_id],
            |row| row.get(0),
        )
        .expect("read repriced alias");
    assert_eq!(alias_cost, Some(8_000_000));
    let alias_model: String = connection
        .query_row(
            "SELECT model FROM usage_events WHERE ledger_epoch=?1 AND event_id=?2",
            params![old_epoch, alias_event_id],
            |row| row.get(0),
        )
        .expect("read stored alias model");
    assert_eq!(alias_model, "codex-auto-review");
    drop(connection);
    let old_summary = UsageLedger::new(&reopened)
        .summary(SummaryQuery::new(
            TimeRange::new(0, i64::MAX).expect("valid old range"),
            UsageFilter::default(),
        ))
        .expect("read old active summary");
    assert_eq!(old_summary.totals.total_tokens, old_token_total);
    assert_eq!(old_summary.totals.estimated_cost_nanos_usd, Some(8_000_000));

    let rebuild_scanner = fixture.scanner(Arc::clone(&reopened));
    request_and_wait(&rebuild_scanner, &reopened);
    assert_eq!(
        reopened
            .app_state()
            .expect("read rebuild scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );

    let connection = Connection::open(&fixture.db).expect("read rebuilt F02 database");
    let new_epoch = active_epoch(&connection);
    assert_ne!(new_epoch, old_epoch);
    let final_versions: (i64, Option<i64>, i64, i64) = connection
        .query_row(
            "SELECT usage_parser_version,usage_build_epoch,
                    cost_algorithm_version,pricing_catalog_version
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read final versions");
    assert_eq!(
        final_versions,
        (
            usagi::usage::USAGE_PARSER_VERSION,
            None,
            1,
            TARGET_PRICING_CATALOG_VERSION,
        )
    );
    let events = active_event_rows(&connection, new_epoch);
    assert_eq!(events.len(), old_event_count as usize);
    assert_eq!(events.iter().map(|row| row.2).sum::<i64>(), old_token_total);
    assert!(events.iter().all(|row| row.3.is_some()));
    assert_eq!(
        events
            .iter()
            .map(|row| row.3.expect("known rebuilt cost"))
            .sum::<i64>(),
        568_000_000
    );
    assert!(events.iter().any(|(model, effort, total, _)| {
        model == "codex-auto-review" && effort.as_deref() == Some("medium") && *total == 40_000
    }));
    assert!(events.iter().any(|(model, effort, total, _)| {
        model == "gpt-5.6-sol" && effort.as_deref() == Some("low") && *total == 60_000
    }));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM usage_event_occurrences WHERE ledger_epoch=?1",
                [new_epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rebuilt occurrences"),
        old_event_count
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(DISTINCT event_id) FROM usage_event_occurrences
                 WHERE ledger_epoch=?1",
                [new_epoch],
                |row| row.get::<_, i64>(0),
            )
            .expect("count distinct rebuilt events"),
        old_event_count
    );
    drop(connection);
    rebuild_scanner
        .shutdown()
        .expect("stop rebuilt F02 scanner");
}

#[test]
fn t_mu04_f03_reserve_historical_reprice_preserves_identity_and_epoch() {
    let fixture = Fixture::new("f03-reserve-reprice", reserve_records());
    let ledger = fixture.ledger();
    let scanner = fixture.scanner(Arc::clone(&ledger));
    wait_scan(&ledger, None);
    assert_eq!(
        ledger
            .app_state()
            .expect("read initial Reserve scan state")
            .scan
            .last_finished_scan_result,
        Some(ScanResult::Completed)
    );
    scanner.shutdown().expect("stop initial Reserve scanner");
    drop(ledger);

    let connection = Connection::open(&fixture.db).expect("open Reserve database");
    let old_epoch = active_epoch(&connection);
    let reserve_event: (String, i64, i64, Option<i64>, i64, Option<i64>) = connection
        .query_row(
            "SELECT model,input_tokens,cached_tokens,cache_write_tokens,
                    output_tokens,estimated_cost_nanos_usd
             FROM usage_events WHERE ledger_epoch=?1",
            [old_epoch],
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
        .expect("read initial Reserve event");
    assert_eq!(
        reserve_event,
        (
            "gpt-reserve".to_owned(),
            40_000,
            0,
            Some(0),
            0,
            Some(8_000_000),
        )
    );

    // Simulate an old catalog while parser and canonical versions remain
    // current, then clear the derived cost for the historical Reserve row.
    connection
        .execute(
            "UPDATE app_meta SET cost_algorithm_version=1,
             pricing_catalog_version=3 WHERE id=1",
            [],
        )
        .expect("mark old Reserve pricing catalog");
    connection
        .execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=NULL
             WHERE ledger_epoch=?1 AND model='gpt-reserve'",
            [old_epoch],
        )
        .expect("clear old Reserve event cost");
    drop(connection);

    let reopened = fixture.ledger();
    let connection = Connection::open(&fixture.db).expect("reopen Reserve database");
    let versions: (i64, i64, i64, i64, Option<i64>) = connection
        .query_row(
            "SELECT cost_algorithm_version,pricing_catalog_version,
                    usage_active_epoch,usage_parser_version,usage_build_epoch
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
        .expect("read repriced Reserve versions");
    assert_eq!(
        versions,
        (
            1,
            TARGET_PRICING_CATALOG_VERSION,
            old_epoch,
            usagi::usage::USAGE_PARSER_VERSION,
            None,
        )
    );

    let repriced_event: (String, i64, i64, Option<i64>, i64, Option<i64>) = connection
        .query_row(
            "SELECT model,input_tokens,cached_tokens,cache_write_tokens,
                    output_tokens,estimated_cost_nanos_usd
             FROM usage_events WHERE ledger_epoch=?1",
            [old_epoch],
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
        .expect("read repriced Reserve event");
    assert_eq!(
        repriced_event,
        (
            "gpt-reserve".to_owned(),
            40_000,
            0,
            Some(0),
            0,
            Some(8_000_000),
        )
    );
    drop(connection);
    drop(reopened);
}
