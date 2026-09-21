//! Spec 01 Phase 5: Source filter, internal API, and session identity DTOs.
//!
//! Tests the Phase 5 Gate and Spec §25 mandatory test cases:
//! - Q01: Codex-only data invariance (no filter == source=codex for KPI, sessions, charts)
//! - Q02: Multi-source isolation and combined filtering (all = codex + fake)
//! - Q03: Session identity and Codex backward compatibility
//! - Q04: Public API v1 /v1/usage/summary isolation (recodex_turns only Codex data)
//! - Q06: Filter options source facts and fallback display names
//! - Q07: All session identity DTOs carry source and native_session_id

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use rusqlite::{Connection, params};
use serde_json::Value;
use tower::ServiceExt;
use usagi::{
    api::{AppContext, QueryApi},
    codex::{CodexAdapter, CodexConfig},
    codex::{CodexSessionErrorSidecar, quota::CodexQuotaService},
    ingestion::ScanHandle,
    ingestion::{IngestionConfig, IngestionCoordinator},
    platform::browser::SystemBrowser,
    source::{
        AdapterAvailability, SourceAdapter, SourceAdapterError, SourceDescriptor, SourceId,
        SourceRegistry, SourceRunContext, SourceRunResult,
    },
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
};

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-phase5-{label}-{}-{stamp}",
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

struct ProbeAdapter {
    descriptor: SourceDescriptor,
}

impl ProbeAdapter {
    fn new(id: &'static str, display_name: &'static str) -> Self {
        Self {
            descriptor: SourceDescriptor::new(
                SourceId::new(id).expect("valid source id"),
                display_name,
            ),
        }
    }
}

impl SourceAdapter for ProbeAdapter {
    fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> Result<AdapterAvailability, SourceAdapterError> {
        Ok(AdapterAvailability::Available)
    }

    fn run_scan(
        &self,
        _context: &SourceRunContext,
        _cancellation: &std::sync::atomic::AtomicBool,
    ) -> SourceRunResult {
        Ok(())
    }
}

struct TestApp {
    _root: TempRoot,
    db_path: PathBuf,
    #[allow(dead_code)]
    ledger: Arc<Ledger>,
    scanner: ScanHandle,
    router: Router,
}

impl TestApp {
    fn new(label: &str) -> Self {
        Self::with_extra_adapters(label, |_| {})
    }

    fn with_extra_adapters(label: &str, configure: impl FnOnce(&mut SourceRegistry)) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let static_dir = root.path().join("static");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("archived_sessions")).unwrap();
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("index.html"), "<html>phase5</html>").unwrap();

        let db_path = root.path().join("mu.sqlite3");
        let ledger = Arc::new(Ledger::open(LedgerOptions::new(&db_path)).unwrap());

        let mut registry = SourceRegistry::new();
        registry
            .register(CodexAdapter::new(CodexConfig::from_home(home.clone())))
            .expect("register Codex source");

        configure(&mut registry);

        let scanner = IngestionCoordinator::start(
            IngestionConfig::default().with_interval(Duration::from_secs(3_600)),
            Arc::clone(&ledger),
            registry.clone(),
        )
        .unwrap();

        wait_scan(&ledger);

        let router = QueryApi::router(
            AppContext {
                ledger: Arc::clone(&ledger),
                scanner: scanner.clone(),
                source_registry: registry,
                codex_quota_service: CodexQuotaService::unavailable(&home),
                codex_session_error_sidecar: Arc::new(CodexSessionErrorSidecar),
                update_service: UpdateService::unavailable(),
                browser_opener: Arc::new(SystemBrowser),
            },
            static_dir,
        )
        .unwrap();

        Self {
            _root: root,
            db_path,
            ledger,
            scanner,
            router,
        }
    }

    fn connect(&self) -> Connection {
        Connection::open(&self.db_path).unwrap()
    }

    async fn call(&self, method: Method, uri: &str) -> axum::response::Response {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:3210")
            .body(Body::empty())
            .unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    async fn json(&self, method: Method, uri: &str) -> Value {
        let response = self.call(method, uri).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Request to {uri} failed with status {}",
            response.status()
        );
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn shutdown(self) {
        let _ = self.scanner.shutdown();
    }
}

