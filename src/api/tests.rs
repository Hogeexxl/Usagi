use super::*;

use std::path::Path;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

use axum::{
    body::to_bytes,
    http::{Method, Request, StatusCode, header},
};
use futures_util::{FutureExt, future::BoxFuture};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};
use tower::ServiceExt;

use crate::{
    codex::quota::{
        CodexQuotaService, CodexQuotaStatus, QuotaFetchError, QuotaProvider, ReadyPayload,
    },
    platform::browser::{BrowserError, BrowserOpener},
    update::{ReleaseInfo, ReleaseProvider, UpdateFailureKind, UpdateService},
};

#[derive(Clone)]
struct FixtureProvider {
    result: Arc<Mutex<Result<ReleaseInfo, UpdateFailureKind>>>,
    calls: Arc<AtomicUsize>,
}

impl FixtureProvider {
    fn success(version: semver::Version) -> Self {
        Self {
            result: Arc::new(Mutex::new(Ok(ReleaseInfo::stable(version).unwrap()))),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn failure(failure: UpdateFailureKind) -> Self {
        Self {
            result: Arc::new(Mutex::new(Err(failure))),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ReleaseProvider for FixtureProvider {
    fn fetch_latest(&self) -> BoxFuture<'_, Result<ReleaseInfo, UpdateFailureKind>> {
        async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.lock().await.clone()
        }
        .boxed()
    }
}

struct BlockingProvider {
    release: ReleaseInfo,
    calls: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    proceed: Arc<Notify>,
}

#[derive(Clone)]
struct QuotaFixtureProvider {
    calls: Arc<AtomicUsize>,
    payload: ReadyPayload,
}

impl QuotaProvider for QuotaFixtureProvider {
    fn fetch<'a>(
        &'a self,
        _auth_path: &'a Path,
        _now_ms: i64,
    ) -> BoxFuture<'a, Result<ReadyPayload, QuotaFetchError>> {
        async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.payload.clone())
        }
        .boxed()
    }
}

fn quota_fixture_provider() -> QuotaFixtureProvider {
    QuotaFixtureProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        payload: ReadyPayload {
            account_email: Some("hoge@example.com".to_owned()),
            plan_type: Some("prolite".to_owned()),
            session: None,
            weekly: crate::codex::quota::CodexQuotaWindow {
                used_percent: 55.0,
                remaining_percent: 45.0,
                limit_window_seconds: 604_800,
                reset_at_ms: Some(1_700_000_120_000),
            },
            reset_credits_available: Some(2),
        },
    }
}

impl ReleaseProvider for BlockingProvider {
    fn fetch_latest(&self) -> BoxFuture<'_, Result<ReleaseInfo, UpdateFailureKind>> {
        async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_waiters();
            self.proceed.notified().await;
            Ok(self.release.clone())
        }
        .boxed()
    }
}

#[derive(Clone, Default)]
struct RecordingBrowser(Arc<StdMutex<Vec<String>>>);

