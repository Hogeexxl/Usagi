//! Read-only API query seam and HTTP-facing DTO projections.
//!
//! Usage arithmetic remains owned by Spec 04. This module maps frozen
//! storage snapshots into the fixed Spec 05 HTTP DTOs.

use std::fmt;

use form_urlencoded::parse as parse_form_urlencoded;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    domain::{AppState, FollowupState, ScanRun, ScanStatusSnapshot, SourceBindingStatus},
    range::ResolvedRange,
    storage::{Ledger, StorageErrorKind},
    usage::{
        aggregate::{
            AggregateError, CostCompleteness, FilterOptions, ModelUsageRow, ProjectFilterOption,
            SessionDataStatus, SessionDetail, SessionSortField, SessionSortIndexItem,
            SessionSortOrder, SessionUsageRow, SubagentModelUsage, TokenTotals, UsageFilter,
            UsageSummary,
        },
        analytics::{
            AnalyticsSnapshot, DistributionCostStatus, ModelDistributionRow,
            ProjectDistributionIdentity, ProjectDistributionRow, SkillsUsage,
        },
        ledger::{
            SessionDetailSnapshot, SessionRowsSnapshot, SessionSnapshot, UsageLedgerError,
            UsageSnapshot,
        },
    },
};

const JSON_SAFE_INTEGER_MAX: i64 = (1_i64 << 53) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiError {
    InvalidRange,
    InvalidFilter,
    InvalidSessionIds,
    InvalidScanId,
    ScanNotFound,
    StaleDataRevision,
    Forbidden,
    ForbiddenHost,
    ForbiddenOrigin,
    NotFound,
    SourceChanged,
    ScannerUnavailable,
    LocalTimeUnavailable,
    QueryOverflow,
    DatabaseBusy,
    QueryFailed,
    ScanStartFailed,
    ScanEnqueueFailed,
    UpdateCheckFailed,
    UpdateNotAvailable,
    UpdateBrowserOpenFailed,
    InternalError,
}

impl ApiError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRange => "INVALID_RANGE",
            Self::InvalidFilter => "INVALID_FILTER",
            Self::InvalidSessionIds => "INVALID_SESSION_IDS",
            Self::InvalidScanId => "INVALID_SCAN_ID",
            Self::ScanNotFound => "SCAN_NOT_FOUND",
            Self::StaleDataRevision => "STALE_DATA_REVISION",
            Self::Forbidden => "FORBIDDEN",
            Self::ForbiddenHost => "FORBIDDEN_HOST",
            Self::ForbiddenOrigin => "FORBIDDEN_ORIGIN",
            Self::NotFound => "NOT_FOUND",
            Self::SourceChanged => "SOURCE_CHANGED",
            Self::ScannerUnavailable => "SCANNER_UNAVAILABLE",
            Self::LocalTimeUnavailable => "LOCAL_TIME_UNAVAILABLE",
            Self::QueryOverflow => "QUERY_OVERFLOW",
            Self::DatabaseBusy => "DATABASE_BUSY",
            Self::QueryFailed => "QUERY_FAILED",
            Self::ScanStartFailed => "SCAN_START_FAILED",
            Self::ScanEnqueueFailed => "SCAN_ENQUEUE_FAILED",
            Self::UpdateCheckFailed => "UPDATE_CHECK_FAILED",
            Self::UpdateNotAvailable => "UPDATE_NOT_AVAILABLE",
            Self::UpdateBrowserOpenFailed => "UPDATE_BROWSER_OPEN_FAILED",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for ApiError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RangeDto {
    pub key: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub timezone: String,
}

