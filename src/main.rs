#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

use std::sync::Arc;

#[cfg(not(feature = "embedded-frontend"))]
use std::path::PathBuf;

use usagi::{
    api::{AppContext, ProcessShutdown, QueryApi},
    codex::quota::CodexQuotaService,
    launcher::{self, BindOutcome},
    platform::browser::{self, BrowserOpener, SystemBrowser},
    scanner::{CodexMetadata, ScanConfig, ScanCoordinator},
    storage::{Ledger, LedgerOptions},
    update::UpdateService,
};

fn report_codex_auth_save_failure() {
    eprintln!("Usagi Codex quota auth.json update failed");
}

#[cfg(target_os = "windows")]
mod windows_shell;

#[cfg(not(target_os = "windows"))]
#[tokio::main]
async fn main() {
    if let Err(error) = run(SystemBrowser).await {
        eprintln!("Usagi startup failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows_shell::run();
}

async fn run(browser_opener: impl BrowserOpener + Clone + 'static) -> Result<(), String> {
    run_with_update_factory(browser_opener, LedgerOptions::default(), || {
        UpdateService::new_github().map_err(|error| error.to_string())
    })
    .await
}

/// Run the production startup lifecycle with a small update-service factory
/// seam.  The factory is called only after the listener, Ledger, Scanner, and
/// HTTP router are ready, so a slow provider can never hold up startup.
async fn run_with_update_factory<B, F>(
    browser_opener: B,
    ledger_options: LedgerOptions,
    update_factory: F,
) -> Result<(), String>
where
    B: BrowserOpener + Clone + 'static,
    F: FnOnce() -> Result<Arc<UpdateService>, String> + Send + 'static,
{
    run_with_update_factory_and_ready(
        browser_opener,
        ledger_options,
        update_factory,
        false,
        |_| {},
    )
    .await
}

async fn run_with_update_factory_and_ready<B, F, R>(
    browser_opener: B,
    ledger_options: LedgerOptions,
    update_factory: F,
    allow_port_fallback: bool,
    on_ready: R,
) -> Result<(), String>
where
    B: BrowserOpener + Clone + 'static,
    F: FnOnce() -> Result<Arc<UpdateService>, String> + Send + 'static,
    R: FnOnce(std::net::SocketAddr) + Send + 'static,
{
    let browser_opener: Arc<dyn BrowserOpener> = Arc::new(browser_opener);
    let bind_outcome = if allow_port_fallback {
        launcher::bind_or_detect_existing_with_port_fallback().await
    } else {
        launcher::bind_or_detect_existing().await
    }
    .map_err(|error| error.to_string())?;

    let listener = match bind_outcome {
        BindOutcome::ExistingInstance(address) => {
            let dashboard_url = browser::dashboard_url(address);
            println!("Usagi is already running at {dashboard_url}");
            if let Err(error) = browser::open_dashboard_at(browser_opener.as_ref(), address) {
                eprintln!(
                    "Usagi is already running, but the browser could not be opened: {error}\n"
                );
                eprintln!("Open {dashboard_url} manually.");
            }
            return Ok(());
        }
        BindOutcome::Listener(listener) => listener,
    };
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not resolve Usagi listener address: {error}"))?;

    let ledger = Arc::new(
        Ledger::open(ledger_options)
            .map_err(|error| format!("could not open Usagi ledger: {error}"))?,
    );
    let scan_config = ScanConfig::new(ledger.codex_home().to_path_buf());
    let scanner = ScanCoordinator::start(
        scan_config,
        Arc::clone(&ledger),
        CodexMetadata::from_home(ledger.codex_home()),
    )
    .map_err(|error| format!("could not start Usagi scanner: {error:?}"))?;
    let codex_quota_service = match CodexQuotaService::new_with_diagnostic(
        ledger.codex_home(),
        report_codex_auth_save_failure,
    ) {
        Ok(service) => service,
        Err(error) => {
            eprintln!("Usagi Codex quota unavailable: {error}");
            CodexQuotaService::unavailable(ledger.codex_home())
        }
    };
    let (process_shutdown, mut shutdown_requested) = ProcessShutdown::channel();
    let update_service = match update_factory() {
        Ok(service) => service,
        Err(error) => {
            eprintln!("Usagi update checks unavailable: {error}");
            UpdateService::unavailable()
        }
    };

    #[cfg(feature = "embedded-frontend")]
    let app = QueryApi::router_with_embedded_frontend_and_shutdown_on_port(
        AppContext {
            ledger,
            scanner,
            codex_quota_service: Arc::clone(&codex_quota_service),
            update_service: Arc::clone(&update_service),
            browser_opener: Arc::clone(&browser_opener),
        },
        process_shutdown,
        address.port(),
    )
    .map_err(|error| format!("could not construct Usagi embedded router: {error}"))?;

    #[cfg(not(feature = "embedded-frontend"))]
    let app = QueryApi::router_with_shutdown_on_port(
        AppContext {
            ledger,
            scanner,
            codex_quota_service: Arc::clone(&codex_quota_service),
            update_service: Arc::clone(&update_service),
            browser_opener: Arc::clone(&browser_opener),
        },
        PathBuf::from("frontend/dist"),
        process_shutdown,
        address.port(),
    )
    .map_err(|error| format!("could not construct Usagi router: {error}"))?;

    println!("Usagi is running at http://{address}");
    let mut server = Box::pin(
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_requested.wait_for(|requested| *requested).await;
            })
            .into_future(),
    );

    tokio::select! {
        result = &mut server => {
            return result.map_err(|error| format!("Usagi server stopped unexpectedly: {error}"));
        }
        result = launcher::wait_until_ready(address) => {
            result.map_err(|error| error.to_string())?;
        }
    }

    on_ready(address);

    let codex_quota_task = codex_quota_service.spawn_background();

    if let Err(error) = browser::open_dashboard_at(browser_opener.as_ref(), address) {
        eprintln!("Usagi server is ready, but the browser could not be opened: {error}");
        eprintln!("Open {} manually.", browser::dashboard_url(address));
    }

    let update_task = update_service.spawn_background();

    let result = server
        .await
        .map_err(|error| format!("Usagi server stopped unexpectedly: {error}"));
    update_task.abort();
    let _ = update_task.await;
    codex_quota_task.abort();
    let _ = codex_quota_task.await;
    result
}

