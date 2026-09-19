use std::{fmt, path::Path, time::Duration};

use axum::http::{HeaderMap, header};
use reqwest::{Client, StatusCode};
use serde_json::Value;

use super::{
    auth::{self, AuthFile, AuthReadError, AuthWriteError},
    mapper::{self, CodexQuotaWindow, MapperError},
};

pub(crate) const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(crate) const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const USAGE_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct CodexQuotaClient {
    usage_client: Client,
    refresh_client: Client,
    usage_url: String,
    refresh_url: String,
    user_agent: String,
    auth_save_diagnostic: fn(),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QuotaPayload {
    pub account_email: Option<String>,
    pub plan_type: Option<String>,
    pub session: Option<CodexQuotaWindow>,
    pub weekly: CodexQuotaWindow,
    pub reset_credits_available: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuotaFetchError {
    AuthRequired,
    Unavailable,
}

impl fmt::Display for QuotaFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AuthRequired => "Codex authentication is required",
            Self::Unavailable => "Codex quota is unavailable",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsageFailure {
    Unauthorized,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefreshFailure {
    RefreshTokenExpired,
    RefreshTokenReused,
    RefreshTokenInvalidated,
    OtherAuthRequired,
    Unavailable,
}

#[derive(serde::Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

impl CodexQuotaClient {
    pub fn new() -> Result<Self, reqwest::Error> {
        Self::with_urls(USAGE_URL, REFRESH_URL)
    }

    pub fn with_urls(
        usage_url: impl Into<String>,
        refresh_url: impl Into<String>,
    ) -> Result<Self, reqwest::Error> {
        Self::with_urls_and_diagnostic(usage_url, refresh_url, ignore_auth_save_diagnostic)
    }

    pub(crate) fn new_with_diagnostic(auth_save_diagnostic: fn()) -> Result<Self, reqwest::Error> {
        Self::with_urls_and_diagnostic(USAGE_URL, REFRESH_URL, auth_save_diagnostic)
    }

    fn with_urls_and_diagnostic(
        usage_url: impl Into<String>,
        refresh_url: impl Into<String>,
        auth_save_diagnostic: fn(),
    ) -> Result<Self, reqwest::Error> {
        let user_agent = format!("Usagi/{}", env!("CARGO_PKG_VERSION"));
        let usage_client = Client::builder().timeout(USAGE_TIMEOUT).build()?;
        let refresh_client = Client::builder().timeout(REFRESH_TIMEOUT).build()?;
        Ok(Self {
            usage_client,
            refresh_client,
            usage_url: usage_url.into(),
            refresh_url: refresh_url.into(),
            user_agent,
            auth_save_diagnostic,
        })
    }

    pub(crate) async fn fetch(
        &self,
        auth_path: &Path,
        now_ms: i64,
    ) -> Result<QuotaPayload, QuotaFetchError> {
        self.fetch_inner(auth_path, now_ms, || {}).await
    }

    #[cfg(test)]
    async fn fetch_with_reload_hook<F>(
        &self,
        auth_path: &Path,
        now_ms: i64,
        before_reload: F,
    ) -> Result<QuotaPayload, QuotaFetchError>
    where
        F: FnMut(),
    {
        self.fetch_inner(auth_path, now_ms, before_reload).await
    }

    async fn fetch_inner<F>(
        &self,
        auth_path: &Path,
        now_ms: i64,
        mut before_reload: F,
    ) -> Result<QuotaPayload, QuotaFetchError>
    where
        F: FnMut(),
    {
        let mut auth = read_auth(auth_path)?;
        let now_seconds = now_ms.div_euclid(1_000);
        if auth
            .credentials()
            .needs_refresh(auth.last_refresh(), now_seconds)
        {
            // Codex may rotate the refresh token while Usagi is idle.  A
            // fresh read prevents reusing the token that the CLI already
            // consumed.
            before_reload();
            auth = read_auth(auth_path)?;
            if auth
                .credentials()
                .needs_refresh(auth.last_refresh(), now_seconds)
            {
                self.refresh_auth(&mut auth).await?;
            }
        }

        let mut access_token = auth
            .credentials()
            .access_token()
            .map(str::to_owned)
            .ok_or(QuotaFetchError::AuthRequired)?;
        let mut account_id = auth.credentials().account_id().map(str::to_owned);
        let mut retried_after_auth = false;

        loop {
            match self
                .request_usage(&access_token, account_id.as_deref())
                .await
            {
                Ok((body, headers)) => {
                    let mapped = mapper::map_usage(&body, &headers, now_ms)
                        .map_err(|_: MapperError| QuotaFetchError::Unavailable)?;
                    return Ok(QuotaPayload {
                        account_email: auth.credentials().email().map(str::to_owned),
                        plan_type: mapped.plan_type,
                        session: mapped.session,
                        weekly: mapped.weekly,
                        reset_credits_available: mapped.reset_credits_available,
                    });
                }
                Err(UsageFailure::Unauthorized) if !retried_after_auth => {
                    retried_after_auth = true;
                    // The CLI can rotate credentials between the first
                    // request and a 401/403 response.  Always reload before
                    // consuming a refresh token, and use a newer live access
                    // token directly when it is already usable.
                    before_reload();
                    let live_auth = read_auth(auth_path)?;
                    let live_access_token =
                        live_auth.credentials().access_token().map(str::to_owned);
                    let live_is_new_and_usable = live_access_token
                        .as_deref()
                        .is_some_and(|token| token != access_token)
                        && !live_auth
                            .credentials()
                            .needs_refresh(live_auth.last_refresh(), now_seconds);
                    auth = live_auth;
                    if live_is_new_and_usable {
                        access_token = live_access_token.ok_or(QuotaFetchError::AuthRequired)?;
                    } else {
                        self.refresh_auth(&mut auth).await?;
                        access_token = auth
                            .credentials()
                            .access_token()
                            .map(str::to_owned)
                            .ok_or(QuotaFetchError::AuthRequired)?;
                    }
                    account_id = auth.credentials().account_id().map(str::to_owned);
                }
                Err(UsageFailure::Unauthorized) => {
                    return Err(QuotaFetchError::AuthRequired);
                }
                Err(UsageFailure::Unavailable) => {
                    return Err(QuotaFetchError::Unavailable);
                }
            }
        }
    }

    async fn refresh_auth(&self, auth: &mut AuthFile) -> Result<(), QuotaFetchError> {
        let refresh_token = auth
            .credentials()
            .refresh_token()
            .ok_or(QuotaFetchError::AuthRequired)?;
        let response = self
            .refresh_client
            .post(&self.refresh_url)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|_| QuotaFetchError::Unavailable)?;

        if !response.status().is_success() {
            return Err(map_refresh_failure(
                classify_refresh_status(response.status(), response).await,
            ));
        }

        let refreshed = response
            .json::<RefreshResponse>()
            .await
            .map_err(|_| QuotaFetchError::Unavailable)?;
        let access_token = refreshed
            .access_token
            .filter(|value| !value.is_empty())
            .ok_or(QuotaFetchError::Unavailable)?;
        let refreshed_at = auth::refreshed_timestamp();
        auth.apply_refresh(
            access_token,
            refreshed.refresh_token,
            refreshed.id_token,
            refreshed_at,
        )
        .map_err(|_: AuthWriteError| QuotaFetchError::Unavailable)?;
        if auth.save().is_err() {
            // The new token remains in memory for this request.  Keep the
            // diagnostic deliberately token-free; the next fetch will reload
            // the file and retry the normal flow.
            (self.auth_save_diagnostic)();
        }
        Ok(())
    }

    async fn request_usage(
        &self,
        access_token: &str,
        account_id: Option<&str>,
    ) -> Result<(Value, HeaderMap), UsageFailure> {
        let mut request = self
            .usage_client
            .get(&self.usage_url)
            .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, &self.user_agent);
        if let Some(account_id) = account_id {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        let response = request
            .send()
            .await
            .map_err(|_| UsageFailure::Unavailable)?;
        if response.status() == StatusCode::UNAUTHORIZED
            || response.status() == StatusCode::FORBIDDEN
        {
            return Err(UsageFailure::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(UsageFailure::Unavailable);
        }
        let headers = response.headers().clone();
        let body = response
            .json::<Value>()
            .await
            .map_err(|_| UsageFailure::Unavailable)?;
        Ok((body, headers))
    }
}

fn ignore_auth_save_diagnostic() {}

fn map_refresh_failure(failure: RefreshFailure) -> QuotaFetchError {
    match failure {
        RefreshFailure::RefreshTokenExpired
        | RefreshFailure::RefreshTokenReused
        | RefreshFailure::RefreshTokenInvalidated
        | RefreshFailure::OtherAuthRequired => QuotaFetchError::AuthRequired,
        RefreshFailure::Unavailable => QuotaFetchError::Unavailable,
    }
}

fn read_auth(path: &Path) -> Result<AuthFile, QuotaFetchError> {
    AuthFile::read(path).map_err(|error| match error {
        AuthReadError::Missing => QuotaFetchError::AuthRequired,
        AuthReadError::Io | AuthReadError::Invalid => QuotaFetchError::Unavailable,
    })
}

async fn classify_refresh_status(
    status: StatusCode,
    response: reqwest::Response,
) -> RefreshFailure {
    let body = response
        .text()
        .await
        .unwrap_or_default()
        .to_ascii_lowercase();
    classify_refresh_body(status, &body)
}

fn classify_refresh_body(status: StatusCode, body: &str) -> RefreshFailure {
    if body.contains("refresh_token_expired") {
        RefreshFailure::RefreshTokenExpired
    } else if body.contains("refresh_token_reused") {
        RefreshFailure::RefreshTokenReused
    } else if body.contains("refresh_token_invalidated") {
        RefreshFailure::RefreshTokenInvalidated
    } else if status.is_client_error() {
        RefreshFailure::OtherAuthRequired
    } else {
        RefreshFailure::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::{
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Json, Router,
        extract::{State, rejection::FormRejection},
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use serde_json::json;
    use tokio::{net::TcpListener, sync::oneshot};

    use crate::codex::quota::auth::AuthFile;

    #[derive(Clone)]
    struct ServerState {
        usage_calls: Arc<std::sync::atomic::AtomicUsize>,
        refresh_calls: Arc<std::sync::atomic::AtomicUsize>,
        seen_account_ids: Arc<Mutex<Vec<String>>>,
        expected_access_token: Option<String>,
        reject_first_usage: bool,
    }

    static AUTH_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn production_request_coordinates_and_timeouts_are_fixed() {
        let client = CodexQuotaClient::new().unwrap();
        assert_eq!(client.usage_url, USAGE_URL);
        assert_eq!(client.refresh_url, REFRESH_URL);
        let request = client
            .usage_client
            .get(USAGE_URL)
            .header(header::AUTHORIZATION, "Bearer token")
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, &client.user_agent)
            .build()
            .unwrap();
        assert_eq!(request.url().as_str(), USAGE_URL);
        assert_eq!(
            request.headers().get(header::ACCEPT).unwrap(),
            "application/json"
        );
    }

    async fn fake_usage(
        State(state): State<ServerState>,
        headers: axum::http::HeaderMap,
    ) -> impl IntoResponse {
        if let Some(account_id) = headers.get("ChatGPT-Account-Id") {
            state
                .seen_account_ids
                .lock()
                .unwrap()
                .push(account_id.to_str().unwrap().to_owned());
        }
        let call = state
            .usage_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let access_matches = match state.expected_access_token.as_deref() {
            None => true,
            Some(expected) => headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == format!("Bearer {expected}")),
        };
        if (state.reject_first_usage && call == 0) || !access_matches {
            return (StatusCode::UNAUTHORIZED, Json(json!({}))).into_response();
        }
        Json(json!({
            "plan_type": "prolite",
            "rate_limit": {
                "primary_window": {"limit_window_seconds": 18_000, "used_percent": 10},
                "secondary_window": {
                    "limit_window_seconds": 604_800,
                    "used_percent": 55,
                    "reset_after_seconds": 120
                }
            },
            "rate_limit_reset_credits": {"available_count": 2}
        }))
        .into_response()
    }

    async fn fake_refresh(
        State(state): State<ServerState>,
        _form: Result<axum::Form<std::collections::HashMap<String, String>>, FormRejection>,
    ) -> impl IntoResponse {
        let call = state
            .refresh_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 2;
        let id_payload = json!({
            "email": "hoge@example.com",
            "https://api.openai.com/auth.chatgpt_account_id": "acct"
        });
        let id_token = format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&id_payload).unwrap())
        );
        Json(json!({
            "access_token": format!("access-new-{call}"),
            "refresh_token": format!("refresh-new-{call}"),
            "id_token": id_token
        }))
    }

    fn unique_auth_path() -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = AUTH_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "usagi-quota-auth-{}-{stamp}-{sequence}.json",
            std::process::id()
        ))
    }

    fn jwt(payload: Value) -> String {
        format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&payload).unwrap())
        )
    }

    #[tokio::test]
    async fn t_q_003_refreshes_and_retries_once_while_preserving_unknown_auth_fields() {
        let state = ServerState {
            usage_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            seen_account_ids: Arc::new(Mutex::new(Vec::new())),
            expected_access_token: None,
            reject_first_usage: true,
        };
        let app = Router::new()
            .route("/usage", get(fake_usage))
            .route("/refresh", post(fake_refresh))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, shutdown_signal) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_signal.await;
                })
                .await
                .unwrap();
        });

        let auth_path = unique_auth_path();
        let id_payload = json!({
            "email": "hoge@example.com",
            "https://api.openai.com/auth.chatgpt_account_id": "acct"
        });
        let id_token = format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&id_payload).unwrap())
        );
        let access_token = format!(
            "header.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({
                    "exp": 1_700_000_100
                }))
                .unwrap(),
            )
        );
        fs::write(
            &auth_path,
            serde_json::to_vec_pretty(&json!({
                "tokens": {
                    "access_token": access_token,
                    "refresh_token": "refresh-old",
                    "id_token": id_token
                },
                "last_refresh": "2020-01-01T00:00:00Z",
                "unknown_future_field": {"kept": true}
            }))
            .unwrap(),
        )
        .unwrap();

        let client = CodexQuotaClient::with_urls(
            format!("http://{address}/usage"),
            format!("http://{address}/refresh"),
        )
        .unwrap();
        let result = client.fetch(&auth_path, 1_700_000_000_000).await.unwrap();
        assert_eq!(result.account_email.as_deref(), Some("hoge@example.com"));
        assert_eq!(result.plan_type.as_deref(), Some("prolite"));
        assert_eq!(result.weekly.remaining_percent, 45.0);
        assert_eq!(result.reset_credits_available, Some(2));
        assert_eq!(
            state.usage_calls.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        assert_eq!(
            state
                .refresh_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        assert_eq!(
            state.seen_account_ids.lock().unwrap().as_slice(),
            &["acct", "acct"]
        );

        let saved = AuthFile::read(&auth_path).unwrap();
        let saved_text = fs::read_to_string(&auth_path).unwrap();
        assert!(saved_text.contains("unknown_future_field"));
        assert_eq!(saved.credentials().refresh_token(), Some("refresh-new-3"));
        assert!(saved.last_refresh().is_some());

        let _ = shutdown.send(());
        server.await.unwrap();
        let _ = fs::remove_file(auth_path);
    }

    #[tokio::test]
    async fn t_q_003_live_rotation_is_used_before_consuming_refresh_token() {
        let rotated_access = jwt(json!({"exp": 1_700_010_000}));
        let state = ServerState {
            usage_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            seen_account_ids: Arc::new(Mutex::new(Vec::new())),
            expected_access_token: Some(rotated_access.clone()),
            reject_first_usage: false,
        };
        let app = Router::new()
            .route("/usage", get(fake_usage))
            .route("/refresh", post(fake_refresh))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, shutdown_signal) = oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_signal.await;
                })
                .await
                .unwrap();
        });

        let auth_path = unique_auth_path();
        let rotation_path = auth_path.clone();
        let id_token = jwt(json!({
            "email": "hoge@example.com",
            "https://api.openai.com/auth.chatgpt_account_id": "acct"
        }));
        fs::write(
            &auth_path,
            serde_json::to_vec_pretty(&json!({
                "tokens": {
                    "access_token": jwt(json!({"exp": 1_700_000_100})),
                    "refresh_token": "refresh-old",
                    "id_token": id_token
                },
                "last_refresh": "2020-01-01T00:00:00Z",
                "unknown_future_field": {"kept": true}
            }))
            .unwrap(),
        )
        .unwrap();

        let client = CodexQuotaClient::with_urls(
            format!("http://{address}/usage"),
            format!("http://{address}/refresh"),
        )
        .unwrap();
        let result = client
            .fetch_with_reload_hook(&auth_path, 1_700_000_000_000, || {
                fs::write(
                    &rotation_path,
                    serde_json::to_vec_pretty(&json!({
                        "tokens": {
                            "access_token": rotated_access.clone(),
                            "refresh_token": "refresh-rotated",
                            "id_token": jwt(json!({
                                "email": "hoge@example.com",
                                "https://api.openai.com/auth.chatgpt_account_id": "acct"
                            }))
                        },
                        "last_refresh": "2023-11-14T22:13:20Z",
                        "unknown_future_field": {"kept": true}
                    }))
                    .unwrap(),
                )
                .unwrap();
            })
            .await
            .unwrap();
        assert_eq!(result.account_email.as_deref(), Some("hoge@example.com"));
        assert_eq!(
            state
                .refresh_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            state.usage_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let saved = AuthFile::read(&auth_path).unwrap();
        assert_eq!(saved.credentials().refresh_token(), Some("refresh-rotated"));
        assert!(
            fs::read_to_string(&auth_path)
                .unwrap()
                .contains("unknown_future_field")
        );

        let _ = shutdown.send(());
        server.await.unwrap();
        let _ = fs::remove_file(auth_path);
    }

    #[test]
    fn t_q_003_refresh_error_categories_are_auth_required_without_body_leakage() {
        assert_eq!(
            classify_refresh_body(StatusCode::BAD_REQUEST, "refresh_token_expired refresh-old"),
            RefreshFailure::RefreshTokenExpired
        );
        assert_eq!(
            classify_refresh_body(StatusCode::BAD_REQUEST, "refresh_token_reused refresh-old"),
            RefreshFailure::RefreshTokenReused
        );
        assert_eq!(
            classify_refresh_body(
                StatusCode::BAD_REQUEST,
                "refresh_token_invalidated refresh-old"
            ),
            RefreshFailure::RefreshTokenInvalidated
        );
        assert_eq!(
            map_refresh_failure(RefreshFailure::RefreshTokenExpired),
            QuotaFetchError::AuthRequired
        );
        assert_eq!(
            map_refresh_failure(RefreshFailure::RefreshTokenReused),
            QuotaFetchError::AuthRequired
        );
        assert_eq!(
            map_refresh_failure(RefreshFailure::RefreshTokenInvalidated),
            QuotaFetchError::AuthRequired
        );
        assert!(
            !QuotaFetchError::AuthRequired
                .to_string()
                .contains("refresh-old")
        );
    }
}
