//! Codex Weekly quota fetching, mapping, and in-memory lifecycle.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use futures_util::{FutureExt, future::BoxFuture};
use tokio::{sync::watch, task::JoinHandle, time};

mod auth;
mod client;
mod mapper;

pub use client::CodexQuotaClient;
#[cfg(test)]
pub(crate) use client::QuotaFetchError;
pub use mapper::CodexQuotaWindow;

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexQuotaStatus {
    Loading,
    Ready,
    AuthRequired,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CodexQuotaResponse {
    pub status: CodexQuotaStatus,
    pub account_email: Option<String>,
    pub plan_type: Option<String>,
    pub session: Option<CodexQuotaWindow>,
    pub weekly: Option<CodexQuotaWindow>,
    pub reset_credits_available: Option<i64>,
    pub fetched_at_ms: Option<i64>,
}

impl CodexQuotaResponse {
    fn loading() -> Self {
        Self {
            status: CodexQuotaStatus::Loading,
            account_email: None,
            plan_type: None,
            session: None,
            weekly: None,
            reset_credits_available: None,
            fetched_at_ms: None,
        }
    }

    fn failure(status: CodexQuotaStatus) -> Self {
        Self {
            status,
            account_email: None,
            plan_type: None,
            session: None,
            weekly: None,
            reset_credits_available: None,
            fetched_at_ms: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReadyPayload {
    pub(crate) account_email: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) session: Option<CodexQuotaWindow>,
    pub(crate) weekly: CodexQuotaWindow,
    pub(crate) reset_credits_available: Option<i64>,
}

pub(crate) trait QuotaProvider: Send + Sync + 'static {
    fn fetch<'a>(
        &'a self,
        auth_path: &'a Path,
        now_ms: i64,
    ) -> BoxFuture<'a, Result<ReadyPayload, client::QuotaFetchError>>;
}

impl QuotaProvider for CodexQuotaClient {
    fn fetch<'a>(
        &'a self,
        auth_path: &'a Path,
        now_ms: i64,
    ) -> BoxFuture<'a, Result<ReadyPayload, client::QuotaFetchError>> {
        async move {
            let payload = client::CodexQuotaClient::fetch(self, auth_path, now_ms).await?;
            Ok(ReadyPayload {
                account_email: payload.account_email,
                plan_type: payload.plan_type,
                session: payload.session,
                weekly: payload.weekly,
                reset_credits_available: payload.reset_credits_available,
            })
        }
        .boxed()
    }
}

struct UnavailableProvider;

impl QuotaProvider for UnavailableProvider {
    fn fetch<'a>(
        &'a self,
        _auth_path: &'a Path,
        _now_ms: i64,
    ) -> BoxFuture<'a, Result<ReadyPayload, client::QuotaFetchError>> {
        futures_util::future::ready(Err(client::QuotaFetchError::Unavailable)).boxed()
    }
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync + 'static>;

struct ServiceState {
    snapshot: CodexQuotaResponse,
    last_good: Option<CodexQuotaResponse>,
}

struct QuotaFlight {
    result: watch::Sender<Option<CodexQuotaResponse>>,
}

impl QuotaFlight {
    fn new() -> Self {
        let (result, _) = watch::channel(None);
        Self { result }
    }

    fn finish(&self, snapshot: CodexQuotaResponse) {
        self.result.send_replace(Some(snapshot));
    }

    async fn wait(&self) -> CodexQuotaResponse {
        let mut receiver = self.result.subscribe();
        loop {
            if let Some(snapshot) = receiver.borrow().clone() {
                return snapshot;
            }
            if receiver.changed().await.is_err() {
                return CodexQuotaResponse::failure(CodexQuotaStatus::Unavailable);
            }
        }
    }
}

pub struct CodexQuotaService {
    auth_path: PathBuf,
    provider: Arc<dyn QuotaProvider>,
    state: RwLock<ServiceState>,
    flight: tokio::sync::Mutex<Option<Arc<QuotaFlight>>>,
    timer_reset: watch::Sender<Option<time::Instant>>,
    clock: Clock,
}

impl CodexQuotaService {
    pub fn new(codex_home: impl Into<PathBuf>) -> Result<Arc<Self>, reqwest::Error> {
        let client = Arc::new(CodexQuotaClient::new()?);
        Ok(Self::with_client(codex_home, client))
    }

