//! Read-only usage aggregation over the active ledger epoch.

use std::{collections::BTreeMap, fmt, path::Path};

use rusqlite::{Connection, Row, params};
use rusqlite::{params_from_iter, types::Value};

use crate::cost::ModelRegistry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl TimeRange {
    pub const fn new(start_ms: i64, end_ms: i64) -> Result<Self, AggregateError> {
        if start_ms < 0 || end_ms < 0 || start_ms > end_ms {
            return Err(AggregateError::InvalidRange);
        }
        Ok(Self { start_ms, end_ms })
    }

    pub const fn is_empty(self) -> bool {
        self.start_ms == self.end_ms
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageFilter {
    models: Vec<String>,
    project_paths: Vec<String>,
    include_projectless: bool,
    include_unknown_project: bool,
}

impl UsageFilter {
    pub fn new(
        mut models: Vec<String>,
        mut project_paths: Vec<String>,
        include_projectless: bool,
        include_unknown_project: bool,
    ) -> Self {
        models.sort();
        models.dedup();
        project_paths.sort();
        project_paths.dedup();
        Self {
            models,
            project_paths,
            include_projectless,
            include_unknown_project,
        }
    }

    pub fn models(&self) -> &[String] {
        &self.models
    }

    pub fn project_paths(&self) -> &[String] {
        &self.project_paths
    }

    pub const fn include_projectless(&self) -> bool {
        self.include_projectless
    }

    pub const fn include_unknown_project(&self) -> bool {
        self.include_unknown_project
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryQuery {
    range: TimeRange,
    filter: UsageFilter,
}

impl SummaryQuery {
    pub const fn new(range: TimeRange, filter: UsageFilter) -> Self {
        Self { range, filter }
    }

    pub const fn range(&self) -> TimeRange {
        self.range
    }

    pub const fn filter(&self) -> &UsageFilter {
        &self.filter
    }
}

pub const MAX_SESSION_PAGE_SIZE: usize = 256;
pub const MAX_SESSION_ROWS: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCursor {
    pub last_activity_at_ms: i64,
    pub root_session_id: String,
}

impl SessionCursor {
    pub fn new(
        last_activity_at_ms: i64,
        root_session_id: impl Into<String>,
    ) -> Result<Self, AggregateError> {
        let root_session_id = root_session_id.into();
        if last_activity_at_ms < 0
            || root_session_id.is_empty()
            || root_session_id.chars().any(char::is_control)
        {
            return Err(AggregateError::InvalidCursor);
        }
        Ok(Self {
            last_activity_at_ms,
            root_session_id,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionPageRequest {
    pub limit: usize,
    pub after: Option<SessionCursor>,
}

impl SessionPageRequest {
    pub const fn new(limit: usize) -> Self {
        Self { limit, after: None }
    }

    pub fn with_after(mut self, after: SessionCursor) -> Self {
        self.after = Some(after);
        self
    }

    fn validate(&self) -> Result<(), AggregateError> {
        if self.limit == 0 || self.limit > MAX_SESSION_PAGE_SIZE {
            return Err(AggregateError::InvalidPage);
        }
        if let Some(cursor) = self.after.as_ref() {
            SessionCursor::new(cursor.last_activity_at_ms, cursor.root_session_id.clone())?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CostCompleteness {
    Empty,
    Complete,
    Partial,
    Unknown,
}

impl CostCompleteness {
    fn merge(self, other: Self) -> Self {
        use CostCompleteness::{Complete, Empty, Partial, Unknown};

        match (self, other) {
            (Empty, state) | (state, Empty) => state,
            (Complete, Complete) => Complete,
            (Unknown, Unknown) => Unknown,
            (Complete, Unknown)
            | (Unknown, Complete)
            | (Complete, Partial)
            | (Partial, Complete)
            | (Partial, Partial)
            | (Partial, Unknown)
            | (Unknown, Partial) => Partial,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenTotals {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub cache_write_tokens: Option<i64>,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub uncached_input_tokens: Option<i64>,
    pub other_output_tokens: i64,
    pub cache_hit_rate: Option<f64>,
    pub estimated_cost_nanos_usd: Option<i64>,
    pub(crate) cost_completeness: CostCompleteness,
}

impl TokenTotals {
    fn zero() -> Self {
        Self {
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
        }
    }

    fn add_assign(&mut self, other: &Self) -> Result<(), AggregateError> {
        self.input_tokens = checked_add(self.input_tokens, other.input_tokens)?;
        self.cached_tokens = checked_add(self.cached_tokens, other.cached_tokens)?;
        self.output_tokens = checked_add(self.output_tokens, other.output_tokens)?;
        self.reasoning_tokens = checked_add(self.reasoning_tokens, other.reasoning_tokens)?;
        self.total_tokens = checked_add(self.total_tokens, other.total_tokens)?;
        self.cache_write_tokens = match (self.cache_write_tokens, other.cache_write_tokens) {
            (Some(left), Some(right)) => Some(checked_add(left, right)?),
            _ => None,
        };
        let cost_completeness = self.cost_completeness.merge(other.cost_completeness);
        let left_cost = matches!(
            self.cost_completeness,
            CostCompleteness::Complete | CostCompleteness::Partial
        )
        .then_some(self.estimated_cost_nanos_usd)
        .flatten();
        let right_cost = matches!(
            other.cost_completeness,
            CostCompleteness::Complete | CostCompleteness::Partial
        )
        .then_some(other.estimated_cost_nanos_usd)
        .flatten();
        self.estimated_cost_nanos_usd = match (left_cost, right_cost) {
            (Some(left), Some(right)) => Some(checked_add(left, right)?),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => (self.cost_completeness == CostCompleteness::Empty
                && other.cost_completeness == CostCompleteness::Empty)
                .then_some(0),
        };
        self.cost_completeness = cost_completeness;
        self.recompute_derived()
    }

    fn recompute_derived(&mut self) -> Result<(), AggregateError> {
        self.total_tokens = checked_add(self.input_tokens, self.output_tokens)?;
        self.other_output_tokens = self
            .output_tokens
            .checked_sub(self.reasoning_tokens)
            .ok_or(AggregateError::ArithmeticOverflow)?;
        self.uncached_input_tokens = self
            .cache_write_tokens
            .map(|write| {
                self.input_tokens
                    .checked_sub(self.cached_tokens)
                    .and_then(|value| value.checked_sub(write))
                    .ok_or(AggregateError::ArithmeticOverflow)
            })
            .transpose()?;
        self.cache_hit_rate =
            (self.input_tokens > 0).then(|| self.cached_tokens as f64 / self.input_tokens as f64);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDataStatus {
    Complete,
    Incomplete,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHealthSummary {
    pub total_sessions: i64,
    pub complete_sessions: i64,
    pub incomplete_sessions: i64,
    pub error_sessions: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageSummary {
    pub totals: TokenTotals,
    /// Healthy sessions contributing usage events. Kept for the existing KPI.
    pub session_count: i64,
    pub cost_incomplete_session_count: i64,
    pub complete_session_cost_per_million_tokens: Option<f64>,
    pub health: SessionHealthSummary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionUsageRow {
    pub root_session_id: String,
    pub title: Option<String>,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub inclusive_usage: TokenTotals,
    pub self_usage: TokenTotals,
    pub subagent_usage: TokenTotals,
    pub subagent_count: i64,
    pub last_activity_at_ms: i64,
    pub models_used: Vec<String>,
    pub data_status: SessionDataStatus,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionUsagePage {
    pub rows: Vec<SessionUsageRow>,
    pub next: Option<SessionCursor>,
}

/// The complete, lightweight ordering information for one eligible root
/// session.  The full usage values remain on `SessionUsageRow`; this type is
/// intentionally limited to values needed by a client-side comparator.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSortIndexItem {
    pub root_session_id: String,
    pub last_activity_at_ms: i64,
    pub project_sort_key: Option<String>,
    pub model_sort_key: Option<String>,
    pub total_tokens: Option<i64>,
    pub combined_total_tokens: Option<i64>,
    pub combined_estimated_cost_nanos_usd: Option<i64>,
    pub cache_hit_rate: Option<f64>,
    pub data_status: SessionDataStatus,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSortField {
    LastActivity,
    Project,
    Model,
    TotalTokens,
    CombinedTotalTokens,
    CombinedEstimatedCost,
    CacheHitRate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSortOrder {
    Asc,
    Desc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionSnapshot {
    pub sort_index: Vec<SessionSortIndexItem>,
    pub rows: Vec<SessionUsageRow>,
}

#[derive(Clone, Debug)]
struct SessionSortAggregate {
    root_session_id: String,
    last_activity_at_ms: i64,
    project_name: Option<String>,
    project_path: Option<String>,
    self_usage: TokenTotals,
    inclusive_usage: TokenTotals,
    model_sort_key: Option<String>,
}

impl SessionSortAggregate {
    fn sort_index_item(&self) -> SessionSortIndexItem {
        SessionSortIndexItem {
            root_session_id: self.root_session_id.clone(),
            last_activity_at_ms: self.last_activity_at_ms,
            project_sort_key: self
                .project_name
                .clone()
                .or_else(|| self.project_path.clone()),
            model_sort_key: self.model_sort_key.clone(),
            total_tokens: Some(self.self_usage.total_tokens),
            combined_total_tokens: Some(self.inclusive_usage.total_tokens),
            combined_estimated_cost_nanos_usd: match self.inclusive_usage.cost_completeness {
                CostCompleteness::Complete | CostCompleteness::Partial => {
                    self.inclusive_usage.estimated_cost_nanos_usd
                }
                CostCompleteness::Empty | CostCompleteness::Unknown => None,
            },
            cache_hit_rate: self.inclusive_usage.cache_hit_rate,
            data_status: status_for_totals(&self.inclusive_usage),
            error_code: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionDetail {
    pub root_session_id: String,
    pub last_activity_at_ms: i64,
    pub main: MainSessionDetail,
    pub subagents: Vec<SubagentDetail>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MainSessionDetail {
    pub title: Option<String>,
    pub thread_id: String,
    pub root_session_id: String,
    pub models_used: Vec<String>,
    pub model_usage: Vec<MainModelUsage>,
    pub self_usage: TokenTotals,
    pub subagent_count: i64,
    pub inclusive_usage: TokenTotals,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MainModelUsage {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub usage: TokenTotals,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentDetail {
    pub thread_id: String,
    pub parent_thread_id: Option<String>,
    pub root_session_id: String,
    pub title: Option<String>,
    pub last_activity_at_ms: i64,
    pub model_usage: Vec<SubagentModelUsage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentModelUsage {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub last_activity_at_ms: i64,
    pub usage: TokenTotals,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelUsageRow {
    pub model: String,
    pub totals: TokenTotals,
    pub session_count: i64,
    pub first_activity_at_ms: i64,
    pub last_activity_at_ms: i64,
}

pub type ModelUsageRows = Vec<ModelUsageRow>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectFilterOption {
    Project {
        project_name: String,
        project_path: String,
    },
    Projectless,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFilterOption {
    pub model: String,
    pub provider: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterOptions {
    pub models: Vec<ModelFilterOption>,
    pub projects: Vec<ProjectFilterOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateError {
    InvalidRange,
    InvalidPage,
    InvalidCursor,
    InvalidSessionIds,
    QueryFailed,
    ArithmeticOverflow,
    InvariantViolation,
}

impl AggregateError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRange => "AGGREGATE_INVALID_RANGE",
            Self::InvalidPage => "AGGREGATE_INVALID_PAGE",
            Self::InvalidCursor => "AGGREGATE_INVALID_CURSOR",
            Self::InvalidSessionIds => "AGGREGATE_INVALID_SESSION_IDS",
            Self::QueryFailed => "AGGREGATE_QUERY_FAILED",
            Self::ArithmeticOverflow => "AGGREGATE_OVERFLOW",
            Self::InvariantViolation => "AGGREGATE_INVARIANT_FAILED",
        }
    }
}

impl fmt::Display for AggregateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for AggregateError {}

pub struct AggregateReader<'connection> {
    connection: &'connection Connection,
}

impl<'connection> AggregateReader<'connection> {
    pub const fn new(connection: &'connection Connection) -> Self {
        Self { connection }
    }

    pub fn summary(&self, query: SummaryQuery) -> Result<UsageSummary, AggregateError> {
        let range = query.range();
        validate_range(range)?;
        let epoch = self.active_epoch()?;
        let (totals, values) = self.aggregate_for_summary(epoch, &query)?;
        let session_count: i64 = self
            .connection
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT ue.root_session_id)
                     FROM usage_events ue
                     LEFT JOIN threads root ON root.thread_id=ue.root_session_id
                     WHERE {}",
                    summary_where_clause(query.filter())
                ),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )
            .map_err(map_sql_error)?;
        let incomplete_sessions: i64 = self
            .connection
            .query_row(
                &format!(
                    "SELECT COUNT(DISTINCT ue.root_session_id)
                     FROM usage_events ue
                     LEFT JOIN threads root ON root.thread_id=ue.root_session_id
                     WHERE {} AND ue.estimated_cost_nanos_usd IS NULL",
                    summary_where_clause(query.filter())
                ),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )
            .map_err(map_sql_error)?;
        let complete_session_cost_per_million_tokens =
            self.complete_session_cost_per_million_tokens(&query, &values)?;
        let error_sessions =
            i64::try_from(self.quarantined_roots(epoch, range, query.filter())?.len())
                .map_err(|_| AggregateError::ArithmeticOverflow)?;
        let complete_sessions = session_count
            .checked_sub(incomplete_sessions)
            .ok_or(AggregateError::InvariantViolation)?;
        let total_sessions = session_count
            .checked_add(error_sessions)
            .ok_or(AggregateError::ArithmeticOverflow)?;
        let cost_incomplete_session_count = incomplete_sessions
            .checked_add(error_sessions)
            .ok_or(AggregateError::ArithmeticOverflow)?;
        if cost_incomplete_session_count < 0 || cost_incomplete_session_count > total_sessions {
            return Err(AggregateError::InvariantViolation);
        }
        Ok(UsageSummary {
            totals,
            session_count,
            cost_incomplete_session_count,
            complete_session_cost_per_million_tokens,
            health: SessionHealthSummary {
                total_sessions,
                complete_sessions,
                incomplete_sessions,
                error_sessions,
            },
        })
    }

    pub fn sessions(
        &self,
        range: TimeRange,
        page: SessionPageRequest,
    ) -> Result<SessionUsagePage, AggregateError> {
        validate_range(range)?;
        page.validate()?;
        let epoch = self.active_epoch()?;
        let limit = i64::try_from(page.limit).map_err(|_| AggregateError::InvalidPage)?;
        let (after_time, after_id) = page
            .after
            .as_ref()
            .map(|cursor| {
                (
                    Some(cursor.last_activity_at_ms),
                    Some(cursor.root_session_id.as_str()),
                )
            })
            .unwrap_or((None, None));
        let mut statement = self
            .connection
            .prepare(
                "WITH roots AS (
               SELECT root_session_id, MAX(occurred_at_ms) AS last_activity_at_ms
               FROM usage_events WHERE ledger_epoch=?1 AND occurred_at_ms>=?2 AND occurred_at_ms<?3
               GROUP BY root_session_id
             )
             SELECT roots.root_session_id, roots.last_activity_at_ms,
                    threads.title, threads.project_name, threads.project_path
             FROM roots LEFT JOIN threads ON threads.thread_id=roots.root_session_id
             WHERE (?4 IS NULL OR roots.last_activity_at_ms<?4
                    OR (roots.last_activity_at_ms=?4 AND roots.root_session_id>?5))
             ORDER BY roots.last_activity_at_ms DESC, roots.root_session_id ASC LIMIT ?6",
            )
            .map_err(map_sql_error)?;
        let rows = statement
            .query_map(
                params![
                    epoch,
                    range.start_ms,
                    range.end_ms,
                    after_time,
                    after_id,
                    limit.saturating_add(1)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        let has_next = rows.len() > page.limit;
        let mut output = Vec::with_capacity(page.limit);
        for (root_session_id, last_activity_at_ms, title, project_name, project_path) in
            rows.into_iter().take(page.limit)
        {
            let inclusive_usage = self.aggregate_for_root(epoch, range, &root_session_id, None)?;
            let self_usage = self.aggregate_for_root(epoch, range, &root_session_id, Some(true))?;
            let subagent_usage =
                self.aggregate_for_root(epoch, range, &root_session_id, Some(false))?;
            let subagent_count = self.connection.query_row(
                "SELECT COUNT(DISTINCT thread_id) FROM usage_events WHERE ledger_epoch=?1 AND root_session_id=?2 AND thread_id<>root_session_id AND occurred_at_ms>=?3 AND occurred_at_ms<?4",
                params![epoch, root_session_id, range.start_ms, range.end_ms], |row| row.get(0),
            ).map_err(map_sql_error)?;
            let models_used = self.models_for_root(epoch, range, &root_session_id)?;
            let data_status = status_for_totals(&inclusive_usage);
            output.push(SessionUsageRow {
                root_session_id,
                title,
                project_name,
                project_path,
                inclusive_usage,
                self_usage,
                subagent_usage,
                subagent_count,
                last_activity_at_ms,
                models_used,
                data_status,
                error_code: None,
            });
        }
        let next = has_next
            .then(|| {
                output.last().and_then(|row| {
                    SessionCursor::new(row.last_activity_at_ms, row.root_session_id.clone()).ok()
                })
            })
            .flatten();
        Ok(SessionUsagePage { rows: output, next })
    }

    fn session_sort_aggregates(
        &self,
        epoch: i64,
        range: TimeRange,
        roots: &[String],
    ) -> Result<Vec<SessionSortAggregate>, AggregateError> {
        let mut aggregates = roots
            .iter()
            .map(|root_session_id| {
                (
                    root_session_id.clone(),
                    SessionSortAggregate {
                        root_session_id: root_session_id.clone(),
                        last_activity_at_ms: 0,
                        project_name: None,
                        project_path: None,
                        self_usage: TokenTotals::zero(),
                        inclusive_usage: TokenTotals::zero(),
                        model_sort_key: None,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if roots.is_empty() {
            return Ok(Vec::new());
        }

        let metadata_placeholders = (1..1 + roots.len())
            .map(|value| format!("?{value}"))
            .collect::<Vec<_>>()
            .join(",");
        let root_placeholders = (4..4 + roots.len())
            .map(|value| format!("?{value}"))
            .collect::<Vec<_>>()
            .join(",");
        let mut values = vec![
            Value::Integer(epoch),
            Value::Integer(range.start_ms),
            Value::Integer(range.end_ms),
        ];
        values.extend(roots.iter().cloned().map(Value::Text));

        let metadata_sql = format!(
            "SELECT thread_id, project_name, project_path
             FROM threads WHERE thread_id IN ({metadata_placeholders})"
        );
        let mut metadata_statement = self
            .connection
            .prepare(&metadata_sql)
            .map_err(map_sql_error)?;
        let metadata = metadata_statement
            .query_map(params_from_iter(values[3..].iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        for (root_session_id, project_name, project_path) in metadata {
            if let Some(aggregate) = aggregates.get_mut(&root_session_id) {
                aggregate.project_name = project_name;
                aggregate.project_path = project_path;
            }
        }

        let usage_sql = format!(
            "SELECT root_session_id, thread_id, MAX(occurred_at_ms),
                    COALESCE(SUM(input_tokens),0), COALESCE(SUM(cached_tokens),0),
                    SUM(cache_write_tokens), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(total_tokens),0),
                    COALESCE(SUM(CASE WHEN cache_write_tokens IS NULL THEN 1 ELSE 0 END),0),
                    SUM(estimated_cost_nanos_usd),
                    COALESCE(SUM(CASE WHEN estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),0),
                    COUNT(*)
             FROM usage_events
             WHERE ledger_epoch=?1 AND occurred_at_ms>=?2 AND occurred_at_ms<?3
               AND root_session_id IN ({root_placeholders})
             GROUP BY root_session_id, thread_id"
        );
        let mut usage_statement = self.connection.prepare(&usage_sql).map_err(map_sql_error)?;
        let usage_groups = usage_statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    AggregateRow {
                        input_tokens: row.get(3)?,
                        cached_tokens: row.get(4)?,
                        cache_write_tokens: row.get(5)?,
                        output_tokens: row.get(6)?,
                        reasoning_tokens: row.get(7)?,
                        total_tokens: row.get(8)?,
                        unknown_count: row.get(9)?,
                        estimated_cost_nanos_usd: row.get(10)?,
                        cost_unknown_count: row.get(11)?,
                        event_count: row.get(12)?,
                    },
                ))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        for (root_session_id, thread_id, last_activity_at_ms, row) in usage_groups {
            let Some(aggregate) = aggregates.get_mut(&root_session_id) else {
                continue;
            };
            let totals = row.into_totals()?;
            aggregate.last_activity_at_ms = aggregate.last_activity_at_ms.max(last_activity_at_ms);
            aggregate.inclusive_usage.add_assign(&totals)?;
            if thread_id == root_session_id {
                aggregate.self_usage.add_assign(&totals)?;
            }
        }

        let model_sql = format!(
            "SELECT root_session_id, model
             FROM usage_events
             WHERE ledger_epoch=?1 AND occurred_at_ms>=?2 AND occurred_at_ms<?3
               AND root_session_id IN ({root_placeholders})
             GROUP BY root_session_id, model
             ORDER BY root_session_id ASC, MIN(occurred_at_ms) ASC,
                      MIN(event_id) ASC, model ASC"
        );
        let mut model_statement = self.connection.prepare(&model_sql).map_err(map_sql_error)?;
        let models = model_statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        for (root_session_id, model) in models {
            if let Some(aggregate) = aggregates.get_mut(&root_session_id)
                && aggregate.model_sort_key.is_none()
            {
                aggregate.model_sort_key = Some(model);
            }
        }
        Ok(aggregates.into_values().collect())
    }

    /// Read the complete Session snapshot for one scope.  Eligibility is
    /// evaluated separately from row aggregation: model filters decide which
    /// roots qualify, while each returned row always aggregates every model
    /// in the requested range.
    pub fn session_snapshot(
        &self,
        range: TimeRange,
        filter: &UsageFilter,
        seed_sort_field: SessionSortField,
        seed_sort_order: SessionSortOrder,
    ) -> Result<SessionSnapshot, AggregateError> {
        validate_range(range)?;
        let epoch = self.active_epoch()?;
        let roots = self.eligible_roots(epoch, range, filter)?;
        let aggregates = self.session_sort_aggregates(epoch, range, &roots)?;
        let mut sort_index = aggregates
            .iter()
            .map(|aggregate| aggregate.sort_index_item())
            .collect::<Vec<_>>();
        let quarantined = self.quarantined_roots(epoch, range, filter)?;
        sort_index.extend(quarantined.iter().map(QuarantinedRoot::sort_index_item));
        let mut seed_index = sort_index.clone();
        seed_index.sort_by(|left, right| {
            compare_sort_index_items(left, right, seed_sort_field, seed_sort_order)
        });
        seed_index.truncate(MAX_SESSION_ROWS);
        let error_roots = quarantined
            .into_iter()
            .map(|root| (root.root_session_id.clone(), root))
            .collect::<BTreeMap<_, _>>();
        let seed_rows = seed_index
            .iter()
            .map(|item| match error_roots.get(&item.root_session_id) {
                Some(root) => Ok(root.session_row()),
                None => self.session_row_for_root(epoch, range, &item.root_session_id),
            })
            .collect::<Result<Vec<_>, _>>()?;
        sort_index.sort_by(|left, right| left.root_session_id.cmp(&right.root_session_id));
        Ok(SessionSnapshot {
            sort_index,
            rows: seed_rows,
        })
    }

    /// Read a bounded batch of complete rows.  Every requested root must be
    /// eligible in the same scope; IDs are returned in caller order.
    pub fn session_rows(
        &self,
        range: TimeRange,
        filter: &UsageFilter,
        root_session_ids: &[String],
    ) -> Result<Vec<SessionUsageRow>, AggregateError> {
        validate_range(range)?;
        if root_session_ids.is_empty() || root_session_ids.len() > MAX_SESSION_ROWS {
            return Err(AggregateError::InvalidSessionIds);
        }
        if root_session_ids
            .iter()
            .any(|id| id.is_empty() || id.chars().any(char::is_control))
        {
            return Err(AggregateError::InvalidSessionIds);
        }
        let epoch = self.active_epoch()?;
        let mut eligible = self
            .eligible_roots(epoch, range, filter)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let error_roots = self
            .quarantined_roots(epoch, range, filter)?
            .into_iter()
            .map(|root| {
                eligible.insert(root.root_session_id.clone());
                (root.root_session_id.clone(), root)
            })
            .collect::<BTreeMap<_, _>>();
        if root_session_ids.iter().any(|id| !eligible.contains(id)) {
            return Err(AggregateError::InvalidSessionIds);
        }
        root_session_ids
            .iter()
            .map(|root| match error_roots.get(root) {
                Some(error) => Ok(error.session_row()),
                None => self.session_row_for_root(epoch, range, root),
            })
            .collect()
    }

    /// Detail aggregation is deliberately performed from one grouped usage
    /// query. Rust then partitions those groups into Main model blocks and
    /// per-model/effort blocks within each Subagent, avoiding per-thread/model SQL.
    pub fn session_detail(
        &self,
        range: TimeRange,
        filter: &UsageFilter,
        root_session_id: &str,
    ) -> Result<SessionDetail, AggregateError> {
        validate_range(range)?;
        if root_session_id.is_empty() || root_session_id.chars().any(char::is_control) {
            return Err(AggregateError::InvalidSessionIds);
        }
        let epoch = self.active_epoch()?;
        let eligible = self.eligible_roots(epoch, range, filter)?;
        if !eligible.iter().any(|id| id == root_session_id) {
            return Err(AggregateError::InvalidSessionIds);
        }

        let mut statement = self
            .connection
            .prepare(
                "SELECT thread_id, model, MAX(occurred_at_ms),
                        COALESCE(SUM(input_tokens),0),
                        COALESCE(SUM(cached_tokens),0), SUM(cache_write_tokens),
                        COALESCE(SUM(output_tokens),0), COALESCE(SUM(reasoning_tokens),0),
                        COALESCE(SUM(total_tokens),0),
                        COALESCE(SUM(CASE WHEN cache_write_tokens IS NULL THEN 1 ELSE 0 END),0),
                        SUM(estimated_cost_nanos_usd),
                        COALESCE(SUM(CASE WHEN estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),0),
                        COUNT(*), reasoning_effort,
                        MAX(file_generation), MAX(source_file_id), MAX(source_end_offset)
                 FROM usage_events
                 WHERE ledger_epoch=?1 AND root_session_id=?2
                   AND occurred_at_ms>=?3 AND occurred_at_ms<?4
                 GROUP BY thread_id, model, reasoning_effort
                 ORDER BY thread_id ASC, MIN(occurred_at_ms) ASC,
                          MIN(event_id) ASC, model ASC, reasoning_effort ASC",
            )
            .map_err(map_sql_error)?;
        let mut groups = statement
            .query_map(
                params![epoch, root_session_id, range.start_ms, range.end_ms],
                detail_row,
            )
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        if groups.is_empty() {
            return Err(AggregateError::InvalidSessionIds);
        }
        for group in &mut groups {
            group.totals.recompute_derived()?;
        }

        let mut metadata = self
            .connection
            .prepare(
                "SELECT thread_id, parent_thread_id, title
                 FROM threads WHERE root_session_id=?1 OR thread_id=?1",
            )
            .map_err(map_sql_error)?;
        let metadata = metadata
            .query_map(params![root_session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        let metadata = metadata
            .into_iter()
            .map(|(thread_id, parent, title)| (thread_id, (parent, title)))
            .collect::<BTreeMap<_, _>>();

        let mut by_thread = BTreeMap::<String, Vec<DetailAggregateRow>>::new();
        for group in groups {
            by_thread
                .entry(group.thread_id.clone())
                .or_default()
                .push(group);
        }
        let main_groups = by_thread.remove(root_session_id).unwrap_or_default();
        let main_models = main_groups
            .iter()
            .map(|group| MainModelUsage {
                model: group.model.clone(),
                reasoning_effort: group.reasoning_effort.clone(),
                usage: group.totals.clone(),
            })
            .collect::<Vec<_>>();
        let mut main_usage = TokenTotals::zero();
        for group in &main_groups {
            main_usage.add_assign(&group.totals)?;
        }

        let mut inclusive_usage = main_usage.clone();
        let mut subagents = Vec::new();
        for (thread_id, mut groups) in by_thread {
            groups.sort_by(|left, right| {
                right
                    .last_activity_at_ms
                    .cmp(&left.last_activity_at_ms)
                    .then_with(|| right.file_generation.cmp(&left.file_generation))
                    .then_with(|| right.source_file_id.cmp(&left.source_file_id))
                    .then_with(|| right.source_end_offset.cmp(&left.source_end_offset))
                    .then_with(|| left.model.cmp(&right.model))
                    .then_with(|| match (&left.reasoning_effort, &right.reasoning_effort) {
                        (None, None) => std::cmp::Ordering::Equal,
                        (None, Some(_)) => std::cmp::Ordering::Less,
                        (Some(_), None) => std::cmp::Ordering::Greater,
                        (Some(left), Some(right)) => left.cmp(right),
                    })
            });
            let model_usage = groups
                .iter()
                .map(|group| SubagentModelUsage {
                    model: group.model.clone(),
                    reasoning_effort: group.reasoning_effort.clone(),
                    last_activity_at_ms: group.last_activity_at_ms,
                    usage: group.totals.clone(),
                })
                .collect::<Vec<_>>();
            let mut usage = TokenTotals::zero();
            for block in &model_usage {
                usage.add_assign(&block.usage)?;
            }
            inclusive_usage.add_assign(&usage)?;
            let (parent_thread_id, title) =
                metadata.get(&thread_id).cloned().unwrap_or((None, None));
            subagents.push(SubagentDetail {
                thread_id,
                parent_thread_id,
                root_session_id: root_session_id.to_owned(),
                title,
                last_activity_at_ms: model_usage
                    .first()
                    .map(|block| block.last_activity_at_ms)
                    .ok_or(AggregateError::InvariantViolation)?,
                model_usage,
            });
        }
        subagents.sort_by(|left, right| {
            right
                .last_activity_at_ms
                .cmp(&left.last_activity_at_ms)
                .then_with(|| left.thread_id.cmp(&right.thread_id))
        });
        let last_activity_at_ms = main_groups
            .iter()
            .map(|group| group.last_activity_at_ms)
            .chain(
                subagents
                    .iter()
                    .map(|subagent| subagent.last_activity_at_ms),
            )
            .max()
            .ok_or(AggregateError::InvariantViolation)?;
        let (title, _) = metadata
            .get(root_session_id)
            .cloned()
            .unwrap_or((None, None));
        Ok(SessionDetail {
            root_session_id: root_session_id.to_owned(),
            last_activity_at_ms,
            main: MainSessionDetail {
                title,
                thread_id: root_session_id.to_owned(),
                root_session_id: root_session_id.to_owned(),
                models_used: main_models.iter().fold(Vec::new(), |mut models, model| {
                    if !models.iter().any(|existing| existing == &model.model) {
                        models.push(model.model.clone());
                    }
                    models
                }),
                model_usage: main_models,
                self_usage: main_usage,
                subagent_count: i64::try_from(subagents.len())
                    .map_err(|_| AggregateError::ArithmeticOverflow)?,
                inclusive_usage,
            },
            subagents,
        })
    }

    pub fn models(&self, range: TimeRange) -> Result<ModelUsageRows, AggregateError> {
        validate_range(range)?;
        let epoch = self.active_epoch()?;
        let mut statement = self.connection.prepare(
            "SELECT model, MIN(occurred_at_ms), MAX(occurred_at_ms), COUNT(DISTINCT root_session_id),
                    COALESCE(SUM(input_tokens),0), COALESCE(SUM(cached_tokens),0),
                    SUM(cache_write_tokens), COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(total_tokens),0),
                    COALESCE(SUM(CASE WHEN cache_write_tokens IS NULL THEN 1 ELSE 0 END),0),
                    SUM(estimated_cost_nanos_usd),
                    COALESCE(SUM(CASE WHEN estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),0),
                    COUNT(*)
             FROM usage_events WHERE ledger_epoch=?1 AND occurred_at_ms>=?2 AND occurred_at_ms<?3
             GROUP BY model ORDER BY model ASC",
        ).map_err(map_sql_error)?;
        statement
            .query_map(params![epoch, range.start_ms, range.end_ms], model_row)
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?
            .into_iter()
            .map(|row| {
                let model = row.model.clone();
                let session_count = row.session_count;
                let first_activity_at_ms = row.first_activity_at_ms;
                let last_activity_at_ms = row.last_activity_at_ms;
                Ok(ModelUsageRow {
                    model,
                    totals: row.into_totals()?,
                    session_count,
                    first_activity_at_ms,
                    last_activity_at_ms,
                })
            })
            .collect()
    }

    pub fn filter_options(&self) -> Result<FilterOptions, AggregateError> {
        let epoch = self.active_epoch()?;
        let mut models_statement = self
            .connection
            .prepare(
                "SELECT DISTINCT model
                 FROM usage_events
                 WHERE ledger_epoch=?1
                 ORDER BY model ASC",
            )
            .map_err(map_sql_error)?;
        let models = models_statement
            .query_map(params![epoch], |row| row.get(0))
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<String>>>()
            .map_err(map_sql_error)?
            .into_iter()
            .map(|model| {
                let provider = ModelRegistry::new().resolve(&model).provider;
                ModelFilterOption {
                    model,
                    provider: provider.as_str().to_owned(),
                }
            })
            .collect();

        let mut projects_statement = self
            .connection
            .prepare(
                "WITH usage_roots AS (
                   SELECT DISTINCT root_session_id
                   FROM usage_events
                   WHERE ledger_epoch=?1
                 )
                 SELECT root.project_kind, root.project_name, root.project_path
                 FROM usage_roots
                 LEFT JOIN threads root ON root.thread_id=usage_roots.root_session_id
                 ORDER BY CASE root.project_kind
                            WHEN 'project' THEN 0
                            WHEN 'projectless' THEN 1
                            WHEN 'unknown' THEN 2
                            ELSE 3
                          END,
                          root.project_path ASC,
                          root.project_name ASC",
            )
            .map_err(map_sql_error)?;
        let project_rows = projects_statement
            .query_map(params![epoch], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(map_sql_error)?;

        let mut projects = BTreeMap::new();
        let mut has_projectless = false;
        let mut has_unknown = false;
        for row in project_rows {
            let (kind, name, path) = row.map_err(map_sql_error)?;
            match kind.as_deref() {
                Some("project") => {
                    let Some(path) = path.filter(|path| Path::new(path).is_absolute()) else {
                        has_unknown = true;
                        continue;
                    };
                    let Some(name) = name else {
                        has_unknown = true;
                        continue;
                    };
                    projects.entry(path).or_insert(name);
                }
                Some("projectless") => has_projectless = true,
                _ => has_unknown = true,
            }
        }

        let mut options = projects
            .into_iter()
            .map(
                |(project_path, project_name)| ProjectFilterOption::Project {
                    project_name,
                    project_path,
                },
            )
            .collect::<Vec<_>>();
        if has_projectless {
            options.push(ProjectFilterOption::Projectless);
        }
        if has_unknown {
            options.push(ProjectFilterOption::Unknown);
        }
        Ok(FilterOptions {
            models,
            projects: options,
        })
    }

    pub fn verify_invariants(&self, range: TimeRange) -> Result<(), AggregateError> {
        let summary = self.summary(SummaryQuery::new(range, UsageFilter::default()))?;
        let mut model_totals = TokenTotals::zero();
        for model in self.models(range)? {
            model_totals.add_assign(&model.totals)?;
        }
        if !same_totals(&summary.totals, &model_totals) {
            return Err(AggregateError::InvariantViolation);
        }
        let mut session_totals = TokenTotals::zero();
        let mut count = 0_i64;
        let mut cursor = None;
        loop {
            let page = self.sessions(
                range,
                SessionPageRequest {
                    limit: MAX_SESSION_PAGE_SIZE,
                    after: cursor,
                },
            )?;
            count = count
                .checked_add(page.rows.len() as i64)
                .ok_or(AggregateError::ArithmeticOverflow)?;
            for row in &page.rows {
                session_totals.add_assign(&row.inclusive_usage)?;
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        if count != summary.session_count || !same_totals(&summary.totals, &session_totals) {
            return Err(AggregateError::InvariantViolation);
        }
        Ok(())
    }

    fn active_epoch(&self) -> Result<i64, AggregateError> {
        self.connection
            .query_row(
                "SELECT usage_active_epoch FROM app_meta WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(map_sql_error)
    }

    fn eligible_roots(
        &self,
        epoch: i64,
        range: TimeRange,
        filter: &UsageFilter,
    ) -> Result<Vec<String>, AggregateError> {
        let mut clauses = vec![
            "root.agent_role='main'".to_owned(),
            "root.root_session_id=root.thread_id".to_owned(),
            "root.parent_thread_id IS NULL".to_owned(),
            "EXISTS (SELECT 1 FROM usage_events ue_any
                     WHERE ue_any.ledger_epoch=?1
                       AND ue_any.root_session_id=root.thread_id
                       AND ue_any.occurred_at_ms>=?2
                       AND ue_any.occurred_at_ms<?3)"
                .to_owned(),
        ];
        let mut values = vec![
            Value::Integer(epoch),
            Value::Integer(range.start_ms),
            Value::Integer(range.end_ms),
        ];
        let mut next = 4_usize;
        if !filter.models.is_empty() {
            let placeholders = (next..next + filter.models.len())
                .map(|value| format!("?{value}"))
                .collect::<Vec<_>>()
                .join(",");
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM usage_events ue_model
                         WHERE ue_model.ledger_epoch=?1
                           AND ue_model.root_session_id=root.thread_id
                           AND ue_model.occurred_at_ms>=?2
                           AND ue_model.occurred_at_ms<?3
                           AND ue_model.model IN ({placeholders}))"
            ));
            values.extend(filter.models.iter().cloned().map(Value::Text));
            next += filter.models.len();
        }
        let mut project_clauses = Vec::new();
        if !filter.project_paths.is_empty() {
            let placeholders = (next..next + filter.project_paths.len())
                .map(|value| format!("?{value}"))
                .collect::<Vec<_>>()
                .join(",");
            project_clauses.push(format!(
                "(root.project_kind='project' AND root.project_path IN ({placeholders}))"
            ));
            values.extend(filter.project_paths.iter().cloned().map(Value::Text));
        }
        if filter.include_projectless {
            project_clauses.push("root.project_kind='projectless'".to_owned());
        }
        if filter.include_unknown_project {
            project_clauses.push("root.project_kind='unknown'".to_owned());
        }
        if !project_clauses.is_empty() {
            clauses.push(format!("({})", project_clauses.join(" OR ")));
        }
        let sql = format!(
            "SELECT root.thread_id FROM threads root WHERE {} ORDER BY root.thread_id ASC",
            clauses.join(" AND ")
        );
        let mut statement = self.connection.prepare(&sql).map_err(map_sql_error)?;
        statement
            .query_map(params_from_iter(values.iter()), |row| row.get(0))
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<String>>>()
            .map_err(map_sql_error)
    }

    fn session_row_for_root(
        &self,
        epoch: i64,
        range: TimeRange,
        root: &str,
    ) -> Result<SessionUsageRow, AggregateError> {
        let (title, project_name, project_path, last_activity_at_ms) = self
            .connection
            .query_row(
                "SELECT root.title, root.project_name, root.project_path,
                        MAX(ue.occurred_at_ms)
                 FROM threads root
                 JOIN usage_events ue ON ue.root_session_id=root.thread_id
                 WHERE root.thread_id=?1 AND ue.ledger_epoch=?2
                   AND ue.occurred_at_ms>=?3 AND ue.occurred_at_ms<?4
                 GROUP BY root.thread_id, root.title, root.project_name, root.project_path",
                params![root, epoch, range.start_ms, range.end_ms],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(map_sql_error)?;
        let inclusive_usage = self.aggregate_for_root(epoch, range, root, None)?;
        let self_usage = self.aggregate_for_root(epoch, range, root, Some(true))?;
        let subagent_usage = self.aggregate_for_root(epoch, range, root, Some(false))?;
        let subagent_count = self
            .connection
            .query_row(
                "SELECT COUNT(DISTINCT thread_id) FROM usage_events
                 WHERE ledger_epoch=?1 AND root_session_id=?2 AND thread_id<>root_session_id
                   AND occurred_at_ms>=?3 AND occurred_at_ms<?4",
                params![epoch, root, range.start_ms, range.end_ms],
                |row| row.get(0),
            )
            .map_err(map_sql_error)?;
        let models_used = self.models_for_root(epoch, range, root)?;
        let data_status = status_for_totals(&inclusive_usage);
        Ok(SessionUsageRow {
            root_session_id: root.to_owned(),
            title,
            project_name,
            project_path,
            inclusive_usage,
            self_usage,
            subagent_usage,
            subagent_count,
            last_activity_at_ms,
            models_used,
            data_status,
            error_code: None,
        })
    }

    fn quarantined_roots(
        &self,
        epoch: i64,
        range: TimeRange,
        filter: &UsageFilter,
    ) -> Result<Vec<QuarantinedRoot>, AggregateError> {
        // A quarantined Session has no trustworthy usage/model ledger. Never
        // guess model-filter membership; under an active model filter it is
        // intentionally omitted from the scoped denominator/list.
        if !filter.models.is_empty() {
            return Ok(Vec::new());
        }
        let quarantine_count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM usage_session_quarantine WHERE ledger_epoch=?1",
                [epoch],
                |row| row.get(0),
            )
            .map_err(map_sql_error)?;
        if quarantine_count == 0 {
            return Ok(Vec::new());
        }
        let mut clauses = vec![
            "q.ledger_epoch=?1".to_owned(),
            "q.last_activity_at_ms>=?2".to_owned(),
            "q.last_activity_at_ms<?3".to_owned(),
        ];
        let mut values = vec![
            Value::Integer(epoch),
            Value::Integer(range.start_ms),
            Value::Integer(range.end_ms),
        ];
        let next = 4_usize;
        let mut projects = Vec::new();
        if !filter.project_paths.is_empty() {
            let placeholders = (next..next + filter.project_paths.len())
                .map(|value| format!("?{value}"))
                .collect::<Vec<_>>()
                .join(",");
            projects.push(format!(
                "(root.project_kind='project' AND root.project_path IN ({placeholders}))"
            ));
            values.extend(filter.project_paths.iter().cloned().map(Value::Text));
        }
        if filter.include_projectless {
            projects.push("root.project_kind='projectless'".to_owned());
        }
        if filter.include_unknown_project {
            projects.push("root.project_kind='unknown'".to_owned());
        }
        if !projects.is_empty() {
            clauses.push(format!("({})", projects.join(" OR ")));
        }
        let sql = format!(
            "SELECT q.root_session_id,q.primary_error_code,q.last_activity_at_ms,
                    root.title,root.project_name,root.project_path,
                    (SELECT COUNT(*) FROM threads child
                     WHERE child.root_session_id=q.root_session_id
                       AND child.thread_id<>q.root_session_id)
             FROM usage_session_quarantine q
             JOIN threads root ON root.thread_id=q.root_session_id
             WHERE {} ORDER BY q.root_session_id",
            clauses.join(" AND ")
        );
        let mut statement = self.connection.prepare(&sql).map_err(map_sql_error)?;
        statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(QuarantinedRoot {
                    root_session_id: row.get(0)?,
                    error_code: row.get(1)?,
                    last_activity_at_ms: row.get(2)?,
                    title: row.get(3)?,
                    project_name: row.get(4)?,
                    project_path: row.get(5)?,
                    subagent_count: row.get(6)?,
                })
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)
    }

    fn aggregate_for_root(
        &self,
        epoch: i64,
        range: TimeRange,
        root: &str,
        self_only: Option<bool>,
    ) -> Result<TokenTotals, AggregateError> {
        let predicate = match self_only {
            None => "root_session_id=?4",
            Some(true) => "root_session_id=?4 AND thread_id=root_session_id",
            Some(false) => "root_session_id=?4 AND thread_id<>root_session_id",
        };
        self.aggregate_for(epoch, range, predicate, &[root])
    }

    fn aggregate_for(
        &self,
        epoch: i64,
        range: TimeRange,
        predicate: &str,
        extra: &[&str],
    ) -> Result<TokenTotals, AggregateError> {
        let sql = format!(
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(cached_tokens),0), SUM(cache_write_tokens),
                    COALESCE(SUM(output_tokens),0), COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(total_tokens),0),
                    COALESCE(SUM(CASE WHEN cache_write_tokens IS NULL THEN 1 ELSE 0 END),0),
                    SUM(estimated_cost_nanos_usd),
                    COALESCE(SUM(CASE WHEN estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),0),
                    COUNT(*)
             FROM usage_events WHERE ledger_epoch=?1 AND occurred_at_ms>=?2 AND occurred_at_ms<?3 AND {predicate}"
        );
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&epoch, &range.start_ms, &range.end_ms];
        values.extend(extra.iter().map(|value| value as &dyn rusqlite::ToSql));
        let row = self
            .connection
            .query_row(&sql, rusqlite::params_from_iter(values), aggregate_row)
            .map_err(map_sql_error)?;
        row.into_totals()
    }

    fn complete_session_cost_per_million_tokens(
        &self,
        query: &SummaryQuery,
        values: &[Value],
    ) -> Result<Option<f64>, AggregateError> {
        let sql = format!(
            "SELECT SUM(session_cost_nanos_usd), SUM(session_tokens)
             FROM (
                 SELECT ue.root_session_id,
                        SUM(ue.estimated_cost_nanos_usd) AS session_cost_nanos_usd,
                        SUM(ue.total_tokens) AS session_tokens
                 FROM usage_events ue
                 LEFT JOIN threads root ON root.thread_id=ue.root_session_id
                 WHERE {}
                 GROUP BY ue.root_session_id
                 HAVING SUM(CASE WHEN ue.estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END)=0
             ) priced_sessions",
            summary_where_clause(query.filter())
        );
        let (cost_nanos, total_tokens): (Option<i64>, Option<i64>) = self
            .connection
            .query_row(&sql, params_from_iter(values.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(map_sql_error)?;
        complete_pricing_cost_per_million_tokens(cost_nanos, total_tokens)
    }

    fn aggregate_for_summary(
        &self,
        epoch: i64,
        query: &SummaryQuery,
    ) -> Result<(TokenTotals, Vec<Value>), AggregateError> {
        let values = summary_values(epoch, query);
        let sql = format!(
            "SELECT COALESCE(SUM(ue.input_tokens),0), COALESCE(SUM(ue.cached_tokens),0), SUM(ue.cache_write_tokens),
                    COALESCE(SUM(ue.output_tokens),0), COALESCE(SUM(ue.reasoning_tokens),0), COALESCE(SUM(ue.total_tokens),0),
                    COALESCE(SUM(CASE WHEN ue.cache_write_tokens IS NULL THEN 1 ELSE 0 END),0),
                    SUM(ue.estimated_cost_nanos_usd),
                    COALESCE(SUM(CASE WHEN ue.estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),0),
                    COUNT(*)
             FROM usage_events ue
             LEFT JOIN threads root ON root.thread_id=ue.root_session_id
             WHERE {}",
            summary_where_clause(query.filter())
        );
        let row = self
            .connection
            .query_row(&sql, params_from_iter(values.iter()), aggregate_row)
            .map_err(map_sql_error)?;
        Ok((row.into_totals()?, values))
    }

    fn models_for_root(
        &self,
        epoch: i64,
        range: TimeRange,
        root: &str,
    ) -> Result<Vec<String>, AggregateError> {
        let mut statement = self.connection.prepare("SELECT model FROM usage_events WHERE ledger_epoch=?1 AND root_session_id=?2 AND occurred_at_ms>=?3 AND occurred_at_ms<?4 GROUP BY model ORDER BY MIN(occurred_at_ms), MIN(event_id), model").map_err(map_sql_error)?;
        statement
            .query_map(params![epoch, root, range.start_ms, range.end_ms], |row| {
                row.get(0)
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)
    }
}

fn summary_values(epoch: i64, query: &SummaryQuery) -> Vec<Value> {
    let mut values = vec![
        Value::Integer(epoch),
        Value::Integer(query.range().start_ms),
        Value::Integer(query.range().end_ms),
    ];
    values.extend(query.filter().models.iter().cloned().map(Value::Text));
    values.extend(
        query
            .filter()
            .project_paths
            .iter()
            .cloned()
            .map(Value::Text),
    );
    values
}

fn summary_where_clause(filter: &UsageFilter) -> String {
    let mut clauses = vec![
        "ue.ledger_epoch=?1".to_owned(),
        "ue.occurred_at_ms>=?2".to_owned(),
        "ue.occurred_at_ms<?3".to_owned(),
    ];
    let mut next_placeholder = 4_usize;
    if !filter.models.is_empty() {
        let placeholders = (next_placeholder..next_placeholder + filter.models.len())
            .map(|value| format!("?{value}"))
            .collect::<Vec<_>>()
            .join(",");
        clauses.push(format!("ue.model IN ({placeholders})"));
        next_placeholder += filter.models.len();
    }

    let mut project_clauses = Vec::new();
    if !filter.project_paths.is_empty() {
        let placeholders = (next_placeholder..next_placeholder + filter.project_paths.len())
            .map(|value| format!("?{value}"))
            .collect::<Vec<_>>()
            .join(",");
        project_clauses.push(format!(
            "(root.project_kind='project' AND root.project_path IN ({placeholders}))"
        ));
    }
    if filter.include_projectless {
        project_clauses.push("root.project_kind='projectless'".to_owned());
    }
    if filter.include_unknown_project {
        project_clauses.push("(root.project_kind='unknown' OR root.thread_id IS NULL)".to_owned());
    }
    if !project_clauses.is_empty() {
        clauses.push(format!("({})", project_clauses.join(" OR ")));
    }
    clauses.join(" AND ")
}

fn validate_range(range: TimeRange) -> Result<(), AggregateError> {
    (range.start_ms >= 0 && range.end_ms >= 0 && range.start_ms <= range.end_ms)
        .then_some(())
        .ok_or(AggregateError::InvalidRange)
}

fn map_sql_error(error: rusqlite::Error) -> AggregateError {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.to_ascii_lowercase().contains("integer overflow") =>
        {
            AggregateError::ArithmeticOverflow
        }
        _ => AggregateError::QueryFailed,
    }
}

fn checked_add(left: i64, right: i64) -> Result<i64, AggregateError> {
    left.checked_add(right)
        .ok_or(AggregateError::ArithmeticOverflow)
}

fn same_totals(left: &TokenTotals, right: &TokenTotals) -> bool {
    left.input_tokens == right.input_tokens
        && left.cached_tokens == right.cached_tokens
        && left.cache_write_tokens == right.cache_write_tokens
        && left.output_tokens == right.output_tokens
        && left.reasoning_tokens == right.reasoning_tokens
        && left.total_tokens == right.total_tokens
        && left.uncached_input_tokens == right.uncached_input_tokens
        && left.other_output_tokens == right.other_output_tokens
        && left.cache_hit_rate == right.cache_hit_rate
        && left.estimated_cost_nanos_usd == right.estimated_cost_nanos_usd
        && left.cost_completeness == right.cost_completeness
}

#[derive(Clone, Debug)]
struct QuarantinedRoot {
    root_session_id: String,
    error_code: String,
    last_activity_at_ms: i64,
    title: Option<String>,
    project_name: Option<String>,
    project_path: Option<String>,
    subagent_count: i64,
}

impl QuarantinedRoot {
    fn sort_index_item(&self) -> SessionSortIndexItem {
        SessionSortIndexItem {
            root_session_id: self.root_session_id.clone(),
            last_activity_at_ms: self.last_activity_at_ms,
            project_sort_key: self
                .project_name
                .clone()
                .or_else(|| self.project_path.clone()),
            model_sort_key: None,
            total_tokens: None,
            combined_total_tokens: None,
            combined_estimated_cost_nanos_usd: None,
            cache_hit_rate: None,
            data_status: SessionDataStatus::Error,
            error_code: Some(self.error_code.clone()),
        }
    }

    fn session_row(&self) -> SessionUsageRow {
        SessionUsageRow {
            root_session_id: self.root_session_id.clone(),
            title: self.title.clone(),
            project_name: self.project_name.clone(),
            project_path: self.project_path.clone(),
            inclusive_usage: TokenTotals::zero(),
            self_usage: TokenTotals::zero(),
            subagent_usage: TokenTotals::zero(),
            subagent_count: self.subagent_count,
            last_activity_at_ms: self.last_activity_at_ms,
            models_used: Vec::new(),
            data_status: SessionDataStatus::Error,
            error_code: Some(self.error_code.clone()),
        }
    }
}

fn status_for_totals(totals: &TokenTotals) -> SessionDataStatus {
    match totals.cost_completeness {
        CostCompleteness::Partial | CostCompleteness::Unknown => SessionDataStatus::Incomplete,
        CostCompleteness::Empty | CostCompleteness::Complete => SessionDataStatus::Complete,
    }
}

fn compare_sort_index_items(
    left: &SessionSortIndexItem,
    right: &SessionSortIndexItem,
    field: SessionSortField,
    order: SessionSortOrder,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let text = |left: Option<&str>, right: Option<&str>| match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match order {
            SessionSortOrder::Asc => left.cmp(right),
            SessionSortOrder::Desc => right.cmp(left),
        },
    };
    let number = |left: Option<i64>, right: Option<i64>| match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match order {
            SessionSortOrder::Asc => left.cmp(&right),
            SessionSortOrder::Desc => right.cmp(&left),
        },
    };
    let ratio = |left: Option<f64>, right: Option<f64>| match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match order {
            SessionSortOrder::Asc => left.total_cmp(&right),
            SessionSortOrder::Desc => right.total_cmp(&left),
        },
    };
    let result = match field {
        SessionSortField::LastActivity => number(
            Some(left.last_activity_at_ms),
            Some(right.last_activity_at_ms),
        ),
        SessionSortField::Project => text(
            left.project_sort_key.as_deref(),
            right.project_sort_key.as_deref(),
        ),
        SessionSortField::Model => text(
            left.model_sort_key.as_deref(),
            right.model_sort_key.as_deref(),
        ),
        SessionSortField::TotalTokens => number(left.total_tokens, right.total_tokens),
        SessionSortField::CombinedTotalTokens => {
            number(left.combined_total_tokens, right.combined_total_tokens)
        }
        SessionSortField::CombinedEstimatedCost => number(
            left.combined_estimated_cost_nanos_usd,
            right.combined_estimated_cost_nanos_usd,
        ),
        SessionSortField::CacheHitRate => ratio(left.cache_hit_rate, right.cache_hit_rate),
    };
    result.then_with(|| left.root_session_id.cmp(&right.root_session_id))
}

fn complete_pricing_cost_per_million_tokens(
    cost_nanos: Option<i64>,
    total_tokens: Option<i64>,
) -> Result<Option<f64>, AggregateError> {
    match (cost_nanos, total_tokens) {
        (None, None) => Ok(None),
        (Some(cost), Some(tokens)) if cost >= 0 && tokens > 0 => {
            Ok(Some(cost as f64 / tokens as f64 / 1_000.0))
        }
        (Some(cost), Some(0)) if cost >= 0 => Ok(None),
        _ => Err(AggregateError::InvariantViolation),
    }
}

struct AggregateRow {
    input_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: Option<i64>,
    output_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    unknown_count: i64,
    estimated_cost_nanos_usd: Option<i64>,
    cost_unknown_count: i64,
    event_count: i64,
}

fn cost_aggregate(
    sum: Option<i64>,
    cost_unknown_count: i64,
    event_count: i64,
) -> (Option<i64>, CostCompleteness) {
    if event_count == 0 {
        (Some(0), CostCompleteness::Empty)
    } else if cost_unknown_count == 0 {
        (sum, CostCompleteness::Complete)
    } else if cost_unknown_count == event_count {
        (None, CostCompleteness::Unknown)
    } else {
        (sum, CostCompleteness::Partial)
    }
}

#[derive(Clone, Debug)]
struct DetailAggregateRow {
    thread_id: String,
    model: String,
    reasoning_effort: Option<String>,
    last_activity_at_ms: i64,
    file_generation: i64,
    source_file_id: i64,
    source_end_offset: i64,
    totals: TokenTotals,
}

fn detail_row(row: &Row<'_>) -> rusqlite::Result<DetailAggregateRow> {
    let aggregate = AggregateRow {
        input_tokens: row.get(3)?,
        cached_tokens: row.get(4)?,
        cache_write_tokens: row.get(5)?,
        output_tokens: row.get(6)?,
        reasoning_tokens: row.get(7)?,
        total_tokens: row.get(8)?,
        unknown_count: row.get(9)?,
        estimated_cost_nanos_usd: row.get(10)?,
        cost_unknown_count: row.get(11)?,
        event_count: row.get(12)?,
    };
    // `into_totals` cannot surface its domain error through rusqlite's row
    // mapper.  The grouped usage query is constrained by storage invariants;
    // retain the checked conversion at the caller boundary instead.
    let (estimated_cost_nanos_usd, cost_completeness) = cost_aggregate(
        aggregate.estimated_cost_nanos_usd,
        aggregate.cost_unknown_count,
        aggregate.event_count,
    );
    let totals = TokenTotals {
        input_tokens: aggregate.input_tokens,
        cached_tokens: aggregate.cached_tokens,
        cache_write_tokens: if aggregate.unknown_count > 0 {
            None
        } else {
            aggregate.cache_write_tokens
        },
        output_tokens: aggregate.output_tokens,
        reasoning_tokens: aggregate.reasoning_tokens,
        total_tokens: aggregate.total_tokens,
        uncached_input_tokens: None,
        other_output_tokens: 0,
        cache_hit_rate: None,
        estimated_cost_nanos_usd,
        cost_completeness,
    };
    Ok(DetailAggregateRow {
        thread_id: row.get(0)?,
        model: row.get(1)?,
        reasoning_effort: row.get(13)?,
        last_activity_at_ms: row.get(2)?,
        file_generation: row.get(14)?,
        source_file_id: row.get(15)?,
        source_end_offset: row.get(16)?,
        totals,
    })
}

impl AggregateRow {
    fn into_totals(self) -> Result<TokenTotals, AggregateError> {
        let (estimated_cost_nanos_usd, cost_completeness) = cost_aggregate(
            self.estimated_cost_nanos_usd,
            self.cost_unknown_count,
            self.event_count,
        );
        let mut totals = TokenTotals {
            input_tokens: self.input_tokens,
            cached_tokens: self.cached_tokens,
            cache_write_tokens: if self.unknown_count > 0 {
                None
            } else {
                self.cache_write_tokens
            },
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
            total_tokens: self.total_tokens,
            uncached_input_tokens: None,
            other_output_tokens: 0,
            cache_hit_rate: None,
            estimated_cost_nanos_usd,
            cost_completeness,
        };
        if self.event_count == 0 {
            totals = TokenTotals::zero();
        }
        totals.recompute_derived()?;
        Ok(totals)
    }
}

fn aggregate_row(row: &Row<'_>) -> rusqlite::Result<AggregateRow> {
    Ok(AggregateRow {
        input_tokens: row.get(0)?,
        cached_tokens: row.get(1)?,
        cache_write_tokens: row.get(2)?,
        output_tokens: row.get(3)?,
        reasoning_tokens: row.get(4)?,
        total_tokens: row.get(5)?,
        unknown_count: row.get(6)?,
        estimated_cost_nanos_usd: row.get(7)?,
        cost_unknown_count: row.get(8)?,
        event_count: row.get(9)?,
    })
}

struct ModelAggregateRow {
    model: String,
    first_activity_at_ms: i64,
    last_activity_at_ms: i64,
    session_count: i64,
    input_tokens: i64,
    cached_tokens: i64,
    cache_write_tokens: Option<i64>,
    output_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    unknown_count: i64,
    estimated_cost_nanos_usd: Option<i64>,
    cost_unknown_count: i64,
    event_count: i64,
}

impl ModelAggregateRow {
    fn into_totals(self) -> Result<TokenTotals, AggregateError> {
        AggregateRow {
            input_tokens: self.input_tokens,
            cached_tokens: self.cached_tokens,
            cache_write_tokens: self.cache_write_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
            total_tokens: self.total_tokens,
            unknown_count: self.unknown_count,
            estimated_cost_nanos_usd: self.estimated_cost_nanos_usd,
            cost_unknown_count: self.cost_unknown_count,
            event_count: self.event_count,
        }
        .into_totals()
    }
}

fn model_row(row: &Row<'_>) -> rusqlite::Result<ModelAggregateRow> {
    Ok(ModelAggregateRow {
        model: row.get(0)?,
        first_activity_at_ms: row.get(1)?,
        last_activity_at_ms: row.get(2)?,
        session_count: row.get(3)?,
        input_tokens: row.get(4)?,
        cached_tokens: row.get(5)?,
        cache_write_tokens: row.get(6)?,
        output_tokens: row.get(7)?,
        reasoning_tokens: row.get(8)?,
        total_tokens: row.get(9)?,
        unknown_count: row.get(10)?,
        estimated_cost_nanos_usd: row.get(11)?,
        cost_unknown_count: row.get(12)?,
        event_count: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, TransactionBehavior, params};

    use super::*;

    fn fixture_path(name: &str) -> String {
        std::env::temp_dir()
            .join("usagi-usage-aggregate")
            .join(name.trim_start_matches('/'))
            .to_string_lossy()
            .into_owned()
    }

    fn fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        let project_a = fixture_path("project/a");
        let project_b = fixture_path("project/b");
        connection
            .execute_batch(&format!(
                "CREATE TABLE app_meta (id INTEGER PRIMARY KEY, usage_active_epoch INTEGER NOT NULL);
                 CREATE TABLE threads (
                    thread_id TEXT PRIMARY KEY, parent_thread_id TEXT, root_session_id TEXT,
                    agent_role TEXT NOT NULL DEFAULT 'main', title TEXT, project_name TEXT, project_path TEXT,
                    project_kind TEXT NOT NULL DEFAULT 'project'
                 );
                 CREATE TABLE IF NOT EXISTS usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL, root_session_id TEXT NOT NULL,
                    primary_error_code TEXT NOT NULL, last_activity_at_ms INTEGER NOT NULL,
                    first_seen_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE usage_events (
                    ledger_epoch INTEGER NOT NULL, event_id TEXT NOT NULL,
                    occurred_at_ms INTEGER NOT NULL, thread_id TEXT NOT NULL,
                    root_session_id TEXT NOT NULL, turn_key TEXT, model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER, output_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
                    reasoning_effort TEXT, estimated_cost_nanos_usd INTEGER,
                    source_file_id INTEGER, file_generation INTEGER,
                    source_start_offset INTEGER, source_end_offset INTEGER
                 );
                 CREATE TABLE IF NOT EXISTS usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL, root_session_id TEXT NOT NULL,
                    primary_error_code TEXT NOT NULL, last_activity_at_ms INTEGER NOT NULL,
                    first_seen_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
                 );
                 INSERT INTO app_meta(id, usage_active_epoch) VALUES (1, 7);
                 INSERT INTO threads(thread_id,parent_thread_id,root_session_id,agent_role,title,project_name,project_path) VALUES
                    ('root-a',NULL,'root-a','main','Root A','project-a','{project_a}'),
                    ('child-a','root-a','root-a','subagent','Child A','project-a','{project_a}'),
                    ('root-b',NULL,'root-b','main','Root B','project-b','{project_b}');"
            ))
            .unwrap();
        insert_event(
            &connection,
            "a-self",
            100,
            "root-a",
            "root-a",
            "gpt-a",
            1000,
            900,
            Some(50),
            100,
            20,
        );
        insert_event(
            &connection,
            "a-child",
            200,
            "child-a",
            "root-a",
            "gpt-b",
            9000,
            900,
            Some(1000),
            900,
            180,
        );
        insert_event(
            &connection,
            "a-unknown",
            250,
            "child-a",
            "root-a",
            "gpt-b",
            500,
            100,
            None,
            50,
            20,
        );
        insert_event(
            &connection,
            "b-self",
            150,
            "root-b",
            "root-b",
            "gpt-a",
            0,
            0,
            Some(0),
            0,
            0,
        );
        connection
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "preserve the established test fixture insertion seam"
    )]
    fn insert_event(
        connection: &Connection,
        event_id: &str,
        occurred_at_ms: i64,
        thread_id: &str,
        root_session_id: &str,
        model: &str,
        input_tokens: i64,
        cached_tokens: i64,
        cache_write_tokens: Option<i64>,
        output_tokens: i64,
        reasoning_tokens: i64,
    ) {
        connection
            .execute(
                "INSERT INTO usage_events(
                    ledger_epoch,event_id,occurred_at_ms,thread_id,root_session_id,model,
                    input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                    total_tokens,reasoning_effort,estimated_cost_nanos_usd,
                    source_file_id,file_generation,source_start_offset,source_end_offset
                 ) VALUES (7,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?6+?9,NULL,NULL,1,1,?2,?2+1)",
                params![
                    event_id,
                    occurred_at_ms,
                    thread_id,
                    root_session_id,
                    model,
                    input_tokens,
                    cached_tokens,
                    cache_write_tokens,
                    output_tokens,
                    reasoning_tokens,
                ],
            )
            .unwrap();
    }

    fn summary_cost_fixture(incomplete: bool, error: bool) -> Connection {
        let connection = fixture();
        connection
            .execute(
                "UPDATE usage_events
                 SET estimated_cost_nanos_usd=CASE event_id
                     WHEN 'a-self' THEN 10
                     WHEN 'a-child' THEN 20
                     WHEN 'a-unknown' THEN 30
                     WHEN 'b-self' THEN 40
                 END
                 WHERE event_id IN ('a-self','a-child','a-unknown','b-self')",
                [],
            )
            .unwrap();
        if incomplete {
            connection
                .execute(
                    "UPDATE usage_events
                     SET estimated_cost_nanos_usd=NULL
                     WHERE event_id='a-unknown'",
                    [],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO threads(
                    thread_id,parent_thread_id,root_session_id,agent_role,title,
                    project_name,project_path,project_kind
                 ) VALUES ('root-c',NULL,'root-c','main','Root C','project-c',?1,'project')",
                params![fixture_path("project/c")],
            )
            .unwrap();
        insert_event(
            &connection,
            "c-self",
            300,
            "root-c",
            "root-c",
            "gpt-c",
            100,
            10,
            Some(1),
            10,
            1,
        );
        connection
            .execute(
                "UPDATE usage_events
                 SET estimated_cost_nanos_usd=30
                 WHERE event_id='c-self'",
                [],
            )
            .unwrap();
        if error {
            connection
                .execute(
                    "INSERT INTO threads(
                        thread_id,parent_thread_id,root_session_id,agent_role,title,
                        project_name,project_path,project_kind
                     ) VALUES ('root-error',NULL,'root-error','main','Root Error',
                               'project-error',?1,'project')",
                    params![fixture_path("project/error")],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO usage_session_quarantine(
                        ledger_epoch,root_session_id,primary_error_code,last_activity_at_ms,
                        first_seen_at_ms,updated_at_ms
                     ) VALUES (7,'root-error','TEST_ERROR',350,350,350)",
                    [],
                )
                .unwrap();
        }
        connection
    }

    fn assert_summary_cost_counts(
        summary: &UsageSummary,
        healthy: i64,
        incomplete: i64,
        error: i64,
        complete_for_footer: i64,
    ) {
        assert_eq!(summary.session_count, healthy);
        assert_eq!(summary.health.incomplete_sessions, incomplete);
        assert_eq!(summary.health.error_sessions, error);
        assert_eq!(
            summary.health.complete_sessions,
            healthy.checked_sub(incomplete).unwrap()
        );
        assert_eq!(
            summary.health.total_sessions,
            healthy.checked_add(error).unwrap()
        );
        assert_eq!(
            summary.cost_incomplete_session_count,
            incomplete.checked_add(error).unwrap()
        );
        assert!(summary.cost_incomplete_session_count >= 0);
        assert!(summary.cost_incomplete_session_count <= summary.health.total_sessions);
        assert_eq!(
            summary
                .health
                .total_sessions
                .checked_sub(summary.cost_incomplete_session_count)
                .unwrap(),
            complete_for_footer
        );
    }

    fn filter_fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        let project_a = fixture_path("project/a");
        let project_b = fixture_path("project/b");
        let child_project = fixture_path("generated/child-a");
        let projectless_path = fixture_path("Users/me/generated-cwd");
        connection
            .execute_batch(&format!(
                "CREATE TABLE app_meta (
                    id INTEGER PRIMARY KEY,
                    data_revision INTEGER NOT NULL DEFAULT 0,
                    usage_active_epoch INTEGER NOT NULL
                 );
                 CREATE TABLE threads (
                    thread_id TEXT PRIMARY KEY, title TEXT, project_name TEXT, project_path TEXT,
                    project_kind TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL, root_session_id TEXT NOT NULL,
                    primary_error_code TEXT NOT NULL, last_activity_at_ms INTEGER NOT NULL,
                    first_seen_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE usage_events (
                    ledger_epoch INTEGER NOT NULL, event_id TEXT NOT NULL,
                    occurred_at_ms INTEGER NOT NULL, thread_id TEXT NOT NULL,
                    root_session_id TEXT NOT NULL, turn_key TEXT, model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER, output_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
                    reasoning_effort TEXT, estimated_cost_nanos_usd INTEGER,
                    source_file_id INTEGER, file_generation INTEGER,
                    source_start_offset INTEGER, source_end_offset INTEGER
                 );
                 INSERT INTO app_meta(id, usage_active_epoch) VALUES (1, 7);
                 INSERT INTO threads(thread_id,title,project_name,project_path,project_kind) VALUES
                    ('root-a','Root A','project-a','{project_a}','project'),
                    ('child-a','Child A','project-a','{child_project}','project'),
                    ('root-b','Root B','project-b','{project_b}','project'),
                    ('root-p','Root P',NULL,'{projectless_path}','projectless'),
                    ('root-u','Root U',NULL,NULL,'unknown');",
            ))
            .unwrap();
        insert_event(
            &connection,
            "a-self",
            100,
            "root-a",
            "root-a",
            "gpt-a",
            1_000,
            100,
            Some(20),
            100,
            20,
        );
        insert_event(
            &connection,
            "a-child",
            200,
            "child-a",
            "root-a",
            "gpt-b",
            2_000,
            200,
            Some(40),
            200,
            40,
        );
        insert_event(
            &connection,
            "a-late",
            400,
            "root-a",
            "root-a",
            "gpt-c",
            300,
            30,
            None,
            30,
            10,
        );
        insert_event(
            &connection,
            "b-self",
            300,
            "root-b",
            "root-b",
            "gpt-a",
            500,
            50,
            Some(5),
            50,
            5,
        );
        insert_event(
            &connection,
            "p-self",
            220,
            "root-p",
            "root-p",
            "gpt-c",
            700,
            70,
            Some(7),
            70,
            7,
        );
        insert_event(
            &connection,
            "u-self",
            230,
            "root-u",
            "root-u",
            "gpt-c",
            800,
            80,
            Some(8),
            80,
            8,
        );
        insert_event(
            &connection,
            "missing-root",
            240,
            "missing-thread",
            "missing-root",
            "gpt-c",
            900,
            90,
            Some(9),
            90,
            9,
        );
        connection
    }

    fn cost_fixture() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        let project_path = fixture_path("project");
        connection
            .execute_batch(&format!(
                "CREATE TABLE app_meta (id INTEGER PRIMARY KEY, usage_active_epoch INTEGER NOT NULL);
                 CREATE TABLE threads (
                    thread_id TEXT PRIMARY KEY, parent_thread_id TEXT, root_session_id TEXT,
                    agent_role TEXT NOT NULL, title TEXT, project_name TEXT, project_path TEXT,
                    project_kind TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL, root_session_id TEXT NOT NULL,
                    primary_error_code TEXT NOT NULL, last_activity_at_ms INTEGER NOT NULL,
                    first_seen_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE usage_events (
                    ledger_epoch INTEGER NOT NULL, event_id TEXT NOT NULL,
                    occurred_at_ms INTEGER NOT NULL, thread_id TEXT NOT NULL,
                    root_session_id TEXT NOT NULL, model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER, output_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
                    reasoning_effort TEXT, estimated_cost_nanos_usd INTEGER,
                    source_file_id INTEGER, file_generation INTEGER,
                    source_start_offset INTEGER, source_end_offset INTEGER
                 );
                 INSERT INTO app_meta(id, usage_active_epoch) VALUES (1, 7);
                 INSERT INTO threads(thread_id,parent_thread_id,root_session_id,agent_role,title,project_name,project_path,project_kind) VALUES
                    ('root','', 'root','main','Root','project','{project_path}','project'),
                    ('child','root','root','subagent','Child','project','{project_path}','project');"
            ))
            .unwrap();
        connection
            .execute(
                "UPDATE threads SET parent_thread_id=NULL WHERE thread_id='root'",
                [],
            )
            .unwrap();
        let insert = |event_id: &str,
                      occurred_at_ms: i64,
                      thread_id: &str,
                      model: &str,
                      effort: Option<&str>,
                      cost: Option<i64>| {
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,occurred_at_ms,thread_id,root_session_id,model,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,reasoning_effort,estimated_cost_nanos_usd,
                        source_file_id,file_generation,source_start_offset,source_end_offset
                     ) VALUES (7,?1,?2,?3,'root',?4,10,2,1,3,1,13,?5,?6,1,1,?2,?2+1)",
                    params![event_id, occurred_at_ms, thread_id, model, effort, cost],
                )
                .unwrap();
        };
        insert("root-high-1", 1, "root", "m-main", Some("high"), Some(100));
        insert("root-high-2", 2, "root", "m-main", Some("high"), Some(200));
        insert(
            "root-medium",
            3,
            "root",
            "m-main",
            Some("medium"),
            Some(300),
        );
        insert("root-unknown", 4, "root", "m-main", None, None);
        insert("root-other", 5, "root", "m-other", Some("high"), Some(400));
        insert("child-high", 6, "child", "m-child", Some("high"), Some(500));
        insert(
            "child-medium",
            7,
            "child",
            "m-child",
            Some("medium"),
            Some(600),
        );
        insert("child-unknown", 8, "child", "m-child", None, None);
        connection
    }

    fn summary_for(
        reader: &AggregateReader<'_>,
        range: TimeRange,
        models: &[&str],
        project_paths: &[&str],
        include_projectless: bool,
        include_unknown_project: bool,
    ) -> UsageSummary {
        let filter = UsageFilter::new(
            models.iter().map(|value| (*value).to_owned()).collect(),
            project_paths
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            include_projectless,
            include_unknown_project,
        );
        reader.summary(SummaryQuery::new(range, filter)).unwrap()
    }

    #[test]
    fn t_dc_031_summary_and_models_propagate_unknown_and_weight_hit_rate() {
        let connection = fixture();
        let reader = AggregateReader::new(&connection);
        let range = TimeRange::new(100, 400).unwrap();
        let summary = reader
            .summary(SummaryQuery::new(range, UsageFilter::default()))
            .unwrap();
        assert_eq!(summary.totals.input_tokens, 10_500);
        assert_eq!(summary.totals.cached_tokens, 1_900);
        assert_eq!(summary.totals.cache_write_tokens, None);
        assert_eq!(summary.totals.output_tokens, 1_050);
        assert_eq!(summary.totals.reasoning_tokens, 220);
        assert_eq!(summary.totals.total_tokens, 11_550);
        assert_eq!(summary.totals.uncached_input_tokens, None);
        assert_eq!(summary.totals.other_output_tokens, 830);
        assert_eq!(summary.totals.cache_hit_rate, Some(1_900.0 / 10_500.0));
        let models = reader.models(range).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|row| row.model.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-a", "gpt-b"]
        );
        assert_eq!(
            models
                .iter()
                .find(|row| row.model == "gpt-b")
                .unwrap()
                .totals
                .cache_write_tokens,
            None
        );
        reader.verify_invariants(range).unwrap();

        let known = Connection::open_in_memory().unwrap();
        known.execute_batch(
            "CREATE TABLE app_meta (id INTEGER PRIMARY KEY, usage_active_epoch INTEGER NOT NULL);
             CREATE TABLE threads (thread_id TEXT PRIMARY KEY, project_path TEXT, project_kind TEXT);
             CREATE TABLE IF NOT EXISTS usage_session_quarantine (
                    ledger_epoch INTEGER NOT NULL, root_session_id TEXT NOT NULL,
                    primary_error_code TEXT NOT NULL, last_activity_at_ms INTEGER NOT NULL,
                    first_seen_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
                 );
                 CREATE TABLE usage_events (ledger_epoch INTEGER,event_id TEXT,occurred_at_ms INTEGER,thread_id TEXT,root_session_id TEXT,model TEXT,input_tokens INTEGER,cached_tokens INTEGER,cache_write_tokens INTEGER,output_tokens INTEGER,reasoning_tokens INTEGER,total_tokens INTEGER,reasoning_effort TEXT,estimated_cost_nanos_usd INTEGER,source_file_id INTEGER,file_generation INTEGER,source_start_offset INTEGER,source_end_offset INTEGER);
             INSERT INTO app_meta VALUES (1,7);
             INSERT INTO usage_events(
                 ledger_epoch,event_id,occurred_at_ms,thread_id,root_session_id,model,
                 input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                 total_tokens,reasoning_effort,estimated_cost_nanos_usd
             ) VALUES (7,'a',1,'r','r','m',1000,900,50,100,20,1100,NULL,NULL);
             INSERT INTO usage_events(
                 ledger_epoch,event_id,occurred_at_ms,thread_id,root_session_id,model,
                 input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                 total_tokens,reasoning_effort,estimated_cost_nanos_usd
             ) VALUES (7,'b',2,'r','r','m',9000,900,1000,900,180,9900,NULL,NULL);",
        ).unwrap();
        let exact = AggregateReader::new(&known)
            .summary(SummaryQuery::new(
                TimeRange::new(0, 3).unwrap(),
                UsageFilter::default(),
            ))
            .unwrap()
            .totals;
        assert_eq!(exact.cache_write_tokens, Some(1050));
        assert_eq!(exact.uncached_input_tokens, Some(7150));
        assert_eq!(exact.other_output_tokens, 800);
        assert_eq!(exact.cache_hit_rate, Some(0.18));
        assert_ne!(exact.cache_hit_rate, Some((0.9 + 0.1) / 2.0));
    }

    #[test]
    fn t_s03_004_summary_cost_count_uses_all_root_sessions() {
        for (
            incomplete,
            error,
            expected_incomplete,
            expected_error,
            footer_complete,
            footer_total,
        ) in [
            (false, false, 0_i64, 0_i64, 3_i64, 3_i64),
            (true, false, 1_i64, 0_i64, 2_i64, 3_i64),
            (true, true, 1_i64, 1_i64, 2_i64, 4_i64),
        ] {
            let connection = summary_cost_fixture(incomplete, error);
            let summary = AggregateReader::new(&connection)
                .summary(SummaryQuery::new(
                    TimeRange::new(0, 400).unwrap(),
                    UsageFilter::default(),
                ))
                .unwrap();
            assert_summary_cost_counts(
                &summary,
                3,
                expected_incomplete,
                expected_error,
                footer_complete,
            );
            assert_eq!(summary.health.total_sessions, footer_total);
            assert_eq!(
                summary
                    .health
                    .total_sessions
                    .checked_sub(summary.cost_incomplete_session_count)
                    .unwrap(),
                footer_complete
            );
        }
    }

    #[test]
    fn complete_session_cost_per_million_tokens_uses_only_complete_filtered_sessions() {
        let connection = cost_fixture();
        let reader = AggregateReader::new(&connection);

        let all = reader
            .summary(SummaryQuery::new(
                TimeRange::new(0, 9).unwrap(),
                UsageFilter::default(),
            ))
            .unwrap();
        assert_eq!(all.complete_session_cost_per_million_tokens, None);

        let filtered = reader
            .summary(SummaryQuery::new(
                TimeRange::new(0, 9).unwrap(),
                UsageFilter::new(vec!["m-other".into()], vec![], false, false),
            ))
            .unwrap();
        let expected = 400.0 / 13.0 / 1_000.0;
        assert!(
            (filtered.complete_session_cost_per_million_tokens.unwrap() - expected).abs() < 1e-12
        );

        let known_slice = reader
            .summary(SummaryQuery::new(
                TimeRange::new(0, 4).unwrap(),
                UsageFilter::new(vec!["m-main".into()], vec![], false, false),
            ))
            .unwrap();
        let expected = 600.0 / 39.0 / 1_000.0;
        assert!(
            (known_slice
                .complete_session_cost_per_million_tokens
                .unwrap()
                - expected)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn t_mu03_b06_backend_cost_aggregation_uses_event_values_only() {
        let connection = cost_fixture();
        let reader = AggregateReader::new(&connection);
        let known_range = TimeRange::new(0, 4).unwrap();
        let known = reader
            .summary(SummaryQuery::new(
                known_range,
                UsageFilter::new(vec!["m-main".into()], vec![], false, false),
            ))
            .unwrap();
        assert_eq!(known.totals.estimated_cost_nanos_usd, Some(600));
        let unknown = reader
            .summary(SummaryQuery::new(
                TimeRange::new(0, 5).unwrap(),
                UsageFilter::default(),
            ))
            .unwrap();
        assert_eq!(unknown.totals.estimated_cost_nanos_usd, Some(600));
        assert_eq!(unknown.totals.cost_completeness, CostCompleteness::Partial);
        let empty = reader
            .summary(SummaryQuery::new(
                TimeRange::new(20, 20).unwrap(),
                UsageFilter::default(),
            ))
            .unwrap();
        assert_eq!(empty.totals.estimated_cost_nanos_usd, Some(0));
        assert_eq!(
            reader
                .models(known_range)
                .unwrap()
                .into_iter()
                .find(|row| row.model == "m-main")
                .unwrap()
                .totals
                .estimated_cost_nanos_usd,
            Some(600)
        );
        let row = reader
            .sessions(TimeRange::new(0, 9).unwrap(), SessionPageRequest::new(10))
            .unwrap()
            .rows
            .into_iter()
            .find(|row| row.root_session_id == "root")
            .unwrap();
        assert_eq!(row.self_usage.estimated_cost_nanos_usd, Some(1_000));
        assert_eq!(row.self_usage.cost_completeness, CostCompleteness::Partial);
        assert_eq!(row.subagent_usage.estimated_cost_nanos_usd, Some(1_100));
        assert_eq!(
            row.subagent_usage.cost_completeness,
            CostCompleteness::Partial
        );
        assert_eq!(row.inclusive_usage.estimated_cost_nanos_usd, Some(2_100));
        assert_eq!(
            row.inclusive_usage.cost_completeness,
            CostCompleteness::Partial
        );
    }

    #[test]
    fn t_mu03_c06_subagent_model_usage_keeps_model_effort_groups_isolated() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_meta (id INTEGER PRIMARY KEY, usage_active_epoch INTEGER NOT NULL);
                 CREATE TABLE threads (
                    thread_id TEXT PRIMARY KEY, parent_thread_id TEXT, root_session_id TEXT,
                    agent_role TEXT NOT NULL, title TEXT, project_name TEXT, project_path TEXT,
                    project_kind TEXT NOT NULL
                 );
                 CREATE TABLE usage_events (
                    ledger_epoch INTEGER NOT NULL, event_id TEXT NOT NULL,
                    occurred_at_ms INTEGER NOT NULL, thread_id TEXT NOT NULL,
                    root_session_id TEXT NOT NULL, turn_key TEXT, model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER, output_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
                    reasoning_effort TEXT, estimated_cost_nanos_usd INTEGER,
                    source_file_id INTEGER, file_generation INTEGER,
                    source_start_offset INTEGER, source_end_offset INTEGER
                 );
                 INSERT INTO app_meta(id, usage_active_epoch) VALUES (1, 7);
                 INSERT INTO threads(
                    thread_id,parent_thread_id,root_session_id,agent_role,title,project_name,project_path,project_kind
                 ) VALUES
                    ('root',NULL,'root','main','Root',NULL,NULL,'project'),
                    ('child','root','root','subagent','Child',NULL,NULL,'project');",
            )
            .unwrap();
        let insert = |event_id: &str,
                      occurred_at_ms: i64,
                      thread_id: &str,
                      model: &str,
                      effort: &str,
                      input_tokens: i64,
                      cached_tokens: i64,
                      cache_write_tokens: i64,
                      output_tokens: i64,
                      reasoning_tokens: i64,
                      cost: i64,
                      source_end_offset: i64| {
            connection
                .execute(
                    "INSERT INTO usage_events(
                        ledger_epoch,event_id,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                        input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                        total_tokens,reasoning_effort,estimated_cost_nanos_usd,
                        source_file_id,file_generation,source_start_offset,source_end_offset
                     ) VALUES (7,?1,?2,?3,'root',NULL,?4,?5,?6,?7,?8,?9,?5+?8,?10,?11,1,1,?12-1,?12)",
                    params![
                        event_id,
                        occurred_at_ms,
                        thread_id,
                        model,
                        input_tokens,
                        cached_tokens,
                        cache_write_tokens,
                        output_tokens,
                        reasoning_tokens,
                        effort,
                        cost,
                        source_end_offset,
                    ],
                )
                .unwrap();
        };
        insert(
            "root-event",
            0,
            "root",
            "root-model",
            "max",
            10,
            0,
            0,
            10,
            0,
            100,
            1,
        );
        insert("luna-max", 5, "child", "Luna", "max", 1, 1, 0, 1, 0, 10, 10);
        insert(
            "sol-medium-1",
            5,
            "child",
            "Sol",
            "medium",
            2,
            0,
            1,
            2,
            1,
            20,
            20,
        );
        insert(
            "luna-high",
            5,
            "child",
            "Luna",
            "high",
            3,
            1,
            1,
            3,
            1,
            30,
            30,
        );
        insert(
            "sol-medium-2",
            5,
            "child",
            "Sol",
            "medium",
            4,
            2,
            1,
            4,
            2,
            40,
            40,
        );
        insert("sol-high", 5, "child", "Sol", "high", 5, 3, 1, 5, 2, 50, 50);

        let detail = AggregateReader::new(&connection)
            .session_detail(
                TimeRange::new(0, 10).unwrap(),
                &UsageFilter::default(),
                "root",
            )
            .unwrap();
        let child = &detail.subagents[0];
        assert_eq!(child.model_usage.len(), 4);
        assert_eq!(
            child
                .model_usage
                .iter()
                .map(|block| (block.model.as_str(), block.reasoning_effort.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("Sol", Some("high")),
                ("Sol", Some("medium")),
                ("Luna", Some("high")),
                ("Luna", Some("max")),
            ]
        );
        assert_eq!(child.model_usage[0].last_activity_at_ms, 5);
        assert_eq!(child.model_usage[1].last_activity_at_ms, 5);
        assert_eq!(child.model_usage[2].last_activity_at_ms, 5);
        assert_eq!(child.model_usage[3].last_activity_at_ms, 5);
        assert_eq!(child.model_usage[0].usage.input_tokens, 5);
        assert_eq!(
            child.model_usage[0].usage.estimated_cost_nanos_usd,
            Some(50)
        );
        assert_eq!(child.model_usage[1].usage.input_tokens, 6);
        assert_eq!(child.model_usage[1].usage.output_tokens, 6);
        assert_eq!(
            child.model_usage[1].usage.estimated_cost_nanos_usd,
            Some(60)
        );
        assert_eq!(child.model_usage[2].usage.input_tokens, 3);
        assert_eq!(
            child.model_usage[2].usage.estimated_cost_nanos_usd,
            Some(30)
        );
        assert_eq!(child.model_usage[3].usage.input_tokens, 1);
        assert_eq!(
            child.model_usage[3].usage.estimated_cost_nanos_usd,
            Some(10)
        );

        let mut summed = TokenTotals::zero();
        for block in &child.model_usage {
            summed.add_assign(&block.usage).unwrap();
        }
        assert_eq!(summed.input_tokens, 15);
        assert_eq!(summed.cached_tokens, 7);
        assert_eq!(summed.cache_write_tokens, Some(4));
        assert_eq!(summed.output_tokens, 15);
        assert_eq!(summed.reasoning_tokens, 6);
        assert_eq!(summed.total_tokens, 30);
        assert_eq!(summed.estimated_cost_nanos_usd, Some(150));
        assert_eq!(detail.main.inclusive_usage.input_tokens, 25);
        assert_eq!(detail.main.inclusive_usage.output_tokens, 25);
        assert_eq!(detail.main.inclusive_usage.total_tokens, 50);
    }

    #[test]
    fn t_mu04_c01_token_totals_cost_completeness_state_machine() {
        fn with_cost(cost: Option<i64>, completeness: CostCompleteness) -> TokenTotals {
            let mut totals = TokenTotals::zero();
            totals.estimated_cost_nanos_usd = cost;
            totals.cost_completeness = completeness;
            totals
        }

        let known = with_cost(Some(7), CostCompleteness::Complete);
        let unknown = with_cost(None, CostCompleteness::Unknown);
        let empty = TokenTotals::zero();

        let mut known_known = known.clone();
        known_known
            .add_assign(&with_cost(Some(5), CostCompleteness::Complete))
            .unwrap();
        assert_eq!(known_known.estimated_cost_nanos_usd, Some(12));
        assert_eq!(known_known.cost_completeness, CostCompleteness::Complete);

        for (left, right) in [(known.clone(), unknown.clone()), (unknown.clone(), known)] {
            let mut merged = left;
            merged.add_assign(&right).unwrap();
            assert_eq!(merged.estimated_cost_nanos_usd, Some(7));
            assert_eq!(merged.cost_completeness, CostCompleteness::Partial);
        }

        let mut unknown_unknown = unknown.clone();
        unknown_unknown.add_assign(&unknown).unwrap();
        assert_eq!(unknown_unknown.estimated_cost_nanos_usd, None);
        assert_eq!(unknown_unknown.cost_completeness, CostCompleteness::Unknown);

        for (left, right, expected) in [
            (
                empty.clone(),
                with_cost(Some(5), CostCompleteness::Complete),
                (Some(5), CostCompleteness::Complete),
            ),
            (
                with_cost(Some(5), CostCompleteness::Complete),
                empty.clone(),
                (Some(5), CostCompleteness::Complete),
            ),
            (
                empty.clone(),
                unknown.clone(),
                (None, CostCompleteness::Unknown),
            ),
            (
                unknown.clone(),
                empty.clone(),
                (None, CostCompleteness::Unknown),
            ),
            (
                empty.clone(),
                empty.clone(),
                (Some(0), CostCompleteness::Empty),
            ),
        ] {
            let mut merged = left;
            merged.add_assign(&right).unwrap();
            assert_eq!(
                (merged.estimated_cost_nanos_usd, merged.cost_completeness),
                expected
            );
        }

        assert_eq!(
            (empty.estimated_cost_nanos_usd, empty.cost_completeness),
            (Some(0), CostCompleteness::Empty)
        );

        let mut overflow = with_cost(Some(i64::MAX), CostCompleteness::Complete);
        assert_eq!(
            overflow.add_assign(&with_cost(Some(1), CostCompleteness::Complete)),
            Err(AggregateError::ArithmeticOverflow)
        );
    }

    #[test]
    fn t_mu03_c05_backend_detail_groups_model_and_effort_buckets() {
        let connection = cost_fixture();
        let reader = AggregateReader::new(&connection);
        let detail = reader
            .session_detail(
                TimeRange::new(0, 9).unwrap(),
                &UsageFilter::default(),
                "root",
            )
            .unwrap();
        assert_eq!(detail.main.models_used, vec!["m-main", "m-other"]);
        assert_eq!(detail.main.model_usage.len(), 4);
        assert_eq!(detail.main.model_usage[0].model, "m-main");
        assert_eq!(
            detail.main.model_usage[0].reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(
            detail.main.model_usage[0].usage.estimated_cost_nanos_usd,
            Some(300)
        );
        assert_eq!(
            detail.main.model_usage[1].reasoning_effort.as_deref(),
            Some("medium")
        );
        assert_eq!(detail.main.model_usage[2].reasoning_effort, None);
        assert_eq!(
            detail.main.model_usage[2].usage.estimated_cost_nanos_usd,
            None
        );
        assert_eq!(
            detail.main.model_usage[2].usage.cost_completeness,
            CostCompleteness::Unknown
        );
        assert_eq!(detail.subagents.len(), 1);
        assert_eq!(
            detail.subagents[0]
                .model_usage
                .iter()
                .map(|block| (block.model.as_str(), block.reasoning_effort.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("m-child", None),
                ("m-child", Some("medium")),
                ("m-child", Some("high")),
            ]
        );
        let mut child_usage = TokenTotals::zero();
        for block in &detail.subagents[0].model_usage {
            child_usage.add_assign(&block.usage).unwrap();
        }
        assert_eq!(child_usage.estimated_cost_nanos_usd, Some(1_100));
        assert_eq!(child_usage.cost_completeness, CostCompleteness::Partial);
    }

    #[test]
    fn t_mu04_c02_cross_view_aggregates_share_cost_completeness() {
        let connection = cost_fixture();
        let reader = AggregateReader::new(&connection);
        let range = TimeRange::new(0, 9).unwrap();
        let summary = reader
            .summary(SummaryQuery::new(range, UsageFilter::default()))
            .unwrap()
            .totals;
        assert_eq!(
            (summary.estimated_cost_nanos_usd, summary.cost_completeness),
            (Some(2_100), CostCompleteness::Partial)
        );

        let model_rows = reader.models(range).unwrap();
        let model_main = model_rows.iter().find(|row| row.model == "m-main").unwrap();
        assert_eq!(
            (
                model_main.totals.estimated_cost_nanos_usd,
                model_main.totals.cost_completeness
            ),
            (Some(600), CostCompleteness::Partial)
        );
        let model_child = model_rows
            .iter()
            .find(|row| row.model == "m-child")
            .unwrap();
        assert_eq!(
            (
                model_child.totals.estimated_cost_nanos_usd,
                model_child.totals.cost_completeness
            ),
            (Some(1_100), CostCompleteness::Partial)
        );

        let row = reader
            .sessions(range, SessionPageRequest::new(10))
            .unwrap()
            .rows
            .into_iter()
            .find(|row| row.root_session_id == "root")
            .unwrap();
        assert_eq!(
            (
                row.self_usage.estimated_cost_nanos_usd,
                row.self_usage.cost_completeness
            ),
            (Some(1_000), CostCompleteness::Partial)
        );
        assert_eq!(
            (
                row.subagent_usage.estimated_cost_nanos_usd,
                row.subagent_usage.cost_completeness
            ),
            (Some(1_100), CostCompleteness::Partial)
        );
        assert_eq!(
            (
                row.inclusive_usage.estimated_cost_nanos_usd,
                row.inclusive_usage.cost_completeness
            ),
            (Some(2_100), CostCompleteness::Partial)
        );

        let detail = reader
            .session_detail(range, &UsageFilter::default(), "root")
            .unwrap();
        assert_eq!(
            (
                detail.main.self_usage.estimated_cost_nanos_usd,
                detail.main.self_usage.cost_completeness
            ),
            (Some(1_000), CostCompleteness::Partial)
        );
        assert_eq!(
            (
                detail.main.inclusive_usage.estimated_cost_nanos_usd,
                detail.main.inclusive_usage.cost_completeness
            ),
            (Some(2_100), CostCompleteness::Partial)
        );
        let mut child_usage = TokenTotals::zero();
        for block in &detail.subagents[0].model_usage {
            child_usage.add_assign(&block.usage).unwrap();
        }
        assert_eq!(
            (
                child_usage.estimated_cost_nanos_usd,
                child_usage.cost_completeness
            ),
            (Some(1_100), CostCompleteness::Partial)
        );

        let all_unknown_range = TimeRange::new(4, 5).unwrap();
        let all_unknown_summary = reader
            .summary(SummaryQuery::new(all_unknown_range, UsageFilter::default()))
            .unwrap()
            .totals;
        assert_eq!(
            (
                all_unknown_summary.estimated_cost_nanos_usd,
                all_unknown_summary.cost_completeness
            ),
            (None, CostCompleteness::Unknown)
        );
        let all_unknown_model = reader
            .models(all_unknown_range)
            .unwrap()
            .into_iter()
            .find(|row| row.model == "m-main")
            .unwrap();
        assert_eq!(
            (
                all_unknown_model.totals.estimated_cost_nanos_usd,
                all_unknown_model.totals.cost_completeness
            ),
            (None, CostCompleteness::Unknown)
        );

        let all_unknown_session = reader
            .sessions(all_unknown_range, SessionPageRequest::new(10))
            .unwrap()
            .rows
            .into_iter()
            .find(|row| row.root_session_id == "root")
            .unwrap();
        assert_eq!(
            (
                all_unknown_session.inclusive_usage.estimated_cost_nanos_usd,
                all_unknown_session.inclusive_usage.cost_completeness
            ),
            (None, CostCompleteness::Unknown)
        );
        let all_unknown_detail = reader
            .session_detail(all_unknown_range, &UsageFilter::default(), "root")
            .unwrap();
        assert_eq!(
            (
                all_unknown_detail
                    .main
                    .inclusive_usage
                    .estimated_cost_nanos_usd,
                all_unknown_detail.main.inclusive_usage.cost_completeness
            ),
            (None, CostCompleteness::Unknown)
        );

        let mut complete = TokenTotals::zero();
        complete.estimated_cost_nanos_usd = Some(3);
        complete.cost_completeness = CostCompleteness::Complete;
        let mut partial = complete.clone();
        partial.cost_completeness = CostCompleteness::Partial;
        assert!(!same_totals(&complete, &partial));
    }

    #[test]
    fn t_dc_032_session_scopes_recompute_inclusive_derived_values() {
        let connection = fixture();
        let page = AggregateReader::new(&connection)
            .sessions(
                TimeRange::new(100, 400).unwrap(),
                SessionPageRequest::new(10),
            )
            .unwrap();
        let root = page
            .rows
            .iter()
            .find(|row| row.root_session_id == "root-a")
            .unwrap();
        assert_eq!(root.self_usage.input_tokens, 1000);
        assert_eq!(root.subagent_usage.input_tokens, 9500);
        assert_eq!(root.inclusive_usage.input_tokens, 10_500);
        assert_eq!(root.inclusive_usage.cached_tokens, 1_900);
        assert_eq!(root.inclusive_usage.output_tokens, 1050);
        assert_eq!(root.inclusive_usage.reasoning_tokens, 220);
        assert_eq!(root.inclusive_usage.total_tokens, 11_550);
        assert_eq!(root.inclusive_usage.cache_write_tokens, None);
        assert_eq!(
            root.inclusive_usage.cache_hit_rate,
            Some(1_900.0 / 10_500.0)
        );
        assert_eq!(root.subagent_count, 1);
        assert_eq!(root.models_used, vec!["gpt-a", "gpt-b"]);
        assert!(
            AggregateReader::new(&connection)
                .verify_invariants(TimeRange::new(100, 400).unwrap())
                .is_ok()
        );
    }

    #[test]
    fn multilevel_subagent_usage_rolls_up_only_to_the_root_session_row() {
        let connection = fixture();
        connection
            .execute(
                "INSERT INTO threads(thread_id,title,project_name,project_path)
                 VALUES ('grandchild-a','Grandchild A','project-a',?1)",
                [fixture_path("project/a")],
            )
            .unwrap();
        insert_event(
            &connection,
            "a-grandchild",
            250,
            "grandchild-a",
            "root-a",
            "gpt-b",
            15,
            2,
            Some(0),
            3,
            0,
        );

        let reader = AggregateReader::new(&connection);
        let range = TimeRange::new(100, 400).unwrap();
        let page = reader.sessions(range, SessionPageRequest::new(10)).unwrap();
        assert_eq!(page.rows.len(), 2);
        assert!(page.rows.iter().all(|row| row.root_session_id != "child-a"));
        assert!(
            page.rows
                .iter()
                .all(|row| row.root_session_id != "grandchild-a")
        );
        let root = page
            .rows
            .iter()
            .find(|row| row.root_session_id == "root-a")
            .unwrap();
        assert_eq!(root.self_usage.input_tokens, 1_000);
        assert_eq!(root.subagent_usage.input_tokens, 9_515);
        assert_eq!(root.inclusive_usage.input_tokens, 10_515);
        assert_eq!(root.subagent_count, 2);
        assert_eq!(root.models_used, vec!["gpt-a", "gpt-b"]);
        assert_eq!(
            reader
                .summary(SummaryQuery::new(range, UsageFilter::default()))
                .unwrap()
                .totals
                .input_tokens,
            10_515
        );
        reader.verify_invariants(range).unwrap();
    }

    #[test]
    fn aggregate_sql_sum_overflow_is_a_structured_error() {
        let connection = fixture();
        let reader = AggregateReader::new(&connection);
        insert_event(
            &connection,
            "overflow",
            350,
            "root-b",
            "root-b",
            "overflow-model",
            i64::MAX,
            0,
            Some(0),
            0,
            0,
        );
        assert_eq!(
            reader.summary(SummaryQuery::new(
                TimeRange::new(0, 400).unwrap(),
                UsageFilter::default(),
            )),
            Err(AggregateError::ArithmeticOverflow)
        );
    }

    #[test]
    fn aggregate_rejects_invalid_ranges_and_pages() {
        let connection = fixture();
        let reader = AggregateReader::new(&connection);
        assert_eq!(TimeRange::new(10, 1), Err(AggregateError::InvalidRange));
        assert_eq!(
            reader.sessions(TimeRange::new(0, 400).unwrap(), SessionPageRequest::new(0)),
            Err(AggregateError::InvalidPage)
        );
        assert_eq!(
            reader.sessions(
                TimeRange::new(0, 400).unwrap(),
                SessionPageRequest::new(MAX_SESSION_PAGE_SIZE + 1)
            ),
            Err(AggregateError::InvalidPage)
        );
    }

    #[test]
    fn t_s04_001_model_filter_is_event_granular_and_canonical() {
        let connection = filter_fixture();
        let reader = AggregateReader::new(&connection);
        let range = TimeRange::new(0, 500).unwrap();

        let single = summary_for(&reader, range, &["gpt-b"], &[], false, false);
        assert_eq!(single.totals.input_tokens, 2_000);
        assert_eq!(single.totals.cached_tokens, 200);
        assert_eq!(single.totals.cache_write_tokens, Some(40));
        assert_eq!(single.totals.output_tokens, 200);
        assert_eq!(single.totals.reasoning_tokens, 40);
        assert_eq!(single.totals.total_tokens, 2_200);
        assert_eq!(single.totals.uncached_input_tokens, Some(1_760));
        assert_eq!(single.totals.other_output_tokens, 160);
        assert_eq!(single.totals.cache_hit_rate, Some(0.1));
        assert_eq!(single.session_count, 1);

        let multi = summary_for(
            &reader,
            range,
            &["gpt-b", "gpt-a", "gpt-b"],
            &[],
            false,
            false,
        );
        assert_eq!(multi.totals.input_tokens, 3_500);
        assert_eq!(multi.totals.cached_tokens, 350);
        assert_eq!(multi.totals.cache_write_tokens, Some(65));
        assert_eq!(multi.totals.output_tokens, 350);
        assert_eq!(multi.totals.reasoning_tokens, 65);
        assert_eq!(multi.totals.total_tokens, 3_850);
        assert_eq!(multi.totals.uncached_input_tokens, Some(3_085));
        assert_eq!(multi.totals.other_output_tokens, 285);
        assert_eq!(multi.totals.cache_hit_rate, Some(0.1));
        assert_eq!(multi.session_count, 2);

        let unknown_cache_write = summary_for(&reader, range, &["gpt-c"], &[], false, false);
        assert_eq!(unknown_cache_write.totals.input_tokens, 2_700);
        assert_eq!(unknown_cache_write.totals.reasoning_tokens, 34);
        assert_eq!(unknown_cache_write.totals.cache_write_tokens, None);
        assert_eq!(unknown_cache_write.totals.cache_hit_rate, Some(0.1));
        assert_eq!(unknown_cache_write.session_count, 4);
    }

    #[test]
    fn t_s04_002_project_filter_uses_root_typed_join_and_specials() {
        let connection = filter_fixture();
        let reader = AggregateReader::new(&connection);
        let range = TimeRange::new(0, 500).unwrap();
        let project_a_path = fixture_path("project/a");
        let project_b_path = fixture_path("project/b");
        let projectless_path = fixture_path("Users/me/generated-cwd");

        let project_a = summary_for(
            &reader,
            range,
            &[],
            &[project_a_path.as_str()],
            false,
            false,
        );
        assert_eq!(project_a.totals.input_tokens, 3_300);
        assert_eq!(project_a.totals.cache_write_tokens, None);
        assert_eq!(project_a.session_count, 1);

        let generated_path = summary_for(
            &reader,
            range,
            &[],
            &[projectless_path.as_str()],
            false,
            false,
        );
        assert_eq!(generated_path.totals.input_tokens, 0);
        assert_eq!(generated_path.session_count, 0);

        let projectless = summary_for(&reader, range, &[], &[], true, false);
        assert_eq!(projectless.totals.input_tokens, 700);
        assert_eq!(projectless.session_count, 1);

        let path_and_projectless = summary_for(
            &reader,
            range,
            &[],
            &[projectless_path.as_str()],
            true,
            false,
        );
        assert_eq!(path_and_projectless.totals.input_tokens, 700);
        assert_eq!(path_and_projectless.session_count, 1);

        let unknown = summary_for(&reader, range, &[], &[], false, true);
        assert_eq!(unknown.totals.input_tokens, 1_700);
        assert_eq!(unknown.totals.cache_write_tokens, Some(17));
        assert_eq!(unknown.session_count, 2);

        let path_or_special =
            summary_for(&reader, range, &[], &[project_b_path.as_str()], true, false);
        assert_eq!(path_or_special.totals.input_tokens, 1_200);
        assert_eq!(path_or_special.session_count, 2);
    }

    #[test]
    fn t_s04_003_filter_dimensions_and_date_are_anded_without_session_effects() {
        let connection = filter_fixture();
        let reader = AggregateReader::new(&connection);
        let all_range = TimeRange::new(0, 500).unwrap();
        let project_a_path = fixture_path("project/a");
        let project_b_path = fixture_path("project/b");

        let unfiltered = summary_for(&reader, all_range, &[], &[], false, false);
        assert_eq!(unfiltered.totals.input_tokens, 6_200);
        assert_eq!(unfiltered.totals.cached_tokens, 620);
        assert_eq!(unfiltered.totals.cache_write_tokens, None);
        assert_eq!(unfiltered.totals.output_tokens, 620);
        assert_eq!(unfiltered.totals.reasoning_tokens, 99);
        assert_eq!(unfiltered.totals.total_tokens, 6_820);
        assert_eq!(unfiltered.totals.uncached_input_tokens, None);
        assert_eq!(unfiltered.totals.other_output_tokens, 521);
        assert_eq!(unfiltered.totals.cache_hit_rate, Some(0.1));
        assert_eq!(unfiltered.session_count, 5);

        let constrained = summary_for(
            &reader,
            TimeRange::new(150, 350).unwrap(),
            &["gpt-b", "gpt-a", "gpt-b"],
            &[
                project_b_path.as_str(),
                project_a_path.as_str(),
                project_a_path.as_str(),
            ],
            false,
            false,
        );
        assert_eq!(constrained.totals.input_tokens, 2_500);
        assert_eq!(constrained.totals.cached_tokens, 250);
        assert_eq!(constrained.totals.cache_write_tokens, Some(45));
        assert_eq!(constrained.totals.output_tokens, 250);
        assert_eq!(constrained.totals.reasoning_tokens, 45);
        assert_eq!(constrained.totals.total_tokens, 2_750);
        assert_eq!(constrained.totals.uncached_input_tokens, Some(2_205));
        assert_eq!(constrained.totals.other_output_tokens, 205);
        assert_eq!(constrained.totals.cache_hit_rate, Some(0.1));
        assert_eq!(constrained.session_count, 2);

        let page = reader
            .sessions(all_range, SessionPageRequest::new(10))
            .unwrap();
        assert_eq!(page.rows.len(), 5);
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.root_session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["root-a", "root-b", "missing-root", "root-u", "root-p"]
        );
        assert_eq!(
            page.rows
                .iter()
                .find(|row| row.root_session_id == "root-a")
                .unwrap()
                .inclusive_usage
                .input_tokens,
            3_300
        );
    }

    #[test]
    fn t_s05_001_filter_options_use_active_epoch_and_typed_project_matrix() {
        let mut connection = filter_fixture();
        connection
            .execute("UPDATE app_meta SET data_revision=17 WHERE id=1", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO threads(
                    thread_id,title,project_name,project_path,project_kind
                 ) VALUES ('root-a-alt','Root A alt','project-a',?1,'project'),
                 ('root-unused','Unused','unused',?2,'project')",
                params![
                    fixture_path("project/other"),
                    fixture_path("project/unused")
                ],
            )
            .unwrap();
        insert_event(
            &connection,
            "a-alt",
            260,
            "root-a-alt",
            "root-a-alt",
            "gpt-alt",
            10,
            1,
            Some(1),
            2,
            0,
        );
        connection
            .execute(
                "INSERT INTO usage_events(
                    ledger_epoch,event_id,occurred_at_ms,thread_id,root_session_id,model,
                    input_tokens,cached_tokens,cache_write_tokens,output_tokens,reasoning_tokens,
                    total_tokens,reasoning_effort,estimated_cost_nanos_usd
                 ) VALUES (6,'inactive',1,'root-a-alt','root-a-alt','gpt-inactive',1,0,0,1,0,2,NULL,NULL)",
                [],
            )
            .unwrap();

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        let metadata = transaction
            .query_row(
                "SELECT data_revision,usage_active_epoch FROM app_meta WHERE id=1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        let options = AggregateReader::new(&transaction).filter_options().unwrap();
        assert_eq!(metadata, (17, 7));
        assert_eq!(
            options.models,
            vec![
                ModelFilterOption {
                    model: "gpt-a".to_owned(),
                    provider: "route-models".to_owned(),
                },
                ModelFilterOption {
                    model: "gpt-alt".to_owned(),
                    provider: "route-models".to_owned(),
                },
                ModelFilterOption {
                    model: "gpt-b".to_owned(),
                    provider: "route-models".to_owned(),
                },
                ModelFilterOption {
                    model: "gpt-c".to_owned(),
                    provider: "route-models".to_owned(),
                },
            ]
        );
        assert_eq!(
            options.projects,
            vec![
                ProjectFilterOption::Project {
                    project_name: "project-a".to_owned(),
                    project_path: fixture_path("project/a"),
                },
                ProjectFilterOption::Project {
                    project_name: "project-b".to_owned(),
                    project_path: fixture_path("project/b"),
                },
                ProjectFilterOption::Project {
                    project_name: "project-a".to_owned(),
                    project_path: fixture_path("project/other"),
                },
                ProjectFilterOption::Projectless,
                ProjectFilterOption::Unknown,
            ]
        );
        assert!(options.projects.iter().all(|option| !matches!(
            option,
            ProjectFilterOption::Project { project_path, .. }
                if project_path == &fixture_path("Users/me/generated-cwd")
        )));
        transaction.commit().unwrap();
    }

    #[test]
    fn t_s05_002_filter_options_classify_openai_provider() {
        let mut connection = filter_fixture();
        insert_event(
            &connection,
            "openai-model",
            250,
            "root-a",
            "root-a",
            "gpt-5.6-luna",
            1,
            0,
            Some(0),
            1,
            0,
        );
        insert_event(
            &connection,
            "reserve-model",
            260,
            "root-a",
            "root-a",
            "gpt-reserve",
            1,
            0,
            Some(0),
            1,
            0,
        );

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .unwrap();
        let options = AggregateReader::new(&transaction).filter_options().unwrap();
        assert!(
            options
                .models
                .iter()
                .any(|option| { option.model == "gpt-5.6-luna" && option.provider == "openai" })
        );
        assert!(
            options
                .models
                .iter()
                .any(|option| { option.model == "gpt-reserve" && option.provider == "openai" })
        );
        transaction.commit().unwrap();
    }
}