impl From<&ResolvedRange> for RangeDto {
    fn from(range: &ResolvedRange) -> Self {
        Self {
            key: range.key.as_str().to_owned(),
            start_ms: range.start_ms,
            end_ms: range.end_ms,
            timezone: range.timezone.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TokenUsageDto {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub uncached_input_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub other_output_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_rate: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub estimated_cost_status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionHealthDto {
    pub total_sessions: i64,
    pub complete_sessions: i64,
    pub incomplete_sessions: i64,
    pub error_sessions: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SummaryUsageDto {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub uncached_input_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub other_output_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_rate: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub estimated_cost_status: String,
    pub session_count: i64,
    pub cost_incomplete_session_count: i64,
    pub complete_session_cost_per_million_tokens: Option<f64>,
    pub session_health: SessionHealthDto,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SummaryResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub usage: SummaryUsageDto,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryParams {
    pub range: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub filter: UsageFilter,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionUsageDto {
    pub root_session_id: String,
    pub title: Option<String>,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub last_activity_at_ms: i64,
    pub models_used: Vec<String>,
    pub subagent_count: i64,
    pub inclusive_usage: Option<TokenUsageDto>,
    pub self_usage: Option<TokenUsageDto>,
    pub subagent_usage: Option<TokenUsageDto>,
    pub data_status: String,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionsResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub total_items: usize,
    pub sort_index: Vec<SessionSortIndexDto>,
    pub items: Vec<SessionUsageDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionSortIndexDto {
    pub root_session_id: String,
    pub last_activity_at_ms: i64,
    pub project_sort_key: Option<String>,
    pub model_sort_key: Option<String>,
    pub total_tokens: Option<i64>,
    pub combined_total_tokens: Option<i64>,
    pub combined_estimated_cost: Option<f64>,
    pub cache_hit_rate: Option<f64>,
    pub data_status: String,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionRowsResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub items: Vec<SessionUsageDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionDetailResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub root_session_id: String,
    pub last_activity_at_ms: i64,
    pub main: MainSessionDetailDto,
    pub subagents: Vec<SubagentDetailDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MainSessionDetailDto {
    pub title: Option<String>,
    pub thread_id: String,
    pub root_session_id: String,
    pub models_used: Vec<String>,
    pub model_usage: Vec<MainModelUsageDto>,
    pub self_usage: TokenUsageDto,
    pub subagent_count: i64,
    pub inclusive_usage: TokenUsageDto,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MainModelUsageDto {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub usage: TokenUsageDto,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubagentDetailDto {
    pub thread_id: String,
    pub parent_thread_id: Option<String>,
    pub root_session_id: String,
    pub title: Option<String>,
    pub last_activity_at_ms: i64,
    pub model_usage: Vec<SubagentModelUsageDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubagentModelUsageDto {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub last_activity_at_ms: i64,
    pub usage: TokenUsageDto,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelUsageDto {
    pub model: String,
    pub usage: TokenUsageDto,
    pub session_count: i64,
    pub first_activity_at_ms: i64,
    pub last_activity_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelsResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub items: Vec<ModelUsageDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DistributionUsageDto {
    pub total_tokens: i64,
    pub estimated_cost: Option<f64>,
    pub estimated_cost_status: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelDistributionItemDto {
    pub model: String,
    pub usage: DistributionUsageDto,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelDistributionResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub items: Vec<ModelDistributionItemDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectDistributionItemDto {
    pub kind: String,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub usage: DistributionUsageDto,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectDistributionResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub items: Vec<ProjectDistributionItemDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SkillCountDto {
    pub skill_name: String,
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SkillDayDto {
    pub date: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub total: i64,
    pub skills: Vec<SkillCountDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SkillsUsageResponse {
    pub range: RangeDto,
    pub data_revision: i64,
    pub data_status: String,
    pub days: Vec<SkillDayDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum ProjectFilterOptionDto {
    #[serde(rename = "project")]
    Project {
        project_name: String,
        project_path: String,
    },
    #[serde(rename = "projectless")]
    Projectless,
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FilterOptionsResponse {
    pub data_revision: i64,
    pub models: Vec<ModelFilterOptionDto>,
    pub projects: Vec<ProjectFilterOptionDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelFilterOptionDto {
    pub model: String,
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RevisionResponse {
    pub data_revision: i64,
    pub status_revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FollowupDto {
    pub scan_id: String,
    pub state: String,
    pub enqueued_status_revision: i64,
    pub requested_at_ms: i64,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TargetScanDto {
    pub scan_id: String,
    pub state: String,
    pub started_status_revision: Option<i64>,
    pub terminal_status_revision: Option<i64>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusResponse {
    pub data_revision: i64,
    pub status_revision: i64,
    pub scan_state: String,
    pub active_scan_id: Option<String>,
    pub last_finished_scan_id: Option<String>,
    pub last_finished_scan_result: Option<String>,
    pub followup: Option<FollowupDto>,
    pub target_scan: Option<TargetScanDto>,
    pub last_scan_started_at_ms: Option<i64>,
    pub last_scan_completed_at_ms: Option<i64>,
    pub last_scan_failed_at_ms: Option<i64>,
    pub last_scan_error_code: Option<String>,
    pub source_binding_status: String,
}

pub fn parse_summary_params(raw_query: Option<&str>) -> Result<SummaryParams, ApiError> {
    let mut range = None;
    let mut from = None;
    let mut to = None;
    let mut models = Vec::new();
    let mut project_paths = Vec::new();
    let mut include_projectless = false;
    let mut include_unknown_project = false;

    for (name, value) in raw_query
        .into_iter()
        .flat_map(|query| parse_form_urlencoded(query.as_bytes()))
    {
        match name.as_ref() {
            "range" => {
                if range.replace(value.into_owned()).is_some() {
                    return Err(ApiError::InvalidRange);
                }
            }
            "from" => {
                if from.replace(value.into_owned()).is_some() {
                    return Err(ApiError::InvalidRange);
                }
            }
            "to" => {
                if to.replace(value.into_owned()).is_some() {
                    return Err(ApiError::InvalidRange);
                }
            }
            "model" => models.push(validate_filter_value(value.into_owned())?),
            "project_path" => {
                project_paths.push(validate_filter_value(value.into_owned())?);
            }
            "include_projectless" => {
                if value != "1" {
                    return Err(ApiError::InvalidFilter);
                }
                include_projectless = true;
            }
            "include_unknown_project" => {
                if value != "1" {
                    return Err(ApiError::InvalidFilter);
                }
                include_unknown_project = true;
            }
            _ => {}
        }
    }

    validate_range_params(range.as_deref(), from.as_deref(), to.as_deref())?;

    Ok(SummaryParams {
        range,
        from,
        to,
        filter: UsageFilter::new(
            models,
            project_paths,
            include_projectless,
            include_unknown_project,
        ),
    })
}

pub fn validate_range_params(
    range: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(), ApiError> {
    match range {
        Some("custom") => {
            let from = from.ok_or(ApiError::InvalidRange)?;
            let to = to.ok_or(ApiError::InvalidRange)?;
            let from = parse_custom_date(from)?;
            let to = parse_custom_date(to)?;
            if from > to {
                return Err(ApiError::InvalidRange);
            }
        }
        Some(_) | None => {
            if from.is_some() || to.is_some() {
                return Err(ApiError::InvalidRange);
            }
        }
    }
    Ok(())
}

fn parse_custom_date(value: &str) -> Result<chrono::NaiveDate, ApiError> {
    if value.len() != 10
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || value
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return Err(ApiError::InvalidRange);
    }
    let date =
        chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| ApiError::InvalidRange)?;
    (date.format("%Y-%m-%d").to_string() == value)
        .then_some(date)
        .ok_or(ApiError::InvalidRange)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionQueryParams {
    pub range: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub filter: UsageFilter,
    pub seed_sort_field: SessionSortField,
    pub seed_sort_order: SessionSortOrder,
    pub expected_data_revision: Option<i64>,
    pub root_session_ids: Vec<String>,
}

pub fn parse_session_query_params(raw_query: Option<&str>) -> Result<SessionQueryParams, ApiError> {
    let summary = parse_summary_params(raw_query)?;
    let mut seed_sort_field = SessionSortField::LastActivity;
    let mut seed_sort_order = SessionSortOrder::Desc;
    let mut expected_data_revision = None;
    let mut root_session_ids = Vec::new();
    let mut seen_roots = std::collections::BTreeSet::new();
    let mut sort_by_seen = false;
    let mut sort_order_seen = false;
    for (name, value) in raw_query
        .into_iter()
        .flat_map(|query| parse_form_urlencoded(query.as_bytes()))
    {
        match name.as_ref() {
            "cursor" | "limit" => return Err(ApiError::InvalidFilter),
            "seed_sort_by" => {
                if sort_by_seen {
                    return Err(ApiError::InvalidFilter);
                }
                sort_by_seen = true;
                seed_sort_field = parse_session_sort_field(value.as_ref())?;
            }
            "seed_sort_order" => {
                if sort_order_seen {
                    return Err(ApiError::InvalidFilter);
                }
                sort_order_seen = true;
                seed_sort_order = match value.as_ref() {
                    "asc" => SessionSortOrder::Asc,
                    "desc" => SessionSortOrder::Desc,
                    _ => return Err(ApiError::InvalidFilter),
                };
            }
            "expected_data_revision" => {
                if expected_data_revision.is_some() {
                    return Err(ApiError::InvalidFilter);
                }
                let parsed = value.parse::<i64>().map_err(|_| ApiError::InvalidFilter)?;
                if parsed < 0 {
                    return Err(ApiError::InvalidFilter);
                }
                expected_data_revision = Some(parsed);
            }
            "root_session_id" => {
                let root = validate_filter_value(value.into_owned())?;
                if seen_roots.insert(root.clone()) {
                    root_session_ids.push(root);
                }
            }
            _ => {}
        }
    }
    Ok(SessionQueryParams {
        range: summary.range,
        from: summary.from,
        to: summary.to,
        filter: summary.filter,
        seed_sort_field,
        seed_sort_order,
        expected_data_revision,
        root_session_ids,
    })
}

fn parse_session_sort_field(value: &str) -> Result<SessionSortField, ApiError> {
    match value {
        "last_activity" => Ok(SessionSortField::LastActivity),
        "project" => Ok(SessionSortField::Project),
        "model" => Ok(SessionSortField::Model),
        "total_tokens" => Ok(SessionSortField::TotalTokens),
        "combined_total_tokens" => Ok(SessionSortField::CombinedTotalTokens),
        "combined_estimated_cost" => Ok(SessionSortField::CombinedEstimatedCost),
        "cache_hit_rate" => Ok(SessionSortField::CacheHitRate),
        _ => Err(ApiError::InvalidFilter),
    }
}

fn validate_filter_value(value: String) -> Result<String, ApiError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(ApiError::InvalidFilter);
    }
    Ok(value)
}

pub fn summary_response(
    range: &ResolvedRange,
    snapshot: UsageSnapshot<UsageSummary>,
) -> Result<SummaryResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    ensure_safe(snapshot.value.session_count)?;
    ensure_safe(snapshot.value.cost_incomplete_session_count)?;
    for value in [
        snapshot.value.health.total_sessions,
        snapshot.value.health.complete_sessions,
        snapshot.value.health.incomplete_sessions,
        snapshot.value.health.error_sessions,
    ] {
        ensure_safe(value)?;
    }
    let tokens = map_totals(snapshot.value.totals)?;
    Ok(SummaryResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        usage: SummaryUsageDto {
            input_tokens: tokens.input_tokens,
            output_tokens: tokens.output_tokens,
            total_tokens: tokens.total_tokens,
            reasoning_tokens: tokens.reasoning_tokens,
            cached_tokens: tokens.cached_tokens,
            cache_write_tokens: tokens.cache_write_tokens,
            uncached_input_tokens: tokens.uncached_input_tokens,
            other_output_tokens: tokens.other_output_tokens,
            cache_hit_rate: tokens.cache_hit_rate,
            estimated_cost: tokens.estimated_cost,
            estimated_cost_status: tokens.estimated_cost_status,
            session_count: snapshot.value.session_count,
            cost_incomplete_session_count: snapshot.value.cost_incomplete_session_count,
            complete_session_cost_per_million_tokens: snapshot
                .value
                .complete_session_cost_per_million_tokens,
            session_health: SessionHealthDto {
                total_sessions: snapshot.value.health.total_sessions,
                complete_sessions: snapshot.value.health.complete_sessions,
                incomplete_sessions: snapshot.value.health.incomplete_sessions,
                error_sessions: snapshot.value.health.error_sessions,
            },
        },
    })
}

pub fn models_response(
    range: &ResolvedRange,
    snapshot: UsageSnapshot<Vec<ModelUsageRow>>,
) -> Result<ModelsResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let mut rows = snapshot.value;
    rows.sort_by(|left, right| {
        right
            .totals
            .total_tokens
            .cmp(&left.totals.total_tokens)
            .then_with(|| left.model.cmp(&right.model))
    });
    let items = rows
        .into_iter()
        .map(map_model)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ModelsResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        items,
    })
}

pub fn model_distribution_response(
    range: &ResolvedRange,
    snapshot: AnalyticsSnapshot<Vec<ModelDistributionRow>>,
) -> Result<ModelDistributionResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let items = snapshot
        .value
        .into_iter()
        .map(|row| {
            Ok(ModelDistributionItemDto {
                model: row.model,
                usage: map_distribution_usage(row.usage)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(ModelDistributionResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        items,
    })
}

pub fn project_distribution_response(
    range: &ResolvedRange,
    snapshot: AnalyticsSnapshot<Vec<ProjectDistributionRow>>,
) -> Result<ProjectDistributionResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let items = snapshot
        .value
        .into_iter()
        .map(|row| {
            let (kind, project_name, project_path) = match row.identity {
                ProjectDistributionIdentity::Project {
                    project_name,
                    project_path,
                } => ("project", Some(project_name), Some(project_path)),
                ProjectDistributionIdentity::Projectless => ("projectless", None, None),
                ProjectDistributionIdentity::Unknown => ("unknown", None, None),
            };
            Ok(ProjectDistributionItemDto {
                kind: kind.to_owned(),
                project_name,
                project_path,
                usage: map_distribution_usage(row.usage)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(ProjectDistributionResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        items,
    })
}

pub fn skills_usage_response(
    range: &ResolvedRange,
    snapshot: AnalyticsSnapshot<SkillsUsage>,
) -> Result<SkillsUsageResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let days = snapshot
        .value
        .days
        .into_iter()
        .map(|day| {
            ensure_safe(day.start_ms)?;
            ensure_safe(day.end_ms)?;
            ensure_safe(day.total)?;
            let skills = day
                .skills
                .into_iter()
                .map(|skill| {
                    ensure_safe(skill.count)?;
                    Ok(SkillCountDto {
                        skill_name: skill.skill_name,
                        count: skill.count,
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()?;
            Ok(SkillDayDto {
                date: day.date,
                start_ms: day.start_ms,
                end_ms: day.end_ms,
                total: day.total,
                skills,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(SkillsUsageResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        data_status: if snapshot.value.ready {
            "ready"
        } else {
            "rebuilding"
        }
        .to_owned(),
        days,
    })
}

fn map_distribution_usage(
    usage: crate::usage::analytics::DistributionUsage,
) -> Result<DistributionUsageDto, ApiError> {
    ensure_safe(usage.total_tokens)?;
    let estimated_cost = match usage.estimated_cost_nanos_usd {
        Some(value) if value >= 0 => Some(value as f64 / 1_000_000_000.0),
        Some(_) => return Err(ApiError::QueryFailed),
        None => None,
    };
    let valid = match usage.cost_status {
        DistributionCostStatus::Complete => estimated_cost.is_some(),
        DistributionCostStatus::Partial => estimated_cost.is_some(),
        DistributionCostStatus::Unknown => estimated_cost.is_none(),
    };
    if !valid {
        return Err(ApiError::QueryFailed);
    }
    Ok(DistributionUsageDto {
        total_tokens: usage.total_tokens,
        estimated_cost,
        estimated_cost_status: usage.cost_status.as_str().to_owned(),
    })
}

pub fn filter_options_response(
    snapshot: UsageSnapshot<FilterOptions>,
) -> Result<FilterOptionsResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let models = snapshot
        .value
        .models
        .into_iter()
        .map(|model| ModelFilterOptionDto {
            model: model.model,
            provider: model.provider,
        })
        .collect();
    let projects = snapshot
        .value
        .projects
        .into_iter()
        .map(map_project_filter_option)
        .collect();
    Ok(FilterOptionsResponse {
        data_revision: snapshot.data_revision,
        models,
        projects,
    })
}

pub fn session_snapshot_response(
    range: &ResolvedRange,
    snapshot: SessionSnapshot,
) -> Result<SessionsResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let items = snapshot
        .rows
        .into_iter()
        .map(map_session)
        .collect::<Result<Vec<_>, _>>()?;
    let sort_index = snapshot
        .sort_index
        .into_iter()
        .map(map_sort_index)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionsResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        total_items: sort_index.len(),
        sort_index,
        items,
    })
}

pub fn session_rows_response(
    range: &ResolvedRange,
    snapshot: SessionRowsSnapshot,
) -> Result<SessionRowsResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    let items = snapshot
        .rows
        .into_iter()
        .map(map_session)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionRowsResponse {
        range: RangeDto::from(range),
        data_revision: snapshot.data_revision,
        items,
    })
}

pub fn session_detail_response(
    range: &ResolvedRange,
    snapshot: SessionDetailSnapshot,
) -> Result<SessionDetailResponse, ApiError> {
    ensure_safe(snapshot.data_revision)?;
    map_detail(range, snapshot.data_revision, snapshot.value)
}

fn map_session(row: SessionUsageRow) -> Result<SessionUsageDto, ApiError> {
    ensure_safe(row.last_activity_at_ms)?;
    ensure_safe(row.subagent_count)?;
    Ok(SessionUsageDto {
        root_session_id: row.root_session_id,
        title: row.title,
        project_name: row.project_name,
        project_path: row.project_path,
        last_activity_at_ms: row.last_activity_at_ms,
        models_used: row.models_used,
        subagent_count: row.subagent_count,
        inclusive_usage: (row.data_status != SessionDataStatus::Error)
            .then(|| map_totals(row.inclusive_usage))
            .transpose()?,
        self_usage: (row.data_status != SessionDataStatus::Error)
            .then(|| map_totals(row.self_usage))
            .transpose()?,
        subagent_usage: (row.data_status != SessionDataStatus::Error)
            .then(|| map_totals(row.subagent_usage))
            .transpose()?,
        data_status: session_status(row.data_status).to_owned(),
        error_code: row.error_code,
    })
}

fn map_sort_index(row: SessionSortIndexItem) -> Result<SessionSortIndexDto, ApiError> {
    ensure_safe(row.last_activity_at_ms)?;
    if let Some(value) = row.total_tokens {
        ensure_safe(value)?;
    }
    if let Some(value) = row.combined_total_tokens {
        ensure_safe(value)?;
    }
    let combined_estimated_cost = match row.combined_estimated_cost_nanos_usd {
        Some(value) if value >= 0 => Some(value as f64 / 1_000_000_000.0),
        Some(_) => return Err(ApiError::QueryFailed),
        None => None,
    };
    if row
        .cache_hit_rate
        .is_some_and(|ratio| !ratio.is_finite() || !(0.0..=1.0).contains(&ratio))
    {
        return Err(ApiError::QueryFailed);
    }
    Ok(SessionSortIndexDto {
        root_session_id: row.root_session_id,
        last_activity_at_ms: row.last_activity_at_ms,
        project_sort_key: row.project_sort_key,
        model_sort_key: row.model_sort_key,
        total_tokens: row.total_tokens,
        combined_total_tokens: row.combined_total_tokens,
        combined_estimated_cost,
        cache_hit_rate: row.cache_hit_rate,
        data_status: session_status(row.data_status).to_owned(),
        error_code: row.error_code,
    })
}

fn session_status(status: SessionDataStatus) -> &'static str {
    match status {
        SessionDataStatus::Complete => "complete",
        SessionDataStatus::Incomplete => "incomplete",
        SessionDataStatus::Error => "error",
    }
}

fn map_detail(
    range: &ResolvedRange,
    data_revision: i64,
    detail: SessionDetail,
) -> Result<SessionDetailResponse, ApiError> {
    ensure_safe(detail.last_activity_at_ms)?;
    ensure_safe(detail.main.subagent_count)?;
    let main = detail.main;
    let main_models = main
        .model_usage
        .into_iter()
        .map(|model| {
            Ok(MainModelUsageDto {
                model: model.model,
                reasoning_effort: model.reasoning_effort,
                usage: map_totals(model.usage)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let subagents = detail
        .subagents
        .into_iter()
        .map(|subagent| {
            ensure_safe(subagent.last_activity_at_ms)?;
            let model_usage = subagent
                .model_usage
                .into_iter()
                .map(map_subagent_model_usage)
                .collect::<Result<Vec<_>, ApiError>>()?;
            Ok(SubagentDetailDto {
                thread_id: subagent.thread_id,
                parent_thread_id: subagent.parent_thread_id,
                root_session_id: subagent.root_session_id,
                title: subagent.title,
                last_activity_at_ms: subagent.last_activity_at_ms,
                model_usage,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(SessionDetailResponse {
        range: RangeDto::from(range),
        data_revision,
        root_session_id: detail.root_session_id,
        last_activity_at_ms: detail.last_activity_at_ms,
        main: MainSessionDetailDto {
            title: main.title,
            thread_id: main.thread_id,
            root_session_id: main.root_session_id,
            models_used: main.models_used,
            model_usage: main_models,
            self_usage: map_totals(main.self_usage)?,
            subagent_count: main.subagent_count,
            inclusive_usage: map_totals(main.inclusive_usage)?,
        },
        subagents,
    })
}

fn map_subagent_model_usage(model: SubagentModelUsage) -> Result<SubagentModelUsageDto, ApiError> {
    ensure_safe(model.last_activity_at_ms)?;
    Ok(SubagentModelUsageDto {
        model: model.model,
        reasoning_effort: model.reasoning_effort,
        last_activity_at_ms: model.last_activity_at_ms,
        usage: map_totals(model.usage)?,
    })
}

fn map_model(row: ModelUsageRow) -> Result<ModelUsageDto, ApiError> {
    ensure_safe(row.session_count)?;
    ensure_safe(row.first_activity_at_ms)?;
    ensure_safe(row.last_activity_at_ms)?;
    Ok(ModelUsageDto {
        model: row.model,
        usage: map_totals(row.totals)?,
        session_count: row.session_count,
        first_activity_at_ms: row.first_activity_at_ms,
        last_activity_at_ms: row.last_activity_at_ms,
    })
}

fn map_project_filter_option(option: ProjectFilterOption) -> ProjectFilterOptionDto {
    match option {
        ProjectFilterOption::Project {
            project_name,
            project_path,
        } => ProjectFilterOptionDto::Project {
            project_name,
            project_path,
        },
        ProjectFilterOption::Projectless => ProjectFilterOptionDto::Projectless,
        ProjectFilterOption::Unknown => ProjectFilterOptionDto::Unknown,
    }
}

fn map_totals(totals: TokenTotals) -> Result<TokenUsageDto, ApiError> {
    for value in [
        totals.input_tokens,
        totals.output_tokens,
        totals.total_tokens,
        totals.reasoning_tokens,
        totals.cached_tokens,
        totals.other_output_tokens,
    ] {
        ensure_safe(value)?;
    }
    if let Some(value) = totals.cache_write_tokens {
        ensure_safe(value)?;
    }
    if let Some(value) = totals.uncached_input_tokens {
        ensure_safe(value)?;
    }
    if totals
        .cache_hit_rate
        .is_some_and(|ratio| !ratio.is_finite() || !(0.0..=1.0).contains(&ratio))
    {
        return Err(ApiError::QueryFailed);
    }
    let estimated_cost = match totals.estimated_cost_nanos_usd {
        Some(nanos) if nanos >= 0 => Some(nanos as f64 / 1_000_000_000.0),
        Some(_) => return Err(ApiError::QueryFailed),
        None => None,
    };
    let estimated_cost_status = match totals.cost_completeness {
        CostCompleteness::Empty | CostCompleteness::Complete if estimated_cost.is_some() => {
            "complete"
        }
        CostCompleteness::Partial if estimated_cost.is_some() => "partial",
        CostCompleteness::Unknown if estimated_cost.is_none() => "unknown",
        _ => return Err(ApiError::QueryFailed),
    };
    Ok(TokenUsageDto {
        input_tokens: totals.input_tokens,
        cached_tokens: totals.cached_tokens,
        cache_write_tokens: totals.cache_write_tokens,
        uncached_input_tokens: totals.uncached_input_tokens,
        output_tokens: totals.output_tokens,
        reasoning_tokens: totals.reasoning_tokens,
        other_output_tokens: totals.other_output_tokens,
        total_tokens: totals.total_tokens,
        cache_hit_rate: totals.cache_hit_rate,
        estimated_cost,
        estimated_cost_status: estimated_cost_status.to_owned(),
    })
}

fn ensure_safe(value: i64) -> Result<(), ApiError> {
    (0..=JSON_SAFE_INTEGER_MAX)
        .contains(&value)
        .then_some(())
        .ok_or(ApiError::QueryOverflow)
}

pub fn revision(ledger: &Ledger) -> Result<RevisionResponse, ApiError> {
    let state = ledger.app_state().map_err(map_storage_error)?;
    revision_from_state(&state)
}

pub fn revision_from_state(state: &AppState) -> Result<RevisionResponse, ApiError> {
    ensure_safe(state.data_revision)?;
    ensure_safe(state.status_revision)?;
    Ok(RevisionResponse {
        data_revision: state.data_revision,
        status_revision: state.status_revision,
    })
}

pub fn status(ledger: &Ledger, target_scan_id: Option<&str>) -> Result<StatusResponse, ApiError> {
    if target_scan_id.is_some_and(|scan_id| Uuid::parse_str(scan_id).is_err()) {
        return Err(ApiError::InvalidScanId);
    }
    let snapshot = ledger
        .scan_status_snapshot(target_scan_id)
        .map_err(map_storage_error)?;
    if target_scan_id.is_some() && snapshot.target_scan.is_none() {
        return Err(ApiError::ScanNotFound);
    }
    status_from_snapshot(snapshot)
}

pub fn status_from_snapshot(snapshot: ScanStatusSnapshot) -> Result<StatusResponse, ApiError> {
    let AppState {
        data_revision,
        scan: state,
        ..
    } = snapshot.app_state;
    ensure_safe(data_revision)?;
    ensure_safe(state.status_revision)?;
    for value in [
        state.last_scan_started_at_ms,
        state.last_scan_completed_at_ms,
        state.last_scan_failed_at_ms,
        state.followup_requested_at_ms,
        state.followup_enqueued_status_revision,
    ]
    .into_iter()
    .flatten()
    {
        ensure_safe(value)?;
    }
    let followup = match state.followup_state {
        None => None,
        Some(followup_state) => Some(FollowupDto {
            scan_id: state
                .followup_scan_id
                .clone()
                .ok_or(ApiError::QueryFailed)?,
            state: followup_state.as_str().to_owned(),
            enqueued_status_revision: state
                .followup_enqueued_status_revision
                .ok_or(ApiError::QueryFailed)?,
            requested_at_ms: state
                .followup_requested_at_ms
                .ok_or(ApiError::QueryFailed)?,
            error_code: match followup_state {
                FollowupState::Queued => None,
                FollowupState::StartFailed => state.followup_error_code.clone(),
            },
        }),
    };
    let target_scan = snapshot.target_scan.map(map_target_scan).transpose()?;
    Ok(StatusResponse {
        data_revision,
        status_revision: state.status_revision,
        scan_state: state.scan_state.as_str().to_owned(),
        active_scan_id: state.active_scan_id,
        last_finished_scan_id: state.last_finished_scan_id,
        last_finished_scan_result: state
            .last_finished_scan_result
            .map(|result| result.as_str().to_owned()),
        followup,
        target_scan,
        last_scan_started_at_ms: state.last_scan_started_at_ms,
        last_scan_completed_at_ms: state.last_scan_completed_at_ms,
        last_scan_failed_at_ms: state.last_scan_failed_at_ms,
        last_scan_error_code: state.last_scan_error_code,
        source_binding_status: match state.source_binding_status {
            SourceBindingStatus::Unbound => "unbound",
            SourceBindingStatus::Ready => "ready",
            SourceBindingStatus::SourceChanged => "source_changed",
        }
        .to_owned(),
    })
}

fn map_target_scan(scan: ScanRun) -> Result<TargetScanDto, ApiError> {
    if let Some(value) = scan.started_status_revision {
        ensure_safe(value)?;
    }
    if let Some(value) = scan.terminal_status_revision {
        ensure_safe(value)?;
    }
    Ok(TargetScanDto {
        scan_id: scan.scan_id,
        state: scan.state.as_str().to_owned(),
        started_status_revision: scan.started_status_revision,
        terminal_status_revision: scan.terminal_status_revision,
        error_code: scan.error_code,
    })
}

pub(crate) fn map_usage_ledger_error(error: UsageLedgerError) -> ApiError {
    match error {
        UsageLedgerError::Storage(error) => map_storage_error(error),
        UsageLedgerError::Aggregate(error) => map_aggregate_error(error),
        UsageLedgerError::StaleDataRevision => ApiError::StaleDataRevision,
        UsageLedgerError::Invalid(_) => ApiError::QueryFailed,
        UsageLedgerError::Pipeline(_) | UsageLedgerError::Rebuild(_) => ApiError::QueryFailed,
    }
}

fn map_aggregate_error(error: AggregateError) -> ApiError {
    match error {
        AggregateError::ArithmeticOverflow => ApiError::QueryOverflow,
        AggregateError::InvalidRange => ApiError::InvalidRange,
        AggregateError::InvalidPage => ApiError::InvalidSessionIds,
        AggregateError::InvalidCursor => ApiError::InvalidSessionIds,
        AggregateError::InvalidSessionIds => ApiError::InvalidSessionIds,
        AggregateError::QueryFailed | AggregateError::InvariantViolation => ApiError::QueryFailed,
    }
}

fn map_storage_error(error: crate::storage::StorageError) -> ApiError {
    match error.kind() {
        StorageErrorKind::DatabaseBusy => ApiError::DatabaseBusy,
        _ => ApiError::QueryFailed,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{
            AppState, FollowupState, ScanLifecycleState, ScanRequestKind, ScanResult, ScanRunState,
            ScanState, ScanTrigger, SourceBindingStatus,
        },
        range::{RangeKey, resolve_utc_range_at_for_test},
        usage::aggregate::{CostCompleteness, ModelUsageRow, SessionUsageRow, TokenTotals},
    };
    use chrono::DateTime;

    fn utc_range(key: RangeKey) -> ResolvedRange {
        let now = DateTime::parse_from_rfc3339("2026-08-08T12:34:56Z")
            .unwrap()
            .timestamp_millis();
        resolve_utc_range_at_for_test(key, now).unwrap()
    }

    fn totals(cache_write_tokens: Option<i64>, input: i64, cached: i64) -> TokenTotals {
        let output_tokens = 2;
        let reasoning_tokens = 1;
        let total_tokens = input + output_tokens;
        let uncached_input_tokens = cache_write_tokens.map(|write| input - cached - write);
        TokenTotals {
            input_tokens: input,
            cached_tokens: cached,
            cache_write_tokens,
            output_tokens,
            reasoning_tokens,
            total_tokens,
            uncached_input_tokens,
            other_output_tokens: output_tokens - reasoning_tokens,
            cache_hit_rate: (input > 0).then_some(cached as f64 / input as f64),
            estimated_cost_nanos_usd: None,
            cost_completeness: CostCompleteness::Unknown,
        }
    }

    #[test]
    fn t_s06_001_summary_query_parser_matrix() {
        let parsed = parse_summary_params(Some(
            "range=year&model=gpt%2Cb&model=gpt%2Ca&model=gpt%2Cb&project_path=%2FUsers%2Fme%2Fmy+path%2F%26%2F%E4%B8%AD&project_path=%2FUsers%2Fme%2Fmy+path%2F%26%2F%E4%B8%AD&include_projectless=1&include_projectless=1&include_unknown_project=1",
        ))
        .unwrap();
        assert_eq!(parsed.range.as_deref(), Some("year"));
        assert_eq!(
            parsed.filter.models(),
            &["gpt,a".to_owned(), "gpt,b".to_owned()]
        );
        assert_eq!(
            parsed.filter.project_paths(),
            &["/Users/me/my path/&/中".to_owned()]
        );
        assert!(parsed.filter.include_projectless());
        assert!(parsed.filter.include_unknown_project());

        let separate_values =
            parse_summary_params(Some("range=year&model=gpt%2Ca&model=gpt%2Cb")).unwrap();
        assert_eq!(
            separate_values.filter.models(),
            &["gpt,a".to_owned(), "gpt,b".to_owned()]
        );

        let empty = parse_summary_params(Some("range=year")).unwrap();
        assert!(empty.filter.models().is_empty());
        assert!(empty.filter.project_paths().is_empty());
        assert!(!empty.filter.include_projectless());
        assert!(!empty.filter.include_unknown_project());

        for raw_query in [
            "range=year&model=",
            "range=year&project_path=",
            "range=year&model=bad%00model",
            "range=year&project_path=%2Ftmp%2Fbad%0Apath",
            "range=year&include_projectless=0",
            "range=year&include_unknown_project=true",
            "range=year&include_projectless=1&include_projectless=0",
        ] {
            assert_eq!(
                parse_summary_params(Some(raw_query)),
                Err(ApiError::InvalidFilter),
                "query should be rejected: {raw_query}"
            );
        }
        assert_eq!(
            parse_summary_params(Some("range=year&range=30d")),
            Err(ApiError::InvalidRange)
        );
    }

    #[test]
    fn t_022_a1_custom_range_query_requires_a_valid_inclusive_date_pair() {
        let parsed = parse_summary_params(Some(
            "range=custom&from=2026-08-01&to=2026-08-03&model=gpt-5",
        ))
        .unwrap();
        assert_eq!(parsed.range.as_deref(), Some("custom"));
        assert_eq!(parsed.from.as_deref(), Some("2026-08-01"));
        assert_eq!(parsed.to.as_deref(), Some("2026-08-03"));

        for query in [
            "range=custom&from=2026-08-01",
            "range=custom&to=2026-08-03",
            "range=custom&from=2026-08-04&to=2026-08-03",
            "range=custom&from=2026-02-30&to=2026-03-01",
            "range=7d&from=2026-08-01&to=2026-08-03",
        ] {
            assert_eq!(
                parse_summary_params(Some(query)),
                Err(ApiError::InvalidRange),
                "query should be rejected: {query}"
            );
        }
    }

    #[test]
    fn t_s05_004_006_dto_mapping_preserves_cache_semantics_cost_and_model_sort() {
        let range = utc_range(RangeKey::Today);
        let summary = summary_response(
            &range,
            UsageSnapshot {
                data_revision: 9,
                value: UsageSummary {
                    totals: TokenTotals {
                        input_tokens: 0,
                        cached_tokens: 0,
                        cache_write_tokens: Some(0),
                        output_tokens: 0,
                        reasoning_tokens: 0,
                        total_tokens: 0,
                        uncached_input_tokens: Some(0),
                        other_output_tokens: 0,
                        cache_hit_rate: None,
                        estimated_cost_nanos_usd: Some(0),
                        cost_completeness: CostCompleteness::Empty,
                    },
                    session_count: 0,
                    cost_incomplete_session_count: 0,
                    complete_session_cost_per_million_tokens: Some(12.345),
                    health: crate::usage::aggregate::SessionHealthSummary {
                        total_sessions: 0,
                        complete_sessions: 0,
                        incomplete_sessions: 0,
                        error_sessions: 0,
                    },
                },
            },
        )
        .unwrap();
        assert_eq!(summary.usage.cache_write_tokens, Some(0));
        assert_eq!(summary.usage.uncached_input_tokens, Some(0));
        assert_eq!(summary.usage.cache_hit_rate, None);
        assert_eq!(summary.usage.estimated_cost, Some(0.0));
        assert_eq!(summary.usage.estimated_cost_status, "complete");
        assert_eq!(
            summary.usage.complete_session_cost_per_million_tokens,
            Some(12.345)
        );

        let response = session_snapshot_response(
            &range,
            SessionSnapshot {
                data_revision: 9,
                rows: vec![SessionUsageRow {
                    root_session_id: "root-a".into(),
                    title: None,
                    project_name: None,
                    project_path: None,
                    inclusive_usage: totals(None, 10, 4),
                    self_usage: totals(Some(3), 10, 4),
                    subagent_usage: totals(Some(0), 10, 4),
                    subagent_count: 1,
                    last_activity_at_ms: 20,
                    models_used: vec!["unknown".into(), "gpt-5".into()],
                    data_status: SessionDataStatus::Incomplete,
                    error_code: None,
                }],
                sort_index: vec![SessionSortIndexItem {
                    root_session_id: "root-a".into(),
                    last_activity_at_ms: 20,
                    project_sort_key: None,
                    model_sort_key: Some("unknown".into()),
                    total_tokens: Some(12),
                    combined_total_tokens: Some(12),
                    combined_estimated_cost_nanos_usd: None,
                    cache_hit_rate: Some(0.4),
                    data_status: SessionDataStatus::Incomplete,
                    error_code: None,
                }],
            },
        )
        .unwrap();
        assert_eq!(
            response.items[0]
                .inclusive_usage
                .as_ref()
                .unwrap()
                .cache_write_tokens,
            None
        );
        assert_eq!(
            response.items[0]
                .inclusive_usage
                .as_ref()
                .unwrap()
                .uncached_input_tokens,
            None
        );
        assert_eq!(
            response.items[0]
                .self_usage
                .as_ref()
                .unwrap()
                .cache_write_tokens,
            Some(3)
        );
        assert_eq!(
            response.items[0]
                .self_usage
                .as_ref()
                .unwrap()
                .cache_hit_rate,
            Some(0.4)
        );
        assert_eq!(
            response.items[0]
                .subagent_usage
                .as_ref()
                .unwrap()
                .cache_write_tokens,
            Some(0)
        );
        assert_eq!(
            response.items[0]
                .subagent_usage
                .as_ref()
                .unwrap()
                .uncached_input_tokens,
            Some(6)
        );
        assert_eq!(
            response.items[0]
                .inclusive_usage
                .as_ref()
                .unwrap()
                .estimated_cost,
            None
        );
        assert_eq!(
            response.items[0]
                .self_usage
                .as_ref()
                .unwrap()
                .estimated_cost,
            None
        );
        assert_eq!(
            response.items[0]
                .subagent_usage
                .as_ref()
                .unwrap()
                .estimated_cost,
            None
        );
        assert_eq!(
            response.items[0]
                .inclusive_usage
                .as_ref()
                .unwrap()
                .estimated_cost_status,
            "unknown"
        );
        assert_eq!(
            response.items[0]
                .self_usage
                .as_ref()
                .unwrap()
                .estimated_cost_status,
            "unknown"
        );
        assert_eq!(
            response.items[0]
                .subagent_usage
                .as_ref()
                .unwrap()
                .estimated_cost_status,
            "unknown"
        );

        let models = models_response(
            &range,
            UsageSnapshot {
                data_revision: 9,
                value: vec![
                    ModelUsageRow {
                        model: "unknown".into(),
                        totals: totals(Some(3), 5, 0),
                        session_count: 1,
                        first_activity_at_ms: 1,
                        last_activity_at_ms: 2,
                    },
                    ModelUsageRow {
                        model: "gpt-5-b".into(),
                        totals: totals(Some(3), 20, 0),
                        session_count: 1,
                        first_activity_at_ms: 1,
                        last_activity_at_ms: 2,
                    },
                    ModelUsageRow {
                        model: "gpt-5-a".into(),
                        totals: totals(Some(3), 20, 0),
                        session_count: 1,
                        first_activity_at_ms: 1,
                        last_activity_at_ms: 2,
                    },
                ],
            },
        )
        .unwrap();
        assert_eq!(
            models
                .items
                .iter()
                .map(|row| row.model.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-5-a", "gpt-5-b", "unknown"]
        );
        assert!(models.items.iter().any(|row| row.model == "unknown"));

        assert_eq!(
            summary_response(
                &range,
                UsageSnapshot {
                    data_revision: JSON_SAFE_INTEGER_MAX + 1,
                    value: UsageSummary {
                        totals: totals(Some(3), 1, 0),
                        session_count: 0,
                        cost_incomplete_session_count: 0,
                        complete_session_cost_per_million_tokens: None,
                        health: crate::usage::aggregate::SessionHealthSummary {
                            total_sessions: 0,
                            complete_sessions: 0,
                            incomplete_sessions: 0,
                            error_sessions: 0,
                        },
                    },
                }
            ),
            Err(ApiError::QueryOverflow)
        );
    }

    #[test]
    fn t_s05_006_subagent_model_usage_dto_has_exact_block_shape() {
        let range = utc_range(RangeKey::Today);
        let usage = totals(Some(3), 10, 4);
        let response = session_detail_response(
            &range,
            SessionDetailSnapshot {
                data_revision: 9,
                value: SessionDetail {
                    root_session_id: "root".into(),
                    last_activity_at_ms: 20,
                    main: crate::usage::aggregate::MainSessionDetail {
                        title: Some("Root".into()),
                        thread_id: "root".into(),
                        root_session_id: "root".into(),
                        models_used: vec!["main-model".into()],
                        model_usage: Vec::new(),
                        self_usage: usage.clone(),
                        subagent_count: 1,
                        inclusive_usage: usage.clone(),
                    },
                    subagents: vec![crate::usage::aggregate::SubagentDetail {
                        thread_id: "child".into(),
                        parent_thread_id: Some("root".into()),
                        root_session_id: "root".into(),
                        title: Some("Child".into()),
                        last_activity_at_ms: 19,
                        model_usage: vec![SubagentModelUsage {
                            model: "child-model".into(),
                            reasoning_effort: Some("high".into()),
                            last_activity_at_ms: 19,
                            usage,
                        }],
                    }],
                },
            },
        )
        .unwrap();
        let subagent = serde_json::to_value(&response.subagents[0]).unwrap();
        assert_eq!(
            subagent,
            serde_json::json!({
                "thread_id": "child",
                "parent_thread_id": "root",
                "root_session_id": "root",
                "title": "Child",
                "last_activity_at_ms": 19,
                "model_usage": [{
                    "model": "child-model",
                    "reasoning_effort": "high",
                    "last_activity_at_ms": 19,
                    "usage": {
                        "input_tokens": 10,
                        "cached_tokens": 4,
                        "cache_write_tokens": 3,
                        "uncached_input_tokens": 3,
                        "output_tokens": 2,
                        "reasoning_tokens": 1,
                        "other_output_tokens": 1,
                        "total_tokens": 12,
                        "cache_hit_rate": 0.4,
                        "estimated_cost": null,
                        "estimated_cost_status": "unknown"
                    }
                }]
            })
        );
    }

    #[test]
    fn t_s05_008_status_mapping_covers_projection_followup_target_and_nullable_fields() {
        let scan = ScanState {
            status_revision: 12,
            scan_state: ScanLifecycleState::Failed,
            active_scan_id: None,
            last_finished_scan_id: Some("finished".into()),
            last_finished_scan_result: Some(ScanResult::Failed),
            last_scan_started_at_ms: Some(1),
            last_scan_completed_at_ms: None,
            last_scan_failed_at_ms: Some(2),
            last_scan_error_code: Some("SCAN_INTERRUPTED".into()),
            followup_scan_id: Some("00000000-0000-0000-0000-000000000001".into()),
            followup_state: Some(FollowupState::StartFailed),
            followup_trigger: Some(ScanTrigger::Manual),
            followup_requested_at_ms: Some(3),
            followup_enqueued_status_revision: Some(11),
            followup_error_code: Some("SCANNER_UNAVAILABLE".into()),
            source_binding_status: SourceBindingStatus::SourceChanged,
        };
        let app_state = AppState::new(8, scan).unwrap();
        let target = ScanRun {
            scan_id: Uuid::nil().to_string(),
            trigger: ScanTrigger::Manual,
            request_kind: ScanRequestKind::Direct,
            state: ScanRunState::Failed,
            requested_at_ms: 0,
            enqueued_status_revision: None,
            started_at_ms: Some(1),
            started_status_revision: Some(9),
            finished_at_ms: Some(2),
            terminal_status_revision: Some(10),
            error_code: Some("SCAN_CANCELLED".into()),
        };
        let response =
            status_from_snapshot(ScanStatusSnapshot::new(app_state, Some(target)).unwrap())
                .unwrap();
        assert_eq!(response.data_revision, 8);
        assert_eq!(response.status_revision, 12);
        assert_eq!(response.scan_state, "failed");
        assert_eq!(
            response.last_finished_scan_result.as_deref(),
            Some("failed")
        );
        assert_eq!(response.followup.as_ref().unwrap().state, "start_failed");
        assert_eq!(
            response.followup.as_ref().unwrap().error_code.as_deref(),
            Some("SCANNER_UNAVAILABLE")
        );
        assert_eq!(response.target_scan.unwrap().state, "failed");
        assert_eq!(response.source_binding_status, "source_changed");
        assert_eq!(response.last_scan_completed_at_ms, None);

        assert_eq!(
            revision_from_state(
                &ScanStatusSnapshot::new(AppState::new(0, ScanState::initial()).unwrap(), None,)
                    .unwrap()
                    .app_state
            )
            .unwrap()
            .data_revision,
            0
        );
    }

    #[test]
    fn t_s05_008_idle_running_queued_and_first_import_projection_matrix() {
        let cases = [
            (ScanState::initial(), "idle", None, None, "unbound"),
            (
                ScanState {
                    status_revision: 4,
                    scan_state: ScanLifecycleState::Running,
                    active_scan_id: Some("00000000-0000-0000-0000-000000000010".into()),
                    last_finished_scan_id: None,
                    last_finished_scan_result: None,
                    last_scan_started_at_ms: Some(10),
                    last_scan_completed_at_ms: None,
                    last_scan_failed_at_ms: None,
                    last_scan_error_code: None,
                    followup_scan_id: Some("00000000-0000-0000-0000-000000000011".into()),
                    followup_state: Some(FollowupState::Queued),
                    followup_trigger: Some(ScanTrigger::Manual),
                    followup_requested_at_ms: Some(11),
                    followup_enqueued_status_revision: Some(4),
                    followup_error_code: None,
                    source_binding_status: SourceBindingStatus::Ready,
                },
                "running",
                Some("queued"),
                None,
                "ready",
            ),
            (
                ScanState {
                    status_revision: 8,
                    scan_state: ScanLifecycleState::Idle,
                    active_scan_id: None,
                    last_finished_scan_id: Some("00000000-0000-0000-0000-000000000012".into()),
                    last_finished_scan_result: Some(ScanResult::Completed),
                    last_scan_started_at_ms: Some(12),
                    last_scan_completed_at_ms: Some(13),
                    last_scan_failed_at_ms: None,
                    last_scan_error_code: None,
                    followup_scan_id: None,
                    followup_state: None,
                    followup_trigger: None,
                    followup_requested_at_ms: None,
                    followup_enqueued_status_revision: None,
                    followup_error_code: None,
                    source_binding_status: SourceBindingStatus::Ready,
                },
                "idle",
                None,
                Some("completed"),
                "ready",
            ),
        ];

        for (scan, expected_state, expected_followup, expected_result, binding) in cases {
            let response = status_from_snapshot(
                ScanStatusSnapshot::new(AppState::new(3, scan).unwrap(), None).unwrap(),
            )
            .unwrap();
            assert_eq!(response.scan_state, expected_state);
            assert_eq!(
                response.followup.as_ref().map(|value| value.state.as_str()),
                expected_followup
            );
            assert_eq!(
                response.last_finished_scan_result.as_deref(),
                expected_result
            );
            assert_eq!(response.source_binding_status, binding);
            if let Some(followup) = response.followup {
                assert_eq!(
                    followup.error_code, None,
                    "queued follow-up never carries an error"
                );
            }
        }
    }

    #[test]
    fn t_s05_019_busy_overflow_and_internal_failures_collapse_to_safe_api_codes() {
        let busy = crate::storage::StorageError::sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("SQL/path/prompt sentinel must not escape".into()),
        ));
        assert_eq!(map_storage_error(busy), ApiError::DatabaseBusy);

        let huge = TokenTotals {
            input_tokens: JSON_SAFE_INTEGER_MAX + 1,
            cached_tokens: 0,
            cache_write_tokens: Some(0),
            output_tokens: 0,
            total_tokens: JSON_SAFE_INTEGER_MAX + 1,
            reasoning_tokens: 0,
            uncached_input_tokens: Some(0),
            other_output_tokens: 0,
            cache_hit_rate: None,
            estimated_cost_nanos_usd: None,
            cost_completeness: CostCompleteness::Unknown,
        };
        assert_eq!(map_totals(huge), Err(ApiError::QueryOverflow));
        assert_eq!(
            map_aggregate_error(AggregateError::ArithmeticOverflow),
            ApiError::QueryOverflow
        );
        assert_eq!(
            map_aggregate_error(AggregateError::InvariantViolation),
            ApiError::QueryFailed
        );
    }

    #[test]
    fn t_mu03_b06_api_cost_boundary_maps_nanos_to_usd() {
        let mut known = totals(Some(0), 10, 2);
        known.estimated_cost_nanos_usd = Some(1_500_000_000);
        known.cost_completeness = CostCompleteness::Complete;
        let known_dto = map_totals(known).unwrap();
        assert_eq!(known_dto.estimated_cost, Some(1.5));
        assert_eq!(known_dto.estimated_cost_status, "complete");

        let mut empty = totals(Some(0), 0, 0);
        empty.estimated_cost_nanos_usd = Some(0);
        empty.cost_completeness = CostCompleteness::Empty;
        let empty_dto = map_totals(empty).unwrap();
        assert_eq!(empty_dto.estimated_cost, Some(0.0));
        assert_eq!(empty_dto.estimated_cost_status, "complete");

        let unknown = totals(Some(0), 10, 2);
        let unknown_dto = map_totals(unknown).unwrap();
        assert_eq!(unknown_dto.estimated_cost, None);
        assert_eq!(unknown_dto.estimated_cost_status, "unknown");
    }

    #[test]
    fn t_mu04_c03_api_cost_status_contract() {
        for (cost, completeness, expected_cost, expected_status) in [
            (
                Some(1_250_000_000),
                CostCompleteness::Complete,
                Some(1.25),
                "complete",
            ),
            (
                Some(750_000_000),
                CostCompleteness::Partial,
                Some(0.75),
                "partial",
            ),
            (None, CostCompleteness::Unknown, None, "unknown"),
            (Some(0), CostCompleteness::Empty, Some(0.0), "complete"),
        ] {
            let mut value = totals(Some(0), 10, 2);
            value.estimated_cost_nanos_usd = cost;
            value.cost_completeness = completeness;
            let dto = map_totals(value).unwrap();
            assert_eq!(dto.estimated_cost, expected_cost);
            assert_eq!(dto.estimated_cost_status, expected_status);
        }

        for (cost, completeness) in [
            (None, CostCompleteness::Complete),
            (None, CostCompleteness::Partial),
            (Some(1), CostCompleteness::Unknown),
        ] {
            let mut value = totals(Some(0), 10, 2);
            value.estimated_cost_nanos_usd = cost;
            value.cost_completeness = completeness;
            assert_eq!(map_totals(value), Err(ApiError::QueryFailed));
        }
    }
}
