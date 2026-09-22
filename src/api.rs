//! Spec 05 local HTTP query, refresh, and revision-notification seam.
//!
//! Handlers compose the storage/query facade and scanner handle only. They do
//! not contain SQL, rollout parsing, or token aggregation logic.

pub(crate) mod live;
pub(crate) mod query;

use std::{convert::Infallible, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream;
use serde::{Deserialize, Serialize};

use crate::{
    codex::CodexSessionErrorSidecar,
    codex::quota::{CodexQuotaResponse, CodexQuotaService},
    ingestion::{CommitFailureKind, ScanHandle, ScanShutdownError},
    platform::browser::BrowserOpener,
    range::{RangeKey, resolve_day_buckets, resolve_system_custom_range, resolve_system_range},
    source::{SourceId, SourceRegistry},
    storage::{Ledger, RevisionTuple},
    update::{ReleaseInfo, UpdateService, UpdateSnapshot},
    usage::{SummaryQuery, ledger::UsageLedger},
};

mod public_v1;
mod static_assets;

pub use query::ApiError;

pub const LISTEN_PORT: u16 = 3210;
pub const LISTEN_IPV4: [u8; 4] = [127, 0, 0, 1];
pub const APP_MARKER_HEADER: &str = "X-Usagi-App";
pub const APP_MARKER_VALUE: &str = "Usagi";
pub const APP_VERSION_HEADER: &str = "X-Usagi-Version";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CACHE_CONTROL_NO_STORE: &str = "no-store";
const REFRESH_HEADER: &str = "x-usagi-request";

#[derive(Clone)]
pub struct AppContext {
    pub ledger: Arc<Ledger>,
    pub scanner: ScanHandle,
    pub source_registry: SourceRegistry,
    pub codex_quota_service: Arc<CodexQuotaService>,
    pub codex_session_error_sidecar: Arc<CodexSessionErrorSidecar>,
    pub update_service: Arc<UpdateService>,
    pub browser_opener: Arc<dyn BrowserOpener>,
}

#[derive(Clone)]
struct ApiState {
    context: AppContext,
    process_shutdown: Option<ProcessShutdown>,
    listen_port: u16,
}

pub struct QueryApi;

impl QueryApi {
    pub fn router(context: AppContext, static_dir: impl Into<PathBuf>) -> Result<Router, ApiError> {
        Self::router_with_static_dir(context, static_dir)
    }

    pub fn router_with_static_dir(
        context: AppContext,
        static_dir: impl Into<PathBuf>,
    ) -> Result<Router, ApiError> {
        let static_dir = static_dir.into();
        let state = ApiState {
            context,
            process_shutdown: None,
            listen_port: LISTEN_PORT,
        };
        Ok(build_router(
            state,
            static_assets::FrontendSource::Filesystem(static_dir),
        ))
    }

    pub fn router_with_shutdown(
        context: AppContext,
        static_dir: impl Into<PathBuf>,
        process_shutdown: ProcessShutdown,
    ) -> Result<Router, ApiError> {
        Self::router_with_shutdown_on_port(context, static_dir, process_shutdown, LISTEN_PORT)
    }

    pub fn router_with_shutdown_on_port(
        context: AppContext,
        static_dir: impl Into<PathBuf>,
        process_shutdown: ProcessShutdown,
        listen_port: u16,
    ) -> Result<Router, ApiError> {
        let static_dir = static_dir.into();
        let state = ApiState {
            context,
            process_shutdown: Some(process_shutdown),
            listen_port,
        };
        Ok(build_router(
            state,
            static_assets::FrontendSource::Filesystem(static_dir),
        ))
    }

    pub fn router_with_static_dir_and_shutdown(
        context: AppContext,
        static_dir: impl Into<PathBuf>,
        process_shutdown: ProcessShutdown,
    ) -> Result<Router, ApiError> {
        Self::router_with_shutdown(context, static_dir, process_shutdown)
    }

    #[cfg(feature = "embedded-frontend")]
    pub fn router_with_embedded_frontend(context: AppContext) -> Result<Router, ApiError> {
        let state = ApiState {
            context,
            process_shutdown: None,
            listen_port: LISTEN_PORT,
        };
        Ok(build_router(state, static_assets::FrontendSource::Embedded))
    }

    #[cfg(feature = "embedded-frontend")]
    pub fn router_with_embedded_frontend_and_shutdown(
        context: AppContext,
        process_shutdown: ProcessShutdown,
    ) -> Result<Router, ApiError> {
        Self::router_with_embedded_frontend_and_shutdown_on_port(
            context,
            process_shutdown,
            LISTEN_PORT,
        )
    }

    #[cfg(feature = "embedded-frontend")]
    pub fn router_with_embedded_frontend_and_shutdown_on_port(
        context: AppContext,
        process_shutdown: ProcessShutdown,
        listen_port: u16,
    ) -> Result<Router, ApiError> {
        let state = ApiState {
            context,
            process_shutdown: Some(process_shutdown),
            listen_port,
        };
        Ok(build_router(state, static_assets::FrontendSource::Embedded))
    }
}

#[derive(Clone)]
pub struct ProcessShutdown {
    sender: tokio::sync::watch::Sender<bool>,
}

impl ProcessShutdown {
    pub fn channel() -> (Self, tokio::sync::watch::Receiver<bool>) {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        (Self { sender }, receiver)
    }

    fn request(&self) -> Result<(), ApiError> {
        self.sender.send(true).map_err(|_| ApiError::InternalError)
    }

    fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.sender.subscribe()
    }
}