    pub fn new_with_diagnostic(
        codex_home: impl Into<PathBuf>,
        auth_save_diagnostic: fn(),
    ) -> Result<Arc<Self>, reqwest::Error> {
        let client = Arc::new(CodexQuotaClient::new_with_diagnostic(auth_save_diagnostic)?);
        Ok(Self::with_client(codex_home, client))
    }

    pub fn with_client(codex_home: impl Into<PathBuf>, client: Arc<CodexQuotaClient>) -> Arc<Self> {
        Self::with_provider_and_clock(codex_home, client, Arc::new(now_ms))
    }

    pub fn unavailable(codex_home: impl Into<PathBuf>) -> Arc<Self> {
        Self::with_provider_and_clock(codex_home, Arc::new(UnavailableProvider), Arc::new(now_ms))
    }

    pub(crate) fn with_provider_and_clock(
        codex_home: impl Into<PathBuf>,
        provider: Arc<dyn QuotaProvider>,
        clock: Clock,
    ) -> Arc<Self> {
        let codex_home = codex_home.into();
        let (timer_reset, _) = watch::channel(None);
        Arc::new(Self {
            auth_path: codex_home.join("auth.json"),
            provider,
            state: RwLock::new(ServiceState {
                snapshot: CodexQuotaResponse::loading(),
                last_good: None,
            }),
            flight: tokio::sync::Mutex::new(None),
            timer_reset,
            clock,
        })
    }

    pub fn snapshot(&self) -> CodexQuotaResponse {
        self.state
            .read()
            .expect("quota state lock poisoned")
            .snapshot
            .clone()
    }

