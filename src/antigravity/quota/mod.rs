//! Antigravity Gemini quota retrieval and its in-memory refresh lifecycle.

use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use futures_util::future::BoxFuture;
use tokio::{sync::watch, task::JoinHandle, time};

mod auth;
mod client;
mod mapper;

use crate::codex::quota::{CodexQuotaStatus, CodexQuotaWindow};
pub use client::AntigravityQuotaClient;
pub(crate) use client::{QuotaFetchError, QuotaPayload};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AntigravityQuotaResponse {
    pub status: CodexQuotaStatus,
    pub account_email: Option<String>,
    pub plan_type: Option<String>,
    pub session: Option<CodexQuotaWindow>,
    pub weekly: Option<CodexQuotaWindow>,
    pub fetched_at_ms: Option<i64>,
}

impl AntigravityQuotaResponse {
    fn loading() -> Self {
        Self {
            status: CodexQuotaStatus::Loading,
            account_email: None,
            plan_type: None,
            session: None,
            weekly: None,
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
            fetched_at_ms: None,
        }
    }
}

pub(crate) trait QuotaProvider: Send + Sync + 'static {
    fn fetch<'a>(&'a self, now_ms: i64) -> BoxFuture<'a, Result<QuotaPayload, QuotaFetchError>>;
}

struct UnavailableProvider;

impl QuotaProvider for UnavailableProvider {
    fn fetch<'a>(&'a self, _now_ms: i64) -> BoxFuture<'a, Result<QuotaPayload, QuotaFetchError>> {
        Box::pin(async { Err(QuotaFetchError::Unavailable) })
    }
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync + 'static>;

struct ServiceState {
    snapshot: AntigravityQuotaResponse,
    last_good: Option<AntigravityQuotaResponse>,
}

struct QuotaFlight {
    result: watch::Sender<Option<AntigravityQuotaResponse>>,
}

impl QuotaFlight {
    fn new() -> Self {
        let (result, _) = watch::channel(None);
        Self { result }
    }

    fn finish(&self, snapshot: AntigravityQuotaResponse) {
        self.result.send_replace(Some(snapshot));
    }

    async fn wait(&self) -> AntigravityQuotaResponse {
        let mut receiver = self.result.subscribe();
        loop {
            if let Some(snapshot) = receiver.borrow().clone() {
                return snapshot;
            }
            if receiver.changed().await.is_err() {
                return AntigravityQuotaResponse::failure(CodexQuotaStatus::Unavailable);
            }
        }
    }
}

pub struct AntigravityQuotaService {
    provider: Arc<dyn QuotaProvider>,
    state: RwLock<ServiceState>,
    flight: tokio::sync::Mutex<Option<Arc<QuotaFlight>>>,
    timer_reset: watch::Sender<Option<time::Instant>>,
    clock: Clock,
}

impl AntigravityQuotaService {
    pub fn new() -> Result<Arc<Self>, reqwest::Error> {
        Ok(Self::with_client(Arc::new(AntigravityQuotaClient::new()?)))
    }

    pub fn with_client(client: Arc<AntigravityQuotaClient>) -> Arc<Self> {
        Self::with_provider_and_clock(client, Arc::new(now_ms))
    }

    pub fn unavailable() -> Arc<Self> {
        Self::with_provider_and_clock(Arc::new(UnavailableProvider), Arc::new(now_ms))
    }

    pub(crate) fn with_provider_and_clock(
        provider: Arc<dyn QuotaProvider>,
        clock: Clock,
    ) -> Arc<Self> {
        let (timer_reset, _) = watch::channel(None);
        Arc::new(Self {
            provider,
            state: RwLock::new(ServiceState {
                snapshot: AntigravityQuotaResponse::loading(),
                last_good: None,
            }),
            flight: tokio::sync::Mutex::new(None),
            timer_reset,
            clock,
        })
    }

    pub fn snapshot(&self) -> AntigravityQuotaResponse {
        self.state
            .read()
            .expect("quota state lock poisoned")
            .snapshot
            .clone()
    }