pub const fn listen_address() -> SocketAddr {
    SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            LISTEN_IPV4[0],
            LISTEN_IPV4[1],
            LISTEN_IPV4[2],
            LISTEN_IPV4[3],
        )),
        LISTEN_PORT,
    )
}

fn build_router(state: ApiState, frontend: static_assets::FrontendSource) -> Router {
    let api = public_v1::routes()
        .route("/health", get(health))
        .route("/codex/quota", get(codex_quota))
        .route("/revision", get(revision))
        .route("/status", get(status))
        .route("/usage/summary", get(summary))
        .route("/usage/sessions", get(sessions))
        .route("/usage/session-rows", get(session_rows))
        .route(
            "/usage/sessions/{root_session_id}/detail",
            get(session_detail),
        )
        .route("/usage/models", get(models))
        .route("/usage/model-distribution", get(model_distribution))
        .route("/usage/projects", get(project_distribution))
        .route("/usage/skills", get(skills_usage))
        .route("/usage/filter-options", get(filter_options))
        .route("/update/status", get(update_status))
        .route("/update/check", post(update_check))
        .route("/update/open-release", post(update_open_release))
        .route("/refresh", post(refresh))
        .route("/service", get(service_status))
        .route("/service/stop", post(stop_service))
        .route("/events", get(events))
        .fallback(api_not_found)
        .layer(middleware::from_fn(api_no_store))
        .with_state(state.clone());

    static_assets::with_fallback(Router::new().nest("/api", api), frontend)
        .layer(middleware::from_fn_with_state(state, local_request_guard))
}

async fn health() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        APP_MARKER_HEADER,
        HeaderValue::from_static(APP_MARKER_VALUE),
    );
    headers.insert(APP_VERSION_HEADER, HeaderValue::from_static(APP_VERSION));
    response
}

async fn codex_quota(State(state): State<ApiState>) -> Json<CodexQuotaResponse> {
    Json(state.context.codex_quota_service.snapshot())
}

#[derive(Deserialize)]
struct StatusParams {
    target_scan_id: Option<String>,
}

async fn revision(
    State(state): State<ApiState>,
) -> Result<Json<query::RevisionResponse>, ApiError> {
    let ledger = Arc::clone(&state.context.ledger);
    let value = run_blocking_query(move || query::revision(&ledger)).await??;
    Ok(Json(value))
}