impl BrowserOpener for RecordingBrowser {
    fn open(&self, url: &str) -> Result<(), BrowserError> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FailingBrowser;

impl BrowserOpener for FailingBrowser {
    fn open(&self, _url: &str) -> Result<(), BrowserError> {
        Err(BrowserError::new("browser fixture failure"))
    }
}

fn fixed_service(provider: Arc<dyn ReleaseProvider>) -> Arc<UpdateService> {
    Arc::new(UpdateService::new_with_clock(provider, Arc::new(|| 1_234)))
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn t_q_004_quota_refresh_isolated_from_scanner_refresh_and_ledger_state() {
    let provider = quota_fixture_provider();
    let calls = Arc::clone(&provider.calls);
    let service = CodexQuotaService::with_provider_and_clock(
        "/tmp/codex-quota-api-test",
        Arc::new(provider),
        Arc::new(|| 1_700_000_000_000),
    );
    let fixture = support::ApiFixture::with_quota_service("quota-isolation", service.clone());

    let loading = fixture.call(Method::GET, "/api/codex/quota", &[]).await;
    assert_eq!(loading.status(), StatusCode::OK);
    assert_eq!(json_body(loading).await["status"], "loading");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let before = fixture.ledger.app_state().unwrap();
    let ready = service.refresh_now().await;
    assert_eq!(ready.status, CodexQuotaStatus::Ready);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let after = fixture.ledger.app_state().unwrap();
    assert_eq!(before.data_revision, after.data_revision);
    assert_eq!(before.scan.status_revision, after.scan.status_revision);
    assert_eq!(
        before.scan.last_scan_started_at_ms,
        after.scan.last_scan_started_at_ms
    );
    assert_eq!(
        before.scan.last_scan_completed_at_ms,
        after.scan.last_scan_completed_at_ms
    );

    let refreshed = fixture
        .call(Method::POST, "/api/refresh", &[("x-usagi-request", "1")])
        .await;
    assert!(matches!(
        refreshed.status(),
        StatusCode::OK | StatusCode::ACCEPTED
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_dist_008_status_dto_is_fixed_and_does_not_check_provider() {
    let provider = Arc::new(FixtureProvider::success(semver::Version::new(0, 1, 1)));
    let service = fixed_service(Arc::clone(&provider) as Arc<dyn ReleaseProvider>);
    let browser = Arc::new(RecordingBrowser::default());
    let fixture = support::ApiFixture::with_updates(
        "dist-008-status",
        service,
        browser as Arc<dyn BrowserOpener>,
    );

    let response = fixture.call(Method::GET, "/api/update/status", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body,
        json!({
            "current_version": env!("CARGO_PKG_VERSION"),
            "latest_version": null,
            "update_available": false,
            "release_url": null,
            "last_checked_at_ms": null,
            "checking": false,
        })
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_dist_008_check_requires_active_header_and_maps_success_or_failure_safely() {
    let mut latest = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    latest.patch = latest.patch.checked_add(1).unwrap();
    let expected_latest = latest.to_string();
    let expected_url = format!("https://github.com/Hogeexxl/Usagi/releases/tag/v{expected_latest}");
    let provider = Arc::new(FixtureProvider::success(latest));
    let service = fixed_service(Arc::clone(&provider) as Arc<dyn ReleaseProvider>);
    let fixture = support::ApiFixture::with_updates(
        "dist-008-check-success",
        service,
        Arc::new(RecordingBrowser::default()),
    );

    let rejected = fixture.call(Method::POST, "/api/update/check", &[]).await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(rejected).await["error"]["code"], "FORBIDDEN");

    let accepted = fixture
        .call(
            Method::POST,
            "/api/update/check",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    let body = json_body(accepted).await;
    assert_eq!(body["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        body["latest_version"].as_str(),
        Some(expected_latest.as_str())
    );
    assert_eq!(body["update_available"], true);
    assert_eq!(body["release_url"].as_str(), Some(expected_url.as_str()));
    assert_eq!(body["checking"], false);
    assert_eq!(body["last_checked_at_ms"], 1_234);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    fixture.scanner.shutdown().unwrap();

    let provider = Arc::new(FixtureProvider::failure(UpdateFailureKind::HttpStatus(599)));
    let service = fixed_service(Arc::clone(&provider) as Arc<dyn ReleaseProvider>);
    let fixture = support::ApiFixture::with_updates(
        "dist-008-check-failure",
        service,
        Arc::new(RecordingBrowser::default()),
    );
    let failed = fixture
        .call(
            Method::POST,
            "/api/update/check",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(failed.into_body(), 64 * 1024).await.unwrap();
    let rendered = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(rendered.contains("UPDATE_CHECK_FAILED"));
    assert!(!rendered.contains("599"));
    assert!(!rendered.contains("github"));
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_dist_008_concurrent_check_requests_share_one_provider_call() {
    let provider = Arc::new(BlockingProvider {
        release: ReleaseInfo::stable(semver::Version::new(0, 1, 1)).unwrap(),
        calls: Arc::new(AtomicUsize::new(0)),
        entered: Arc::new(Notify::new()),
        proceed: Arc::new(Notify::new()),
    });
    let service = fixed_service(Arc::clone(&provider) as Arc<dyn ReleaseProvider>);
    let fixture = support::ApiFixture::with_updates(
        "dist-008-single-flight",
        service,
        Arc::new(RecordingBrowser::default()),
    );
    let first_app = fixture.app.clone();
    let first = tokio::spawn(async move {
        first_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/update/check")
                    .header("host", "127.0.0.1:3210")
                    .header("x-usagi-request", "1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    provider.entered.notified().await;
    let second_app = fixture.app.clone();
    let second = tokio::spawn(async move {
        second_app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/update/check")
                    .header("host", "127.0.0.1:3210")
                    .header("x-usagi-request", "1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    tokio::task::yield_now().await;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    provider.proceed.notify_waiters();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    assert_eq!(second.await.unwrap().status(), StatusCode::OK);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_dist_008_open_release_requires_valid_state_and_preserves_state_on_browser_failure() {
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let mut latest = current.clone();
    latest.patch = latest.patch.checked_add(1).unwrap();
    let expected_url = format!("https://github.com/Hogeexxl/Usagi/releases/tag/v{latest}");
    let no_update_provider = Arc::new(FixtureProvider::success(current));
    let no_update_browser = Arc::new(RecordingBrowser::default());
    let fixture = support::ApiFixture::with_updates(
        "dist-008-open-no-update",
        fixed_service(no_update_provider as Arc<dyn ReleaseProvider>),
        no_update_browser.clone() as Arc<dyn BrowserOpener>,
    );
    let no_update = fixture
        .call(
            Method::POST,
            "/api/update/open-release",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(no_update.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(no_update).await["error"]["code"],
        "UPDATE_NOT_AVAILABLE"
    );
    assert!(no_update_browser.0.lock().unwrap().is_empty());
    fixture.scanner.shutdown().unwrap();

    let provider = Arc::new(FixtureProvider::success(latest.clone()));
    let browser = Arc::new(RecordingBrowser::default());
    let fixture = support::ApiFixture::with_updates(
        "dist-008-open-success",
        fixed_service(provider as Arc<dyn ReleaseProvider>),
        browser.clone() as Arc<dyn BrowserOpener>,
    );
    let checked = fixture
        .call(
            Method::POST,
            "/api/update/check",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(checked.status(), StatusCode::OK);
    let opened = fixture
        .call(
            Method::POST,
            "/api/update/open-release",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(opened.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        browser.0.lock().unwrap().as_slice(),
        [expected_url.as_str()]
    );
    fixture.scanner.shutdown().unwrap();

    let provider = Arc::new(FixtureProvider::success(latest.clone()));
    let service = fixed_service(provider as Arc<dyn ReleaseProvider>);
    let fixture = support::ApiFixture::with_updates(
        "dist-008-open-browser-failure",
        service,
        Arc::new(FailingBrowser),
    );
    let checked = fixture
        .call(
            Method::POST,
            "/api/update/check",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(checked.status(), StatusCode::OK);
    let failed = fixture
        .call(
            Method::POST,
            "/api/update/open-release",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json_body(failed).await["error"]["code"],
        "UPDATE_BROWSER_OPEN_FAILED"
    );
    let status = fixture.call(Method::GET, "/api/update/status", &[]).await;
    let status = json_body(status).await;
    assert_eq!(status["update_available"], true);
    assert_eq!(status["release_url"].as_str(), Some(expected_url.as_str()));
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_dist_008_update_routes_keep_existing_http_security_guard() {
    let fixture = support::ApiFixture::new("dist-008-security");
    for (method, uri) in [
        (Method::GET, "/api/update/status"),
        (Method::POST, "/api/update/check"),
        (Method::POST, "/api/update/open-release"),
    ] {
        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(uri)
                    .header("host", "example.test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        assert_eq!(json_body(response).await["error"]["code"], "FORBIDDEN_HOST");

        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(uri)
                    .header("host", "127.0.0.1:3210")
                    .header("origin", "http://example.test")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        assert_eq!(
            json_body(response).await["error"]["code"],
            "FORBIDDEN_ORIGIN"
        );

        let response = fixture
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", "127.0.0.1:3210")
                    .header("sec-fetch-site", "cross-site")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(
            json_body(response).await["error"]["code"],
            "FORBIDDEN_ORIGIN"
        );
    }
    fixture.scanner.shutdown().unwrap();
}

#[test]
fn listen_contract_is_fixed_loopback_only() {
    let address = listen_address();
    assert_eq!(address.ip().to_string(), "127.0.0.1");
    assert_eq!(address.port(), 3210);
    assert!(!address.ip().is_unspecified());
}

#[tokio::test]
async fn health_exposes_exact_launcher_markers() {
    let fixture = support::ApiFixture::new("health-marker");
    let health = fixture.call(Method::GET, "/api/health", &[]).await;
    assert_eq!(health.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        health.headers()[header::HeaderName::from_static("x-usagi-app")],
        "Usagi"
    );
    assert_eq!(
        health.headers()[header::HeaderName::from_static("x-usagi-version")],
        env!("CARGO_PKG_VERSION")
    );
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn service_control_stops_the_scanner_and_requests_full_process_shutdown() {
    let fixture = support::ApiFixture::new("service-control");

    let running = fixture.call(Method::GET, "/api/service", &[]).await;
    assert_eq!(running.status(), StatusCode::OK);
    let running: Value =
        serde_json::from_slice(&to_bytes(running.into_body(), 1024).await.unwrap()).unwrap();
    assert_eq!(running["state"], "running");

    let events = fixture.call(Method::GET, "/api/events", &[]).await;
    let events_finished =
        tokio::spawn(async move { to_bytes(events.into_body(), 64 * 1024).await.unwrap() });

    let rejected = fixture.call(Method::POST, "/api/service/stop", &[]).await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert!(!*fixture.process_shutdown.borrow());

    let stopped = fixture
        .call(
            Method::POST,
            "/api/service/stop",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(stopped.status(), StatusCode::OK);
    let stopped: Value =
        serde_json::from_slice(&to_bytes(stopped.into_body(), 1024).await.unwrap()).unwrap();
    assert_eq!(stopped["state"], "stopped");
    assert!(*fixture.process_shutdown.borrow());
    let event_bytes = tokio::time::timeout(std::time::Duration::from_secs(1), events_finished)
        .await
        .expect("SSE must close during process shutdown")
        .unwrap();
    assert!(String::from_utf8_lossy(&event_bytes).contains("event: revision"));

    let refresh = fixture
        .call(Method::POST, "/api/refresh", &[("x-usagi-request", "1")])
        .await;
    assert_eq!(refresh.status(), StatusCode::SERVICE_UNAVAILABLE);

    let start_is_not_implemented = fixture.call(Method::POST, "/api/service/start", &[]).await;
    assert_eq!(start_is_not_implemented.status(), StatusCode::NOT_FOUND);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_s06_002_http_compatibility_and_filter_boundary_matrix() {
    let fixture = support::ApiFixture::new("s06-http");

    let summary = fixture
        .call(Method::GET, "/api/usage/summary?range=year", &[])
        .await;
    assert_eq!(summary.status(), StatusCode::OK);
    let summary: Value =
        serde_json::from_slice(&to_bytes(summary.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(summary["range"].is_object());
    assert!(summary["data_revision"].is_i64());
    assert!(summary["usage"].is_object());
    assert!(summary["usage"].get("cache_write_tokens").is_some());
    assert!(summary["usage"]["reasoning_tokens"].is_i64());

    let options = fixture
        .call(Method::GET, "/api/usage/filter-options", &[])
        .await;
    assert_eq!(options.status(), StatusCode::OK);
    let options: Value =
        serde_json::from_slice(&to_bytes(options.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert!(options["data_revision"].is_i64());
    assert!(options["sources"].is_array());
    assert!(options["models"].is_array());
    assert!(options["projects"].is_array());
    for source in options["sources"].as_array().unwrap() {
        assert!(source["source"].is_string());
        assert!(source["display_name"].is_string());
    }
    for model in options["models"].as_array().unwrap() {
        assert!(model["model"].is_string());
        assert!(matches!(
            model["provider"].as_str(),
            Some("openai" | "route-models")
        ));
    }

    let models = fixture
        .call(Method::GET, "/api/usage/models?range=year", &[])
        .await;
    let models: Value =
        serde_json::from_slice(&to_bytes(models.into_body(), 64 * 1024).await.unwrap()).unwrap();
    let models_with_filter = fixture
        .call(
            Method::GET,
            "/api/usage/models?range=year&model=ignored&project_path=%2Fignored",
            &[],
        )
        .await;
    assert_eq!(models_with_filter.status(), StatusCode::OK);
    let models_with_filter: Value = serde_json::from_slice(
        &to_bytes(models_with_filter.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(models_with_filter, models);

    let sessions = fixture
        .call(Method::GET, "/api/usage/sessions?range=year", &[])
        .await;
    let sessions: Value =
        serde_json::from_slice(&to_bytes(sessions.into_body(), 64 * 1024).await.unwrap()).unwrap();
    for item in sessions["items"].as_array().unwrap() {
        assert!(item["source"].is_string());
        assert!(item["native_session_id"].is_string());
    }
    for item in sessions["sort_index"].as_array().unwrap() {
        assert!(item["source"].is_string());
        assert!(item["native_session_id"].is_string());
    }
    let sessions_with_filter = fixture
        .call(
            Method::GET,
            "/api/usage/sessions?range=year&model=ignored&project_path=%2Fignored&sort=last_activity",
            &[],
        )
        .await;
    assert_eq!(sessions_with_filter.status(), StatusCode::OK);
    let sessions_with_filter: Value = serde_json::from_slice(
        &to_bytes(sessions_with_filter.into_body(), 64 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(sessions_with_filter, sessions);

    for query in [
        "/api/usage/summary?range=year&model=",
        "/api/usage/summary?range=year&project_path=",
        "/api/usage/summary?range=year&model=%00",
        "/api/usage/summary?range=year&include_projectless=0",
        "/api/usage/summary?range=year&include_unknown_project=2",
    ] {
        let response = fixture.call(Method::GET, query, &[]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "INVALID_FILTER", "{query}");
    }

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_022_a1_custom_range_resolves_inclusive_dates_and_rejects_invalid_pairs() {
    let fixture = support::ApiFixture::new("t-022-a1-custom-range");

    let response = fixture
        .call(
            Method::GET,
            "/api/usage/summary?range=custom&from=2026-08-01&to=2026-08-03",
            &[],
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["range"]["key"], "custom");
    assert!(
        body["range"]["end_ms"].as_i64().unwrap() > body["range"]["start_ms"].as_i64().unwrap()
    );

    for query in [
        "/api/usage/summary?range=custom&from=2026-08-01",
        "/api/usage/summary?range=custom&from=2026-08-04&to=2026-08-03",
    ] {
        let response = fixture.call(Method::GET, query, &[]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(json_body(response).await["error"]["code"], "INVALID_RANGE");
    }

    fixture.scanner.shutdown().unwrap();
}

mod support;

mod spec05_concurrency;

mod spec05_p2;

mod track_d;

#[test]
fn runtime_port_guard_accepts_only_selected_loopback_port() {
    assert!(allowed_local_authority("127.0.0.1:3217", 3217));
    assert!(allowed_local_authority("localhost:3217", 3217));
    assert!(!allowed_local_authority("127.0.0.1:3210", 3217));
    assert!(!allowed_local_authority("localhost:3210", 3217));

    assert!(allowed_local_origin("http://127.0.0.1:3217", 3217));
    assert!(allowed_local_origin("http://localhost:3217", 3217));
    assert!(!allowed_local_origin("http://127.0.0.1:3210", 3217));
    assert!(!allowed_local_origin("http://localhost:3210", 3217));
}

#[tokio::test]
async fn t_public_api_v1_info_contract_is_stable() {
    let fixture = support::ApiFixture::new("public-v1-info");
    let response = fixture.call(Method::GET, "/api/v1/info", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        json_body(response).await,
        json!({
            "service": "usagi",
            "app_version": env!("CARGO_PKG_VERSION"),
            "api_version": "1",
            "capabilities": [
                "revision",
                "revision-events",
                "status",
                "codex-quota",
                "usage-summary"
            ]
        })
    );
    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_public_api_v1_revision_and_status_expose_only_public_contract() {
    let fixture = support::ApiFixture::new("public-v1-status");

    let revision = fixture.call(Method::GET, "/api/v1/revision", &[]).await;
    assert_eq!(revision.status(), StatusCode::OK);
    let revision = json_body(revision).await;
    assert!(revision["data_revision"].as_i64().is_some());
    assert!(revision["status_revision"].as_i64().is_some());

    let status = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(status.status(), StatusCode::OK);
    let status = json_body(status).await;
    for key in [
        "data_revision",
        "status_revision",
        "scan_state",
        "source_binding_status",
        "last_finished_scan_result",
        "last_scan_started_at_ms",
        "last_scan_completed_at_ms",
        "last_scan_failed_at_ms",
        "last_scan_error_code",
    ] {
        assert!(
            status.get(key).is_some(),
            "missing public status field: {key}"
        );
    }
    for internal_key in [
        "active_scan_id",
        "last_finished_scan_id",
        "followup",
        "target_scan",
    ] {
        assert!(
            status.get(internal_key).is_none(),
            "internal status field leaked: {internal_key}"
        );
    }

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn q05_c_public_status_excludes_zero_child_followup_from_prev11_fallback() {
    let fixture = support::ApiFixture::new("public-v1-followup-fallback");
    let base_t = crate::codex::status::snapshot(&fixture.ledger)
        .unwrap()
        .last_scan_started_at_ms
        .unwrap_or(0)
        + 10_000;

    fixture
        .ledger
        .mark_scan_started(
            crate::domain::ScanStartEvent::new(
                "q05-parent",
                crate::domain::ScanTrigger::Manual,
                base_t,
            )
            .unwrap(),
        )
        .unwrap();
    fixture
        .ledger
        .reserve_scan_followup(
            crate::domain::ReserveScanFollowupEvent::new(
                "q05-followup",
                crate::domain::ScanTrigger::Scheduled,
                base_t + 1,
            )
            .unwrap(),
        )
        .unwrap();
    fixture
        .ledger
        .mark_scan_completed(
            crate::domain::ScanCompletedEvent::new("q05-parent", base_t + 2).unwrap(),
        )
        .unwrap();

    let db_path = fixture._root.path().join("mu.sqlite3");
    let read_followup = || {
        let connection = rusqlite::Connection::open(&db_path).unwrap();
        connection
            .query_row(
                "SELECT state,
                        (SELECT count(*) FROM source_scan_runs WHERE scan_id = ?1)
                 FROM scan_runs WHERE scan_id = ?1",
                ["q05-followup"],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap()
    };
    assert_eq!(read_followup(), ("queued".to_owned(), 0));

    let queued = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(queued.status(), StatusCode::OK);
    let queued = json_body(queued).await;
    assert_eq!(queued["scan_state"], "idle");
    assert_eq!(queued["last_finished_scan_result"], "completed");
    assert_eq!(queued["last_scan_started_at_ms"], base_t);
    assert_eq!(queued["last_scan_completed_at_ms"], base_t + 2);
    let queued_failed_at = queued["last_scan_failed_at_ms"].clone();
    let queued_error_code = queued["last_scan_error_code"].clone();

    fixture
        .ledger
        .mark_followup_start_failed(
            crate::domain::FollowupStartFailedEvent::new(
                "q05-followup",
                base_t + 3,
                "SCAN_START_FAILED",
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(read_followup(), ("start_failed".to_owned(), 0));

    let start_failed = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(start_failed.status(), StatusCode::OK);
    let start_failed = json_body(start_failed).await;
    assert_eq!(start_failed["scan_state"], "idle");
    assert_eq!(start_failed["last_finished_scan_result"], "completed");
    assert_eq!(start_failed["last_scan_started_at_ms"], base_t);
    assert_eq!(start_failed["last_scan_completed_at_ms"], base_t + 2);
    assert_eq!(start_failed["last_scan_failed_at_ms"], queued_failed_at);
    assert_eq!(start_failed["last_scan_error_code"], queued_error_code);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn q05_public_v1_status_state_matrix_keeps_ready_binding() {
    let fixture = support::ApiFixture::new("public-v1-state-matrix");
    let base_t = crate::codex::status::snapshot(&fixture.ledger)
        .unwrap()
        .last_scan_started_at_ms
        .unwrap_or(0)
        + 20_000;

    fixture
        .ledger
        .mark_scan_started_with_sources(
            crate::domain::ScanStartEvent::new(
                "q05-matrix-complete",
                crate::domain::ScanTrigger::Manual,
                base_t,
            )
            .unwrap(),
            &["codex".to_owned()],
        )
        .unwrap();

    // A queued Codex child is publicly observable as running.
    let queued = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(queued.status(), StatusCode::OK);
    let queued = json_body(queued).await;
    assert_eq!(queued["scan_state"], "running");
    assert_eq!(queued["source_binding_status"], "ready");
    assert_eq!(queued["last_scan_started_at_ms"], base_t);

    fixture
        .ledger
        .mark_source_scan_started("q05-matrix-complete", "codex", base_t + 1)
        .unwrap();
    let running = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(running.status(), StatusCode::OK);
    let running = json_body(running).await;
    assert_eq!(running["scan_state"], "running");
    assert_eq!(running["source_binding_status"], "ready");

    fixture
        .ledger
        .mark_source_scan_completed("q05-matrix-complete", "codex", base_t + 2)
        .unwrap();
    // A completed Codex child projects idle/completed even before the global
    // parent terminal commit; this is the public Codex projection contract.
    let completed_child = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(completed_child.status(), StatusCode::OK);
    let completed_child = json_body(completed_child).await;
    assert_eq!(completed_child["scan_state"], "idle");
    assert_eq!(completed_child["last_finished_scan_result"], "completed");
    assert_eq!(completed_child["last_scan_completed_at_ms"], base_t + 2);
    assert!(completed_child["last_scan_error_code"].is_null());
    assert_eq!(completed_child["source_binding_status"], "ready");

    fixture
        .ledger
        .mark_scan_completed(
            crate::domain::ScanCompletedEvent::new("q05-matrix-complete", base_t + 3).unwrap(),
        )
        .unwrap();
    let completed = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(completed.status(), StatusCode::OK);
    let completed = json_body(completed).await;
    assert_eq!(completed["scan_state"], "idle");
    assert_eq!(completed["last_finished_scan_result"], "completed");
    assert_eq!(completed["last_scan_completed_at_ms"], base_t + 2);
    assert_eq!(completed["source_binding_status"], "ready");

    fixture
        .ledger
        .mark_scan_started_with_sources(
            crate::domain::ScanStartEvent::new(
                "q05-matrix-failed",
                crate::domain::ScanTrigger::Manual,
                base_t + 10,
            )
            .unwrap(),
            &["codex".to_owned()],
        )
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_started("q05-matrix-failed", "codex", base_t + 11)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_failed("q05-matrix-failed", "codex", base_t + 12, "CODEX_FAILED")
        .unwrap();
    let failed_child = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(failed_child.status(), StatusCode::OK);
    let failed_child = json_body(failed_child).await;
    assert_eq!(failed_child["scan_state"], "failed");
    assert_eq!(failed_child["last_finished_scan_result"], "failed");
    assert_eq!(failed_child["last_scan_failed_at_ms"], base_t + 12);
    assert_eq!(failed_child["last_scan_error_code"], "CODEX_FAILED");
    assert_eq!(failed_child["source_binding_status"], "ready");

    fixture
        .ledger
        .mark_scan_completed(
            crate::domain::ScanCompletedEvent::new("q05-matrix-failed", base_t + 13).unwrap(),
        )
        .unwrap();
    let failed = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(failed.status(), StatusCode::OK);
    let failed = json_body(failed).await;
    assert_eq!(failed["scan_state"], "failed");
    assert_eq!(failed["last_finished_scan_result"], "failed");
    assert_eq!(failed["last_scan_failed_at_ms"], base_t + 12);
    assert_eq!(failed["last_scan_error_code"], "CODEX_FAILED");
    assert_eq!(failed["source_binding_status"], "ready");

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_internal_status_projects_current_source_children_in_stable_order() {
    let fixture = support::ApiFixture::new("internal-status-sources");
    let sources = vec!["zeta".to_owned(), "codex".to_owned(), "alpha".to_owned()];
    fixture
        .ledger
        .mark_scan_started_with_sources(
            crate::domain::ScanStartEvent::new(
                "internal-status-scan",
                crate::domain::ScanTrigger::Manual,
                10_000,
            )
            .unwrap(),
            &sources,
        )
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_started("internal-status-scan", "alpha", 10_001)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_completed("internal-status-scan", "alpha", 10_002)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_started("internal-status-scan", "codex", 10_003)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_failed("internal-status-scan", "codex", 10_004, "CODEX_FAILED")
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_started("internal-status-scan", "zeta", 10_005)
        .unwrap();

    let response = fixture.call(Method::GET, "/api/status", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["sources"],
        json!([
            {"source": "alpha", "state": "completed", "error_code": null},
            {"source": "codex", "state": "failed", "error_code": "CODEX_FAILED"},
            {"source": "zeta", "state": "running", "error_code": null},
        ])
    );

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_public_api_v1_status_multi_source_isolation() {
    let fixture = support::ApiFixture::new("public-v1-multisource");
    let sources = vec!["codex".to_owned(), "fake".to_owned()];

    let base_t = crate::codex::status::snapshot(&fixture.ledger)
        .unwrap()
        .last_scan_started_at_ms
        .unwrap_or(0)
        + 10_000;

    fixture
        .ledger
        .mark_scan_started_with_sources(
            crate::domain::ScanStartEvent::new(
                "scan-multi",
                crate::domain::ScanTrigger::Manual,
                base_t,
            )
            .unwrap(),
            &sources,
        )
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_started("scan-multi", "codex", base_t + 5)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_completed("scan-multi", "codex", base_t + 10)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_started("scan-multi", "fake", base_t + 15)
        .unwrap();
    fixture
        .ledger
        .mark_source_scan_failed("scan-multi", "fake", base_t + 20, "FAKE_FAILED")
        .unwrap();
    fixture
        .ledger
        .mark_scan_completed(
            crate::domain::ScanCompletedEvent::new("scan-multi", base_t + 25).unwrap(),
        )
        .unwrap();

    let status = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(status.status(), StatusCode::OK);
    let status = json_body(status).await;

    // Spec §12.2.4: Codex completed early; global failed due to fake; Public v1 must see idle + completed!
    assert_eq!(status["scan_state"], "idle");
    assert_eq!(status["last_finished_scan_result"], "completed");
    assert_eq!(status["last_scan_completed_at_ms"], base_t + 10);
    assert!(status["last_scan_error_code"].is_null());

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_public_api_v1_status_codex_skipped_returns_error() {
    let fixture = support::ApiFixture::new("public-v1-skipped");
    let sources = vec!["codex".to_owned()];

    fixture
        .ledger
        .mark_scan_started_with_sources(
            crate::domain::ScanStartEvent::new(
                "scan-skip",
                crate::domain::ScanTrigger::Manual,
                100,
            )
            .unwrap(),
            &sources,
        )
        .unwrap();

    let db_path = fixture._root.path().join("mu.sqlite3");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE source_scan_runs SET state='skipped', finished_at_ms=110 WHERE scan_id='scan-skip' AND source='codex'",
        [],
    )
    .unwrap();

    let status = fixture.call(Method::GET, "/api/v1/status", &[]).await;
    assert_eq!(status.status(), StatusCode::INTERNAL_SERVER_ERROR);

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_public_api_v1_quota_redacts_account_and_internal_fields() {
    let provider = quota_fixture_provider();
    let service = CodexQuotaService::with_provider_and_clock(
        "/tmp/codex-public-api-v1-quota",
        Arc::new(provider),
        Arc::new(|| 1_700_000_000_000),
    );
    let ready = service.refresh_now().await;
    assert_eq!(ready.status, CodexQuotaStatus::Ready);

    let fixture = support::ApiFixture::with_quota_service("public-v1-quota", service);
    let response = fixture.call(Method::GET, "/api/v1/codex/quota", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;

    assert_eq!(body["status"], "ready");
    assert!(body.get("five_hour").is_some());
    assert!(body.get("weekly").is_some());
    assert!(body.get("session").is_none());
    assert_eq!(body["fetched_at_ms"], 1_700_000_000_000_i64);
    assert!(body.get("account_email").is_none());
    assert!(body.get("plan_type").is_none());
    assert!(body.get("reset_credits_available").is_none());

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_public_api_v1_summary_supports_named_and_custom_ranges_only() {
    let fixture = support::ApiFixture::new("public-v1-summary-ranges");

    for range in ["today", "yesterday", "7d", "30d", "year"] {
        let uri = format!("/api/v1/usage/summary?range={range}");
        let response = fixture.call(Method::GET, &uri, &[]).await;
        assert_eq!(response.status(), StatusCode::OK, "range={range}");
        let body = json_body(response).await;
        assert_eq!(body["range"]["key"], range);
        assert!(
            body["range"]["end_ms"].as_i64().unwrap() > body["range"]["start_ms"].as_i64().unwrap()
        );
        assert!(
            body["range"]["timezone"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(body["usage"].is_object());
    }

    let custom = fixture
        .call(
            Method::GET,
            "/api/v1/usage/summary?range=custom&from=2026-09-01&to=2026-09-14",
            &[],
        )
        .await;
    assert_eq!(custom.status(), StatusCode::OK);
    let custom = json_body(custom).await;
    assert_eq!(custom["range"]["key"], "custom");
    assert!(
        custom["range"]["end_ms"].as_i64().unwrap() > custom["range"]["start_ms"].as_i64().unwrap()
    );

    for uri in [
        "/api/v1/usage/summary?range=today&model=gpt-5",
        "/api/v1/usage/summary?range=today&project_path=%2Ftmp",
        "/api/v1/usage/summary?range=today&unknown=1",
    ] {
        let response = fixture.call(Method::GET, uri, &[]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(json_body(response).await["error"]["code"], "INVALID_FILTER");
    }

    fixture.scanner.shutdown().unwrap();
}

#[tokio::test]
async fn t_public_api_v1_events_is_sse_and_keeps_existing_local_security() {
    let fixture = support::ApiFixture::new("public-v1-events");

    let response = fixture.call(Method::GET, "/api/v1/events", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        response
            .headers()
            .get("x-accel-buffering")
            .and_then(|value| value.to_str().ok()),
        Some("no")
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let events_finished =
        tokio::spawn(async move { to_bytes(response.into_body(), 64 * 1024).await.unwrap() });

    let rejected = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/revision")
                .header("host", "example.test")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(json_body(rejected).await["error"]["code"], "FORBIDDEN_HOST");

    let stopped = fixture
        .call(
            Method::POST,
            "/api/service/stop",
            &[("x-usagi-request", "1")],
        )
        .await;
    assert_eq!(stopped.status(), StatusCode::OK);
    let event_bytes = tokio::time::timeout(std::time::Duration::from_secs(1), events_finished)
        .await
        .expect("Public API SSE must close during process shutdown")
        .unwrap();
    let rendered = String::from_utf8_lossy(&event_bytes);
    assert!(rendered.contains("event: revision"));
    assert!(rendered.contains("data_revision"));
    assert!(rendered.contains("status_revision"));

    fixture.scanner.shutdown().unwrap();
}
