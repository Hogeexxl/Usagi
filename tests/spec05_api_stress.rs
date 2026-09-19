use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use futures_util::StreamExt;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use usagi::{
    api::{AppContext, QueryApi},
    codex::quota::CodexQuotaService,
    platform::browser::SystemBrowser,
    scanner::{CodexMetadata, ScanConfig, ScanCoordinator},
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
};

const ROOT: &str = "00000000-03e8-7000-8000-000000000001";

struct TempRoot(PathBuf);
impl TempRoot {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec05-stress-{}-{stamp}",
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
    rollout: PathBuf,
    ledger: Arc<Ledger>,
    scanner: usagi::scanner::ScanHandle,
    app: Router,
}
impl Fixture {
    fn new() -> Self {
        let root = TempRoot::new();
        let home = root.path().join("codex");
        let static_dir = root.path().join("static");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("archived_sessions")).unwrap();
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("index.html"), "<html>stress</html>").unwrap();
        let rollout = home.join("sessions/rollout-stress.jsonl");
        let ts = chrono::DateTime::from_timestamp_millis(now_ms())
            .unwrap()
            .to_rfc3339();
        fs::write(&rollout, encode(&[
            json!({"type":"session_meta","timestamp":ts,"payload":{"id":ROOT,"cwd":"/work","agent_role":"main"}}),
            json!({"type":"turn_context","timestamp":ts,"payload":{"turn_id":"stress-turn","model":"stress-model"}}),
            token(1, 1, &ts),
        ])).unwrap();
        let state = Connection::open(home.join("state_5.sqlite")).unwrap();
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
        state.execute(
            "INSERT INTO threads(id,rollout_path,created_at_ms,updated_at_ms,archived,cwd,title,name,model,agent_role)
             VALUES (?1,?2,1,2,0,'/work','Stress',NULL,'stress-model','main')",
            params![ROOT, rollout.to_str().unwrap()],
        ).unwrap();
        fs::write(
            home.join("session_index.jsonl"),
            format!("{{\"id\":\"{ROOT}\",\"thread_name\":\"Stress\"}}\n"),
        )
        .unwrap();

        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let scanner = ScanCoordinator::start(
            ScanConfig::new(home.clone()).with_interval(std::time::Duration::from_secs(3_600)),
            Arc::clone(&ledger),
            CodexMetadata::from_home(home.clone()),
        )
        .unwrap();
        wait_quiet(&ledger, Duration::from_secs(8));
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
        Self {
            _root: root,
            rollout,
            ledger,
            scanner,
            app,
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}
fn encode(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for value in values {
        out.extend(serde_json::to_vec(value).unwrap());
        out.push(b'\n');
    }
    out
}
fn token(total: i64, last: i64, at: &str) -> Value {
    json!({"timestamp":at,"type":"event_msg","payload":{"type":"token_count","info":{
        "total_token_usage":{"input_tokens":total,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":total},
        "last_token_usage":{"input_tokens":last,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":last}
    }}})
}
fn wait_quiet(ledger: &Ledger, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let state = ledger.app_state().unwrap();
        if state.scan.last_finished_scan_id.is_some()
            && state.scan.active_scan_id.is_none()
            && state.scan.followup_state.is_none()
        {
            return;
        }
        assert!(Instant::now() < deadline, "scanner did not become idle");
        std::thread::sleep(Duration::from_millis(10));
    }
}
async fn call(app: &Router, method: Method, uri: &str, refresh: bool) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:3210");
    if refresh {
        builder = builder.header("x-usagi-request", "1");
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

#[cfg(target_os = "linux")]
fn proc_metric(name: &str) -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key == name).then(|| value.split_whitespace().next()?.parse().ok())?
        })
}
#[cfg(not(target_os = "linux"))]
fn proc_metric(_name: &str) -> Option<u64> {
    None
}

#[ignore = "Spec05 P2 concurrent HTTP/scanner/SSE stress gate; run explicitly with --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t_s05_022_query_scan_refresh_sse_stress_is_bounded_and_nonstarving() {
    let fixture = Fixture::new();
    let rss_before = proc_metric("VmRSS");
    let threads_before = proc_metric("Threads");
    let started = Instant::now();
    let ticks = Arc::new(AtomicUsize::new(0));

    let ticker_ticks = Arc::clone(&ticks);
    let ticker = tokio::spawn(async move {
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            ticker_ticks.fetch_add(1, Ordering::Relaxed);
        }
    });

    let mut jobs = tokio::task::JoinSet::new();
    for worker in 0..4 {
        let app = fixture.app.clone();
        jobs.spawn(async move {
            for index in 0..120 {
                let uri = match (worker + index) % 5 {
                    0 => "/api/revision",
                    1 => "/api/status",
                    2 => "/api/usage/summary?range=year",
                    3 => "/api/usage/sessions?range=year",
                    _ => "/api/usage/models?range=year",
                };
                let response = call(&app, Method::GET, uri, false).await;
                assert_eq!(response.status(), StatusCode::OK, "query failed: {uri}");
                let _ = json_body(response).await;
                if index % 12 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        });
    }

    let app = fixture.app.clone();
    let rollout = fixture.rollout.clone();
    jobs.spawn(async move {
        for total in 2..=40_i64 {
            let ts = chrono::DateTime::from_timestamp_millis(now_ms())
                .unwrap()
                .to_rfc3339();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&rollout)
                .unwrap();
            use std::io::Write;
            file.write_all(&encode(&[token(total, 1, &ts)])).unwrap();
            drop(file);
            let response = call(&app, Method::POST, "/api/refresh", true).await;
            assert!(matches!(
                response.status(),
                StatusCode::ACCEPTED | StatusCode::OK
            ));
            let body = json_body(response).await;
            assert!(matches!(
                body["disposition"].as_str(),
                Some("started" | "coalesced")
            ));
            tokio::time::sleep(Duration::from_millis(4)).await;
        }
    });

    let app = fixture.app.clone();
    jobs.spawn(async move {
        for _ in 0..16 {
            let response = call(&app, Method::GET, "/api/events", false).await;
            assert_eq!(response.status(), StatusCode::OK);
            let mut stream = response.into_body().into_data_stream();
            let first = tokio::time::timeout(Duration::from_secs(1), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(String::from_utf8_lossy(&first).contains("event: revision"));
            // Deliberately disconnect without draining every intermediate event.
            drop(stream);
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    });

    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(result) = jobs.join_next().await {
            result.unwrap();
        }
        ticker.await.unwrap();
    })
    .await
    .expect("Spec05 stress workload deadlocked or starved");

    wait_quiet(&fixture.ledger, Duration::from_secs(10));
    let final_summary = json_body(
        call(
            &fixture.app,
            Method::GET,
            "/api/usage/summary?range=year",
            false,
        )
        .await,
    )
    .await;
    assert_eq!(final_summary["usage"]["total_tokens"], 40);
    assert!(
        ticks.load(Ordering::Relaxed) >= 100,
        "Tokio timer was starved under load"
    );
    assert!(
        started.elapsed() <= Duration::from_secs(30),
        "stress workload exceeded 30s budget"
    );

    if let (Some(before), Some(after)) = (rss_before, proc_metric("VmRSS")) {
        assert!(
            after.saturating_sub(before) <= 64 * 1024,
            "stress RSS grew by >64MiB: before={before}KiB after={after}KiB"
        );
    }
    if let (Some(before), Some(after)) = (threads_before, proc_metric("Threads")) {
        assert!(
            after <= before + 8,
            "stress leaked OS threads: before={before} after={after}"
        );
    }
    fixture.scanner.shutdown().unwrap();
}