    pub async fn refresh_now(&self) -> AntigravityQuotaResponse {
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
    pub async fn refresh_now_and_reset_timer(&self) -> AntigravityQuotaResponse {
        self.timer_reset
            .send_replace(Some(time::Instant::now() + REFRESH_INTERVAL));
        self.refresh_now().await
    }

    async fn perform_fetch(&self) -> AntigravityQuotaResponse {
        let fetched_at_ms = (self.clock)();
        let result = self.provider.fetch(fetched_at_ms).await;
        let mut state = self.state.write().expect("quota state lock poisoned");
        match result {
            Ok(payload) => {
                let snapshot = AntigravityQuotaResponse {
                    status: CodexQuotaStatus::Ready,
                    account_email: payload.account_email,
                    plan_type: payload.plan_type,
                    session: payload.session,
                    weekly: payload.weekly,
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
                    QuotaFetchError::AuthRequired => CodexQuotaStatus::AuthRequired,
                    QuotaFetchError::Unavailable => CodexQuotaStatus::Unavailable,
                };
                let snapshot = AntigravityQuotaResponse::failure(status);
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
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::Notify;

    struct FixtureProvider {
        calls: Arc<AtomicUsize>,
        result: Mutex<Vec<Result<QuotaPayload, QuotaFetchError>>>,
        entered: Option<Arc<Notify>>,
        release: Option<Arc<Notify>>,
    }

    impl QuotaProvider for FixtureProvider {
        fn fetch<'a>(
            &'a self,
            _now_ms: i64,
        ) -> BoxFuture<'a, Result<QuotaPayload, QuotaFetchError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Some(entered) = &self.entered {
                    entered.notify_waiters();
                }
                if let Some(release) = &self.release {
                    release.notified().await;
                }
                self.result.lock().unwrap().remove(0)
            })
        }
    }

    fn payload() -> QuotaPayload {
        QuotaPayload {
            account_email: Some("antigravity.fixture@example.test".to_owned()),
            plan_type: Some("Pro".to_owned()),
            session: Some(CodexQuotaWindow {
                used_percent: 12.0,
                remaining_percent: 88.0,
                limit_window_seconds: 18_000,
                reset_at_ms: Some(1_700_000_100_000),
            }),
            weekly: None,
        }
    }

    #[tokio::test]
    async fn refresh_returns_the_new_snapshot_and_retains_last_good_on_failure() {
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Mutex::new(vec![Ok(payload()), Err(QuotaFetchError::Unavailable)]),
            entered: None,
            release: None,
        });
        let service = AntigravityQuotaService::with_provider_and_clock(provider, Arc::new(|| 1234));

        let ready = service.refresh_now_and_reset_timer().await;
        assert_eq!(ready.status, CodexQuotaStatus::Ready);
        assert_eq!(ready.plan_type.as_deref(), Some("Pro"));
        assert_eq!(ready.fetched_at_ms, Some(1234));
        assert_eq!(ready.session.as_ref().unwrap().used_percent, 12.0);

        let retained = service.refresh_now().await;
        assert_eq!(retained, ready);
    }

    #[tokio::test]
    async fn overlapping_refreshes_share_a_single_fetch() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Mutex::new(vec![Ok(payload())]),
            entered: Some(Arc::clone(&entered)),
            release: Some(Arc::clone(&release)),
        });
        let calls = Arc::clone(&provider.calls);
        let service = AntigravityQuotaService::with_provider_and_clock(provider, Arc::new(|| 1234));

        let first = tokio::spawn({
            let service = Arc::clone(&service);
            async move { service.refresh_now().await }
        });
        entered.notified().await;
        let second = tokio::spawn({
            let service = Arc::clone(&service);
            async move { service.refresh_now().await }
        });
        tokio::task::yield_now().await;
        release.notify_waiters();

        assert_eq!(first.await.unwrap(), second.await.unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn manual_refresh_resets_the_background_deadline_from_request_time() {
        let provider = Arc::new(FixtureProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Mutex::new(vec![Ok(payload()), Ok(payload()), Ok(payload())]),
            entered: None,
            release: None,
        });
        let calls = Arc::clone(&provider.calls);
        let service = AntigravityQuotaService::with_provider_and_clock(provider, Arc::new(|| 1234));
        let task = Arc::clone(&service).spawn_background();
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_secs(240)).await;
        service.refresh_now_and_reset_timer().await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_secs(299)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        task.abort();
        let _ = task.await;
    }
}
