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
    api::{AppContext, ProcessShutdown, QueryApi},
    codex::quota::CodexQuotaService,
    platform::browser::{BrowserOpener, SystemBrowser},
    scanner::{CodexMetadata, ScanConfig, ScanCoordinator, ScanHandle},
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

    fn path(&self) -> &Path {
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
        Self::with_updates_and_quota_service(
            label,
            UpdateService::unavailable(),
            Arc::new(SystemBrowser),
            Some(codex_quota_service),
        )
    }

    pub fn with_updates(
        label: &str,
        update_service: Arc<UpdateService>,
        browser_opener: Arc<dyn BrowserOpener>,
    ) -> Self {
        Self::with_updates_and_quota_service(label, update_service, browser_opener, None)
    }

    fn with_updates_and_quota_service(
        label: &str,
        update_service: Arc<UpdateService>,
        browser_opener: Arc<dyn BrowserOpener>,
        quota_service: Option<Arc<CodexQuotaService>>,
    ) -> Self {
        let root = TempRoot::new(label);
        let home = root.path().join("codex");
        let static_dir = root.path().join("static");
        fs::create_dir_all(home.join("sessions")).unwrap();
        fs::create_dir_all(home.join("archived_sessions")).unwrap();
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("index.html"), "<html>spec05</html>").unwrap();
        let ledger = Arc::new(
            Ledger::open(LedgerOptions::new(root.path().join("mu.sqlite3"), &home)).unwrap(),
        );
        let scanner = ScanCoordinator::start(
            ScanConfig::new(home.clone()).with_interval(std::time::Duration::from_secs(3_600)),
            Arc::clone(&ledger),
            CodexMetadata::from_home(home.clone()),
        )
        .unwrap();
        wait_scan(&ledger);
        let (process_shutdown, process_shutdown_receiver) = ProcessShutdown::channel();
        let codex_quota_service =
            quota_service.unwrap_or_else(|| CodexQuotaService::unavailable(&home));
        let app = QueryApi::router_with_shutdown(
            AppContext {
                ledger: Arc::clone(&ledger),
                scanner: scanner.clone(),
                codex_quota_service,
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