#[cfg(target_os = "windows")]
async fn run_windows_backend<R>(on_ready: R) -> Result<(), String>
where
    R: FnOnce(std::net::SocketAddr) + Send + 'static,
{
    run_with_update_factory_and_ready(
        SystemBrowser,
        LedgerOptions::default(),
        || UpdateService::new_github().map_err(|error| error.to_string()),
        true,
        on_ready,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use futures_util::{FutureExt, future::BoxFuture};
    use reqwest::StatusCode;
    use semver::Version;
    use tokio::sync::Notify;
    use usagi::{
        api::listen_address,
        platform::browser::{BrowserError, BrowserOpener},
        update::{ReleaseInfo, ReleaseProvider, UpdateFailureKind, UpdateService},
    };

    use super::run_with_update_factory;

    #[derive(Clone, Copy)]
    struct TestBrowser;

    impl BrowserOpener for TestBrowser {
        fn open(&self, _url: &str) -> Result<(), BrowserError> {
            Ok(())
        }
    }

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> io::Result<Self> {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "usagi-update-startup-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
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

    struct HangingProvider {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl ReleaseProvider for HangingProvider {
        fn fetch_latest(&self) -> BoxFuture<'_, Result<ReleaseInfo, UpdateFailureKind>> {
            async move {
                self.entered.notify_one();
                self.release.notified().await;
                ReleaseInfo::stable(Version::new(0, 1, 1))
            }
            .boxed()
        }
    }

    #[test]
    fn server_address_is_fixed_loopback() {
        assert_eq!(listen_address().to_string(), "127.0.0.1:3210");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn t_dist_007_slow_update_provider_does_not_block_real_startup_lifecycle() {
        let root = TempRoot::new().unwrap();
        let codex_home = root.path().join("codex");
        fs::create_dir_all(codex_home.join("sessions")).unwrap();
        fs::create_dir_all(codex_home.join("archived_sessions")).unwrap();
        let ledger_options =
            usagi::storage::LedgerOptions::new(root.path().join("mu.sqlite3"), codex_home);

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let provider = Arc::new(HangingProvider {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let update_service = Arc::new(UpdateService::new(provider));
        let startup = tokio::spawn(async move {
            run_with_update_factory(TestBrowser, ledger_options, move || {
                Ok(Arc::clone(&update_service))
            })
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("startup update check did not begin");

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let health = tokio::time::timeout(
            Duration::from_secs(1),
            client.get("http://127.0.0.1:3210/api/health").send(),
        )
        .await
        .expect("health request timed out while update provider was hung")
        .unwrap();
        assert_eq!(health.status(), StatusCode::NO_CONTENT);

        let status = tokio::time::timeout(
            Duration::from_secs(1),
            client.get("http://127.0.0.1:3210/api/status").send(),
        )
        .await
        .expect("status API timed out while update provider was hung")
        .unwrap();
        assert_eq!(status.status(), StatusCode::OK);

        let refresh = tokio::time::timeout(
            Duration::from_secs(5),
            client
                .post("http://127.0.0.1:3210/api/refresh")
                .header("x-usagi-request", "1")
                .send(),
        )
        .await
        .expect("manual refresh timed out while update provider was hung")
        .unwrap();
        assert!(matches!(
            refresh.status(),
            StatusCode::OK | StatusCode::ACCEPTED
        ));

        let stop = client
            .post("http://127.0.0.1:3210/api/service/stop")
            .header("x-usagi-request", "1")
            .send()
            .await
            .unwrap();
        assert_eq!(stop.status(), StatusCode::OK);
        let result = tokio::time::timeout(Duration::from_secs(5), startup)
            .await
            .expect("startup task did not stop")
            .unwrap();
        assert!(result.is_ok(), "startup failed: {result:?}");
    }
}