    pub async fn refresh_now(&self) -> CodexQuotaResponse {
        let (flight, owner) = {
            let mut current = self.flight.lock().await;
            if let Some(flight) = current.as_ref() {
                (Arc::clone(flight), false)
            } else {
                let flight = Arc::new(QuotaFlight::new());
                *current = Some(Arc::clone(&flight));
                (flight, true)
            }
        };

        if !owner {
            return flight.wait().await;
        }

        let snapshot = self.perform_fetch().await;
        flight.finish(snapshot.clone());
        let mut current = self.flight.lock().await;
        if current
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &flight))
        {
            *current = None;
        }
        snapshot
    }

    /// Refresh on demand and schedule the next background refresh five minutes from the request.
    pub async fn refresh_now_and_reset_timer(&self) -> CodexQuotaResponse {
        self.timer_reset
            .send_replace(Some(time::Instant::now() + REFRESH_INTERVAL));
        self.refresh_now().await
    }

    async fn perform_fetch(&self) -> CodexQuotaResponse {
        let fetched_at_ms = (self.clock)();
        let result = self.provider.fetch(&self.auth_path, fetched_at_ms).await;
        let mut state = self.state.write().expect("quota state lock poisoned");
        match result {
            Ok(payload) => {
                let snapshot = CodexQuotaResponse {
                    status: CodexQuotaStatus::Ready,
                    account_email: payload.account_email,
                    plan_type: payload.plan_type,
                    session: payload.session,
                    weekly: Some(payload.weekly),
                    reset_credits_available: payload.reset_credits_available,
                    fetched_at_ms: Some(fetched_at_ms),
                };
                state.last_good = Some(snapshot.clone());
                state.snapshot = snapshot.clone();
                snapshot
            }
            Err(error) => {
                if let Some(last_good) = state.last_good.clone() {
                    state.snapshot = last_good.clone();
                    return last_good;
                }
                let status = match error {
                    client::QuotaFetchError::AuthRequired => CodexQuotaStatus::AuthRequired,
                    client::QuotaFetchError::Unavailable => CodexQuotaStatus::Unavailable,
                };
                let snapshot = CodexQuotaResponse::failure(status);
                state.snapshot = snapshot.clone();
                snapshot
            }
        }
    }

    pub async fn run_background(self: Arc<Self>) {
        let mut timer_reset = self.timer_reset.subscribe();
        let _ = self.refresh_now().await;
        let mut deadline = (*timer_reset.borrow_and_update())
            .unwrap_or_else(|| time::Instant::now() + REFRESH_INTERVAL);
        loop {
            tokio::select! {
                _ = time::sleep_until(deadline) => {
                    self.timer_reset.send_if_modified(|current| {
                        if *current == Some(deadline) {
                            *current = None;
                            true
                        } else {
                            false
                        }
                    });
                    let _ = self.refresh_now().await;
                    deadline = time::Instant::now() + REFRESH_INTERVAL;
                }
                changed = timer_reset.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if let Some(next) = *timer_reset.borrow_and_update() {
                        deadline = next;
                    }
                }
            }
        }
    }

    pub fn spawn_background(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move { self.run_background().await })
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    struct FixtureProvider {
        calls: Arc<AtomicUsize>,
        results: Mutex<Vec<Result<ReadyPayload, client::QuotaFetchError>>>,
        entered: Option<Arc<Notify>>,
        release: Option<Arc<Notify>>,
    }

    impl QuotaProvider for FixtureProvider {
        fn fetch<'a>(
            &'a self,
            _auth_path: &'a Path,
            _now_ms: i64,
        ) -> BoxFuture<'a, Result<ReadyPayload, client::QuotaFetchError>> {
            async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let Some(entered) = &self.entered {
                    entered.notify_waiters();
                }
                if let Some(release) = &self.release {
                    release.notified().await;
                }
                self.results
                    .lock()
                    .expect("fixture lock poisoned")
                    .pop()
                    .unwrap_or(Err(client::QuotaFetchError::Unavailable))
            }
            .boxed()
        }
    }

    fn payload() -> ReadyPayload {
        ReadyPayload {
            account_email: Some("hoge@example.com".to_owned()),
            plan_type: Some("prolite".to_owned()),
            session: None,
            weekly: CodexQuotaWindow {
                used_percent: 55.0,
                remaining_percent: 45.0,
                limit_window_seconds: 604_800,
                reset_at_ms: Some(1_700_000_000_000),
            },
            reset_credits_available: Some(2),
        }
    }

    fn service(provider: Arc<dyn QuotaProvider>) -> Arc<CodexQuotaService> {
        CodexQuotaService::with_provider_and_clock(
            "/tmp/codex",
            provider,
            Arc::new(|| 1_700_000_000_000),
        )
    }

    #[tokio::test]
    async fn first_fetch_is_loading_then_ready_and_failure_keeps_last_good() {
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            results: Mutex::new(vec![
                Err(client::QuotaFetchError::Unavailable),
                Ok(payload()),
            ]),
            entered: None,
            release: None,
        });
        let service = service(provider.clone());
        assert_eq!(service.snapshot().status, CodexQuotaStatus::Loading);
        let ready = service.refresh_now().await;
        assert_eq!(ready.status, CodexQuotaStatus::Ready);
        let retained = service.refresh_now().await;
        assert_eq!(retained, ready);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_provider_call() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            results: Mutex::new(vec![Ok(payload())]),
            entered: Some(Arc::clone(&entered)),
            release: Some(Arc::clone(&release)),
        });
        let service = service(provider.clone());
        let first = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.refresh_now().await })
        };
        entered.notified().await;
        let second = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.refresh_now().await })
        };
        release.notify_waiters();
        assert_eq!(first.await.unwrap().status, CodexQuotaStatus::Ready);
        assert_eq!(second.await.unwrap().status, CodexQuotaStatus::Ready);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn quota_flight_completion_before_wait_is_not_lost() {
        let flight = QuotaFlight::new();
        let expected = CodexQuotaResponse {
            status: CodexQuotaStatus::Ready,
            account_email: Some("hoge@example.com".to_owned()),
            plan_type: Some("prolite".to_owned()),
            session: None,
            weekly: Some(payload().weekly),
            reset_credits_available: Some(2),
            fetched_at_ms: Some(1_700_000_000_000),
        };
        flight.finish(expected.clone());
        assert_eq!(flight.wait().await, expected);
    }

    #[tokio::test(start_paused = true)]
    async fn t_q_004_background_refresh_uses_immediate_then_delayed_fetches() {
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            results: Mutex::new(vec![
                Err(client::QuotaFetchError::Unavailable),
                Ok(payload()),
            ]),
            entered: None,
            release: None,
        });
        let service = service(provider.clone());
        let task = Arc::clone(&service).spawn_background();
        tokio::task::yield_now().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(service.snapshot().status, CodexQuotaStatus::Ready);

        tokio::time::advance(REFRESH_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(service.snapshot().status, CodexQuotaStatus::Ready);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test(start_paused = true)]
    async fn manual_refresh_resets_the_background_deadline_from_request_time() {
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            results: Mutex::new(vec![Ok(payload()), Ok(payload()), Ok(payload())]),
            entered: None,
            release: None,
        });
        let service = service(provider.clone());
        let task = Arc::clone(&service).spawn_background();
        tokio::task::yield_now().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(240)).await;
        service.refresh_now_and_reset_timer().await;
        tokio::task::yield_now().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_secs(299)).await;
        tokio::task::yield_now().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        task.abort();
        let _ = task.await;
    }
}