async fn status(
    State(state): State<ApiState>,
    Query(params): Query<StatusParams>,
) -> Result<Json<query::StatusResponse>, ApiError> {
    let ledger = Arc::clone(&state.context.ledger);
    let target = params.target_scan_id;
    let value = run_blocking_query(move || query::status(&ledger, target.as_deref())).await??;
    Ok(Json(value))
}

async fn summary(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::SummaryResponse>, ApiError> {
    let params = query::parse_summary_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let summary_query = SummaryQuery::new(aggregate_range, params.filter);
    let ledger = Arc::clone(&state.context.ledger);
    let codex_sidecar = Arc::clone(&state.context.codex_session_error_sidecar);
    let snapshot = run_blocking_query(move || {
        let sidecars: [&dyn crate::usage::aggregate::SessionErrorSidecar; 1] =
            [codex_sidecar.as_ref()];
        UsageLedger::new(&ledger, &sidecars).summary_snapshot(summary_query)
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::summary_response(&range, snapshot)?))
}

async fn sessions(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::SessionsResponse>, ApiError> {
    let params = query::parse_session_query_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let ledger = Arc::clone(&state.context.ledger);
    let codex_sidecar = Arc::clone(&state.context.codex_session_error_sidecar);
    let snapshot = run_blocking_query(move || {
        let sidecars: [&dyn crate::usage::aggregate::SessionErrorSidecar; 1] =
            [codex_sidecar.as_ref()];
        UsageLedger::new(&ledger, &sidecars).sessions_snapshot(
            aggregate_range,
            params.filter,
            params.seed_sort_field,
            params.seed_sort_order,
        )
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::session_snapshot_response(&range, snapshot)?))
}

async fn session_rows(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::SessionRowsResponse>, ApiError> {
    let params = query::parse_session_query_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let ledger = Arc::clone(&state.context.ledger);
    let codex_sidecar = Arc::clone(&state.context.codex_session_error_sidecar);
    let snapshot = run_blocking_query(move || {
        let sidecars: [&dyn crate::usage::aggregate::SessionErrorSidecar; 1] =
            [codex_sidecar.as_ref()];
        UsageLedger::new(&ledger, &sidecars).session_rows_snapshot(
            aggregate_range,
            params.filter,
            params.expected_data_revision,
            params.root_session_ids,
        )
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::session_rows_response(&range, snapshot)?))
}

async fn session_detail(
    State(state): State<ApiState>,
    Path(root_session_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::SessionDetailResponse>, ApiError> {
    let mut params = query::parse_session_query_params(raw_query.as_deref())?;
    if params.root_session_ids.is_empty() {
        params.root_session_ids.push(root_session_id.clone());
    }
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let ledger = Arc::clone(&state.context.ledger);
    let codex_sidecar = Arc::clone(&state.context.codex_session_error_sidecar);
    let snapshot = run_blocking_query(move || {
        let sidecars: [&dyn crate::usage::aggregate::SessionErrorSidecar; 1] =
            [codex_sidecar.as_ref()];
        UsageLedger::new(&ledger, &sidecars).session_detail_with_project_snapshot(
            aggregate_range,
            params.filter,
            params.expected_data_revision,
            root_session_id,
        )
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::session_detail_with_project_response(
        &range, snapshot,
    )?))
}

async fn models(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::ModelsResponse>, ApiError> {
    let params = query::parse_summary_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let ledger = Arc::clone(&state.context.ledger);
    let codex_sidecar = Arc::clone(&state.context.codex_session_error_sidecar);
    let snapshot = run_blocking_query(move || {
        let sidecars: [&dyn crate::usage::aggregate::SessionErrorSidecar; 1] =
            [codex_sidecar.as_ref()];
        UsageLedger::new(&ledger, &sidecars)
            .models_snapshot_filtered(aggregate_range, &params.filter)
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::models_response(&range, snapshot)?))
}

async fn model_distribution(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::ModelDistributionResponse>, ApiError> {
    let params = query::parse_summary_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let ledger = Arc::clone(&state.context.ledger);
    let snapshot = run_blocking_query(move || {
        crate::usage::analytics::model_distribution_snapshot(
            &ledger,
            aggregate_range,
            &params.filter,
        )
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::model_distribution_response(&range, snapshot)?))
}

async fn project_distribution(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::ProjectDistributionResponse>, ApiError> {
    let params = query::parse_summary_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let ledger = Arc::clone(&state.context.ledger);
    let snapshot = run_blocking_query(move || {
        crate::usage::analytics::project_distribution_snapshot(
            &ledger,
            aggregate_range,
            &params.filter,
        )
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::project_distribution_response(
        &range, snapshot,
    )?))
}

async fn skills_usage(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<query::SkillsUsageResponse>, ApiError> {
    let params = query::parse_summary_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    if range.key != RangeKey::SevenDays {
        return Err(ApiError::InvalidRange);
    }
    let days = resolve_day_buckets(&range)?;
    if days.len() != 7 {
        return Err(ApiError::LocalTimeUnavailable);
    }
    let ledger = Arc::clone(&state.context.ledger);
    let snapshot = run_blocking_query(move || {
        crate::codex::analytics::skills_usage_snapshot(&ledger, &days, &params.filter)
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    Ok(Json(query::skills_usage_response(&range, snapshot)?))
}

async fn filter_options(
    State(state): State<ApiState>,
) -> Result<Json<query::FilterOptionsResponse>, ApiError> {
    let ledger = Arc::clone(&state.context.ledger);
    let codex_sidecar = Arc::clone(&state.context.codex_session_error_sidecar);
    let snapshot = run_blocking_query(move || {
        let sidecars: [&dyn crate::usage::aggregate::SessionErrorSidecar; 1] =
            [codex_sidecar.as_ref()];
        UsageLedger::new(&ledger, &sidecars).filter_options_snapshot()
    })
    .await?
    .map_err(query::map_usage_ledger_error)?;
    let response =
        query::registered_filter_options_response(snapshot, &state.context.source_registry)?;
    Ok(Json(response))
}

/// The S8 update API DTO is intentionally kept separate from the richer
/// in-memory [`UpdateSnapshot`] so that implementation details cannot leak
/// into the frozen HTTP contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdateStatusResponse {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub release_url: Option<String>,
    pub last_checked_at_ms: Option<i64>,
    pub checking: bool,
}

impl From<UpdateSnapshot> for UpdateStatusResponse {
    fn from(snapshot: UpdateSnapshot) -> Self {
        Self {
            current_version: snapshot.current_version.to_string(),
            latest_version: snapshot.latest_version.map(|version| version.to_string()),
            update_available: snapshot.update_available,
            release_url: snapshot.release_url,
            last_checked_at_ms: snapshot.last_successful_checked_at_ms,
            checking: snapshot.checking,
        }
    }
}

async fn update_status(
    State(state): State<ApiState>,
) -> Result<Json<UpdateStatusResponse>, ApiError> {
    Ok(Json(state.context.update_service.status().await.into()))
}

async fn update_check(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<UpdateStatusResponse>, ApiError> {
    require_action_header(&headers)?;
    let snapshot = state
        .context
        .update_service
        .check_now()
        .await
        .map_err(|_| ApiError::UpdateCheckFailed)?;
    Ok(Json(snapshot.into()))
}

async fn update_open_release(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_action_header(&headers)?;
    let snapshot = state.context.update_service.status().await;
    let latest_version = snapshot
        .latest_version
        .as_ref()
        .filter(|_| snapshot.update_available)
        .ok_or(ApiError::UpdateNotAvailable)?;
    let release =
        ReleaseInfo::stable(latest_version.clone()).map_err(|_| ApiError::UpdateNotAvailable)?;
    if snapshot.release_url.as_deref() != Some(release.release_url()) {
        return Err(ApiError::UpdateNotAvailable);
    }
    state
        .context
        .browser_opener
        .open(release.release_url())
        .map_err(|_| ApiError::UpdateBrowserOpenFailed)?;
    Ok(StatusCode::NO_CONTENT)
}

fn resolve_request_range(
    value: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<crate::range::ResolvedRange, ApiError> {
    match RangeKey::parse(value)? {
        RangeKey::Custom => resolve_system_custom_range(
            from.ok_or(ApiError::InvalidRange)?,
            to.ok_or(ApiError::InvalidRange)?,
        ),
        key => {
            if from.is_some() || to.is_some() {
                return Err(ApiError::InvalidRange);
            }
            resolve_system_range(key)
        }
    }
}

struct AbortBlockingOnDrop(Option<tokio::task::AbortHandle>);

impl Drop for AbortBlockingOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // Tokio can cancel a spawn_blocking job only before it starts.
            // Once SQLite work has begun, abort is intentionally a no-op and
            // the short read transaction is allowed to finish.
            handle.abort();
        }
    }
}

async fn run_blocking_query<F, T>(work: F) -> Result<T, ApiError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let task = tokio::task::spawn_blocking(work);
    let mut abort_on_drop = AbortBlockingOnDrop(Some(task.abort_handle()));
    let result = task.await.map_err(|_| ApiError::InternalError)?;
    abort_on_drop.0.take();
    Ok(result)
}

async fn refresh(State(state): State<ApiState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let header_value = headers
        .get(REFRESH_HEADER)
        .and_then(|value| value.to_str().ok());
    let accepted = live::refresh(header_value, state.context.scanner.clone())
        .await
        .map_err(map_live_error)?;
    let status = StatusCode::from_u16(accepted.http_status).map_err(|_| ApiError::InternalError)?;
    Ok((status, Json(accepted)).into_response())
}

#[derive(Serialize)]
struct ServiceResponse {
    state: &'static str,
}

async fn service_status() -> Json<ServiceResponse> {
    Json(ServiceResponse { state: "running" })
}

async fn stop_service(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ServiceResponse>, ApiError> {
    require_action_header(&headers)?;
    let process_shutdown = state
        .process_shutdown
        .clone()
        .ok_or(ApiError::InternalError)?;
    let scanner = state.context.scanner.clone();
    run_blocking_query(move || scanner.shutdown()).await??;
    process_shutdown.request()?;
    Ok(Json(ServiceResponse { state: "stopped" }))
}

fn require_action_header(headers: &HeaderMap) -> Result<(), ApiError> {
    match headers
        .get(REFRESH_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some("1") => Ok(()),
        _ => Err(ApiError::Forbidden),
    }
}

async fn events(State(state): State<ApiState>) -> Response {
    #[derive(Clone)]
    struct StreamState {
        receiver: tokio::sync::watch::Receiver<RevisionTuple>,
        process_shutdown: Option<tokio::sync::watch::Receiver<bool>>,
        initial: bool,
    }

    let stream = stream::unfold(
        StreamState {
            receiver: state.context.ledger.subscribe_revisions(),
            process_shutdown: state
                .process_shutdown
                .as_ref()
                .map(ProcessShutdown::subscribe),
            initial: true,
        },
        |mut state| async move {
            let revision = if state.initial {
                state.initial = false;
                *state.receiver.borrow_and_update()
            } else if let Some(process_shutdown) = state.process_shutdown.as_mut() {
                tokio::select! {
                    revision_changed = state.receiver.changed() => {
                        if revision_changed.is_err() {
                            return None;
                        }
                        *state.receiver.borrow_and_update()
                    }
                    _ = process_shutdown.wait_for(|requested| *requested) => {
                        return None;
                    }
                }
            } else {
                if state.receiver.changed().await.is_err() {
                    return None;
                }
                *state.receiver.borrow_and_update()
            };
            let data = format!(
                "{{\"data_revision\":{},\"status_revision\":{}}}",
                revision.data_revision, revision.status_revision
            );
            let event = Event::default()
                .event("revision")
                .id(format!(
                    "{}-{}",
                    revision.status_revision, revision.data_revision
                ))
                .data(data);
            Some((Ok::<Event, Infallible>(event), state))
        },
    );
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().text("keepalive"))
        .into_response();
    response.headers_mut().insert(
        "x-accel-buffering",
        HeaderValue::from_static(live::SSE_ACCEL_BUFFERING),
    );
    response
}

async fn api_not_found() -> ApiError {
    ApiError::NotFound
}

async fn api_no_store(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL_NO_STORE),
    );
    response
}

fn allowed_local_authority(value: &str, port: u16) -> bool {
    value == format!("127.0.0.1:{port}") || value == format!("localhost:{port}")
}

fn allowed_local_origin(value: &str, port: u16) -> bool {
    value == format!("http://127.0.0.1:{port}") || value == format!("http://localhost:{port}")
}

async fn local_request_guard(
    State(state): State<ApiState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let is_api = request.uri().path() == "/api" || request.uri().path().starts_with("/api/");
    let headers = request.headers();
    if !headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| allowed_local_authority(value, state.listen_port))
    {
        return guarded_error(ApiError::ForbiddenHost, is_api);
    }

    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
    {
        return guarded_error(ApiError::ForbiddenOrigin, is_api);
    }

    if let Some(origin) = headers.get(header::ORIGIN)
        && !origin
            .to_str()
            .ok()
            .is_some_and(|value| allowed_local_origin(value, state.listen_port))
    {
        return guarded_error(ApiError::ForbiddenOrigin, is_api);
    }

    next.run(request).await
}

fn guarded_error(error: ApiError, is_api: bool) -> Response {
    let mut response = error.into_response();
    if is_api {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL_NO_STORE),
        );
    }
    response
}

fn map_live_error(error: live::LiveError) -> ApiError {
    match error {
        live::LiveError::Forbidden => ApiError::Forbidden,
        live::LiveError::ScannerUnavailable => ApiError::ScannerUnavailable,
        live::LiveError::DatabaseBusy => ApiError::DatabaseBusy,
        live::LiveError::ScanStartFailed => ApiError::ScanStartFailed,
        live::LiveError::ScanEnqueueFailed => ApiError::ScanEnqueueFailed,
    }
}

impl From<ScanShutdownError> for ApiError {
    fn from(error: ScanShutdownError) -> Self {
        match error {
            ScanShutdownError::CoordinatorUnavailable => Self::ScannerUnavailable,
            ScanShutdownError::Persistence(CommitFailureKind::Busy) => Self::DatabaseBusy,
            ScanShutdownError::Persistence(CommitFailureKind::Internal) => Self::InternalError,
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
}

impl ApiError {
    const fn http_status(self) -> StatusCode {
        match self {
            Self::InvalidRange
            | Self::InvalidFilter
            | Self::InvalidSessionIds
            | Self::InvalidScanId => StatusCode::BAD_REQUEST,
            Self::Forbidden | Self::ForbiddenHost | Self::ForbiddenOrigin => StatusCode::FORBIDDEN,
            Self::NotFound | Self::ScanNotFound => StatusCode::NOT_FOUND,
            Self::StaleDataRevision | Self::SourceChanged => StatusCode::CONFLICT,
            Self::DatabaseBusy | Self::ScannerUnavailable | Self::UpdateCheckFailed => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::UpdateNotAvailable => StatusCode::CONFLICT,
            Self::LocalTimeUnavailable
            | Self::QueryOverflow
            | Self::QueryFailed
            | Self::ScanStartFailed
            | Self::ScanEnqueueFailed
            | Self::UpdateBrowserOpenFailed
            | Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.http_status(),
            Json(ErrorEnvelope {
                error: ErrorBody { code: self.code() },
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests;