fn wait_scan(ledger: &Ledger) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = ledger.app_state().unwrap();
        if state.scan.last_finished_scan_id.is_some() && state.scan.active_scan_id.is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "startup scan timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn insert_epoch(conn: &Connection, source: &str, active_epoch: i64) {
    conn.execute(
        "INSERT INTO source_usage_epochs(source, active_epoch, active_parser_version)
         VALUES (?1, ?2, 1)
         ON CONFLICT(source) DO UPDATE SET active_epoch = excluded.active_epoch",
        params![source, active_epoch],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_thread(
    conn: &Connection,
    thread_id: &str,
    source: &str,
    native_session_id: &str,
    parent_thread_id: Option<&str>,
    root_session_id: &str,
    agent_role: &str,
    title: Option<&str>,
    project_name: Option<&str>,
    project_path: Option<&str>,
    project_kind: &str,
    timestamp_ms: i64,
) {
    conn.execute(
        "INSERT INTO threads(
            thread_id, source, native_session_id, parent_thread_id, root_session_id,
            agent_role, title, project_name, project_path, project_kind, metadata_model,
            created_at_ms, updated_at_ms, archived, metadata_quality_status, metadata_resolved_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11, ?11, 0, 'complete', ?11)",
        params![
            thread_id,
            source,
            native_session_id,
            parent_thread_id,
            root_session_id,
            agent_role,
            title,
            project_name,
            project_path,
            project_kind,
            timestamp_ms,
        ],
    )
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn insert_event(
    conn: &Connection,
    source: &str,
    source_epoch: i64,
    event_id: &str,
    thread_id: &str,
    root_session_id: &str,
    model: &str,
    input_tokens: i64,
    cached_tokens: i64,
    output_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    cost_nanos: i64,
    occurred_at_ms: i64,
) {
    conn.execute(
        "INSERT INTO usage_events(
            source, source_epoch, event_id, event_kind, occurred_at_ms,
            thread_id, root_session_id, turn_key, model, reasoning_effort,
            estimated_cost_nanos_usd, input_tokens, cached_tokens, cache_write_tokens,
            output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms
        ) VALUES (?1, ?2, ?3, 'normal', ?4, ?5, ?6, NULL, ?7, NULL, ?8, ?9, ?10, 0, ?11, ?12, ?13, 'complete', ?4)",
        params![
            source,
            source_epoch,
            event_id,
            occurred_at_ms,
            thread_id,
            root_session_id,
            model,
            cost_nanos,
            input_tokens,
            cached_tokens,
            output_tokens,
            reasoning_tokens,
            total_tokens,
        ],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Q01: Codex-only 数据不变性
// ---------------------------------------------------------------------------
#[tokio::test]
async fn q01_codex_only_data_invariance() {
    let app = TestApp::new("q01");
    let ts = now_ms();

    // Seed Codex-only fixture with multiple sessions, subagent, models, and projects
    {
        let conn = app.connect();
        insert_epoch(&conn, "codex", 1);

        // Root session 1 + subagent in project-alpha
        insert_thread(
            &conn,
            "codex-root-01",
            "codex",
            "codex-root-01",
            None,
            "codex-root-01",
            "main",
            Some("Alpha Main Session"),
            Some("project-alpha"),
            Some("/work/project-alpha"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "codex-sub-01",
            "codex",
            "codex-sub-01",
            Some("codex-root-01"),
            "codex-root-01",
            "subagent",
            Some("Alpha Subagent"),
            Some("project-alpha"),
            Some("/work/project-alpha"),
            "project",
            ts,
        );

        // Root session 2 in project-beta
        insert_thread(
            &conn,
            "codex-root-02",
            "codex",
            "codex-root-02",
            None,
            "codex-root-02",
            "main",
            Some("Beta Main Session"),
            Some("project-beta"),
            Some("/work/project-beta"),
            "project",
            ts,
        );

        // Usage events for Root 1 (gpt-4o), Subagent 1 (gpt-4o-mini), Root 2 (gpt-5.6-sol)
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-01",
            "codex-root-01",
            "codex-root-01",
            "gpt-4o",
            500,
            100,
            200,
            50,
            700,
            5_000_000,
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-02",
            "codex-sub-01",
            "codex-root-01",
            "gpt-4o-mini",
            300,
            50,
            100,
            20,
            400,
            1_000_000,
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-03",
            "codex-root-02",
            "codex-root-02",
            "gpt-5.6-sol",
            1000,
            200,
            400,
            100,
            1400,
            15_000_000,
            ts,
        );

        drop(conn);
    }

    // 1. KPI / Summary
    let summary_no_filter = app.json(Method::GET, "/api/usage/summary?range=year").await;
    let summary_codex = app
        .json(Method::GET, "/api/usage/summary?range=year&source=codex")
        .await;
    assert_eq!(
        summary_no_filter, summary_codex,
        "Q01: Summary KPI must be identical between unfiltered and source=codex"
    );
    assert_eq!(summary_no_filter["usage"]["total_tokens"], 2500);
    assert_eq!(summary_no_filter["usage"]["input_tokens"], 1800);
    assert_eq!(summary_no_filter["usage"]["output_tokens"], 700);
    assert_eq!(summary_no_filter["usage"]["cached_tokens"], 350);
    assert_eq!(summary_no_filter["usage"]["reasoning_tokens"], 170);
    assert_eq!(summary_no_filter["usage"]["session_count"], 2);

    // 2. Sessions list and sort_index
    let sessions_no_filter = app
        .json(Method::GET, "/api/usage/sessions?range=year")
        .await;
    let sessions_codex = app
        .json(Method::GET, "/api/usage/sessions?range=year&source=codex")
        .await;
    assert_eq!(
        sessions_no_filter, sessions_codex,
        "Q01: Sessions and sort_index must be identical between unfiltered and source=codex"
    );
    let items = sessions_no_filter["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let sort_index = sessions_no_filter["sort_index"].as_array().unwrap();
    assert_eq!(sort_index.len(), 2);

    // 3. Model distribution chart
    let models_no_filter = app
        .json(Method::GET, "/api/usage/model-distribution?range=year")
        .await;
    let models_codex = app
        .json(
            Method::GET,
            "/api/usage/model-distribution?range=year&source=codex",
        )
        .await;
    assert_eq!(
        models_no_filter, models_codex,
        "Q01: Model distribution must be identical between unfiltered and source=codex"
    );
    assert_eq!(models_no_filter["items"].as_array().unwrap().len(), 3);

    let models_endpoint_no_filter = app.json(Method::GET, "/api/usage/models?range=year").await;
    let models_endpoint_codex = app
        .json(Method::GET, "/api/usage/models?range=year&source=codex")
        .await;
    assert_eq!(
        models_endpoint_no_filter, models_endpoint_codex,
        "Q01: /usage/models must preserve Codex-only results with source=codex"
    );

    // 4. Project distribution chart
    let projects_no_filter = app
        .json(Method::GET, "/api/usage/projects?range=year")
        .await;
    let projects_codex = app
        .json(Method::GET, "/api/usage/projects?range=year&source=codex")
        .await;
    assert_eq!(
        projects_no_filter, projects_codex,
        "Q01: Project distribution must be identical between unfiltered and source=codex"
    );
    assert_eq!(projects_no_filter["items"].as_array().unwrap().len(), 2);

    app.shutdown();
}

// ---------------------------------------------------------------------------
// Q02: 多 Source 隔离与组合过滤
// ---------------------------------------------------------------------------
#[tokio::test]
async fn q02_multisource_isolation_and_combined_filtering() {
    let app = TestApp::new("q02");
    let ts = now_ms();

    {
        let conn = app.connect();
        insert_epoch(&conn, "codex", 1);
        insert_epoch(&conn, "fake", 1);

        // Codex session: root + subagent
        insert_thread(
            &conn,
            "codex-root",
            "codex",
            "codex-root",
            None,
            "codex-root",
            "main",
            Some("Codex Main"),
            Some("codex-proj"),
            Some("/work/codex-proj"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "codex-child",
            "codex",
            "codex-child",
            Some("codex-root"),
            "codex-root",
            "subagent",
            Some("Codex Sub"),
            Some("codex-proj"),
            Some("/work/codex-proj"),
            "project",
            ts,
        );

        // Fake session: root + subagent
        insert_thread(
            &conn,
            "fake-root",
            "fake",
            "fake-native-root",
            None,
            "fake-root",
            "main",
            Some("Fake Main"),
            Some("fake-proj"),
            Some("/work/fake-proj"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "fake-child",
            "fake",
            "fake-native-child",
            Some("fake-root"),
            "fake-root",
            "subagent",
            Some("Fake Sub"),
            Some("fake-proj"),
            Some("/work/fake-proj"),
            "project",
            ts,
        );

        // Codex usage: total 700 + 400 = 1100 tokens, input 500+300=800, output 200+100=300
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-1",
            "codex-root",
            "codex-root",
            "gpt-4o",
            500,
            100,
            200,
            50,
            700,
            5_000_000,
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-2",
            "codex-child",
            "codex-root",
            "gpt-4o-mini",
            300,
            50,
            100,
            20,
            400,
            1_000_000,
            ts,
        );

        // Fake usage: total 1500 + 3000 = 4500 tokens, input 1000+2000=3000, output 500+1000=1500
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev-1",
            "fake-root",
            "fake-root",
            "fake-model-alpha",
            1000,
            200,
            500,
            100,
            1500,
            15_000_000,
            ts,
        );
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev-2",
            "fake-child",
            "fake-root",
            "fake-model-beta",
            2000,
            400,
            1000,
            200,
            3000,
            30_000_000,
            ts,
        );

        drop(conn);
    }

    // 1. Summary isolation and combination
    let sum_all = app.json(Method::GET, "/api/usage/summary?range=year").await;
    let sum_codex = app
        .json(Method::GET, "/api/usage/summary?range=year&source=codex")
        .await;
    let sum_fake = app
        .json(Method::GET, "/api/usage/summary?range=year&source=fake")
        .await;

    // Verify codex values
    assert_eq!(sum_codex["usage"]["total_tokens"], 1100);
    assert_eq!(sum_codex["usage"]["input_tokens"], 800);
    assert_eq!(sum_codex["usage"]["output_tokens"], 300);
    assert_eq!(sum_codex["usage"]["cached_tokens"], 150);
    assert_eq!(sum_codex["usage"]["session_count"], 1);

    // Verify fake values
    assert_eq!(sum_fake["usage"]["total_tokens"], 4500);
    assert_eq!(sum_fake["usage"]["input_tokens"], 3000);
    assert_eq!(sum_fake["usage"]["output_tokens"], 1500);
    assert_eq!(sum_fake["usage"]["cached_tokens"], 600);
    assert_eq!(sum_fake["usage"]["session_count"], 1);

    // Verify combined values equal sum of components
    assert_eq!(sum_all["usage"]["total_tokens"], 5600);
    assert_eq!(sum_all["usage"]["input_tokens"], 3800);
    assert_eq!(sum_all["usage"]["output_tokens"], 1800);
    assert_eq!(sum_all["usage"]["cached_tokens"], 750);
    assert_eq!(sum_all["usage"]["session_count"], 2);

    assert_eq!(
        sum_all["usage"]["total_tokens"].as_i64().unwrap(),
        sum_codex["usage"]["total_tokens"].as_i64().unwrap()
            + sum_fake["usage"]["total_tokens"].as_i64().unwrap()
    );
    assert_eq!(
        sum_all["usage"]["session_count"].as_i64().unwrap(),
        sum_codex["usage"]["session_count"].as_i64().unwrap()
            + sum_fake["usage"]["session_count"].as_i64().unwrap()
    );

    // 2. Sessions isolation and combination
    let sess_all = app
        .json(Method::GET, "/api/usage/sessions?range=year")
        .await;
    let sess_codex = app
        .json(Method::GET, "/api/usage/sessions?range=year&source=codex")
        .await;
    let sess_fake = app
        .json(Method::GET, "/api/usage/sessions?range=year&source=fake")
        .await;

    assert_eq!(sess_all["items"].as_array().unwrap().len(), 2);
    assert_eq!(sess_all["sort_index"].as_array().unwrap().len(), 2);

    assert_eq!(sess_codex["items"].as_array().unwrap().len(), 1);
    assert_eq!(sess_codex["items"][0]["root_session_id"], "codex-root");
    assert_eq!(sess_codex["sort_index"].as_array().unwrap().len(), 1);
    assert_eq!(sess_codex["sort_index"][0]["root_session_id"], "codex-root");

    assert_eq!(sess_fake["items"].as_array().unwrap().len(), 1);
    assert_eq!(sess_fake["items"][0]["root_session_id"], "fake-root");
    assert_eq!(sess_fake["sort_index"].as_array().unwrap().len(), 1);
    assert_eq!(sess_fake["sort_index"][0]["root_session_id"], "fake-root");

    // 3. Model distribution isolation
    let models_codex = app
        .json(
            Method::GET,
            "/api/usage/model-distribution?range=year&source=codex",
        )
        .await;
    let codex_models: Vec<String> = models_codex["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["model"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(codex_models, vec!["gpt-4o", "gpt-4o-mini"]);

    let models_fake = app
        .json(
            Method::GET,
            "/api/usage/model-distribution?range=year&source=fake",
        )
        .await;
    let fake_models: Vec<String> = models_fake["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["model"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(fake_models, vec!["fake-model-alpha", "fake-model-beta"]);

    let models_all = app.json(Method::GET, "/api/usage/models?range=year").await;
    let models_codex_endpoint = app
        .json(Method::GET, "/api/usage/models?range=year&source=codex")
        .await;
    let models_fake_endpoint = app
        .json(Method::GET, "/api/usage/models?range=year&source=fake")
        .await;
    let model_names = |value: &Value| {
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["model"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(model_names(&models_codex_endpoint), codex_models);
    assert_eq!(
        model_names(&models_fake_endpoint),
        vec!["fake-model-beta".to_owned(), "fake-model-alpha".to_owned()]
    );
    assert_eq!(
        model_names(&models_all),
        vec![
            "fake-model-beta".to_owned(),
            "fake-model-alpha".to_owned(),
            "gpt-4o".to_owned(),
            "gpt-4o-mini".to_owned(),
        ]
    );
    let models_unknown = app
        .json(
            Method::GET,
            "/api/usage/models?range=year&source=unknown-source",
        )
        .await;
    assert!(models_unknown["items"].as_array().unwrap().is_empty());

    // 4. Project distribution isolation
    let projs_codex = app
        .json(Method::GET, "/api/usage/projects?range=year&source=codex")
        .await;
    let codex_paths: Vec<&str> = projs_codex["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["project_path"].as_str().unwrap())
        .collect();
    assert_eq!(codex_paths, vec!["/work/codex-proj"]);

    let projs_fake = app
        .json(Method::GET, "/api/usage/projects?range=year&source=fake")
        .await;
    let fake_paths: Vec<&str> = projs_fake["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["project_path"].as_str().unwrap())
        .collect();
    assert_eq!(fake_paths, vec!["/work/fake-proj"]);

    app.shutdown();
}

#[tokio::test]
async fn q05_codex_quarantine_and_session_health_are_source_isolated() {
    let app = TestApp::new("q05-quarantine-isolation");
    let ts = now_ms();
    {
        let conn = app.connect();
        insert_epoch(&conn, "codex", 1);
        insert_epoch(&conn, "fake", 1);
        insert_thread(
            &conn,
            "codex-quarantined",
            "codex",
            "codex-quarantined",
            None,
            "codex-quarantined",
            "main",
            Some("Codex quarantined"),
            None,
            None,
            "projectless",
            ts,
        );
        insert_thread(
            &conn,
            "fake-quarantined",
            "fake",
            "fake-quarantined",
            None,
            "fake-quarantined",
            "main",
            Some("Fake quarantined"),
            None,
            None,
            "projectless",
            ts,
        );
        conn.execute(
            "INSERT INTO codex_usage_session_quarantine(
                 ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,
                 first_seen_at_ms,updated_at_ms
             ) VALUES (?1,?2,?3,?4,?4,?4), (?1,?5,?3,?4,?4,?4)",
            params![
                1_i64,
                "codex-quarantined",
                "ERR_TEST",
                ts,
                "fake-quarantined"
            ],
        )
        .unwrap();
    }

    let all = app.json(Method::GET, "/api/usage/summary?range=year").await;
    let codex = app
        .json(Method::GET, "/api/usage/summary?range=year&source=codex")
        .await;
    let fake = app
        .json(Method::GET, "/api/usage/summary?range=year&source=fake")
        .await;
    assert_eq!(all["usage"]["session_health"]["error_sessions"], 1);
    assert_eq!(codex["usage"]["session_health"]["error_sessions"], 1);
    assert_eq!(fake["usage"]["session_health"]["error_sessions"], 0);
    assert_eq!(
        fake["usage"]["session_health"]["total_sessions"],
        fake["usage"]["session_count"]
    );

    let fake_sessions = app
        .json(Method::GET, "/api/usage/sessions?range=year&source=fake")
        .await;
    assert!(fake_sessions["items"].as_array().unwrap().is_empty());
    assert!(fake_sessions["sort_index"].as_array().unwrap().is_empty());
    app.shutdown();
}

// ---------------------------------------------------------------------------
// Q03: Session 标识与 Codex 兼容
// ---------------------------------------------------------------------------
#[tokio::test]
async fn q03_session_identity_and_codex_compatibility() {
    let app = TestApp::new("q03");
    let ts = now_ms();

    {
        let conn = app.connect();
        insert_epoch(&conn, "codex", 1);
        insert_epoch(&conn, "fake", 1);

        // Historical Codex session: original UUID-like ID, no namespace prefix
        insert_thread(
            &conn,
            "00000000-03e8-7000-8000-000000000001",
            "codex",
            "00000000-03e8-7000-8000-000000000001",
            None,
            "00000000-03e8-7000-8000-000000000001",
            "main",
            Some("Codex Session Title"),
            Some("codex-proj"),
            Some("/work/codex-proj"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "00000000-03e8-7000-8000-000000000002",
            "codex",
            "00000000-03e8-7000-8000-000000000002",
            Some("00000000-03e8-7000-8000-000000000001"),
            "00000000-03e8-7000-8000-000000000001",
            "subagent",
            Some("Codex Subagent Title"),
            Some("codex-proj"),
            Some("/work/codex-proj"),
            "project",
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-1",
            "00000000-03e8-7000-8000-000000000001",
            "00000000-03e8-7000-8000-000000000001",
            "gpt-4o",
            100,
            0,
            50,
            0,
            150,
            1_000_000,
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-2",
            "00000000-03e8-7000-8000-000000000002",
            "00000000-03e8-7000-8000-000000000001",
            "gpt-4o-mini",
            200,
            0,
            100,
            0,
            300,
            1_000_000,
            ts,
        );

        // New Fake session: distinct native_session_id
        insert_thread(
            &conn,
            "fake:conv-100",
            "fake",
            "conv-100",
            None,
            "fake:conv-100",
            "main",
            Some("Fake Session Title"),
            Some("fake-proj"),
            Some("/work/fake-proj"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "fake:conv-100-sub",
            "fake",
            "conv-100-sub",
            Some("fake:conv-100"),
            "fake:conv-100",
            "subagent",
            Some("Fake Subagent Title"),
            Some("fake-proj"),
            Some("/work/fake-proj"),
            "project",
            ts,
        );
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev-1",
            "fake:conv-100",
            "fake:conv-100",
            "fake-model",
            300,
            0,
            150,
            0,
            450,
            2_000_000,
            ts,
        );
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev-2",
            "fake:conv-100-sub",
            "fake:conv-100",
            "fake-model",
            200,
            0,
            100,
            0,
            300,
            1_000_000,
            ts,
        );

        drop(conn);
    }

    // 1. Check Session list and sort_index
    let sessions = app
        .json(Method::GET, "/api/usage/sessions?range=year")
        .await;
    let items = sessions["items"].as_array().unwrap();

    let codex_item = items
        .iter()
        .find(|i| i["root_session_id"] == "00000000-03e8-7000-8000-000000000001")
        .expect("codex item present");
    assert_eq!(
        codex_item["root_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );
    assert_eq!(codex_item["source"], "codex");
    assert_eq!(
        codex_item["native_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );

    let fake_item = items
        .iter()
        .find(|i| i["root_session_id"] == "fake:conv-100")
        .expect("fake item present");
    assert_eq!(fake_item["root_session_id"], "fake:conv-100");
    assert_eq!(fake_item["source"], "fake");
    assert_eq!(fake_item["native_session_id"], "conv-100");

    let sort_index = sessions["sort_index"].as_array().unwrap();
    let codex_index = sort_index
        .iter()
        .find(|i| i["root_session_id"] == "00000000-03e8-7000-8000-000000000001")
        .expect("codex sort index present");
    assert_eq!(codex_index["source"], "codex");
    assert_eq!(
        codex_index["native_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );

    let fake_index = sort_index
        .iter()
        .find(|i| i["root_session_id"] == "fake:conv-100")
        .expect("fake sort index present");
    assert_eq!(fake_index["source"], "fake");
    assert_eq!(fake_index["native_session_id"], "conv-100");

    // 2. Check Session detail for Codex (preserves root_session_id and thread_id)
    let codex_detail = app
        .json(
            Method::GET,
            "/api/usage/sessions/00000000-03e8-7000-8000-000000000001/detail?range=year",
        )
        .await;
    assert_eq!(
        codex_detail["root_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );
    assert_eq!(codex_detail["source"], "codex");
    assert_eq!(
        codex_detail["native_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );

    let codex_main = &codex_detail["main"];
    assert_eq!(
        codex_main["thread_id"],
        "00000000-03e8-7000-8000-000000000001"
    );
    assert_eq!(
        codex_main["root_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );
    assert_eq!(codex_main["source"], "codex");
    assert_eq!(
        codex_main["native_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );

    let codex_subs = codex_detail["subagents"].as_array().unwrap();
    assert_eq!(codex_subs.len(), 1);
    assert_eq!(
        codex_subs[0]["thread_id"],
        "00000000-03e8-7000-8000-000000000002"
    );
    assert_eq!(
        codex_subs[0]["root_session_id"],
        "00000000-03e8-7000-8000-000000000001"
    );
    assert_eq!(codex_subs[0]["source"], "codex");
    assert_eq!(
        codex_subs[0]["native_session_id"],
        "00000000-03e8-7000-8000-000000000002"
    );

    // 3. Check Session detail for Fake
    let fake_detail = app
        .json(
            Method::GET,
            "/api/usage/sessions/fake:conv-100/detail?range=year",
        )
        .await;
    assert_eq!(fake_detail["root_session_id"], "fake:conv-100");
    assert_eq!(fake_detail["source"], "fake");
    assert_eq!(fake_detail["native_session_id"], "conv-100");

    let fake_main = &fake_detail["main"];
    assert_eq!(fake_main["thread_id"], "fake:conv-100");
    assert_eq!(fake_main["root_session_id"], "fake:conv-100");
    assert_eq!(fake_main["source"], "fake");
    assert_eq!(fake_main["native_session_id"], "conv-100");

    let fake_subs = fake_detail["subagents"].as_array().unwrap();
    assert_eq!(fake_subs.len(), 1);
    assert_eq!(fake_subs[0]["thread_id"], "fake:conv-100-sub");
    assert_eq!(fake_subs[0]["root_session_id"], "fake:conv-100");
    assert_eq!(fake_subs[0]["source"], "fake");
    assert_eq!(fake_subs[0]["native_session_id"], "conv-100-sub");

    app.shutdown();
}

// ---------------------------------------------------------------------------
// Q04: Public API v1 /v1/usage/summary 隔离
// ---------------------------------------------------------------------------
#[tokio::test]
async fn q04_public_v1_usage_summary_isolation() {
    let app = TestApp::new("q04");
    let ts = now_ms();

    {
        let conn = app.connect();
        insert_epoch(&conn, "codex", 1);
        insert_epoch(&conn, "fake", 1);

        insert_thread(
            &conn,
            "codex-root",
            "codex",
            "codex-root",
            None,
            "codex-root",
            "main",
            Some("Codex"),
            None,
            None,
            "projectless",
            ts,
        );
        insert_thread(
            &conn,
            "fake-root",
            "fake",
            "fake-native-root",
            None,
            "fake-root",
            "main",
            Some("Fake"),
            None,
            None,
            "projectless",
            ts,
        );

        // Codex: 1100 tokens
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev",
            "codex-root",
            "codex-root",
            "gpt-4o",
            800,
            150,
            300,
            70,
            1100,
            8_000_000,
            ts,
        );

        // Fake: 4500 tokens
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev",
            "fake-root",
            "fake-root",
            "fake-model",
            3000,
            600,
            1500,
            300,
            4500,
            30_000_000,
            ts,
        );

        drop(conn);
    }

    // Public API v1 /api/v1/usage/summary MUST return ONLY Codex data (§12.1, §25 Q04)
    let public_sum = app
        .json(Method::GET, "/api/v1/usage/summary?range=year")
        .await;
    assert_eq!(
        public_sum["usage"]["total_tokens"], 1100,
        "Public API v1 summary total_tokens must match Codex usage only"
    );
    assert_eq!(
        public_sum["usage"]["session_count"], 1,
        "Public API v1 summary session_count must count only Codex sessions"
    );
    assert_eq!(public_sum["usage"]["input_tokens"], 800);
    assert_eq!(public_sum["usage"]["output_tokens"], 300);
    assert_eq!(public_sum["usage"]["cached_tokens"], 150);
    assert_eq!(public_sum["usage"]["reasoning_tokens"], 70);

    // Matches internal summary when source=codex
    let internal_codex_sum = app
        .json(Method::GET, "/api/usage/summary?range=year&source=codex")
        .await;
    assert_eq!(
        public_sum["usage"]["total_tokens"],
        internal_codex_sum["usage"]["total_tokens"]
    );
    assert_eq!(
        public_sum["usage"]["input_tokens"],
        internal_codex_sum["usage"]["input_tokens"]
    );
    assert_eq!(
        public_sum["usage"]["output_tokens"],
        internal_codex_sum["usage"]["output_tokens"]
    );
    assert_eq!(
        public_sum["usage"]["session_count"],
        internal_codex_sum["usage"]["session_count"]
    );

    // Meanwhile, internal summary without filter sees all 5600 tokens
    let internal_all = app.json(Method::GET, "/api/usage/summary?range=year").await;
    assert_eq!(internal_all["usage"]["total_tokens"], 5600);
    assert_eq!(internal_all["usage"]["session_count"], 2);

    // Public API v1 rejects source filter attempts (400 Bad Request / INVALID_FILTER)
    let bad_filter = app
        .call(Method::GET, "/api/v1/usage/summary?range=year&source=fake")
        .await;
    assert_eq!(bad_filter.status(), StatusCode::BAD_REQUEST);

    app.shutdown();
}

// ---------------------------------------------------------------------------
// Q06: Filter options 事实源与 fallback
// ---------------------------------------------------------------------------
#[tokio::test]
async fn q06_filter_options_source_facts_and_fallback() {
    // Register an extra adapter in registry that has NO active usage in database
    let app = TestApp::with_extra_adapters("q06", |registry| {
        registry
            .register(ProbeAdapter::new("registered-unused", "Registered Unused"))
            .expect("register probe adapter");
        registry
            .register(ProbeAdapter::new("registered-active", "Registered Active"))
            .expect("register active probe adapter");
    });
    let ts = now_ms();

    {
        let conn = app.connect();

        // Active epochs
        insert_epoch(&conn, "codex", 1);
        insert_epoch(&conn, "legacy-source", 1);
        insert_epoch(&conn, "threads-only", 1);
        insert_epoch(&conn, "registered-active", 1);

        // Threads
        insert_thread(
            &conn,
            "codex-thread-1",
            "codex",
            "codex-thread-1",
            None,
            "codex-thread-1",
            "main",
            Some("Codex Session"),
            None,
            None,
            "projectless",
            ts,
        );
        insert_thread(
            &conn,
            "legacy-thread-1",
            "legacy-source",
            "legacy-thread-1",
            None,
            "legacy-thread-1",
            "main",
            Some("Legacy Session"),
            None,
            None,
            "projectless",
            ts,
        );
        insert_thread(
            &conn,
            "threads-only-1",
            "threads-only",
            "threads-only-1",
            None,
            "threads-only-1",
            "main",
            Some("Threads Only Session"),
            None,
            None,
            "projectless",
            ts,
        );
        insert_thread(
            &conn,
            "registered-active-1",
            "registered-active",
            "registered-active-1",
            None,
            "registered-active-1",
            "main",
            Some("Registered Active Session"),
            None,
            None,
            "projectless",
            ts,
        );

        // Usage events for codex, legacy-source, and registered-active only.
        insert_event(
            &conn,
            "codex",
            1,
            "ev-codex-1",
            "codex-thread-1",
            "codex-thread-1",
            "gpt-4o",
            100,
            0,
            50,
            0,
            150,
            1_000_000,
            ts,
        );
        insert_event(
            &conn,
            "legacy-source",
            1,
            "ev-legacy-1",
            "legacy-thread-1",
            "legacy-thread-1",
            "legacy-model",
            200,
            0,
            100,
            0,
            300,
            2_000_000,
            ts,
        );
        insert_event(
            &conn,
            "registered-active",
            1,
            "ev-registered-active-1",
            "registered-active-1",
            "registered-active-1",
            "registered-model",
            300,
            0,
            100,
            0,
            400,
            3_000_000,
            ts,
        );

        drop(conn);
    }

    let options = app.json(Method::GET, "/api/usage/filter-options").await;
    let sources = options["sources"].as_array().expect("sources array");

    // Must return only sources with active canonical usage.
    assert_eq!(
        sources.len(),
        3,
        "Only sources with active usage events should appear, got: {sources:?}"
    );

    // codex display_name == 'Codex'
    let codex_opt = sources
        .iter()
        .find(|s| s["source"] == "codex")
        .expect("codex source option");
    assert_eq!(codex_opt["display_name"], "Codex");

    // legacy-source (unregistered in Registry) display_name fallback == 'legacy-source'
    let legacy_opt = sources
        .iter()
        .find(|s| s["source"] == "legacy-source")
        .expect("legacy-source option");
    assert_eq!(legacy_opt["display_name"], "legacy-source");

    let registered_opt = sources
        .iter()
        .find(|s| s["source"] == "registered-active")
        .expect("registered-active source option");
    assert_eq!(registered_opt["display_name"], "Registered Active");

    // threads-only (has thread, no usage_events) MUST NEVER appear
    assert!(
        sources.iter().all(|s| s["source"] != "threads-only"),
        "threads-only source must never appear without active usage_events"
    );

    // registered-unused (registered in Registry, no active usage) MUST NEVER appear
    assert!(
        sources.iter().all(|s| s["source"] != "registered-unused"),
        "Registered adapter without active usage must never appear in filter-options"
    );

    app.shutdown();
}

// ---------------------------------------------------------------------------
// Q07: 全量 Session DTO 身份字段覆盖
// ---------------------------------------------------------------------------
#[tokio::test]
async fn q07_all_session_dtos_carry_identity_fields() {
    let app = TestApp::new("q07");
    let ts = now_ms();

    {
        let conn = app.connect();
        insert_epoch(&conn, "codex", 1);
        insert_epoch(&conn, "fake", 1);

        // Codex root session + subagent
        insert_thread(
            &conn,
            "codex-root",
            "codex",
            "codex-root",
            None,
            "codex-root",
            "main",
            Some("Codex Root Title"),
            Some("codex-proj"),
            Some("/work/codex-proj"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "codex-sub-1",
            "codex",
            "codex-sub-1",
            Some("codex-root"),
            "codex-root",
            "subagent",
            Some("Codex Sub 1"),
            Some("codex-proj"),
            Some("/work/codex-proj"),
            "project",
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-1",
            "codex-root",
            "codex-root",
            "gpt-4o",
            500,
            100,
            200,
            50,
            700,
            5_000_000,
            ts,
        );
        insert_event(
            &conn,
            "codex",
            1,
            "codex-ev-2",
            "codex-sub-1",
            "codex-root",
            "gpt-4o-mini",
            200,
            0,
            100,
            0,
            300,
            1_000_000,
            ts,
        );

        // Fake root session + subagent
        insert_thread(
            &conn,
            "fake:root",
            "fake",
            "fake-native-root",
            None,
            "fake:root",
            "main",
            Some("Fake Root Title"),
            Some("fake-proj"),
            Some("/work/fake-proj"),
            "project",
            ts,
        );
        insert_thread(
            &conn,
            "fake:sub-1",
            "fake",
            "fake-native-sub-1",
            Some("fake:root"),
            "fake:root",
            "subagent",
            Some("Fake Sub 1"),
            Some("fake-proj"),
            Some("/work/fake-proj"),
            "project",
            ts,
        );
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev-1",
            "fake:root",
            "fake:root",
            "fake-model",
            400,
            0,
            200,
            0,
            600,
            3_000_000,
            ts,
        );
        insert_event(
            &conn,
            "fake",
            1,
            "fake-ev-2",
            "fake:sub-1",
            "fake:root",
            "fake-model",
            300,
            0,
            150,
            0,
            450,
            2_000_000,
            ts,
        );

        drop(conn);
    }

    // 1. Session list items (SessionUsageDto)
    let sessions_resp = app
        .json(Method::GET, "/api/usage/sessions?range=year")
        .await;
    let items = sessions_resp["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for item in items {
        let root_id = item["root_session_id"].as_str().expect("root_session_id");
        let source = item["source"].as_str().expect("source");
        let native_id = item["native_session_id"]
            .as_str()
            .expect("native_session_id");
        assert!(!root_id.is_empty(), "root_session_id must not be empty");
        assert!(!source.is_empty(), "source must not be empty");
        assert!(!native_id.is_empty(), "native_session_id must not be empty");
    }

    // 2. Session sort_index items (SessionSortIndexDto)
    let sort_index = sessions_resp["sort_index"].as_array().unwrap();
    assert!(!sort_index.is_empty());
    for idx in sort_index {
        let root_id = idx["root_session_id"].as_str().expect("root_session_id");
        let source = idx["source"].as_str().expect("source");
        let native_id = idx["native_session_id"]
            .as_str()
            .expect("native_session_id");
        assert!(!root_id.is_empty(), "root_session_id must not be empty");
        assert!(!source.is_empty(), "source must not be empty");
        assert!(!native_id.is_empty(), "native_session_id must not be empty");
    }

    // 3. Session rows items (SessionRowsResponse -> SessionUsageDto)
    let rows_resp = app
        .json(
            Method::GET,
            "/api/usage/session-rows?range=year&root_session_id=codex-root&root_session_id=fake:root",
        )
        .await;
    let row_items = rows_resp["items"].as_array().unwrap();
    assert_eq!(row_items.len(), 2);
    for row in row_items {
        let root_id = row["root_session_id"].as_str().expect("root_session_id");
        let source = row["source"].as_str().expect("source");
        let native_id = row["native_session_id"]
            .as_str()
            .expect("native_session_id");
        assert!(!root_id.is_empty(), "root_session_id must not be empty");
        assert!(!source.is_empty(), "source must not be empty");
        assert!(!native_id.is_empty(), "native_session_id must not be empty");
    }

    // 4. Session detail: root session, main session, subagent sessions
    for root_id in ["codex-root", "fake:root"] {
        let detail_resp = app
            .json(
                Method::GET,
                &format!("/api/usage/sessions/{root_id}/detail?range=year"),
            )
            .await;

        // Top-level detail identity
        let detail_root = detail_resp["root_session_id"]
            .as_str()
            .expect("root_session_id");
        let detail_source = detail_resp["source"].as_str().expect("source");
        let detail_native = detail_resp["native_session_id"]
            .as_str()
            .expect("native_session_id");
        assert_eq!(detail_root, root_id);
        assert!(!detail_source.is_empty());
        assert!(!detail_native.is_empty());

        // Main session identity
        let main = &detail_resp["main"];
        let main_thread = main["thread_id"].as_str().expect("main thread_id");
        let main_root = main["root_session_id"]
            .as_str()
            .expect("main root_session_id");
        let main_source = main["source"].as_str().expect("main source");
        let main_native = main["native_session_id"]
            .as_str()
            .expect("main native_session_id");
        assert_eq!(main_thread, root_id);
        assert_eq!(main_root, root_id);
        assert_eq!(main_source, detail_source);
        assert_eq!(main_native, detail_native);

        // Subagent sessions identity
        let subagents = detail_resp["subagents"]
            .as_array()
            .expect("subagents array");
        assert!(!subagents.is_empty(), "each test session has a subagent");
        for sub in subagents {
            let sub_thread = sub["thread_id"].as_str().expect("subagent thread_id");
            let sub_root = sub["root_session_id"]
                .as_str()
                .expect("subagent root_session_id");
            let sub_source = sub["source"].as_str().expect("subagent source");
            let sub_native = sub["native_session_id"]
                .as_str()
                .expect("subagent native_session_id");
            assert!(
                !sub_thread.is_empty(),
                "subagent thread_id must not be empty"
            );
            assert_eq!(sub_root, root_id);
            assert_eq!(sub_source, detail_source);
            assert!(
                !sub_native.is_empty(),
                "subagent native_session_id must not be empty"
            );
        }
    }

    app.shutdown();
}
