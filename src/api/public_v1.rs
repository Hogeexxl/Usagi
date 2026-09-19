//! Stable, read-only Public API v1 projection.
//!
//! Public API handlers share Usagi's query/aggregate layer with the
//! internal Dashboard API, but own their HTTP DTOs and accepted parameters so
//! internal API evolution cannot silently change the public contract.

use std::{convert::Infallible, sync::Arc};

use axum::{
    Json, Router,
    extract::{RawQuery, State},
    http::HeaderValue,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::get,
};
use futures_util::stream;
use serde::Serialize;

use crate::{
    codex::quota::{CodexQuotaResponse, CodexQuotaStatus, CodexQuotaWindow},
    storage::RevisionTuple,
    usage::{SummaryQuery, aggregate::UsageFilter, ledger::UsageLedger},
};

use super::{APP_VERSION, ApiError, ApiState, query, resolve_request_range, run_blocking_query};

const PUBLIC_API_VERSION: &str = "1";
const CAPABILITIES: [&str; 5] = [
    "revision",
    "revision-events",
    "status",
    "codex-quota",
    "usage-summary",
];

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/revision", get(revision))
        .route("/v1/events", get(events))
        .route("/v1/status", get(status))
        .route("/v1/codex/quota", get(codex_quota))
        .route("/v1/usage/summary", get(summary))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct InfoResponse {
    service: &'static str,
    app_version: &'static str,
    api_version: &'static str,
    capabilities: [&'static str; 5],
}

async fn info() -> Json<InfoResponse> {
    Json(InfoResponse {
        service: "usagi",
        app_version: APP_VERSION,
        api_version: PUBLIC_API_VERSION,
        capabilities: CAPABILITIES,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct RevisionResponse {
    data_revision: i64,
    status_revision: i64,
}

impl From<query::RevisionResponse> for RevisionResponse {
    fn from(value: query::RevisionResponse) -> Self {
        Self {
            data_revision: value.data_revision,
            status_revision: value.status_revision,
        }
    }
}

async fn revision(State(state): State<ApiState>) -> Result<Json<RevisionResponse>, ApiError> {
    let ledger = Arc::clone(&state.context.ledger);
    let value = run_blocking_query(move || query::revision(&ledger)).await??;
    Ok(Json(value.into()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct StatusResponse {
    data_revision: i64,
    status_revision: i64,
    scan_state: String,
    source_binding_status: String,
    last_finished_scan_result: Option<String>,
    last_scan_started_at_ms: Option<i64>,
    last_scan_completed_at_ms: Option<i64>,
    last_scan_failed_at_ms: Option<i64>,
    last_scan_error_code: Option<String>,
}

impl From<query::StatusResponse> for StatusResponse {
    fn from(value: query::StatusResponse) -> Self {
        Self {
            data_revision: value.data_revision,
            status_revision: value.status_revision,
            scan_state: value.scan_state,
            source_binding_status: value.source_binding_status,
            last_finished_scan_result: value.last_finished_scan_result,
            last_scan_started_at_ms: value.last_scan_started_at_ms,
            last_scan_completed_at_ms: value.last_scan_completed_at_ms,
            last_scan_failed_at_ms: value.last_scan_failed_at_ms,
            last_scan_error_code: value.last_scan_error_code,
        }
    }
}

async fn status(State(state): State<ApiState>) -> Result<Json<StatusResponse>, ApiError> {
    let ledger = Arc::clone(&state.context.ledger);
    let value = run_blocking_query(move || query::status(&ledger, None)).await??;
    Ok(Json(value.into()))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct QuotaWindowResponse {
    used_percent: f64,
    remaining_percent: f64,
    limit_window_seconds: u64,
    reset_at_ms: Option<i64>,
}

impl From<CodexQuotaWindow> for QuotaWindowResponse {
    fn from(value: CodexQuotaWindow) -> Self {
        Self {
            used_percent: value.used_percent,
            remaining_percent: value.remaining_percent,
            limit_window_seconds: value.limit_window_seconds,
            reset_at_ms: value.reset_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct QuotaResponse {
    status: &'static str,
    five_hour: Option<QuotaWindowResponse>,
    weekly: Option<QuotaWindowResponse>,
    fetched_at_ms: Option<i64>,
}

impl From<CodexQuotaResponse> for QuotaResponse {
    fn from(value: CodexQuotaResponse) -> Self {
        Self {
            status: match value.status {
                CodexQuotaStatus::Loading => "loading",
                CodexQuotaStatus::Ready => "ready",
                CodexQuotaStatus::AuthRequired => "auth_required",
                CodexQuotaStatus::Unavailable => "unavailable",
            },
            five_hour: value.session.map(Into::into),
            weekly: value.weekly.map(Into::into),
            fetched_at_ms: value.fetched_at_ms,
        }
    }
}

async fn codex_quota(State(state): State<ApiState>) -> Json<QuotaResponse> {
    Json(state.context.codex_quota_service.snapshot().into())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct RangeResponse {
    key: String,
    start_ms: i64,
    end_ms: i64,
    timezone: String,
}

impl From<query::RangeDto> for RangeResponse {
    fn from(value: query::RangeDto) -> Self {
        Self {
            key: value.key,
            start_ms: value.start_ms,
            end_ms: value.end_ms,
            timezone: value.timezone,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SessionHealthResponse {
    total_sessions: i64,
    complete_sessions: i64,
    incomplete_sessions: i64,
    error_sessions: i64,
}

impl From<query::SessionHealthDto> for SessionHealthResponse {
    fn from(value: query::SessionHealthDto) -> Self {
        Self {
            total_sessions: value.total_sessions,
            complete_sessions: value.complete_sessions,
            incomplete_sessions: value.incomplete_sessions,
            error_sessions: value.error_sessions,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct UsageSummaryResponse {
    input_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: Option<i64>,
    uncached_input_tokens: Option<i64>,
    output_tokens: i64,
    reasoning_tokens: i64,
    other_output_tokens: i64,
    total_tokens: i64,
    cache_hit_rate: Option<f64>,
    estimated_cost: Option<f64>,
    estimated_cost_status: String,
    session_count: i64,
    cost_incomplete_session_count: i64,
    complete_session_cost_per_million_tokens: Option<f64>,
    session_health: SessionHealthResponse,
}

impl From<query::SummaryUsageDto> for UsageSummaryResponse {
    fn from(value: query::SummaryUsageDto) -> Self {
        Self {
            input_tokens: value.input_tokens,
            cached_tokens: value.cached_tokens,
            cache_write_tokens: value.cache_write_tokens,
            uncached_input_tokens: value.uncached_input_tokens,
            output_tokens: value.output_tokens,
            reasoning_tokens: value.reasoning_tokens,
            other_output_tokens: value.other_output_tokens,
            total_tokens: value.total_tokens,
            cache_hit_rate: value.cache_hit_rate,
            estimated_cost: value.estimated_cost,
            estimated_cost_status: value.estimated_cost_status,
            session_count: value.session_count,
            cost_incomplete_session_count: value.cost_incomplete_session_count,
            complete_session_cost_per_million_tokens: value
                .complete_session_cost_per_million_tokens,
            session_health: value.session_health.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct SummaryResponse {
    range: RangeResponse,
    data_revision: i64,
    usage: UsageSummaryResponse,
}

impl From<query::SummaryResponse> for SummaryResponse {
    fn from(value: query::SummaryResponse) -> Self {
        Self {
            range: value.range.into(),
            data_revision: value.data_revision,
            usage: value.usage.into(),
        }
    }
}

#[derive(Default)]
struct SummaryParams {
    range: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

fn parse_summary_params(raw_query: Option<&str>) -> Result<SummaryParams, ApiError> {
    let mut params = SummaryParams::default();

    for (name, value) in raw_query
        .into_iter()
        .flat_map(|query| form_urlencoded::parse(query.as_bytes()))
    {
        match name.as_ref() {
            "range" => {
                if params.range.replace(value.into_owned()).is_some() {
                    return Err(ApiError::InvalidRange);
                }
            }
            "from" => {
                if params.from.replace(value.into_owned()).is_some() {
                    return Err(ApiError::InvalidRange);
                }
            }
            "to" => {
                if params.to.replace(value.into_owned()).is_some() {
                    return Err(ApiError::InvalidRange);
                }
            }
            _ => return Err(ApiError::InvalidFilter),
        }
    }

    Ok(params)
}

async fn summary(
    State(state): State<ApiState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SummaryResponse>, ApiError> {
    let params = parse_summary_params(raw_query.as_deref())?;
    let range = resolve_request_range(
        params.range.as_deref(),
        params.from.as_deref(),
        params.to.as_deref(),
    )?;
    let aggregate_range = range.aggregate_range()?;
    let summary_query = SummaryQuery::new(aggregate_range, UsageFilter::default());
    let ledger = Arc::clone(&state.context.ledger);
    let snapshot =
        run_blocking_query(move || UsageLedger::new(&ledger).summary_snapshot(summary_query))
            .await?
            .map_err(query::map_usage_ledger_error)?;
    let response = query::summary_response(&range, snapshot)?;
    Ok(Json(response.into()))
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
                .map(super::ProcessShutdown::subscribe),
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
        HeaderValue::from_static(super::live::SSE_ACCEL_BUFFERING),
    );
    response
}
