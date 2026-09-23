use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use tower::ServiceExt;

use crate::{
    antigravity::quota::AntigravityQuotaService,
    api::{AppContext, ProcessShutdown, QueryApi},
    codex::quota::CodexQuotaService,
    codex::{CodexAdapter, CodexConfig, CodexSessionErrorSidecar},
    ingestion::{IngestionConfig, IngestionCoordinator, ScanHandle},
    platform::browser::{BrowserOpener, SystemBrowser},
    source::SourceRegistry,
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
};

pub(super) struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "usagi-spec05-api-private-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) struct ApiFixture {
    pub _root: TempRoot,
    pub ledger: Arc<Ledger>,
    pub scanner: ScanHandle,
    pub process_shutdown: tokio::sync::watch::Receiver<bool>,
    pub app: Router,
}

impl ApiFixture {
    pub fn new(label: &str) -> Self {
        Self::with_updates(label, UpdateService::unavailable(), Arc::new(SystemBrowser))
    }

    pub fn with_quota_service(label: &str, codex_quota_service: Arc<CodexQuotaService>) -> Self {
        Self::with_updates_and_quota_services(
            label,
            UpdateService::unavailable(),
            Arc::new(SystemBrowser),
            Some(codex_quota_service),
            None,
        )
    }

    pub fn with_antigravity_quota_service(
        label: &str,
        antigravity_quota_service: Arc<AntigravityQuotaService>,
    ) -> Self {
        Self::with_updates_and_quota_services(
            label,
            UpdateService::unavailable(),
            Arc::new(SystemBrowser),
            None,
            Some(antigravity_quota_service),
        )
    }

    pub fn with_updates(
        label: &str,
        update_service: Arc<UpdateService>,
        browser_opener: Arc<dyn BrowserOpener>,
    ) -> Self {
        Self::with_updates_and_quota_service(label, update_service, browser_opener, None)
    }

    pub fn track_d(label: &str) -> Self {
        Self::with_updates_and_quota_services_and_registry(
            label,
            UpdateService::unavailable(),
            Arc::new(SystemBrowser),
            None,
            None,
            true,
        )
    }

    fn with_updates_and_quota_service(
        label: &str,
        update_service: Arc<UpdateService>,
        browser_opener: Arc<dyn BrowserOpener>,
        quota_service: Option<Arc<CodexQuotaService>>,
    ) -> Self {
        Self::with_updates_and_quota_services_and_registry(
            label,
            update_service,
            browser_opener,
            quota_service,
            None,
            false,
        )
    }

    fn with_updates_and_quota_services(
        label: &str,
        update_service: Arc<UpdateService>,
        browser_opener: Arc<dyn BrowserOpener>,
        codex_quota_service: Option<Arc<CodexQuotaService>>,
        antigravity_quota_service: Option<Arc<AntigravityQuotaService>>,
    ) -> Self {
        Self::with_updates_and_quota_services_and_registry(
            label,
            update_service,
            browser_opener,
            codex_quota_service,
            antigravity_quota_service,
            false,
        )
    }

    fn with_updates_and_quota_services_and_registry(
        label: &str,
        update_service: Arc<UpdateService>,
        browser_opener: Arc<dyn BrowserOpener>,
        codex_quota_service: Option<Arc<CodexQuotaService>>,
        antigravity_quota_service: Option<Arc<AntigravityQuotaService>>,
        register_antigravity: bool,
    ) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let static_dir = root.path().join("static");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("archived_sessions")).unwrap();
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("index.html"), "<html>spec05</html>").unwrap();
        let ledger =
            Arc::new(Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"))).unwrap());
        let mut registry = SourceRegistry::new();
        registry
            .register(CodexAdapter::new(CodexConfig::from_home(home.clone())))
            .expect("register Codex source");
        if register_antigravity {
            registry
                .register(crate::antigravity::AntigravityAdapter::new(
                    crate::antigravity::AntigravityConfigResolution::NotInstalled,
                ))
                .expect("register Antigravity source");
        }
        let scanner = IngestionCoordinator::start(
            IngestionConfig::default().with_interval(std::time::Duration::from_secs(3_600)),
            Arc::clone(&ledger),
            registry.clone(),
        )
        .unwrap();
        wait_scan(&ledger);
        let (process_shutdown, process_shutdown_receiver) = ProcessShutdown::channel();
        let codex_quota_service =
            codex_quota_service.unwrap_or_else(|| CodexQuotaService::unavailable(&home));
        let antigravity_quota_service =
            antigravity_quota_service.unwrap_or_else(AntigravityQuotaService::unavailable);
        let app = QueryApi::router_with_shutdown(
            AppContext {
                ledger: Arc::clone(&ledger),
                scanner: scanner.clone(),
                source_registry: registry,
                codex_quota_service,
                antigravity_quota_service,
                codex_session_error_sidecar: Arc::new(CodexSessionErrorSidecar),
                update_service,
                browser_opener,
            },
            static_dir,
            process_shutdown,
        )
        .unwrap();
        Self {
            _root: root,
            ledger,
            scanner,
            process_shutdown: process_shutdown_receiver,
            app,
        }
    }

    pub async fn call(
        &self,
        method: Method,
        uri: &str,
        extra: &[(&str, &str)],
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:3210");
        for (name, value) in extra {
            builder = builder.header(*name, *value);
        }
        self.app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
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
