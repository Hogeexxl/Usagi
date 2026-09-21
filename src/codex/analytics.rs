//! Codex-specific read sidecars and Skills analytics.

use rusqlite::{Connection, params, params_from_iter, types::Value};

use crate::{
    range::ResolvedDay,
    storage::{Ledger, StorageError},
    usage::{
        aggregate::{
            AggregateError, SessionErrorProjection, SessionErrorSidecar, TimeRange, UsageFilter,
        },
        analytics::AnalyticsSnapshot,
        ledger::UsageLedgerError,
    },
};

pub(crate) const SKILL_USAGE_PARSER_VERSION: i64 = 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistributionCostStatus {
    Complete,
    Partial,
    Unknown,
}

impl DistributionCostStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCount {
    pub skill_name: String,
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDayUsage {
    pub date: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub total: i64,
    pub skills: Vec<SkillCount>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillsUsage {
    pub ready: bool,
    pub days: Vec<SkillDayUsage>,
}

/// Supplies Codex's private quarantine rows to the source-neutral aggregate.
pub struct CodexSessionErrorSidecar;

impl SessionErrorSidecar for CodexSessionErrorSidecar {
    fn error_roots(
        &self,
        connection: &Connection,
        range: TimeRange,
        filter: &UsageFilter,
    ) -> Result<Vec<SessionErrorProjection>, AggregateError> {
        if filter.models().iter().next().is_some()
            || filter
                .sources()
                .iter()
                .any(|source| source.as_str() != "codex")
        {
            return Ok(Vec::new());
        }

        let epoch: i64 = connection
            .query_row(
                "SELECT active_epoch FROM source_usage_epochs WHERE source='codex'",
                [],
                |row| row.get(0),
            )
            .map_err(map_sql_error)?;
        if epoch <= 0 {
            return Ok(Vec::new());
        }

        let mut clauses = vec![
            "q.ledger_epoch=?1".to_owned(),
            "q.last_activity_at_ms>=?2".to_owned(),
            "q.last_activity_at_ms<?3".to_owned(),
            "root.source='codex'".to_owned(),
        ];
        let mut values = vec![
            Value::Integer(epoch),
            Value::Integer(range.start_ms),
            Value::Integer(range.end_ms),
        ];
        let mut next = 4_usize;
        if !filter.project_paths().is_empty() {
            let placeholders = (next..next + filter.project_paths().len())
                .map(|value| format!("?{value}"))
                .collect::<Vec<_>>()
                .join(",");
            clauses.push(format!(
                "(root.project_kind='project' AND root.project_path IN ({placeholders}))"
            ));
            values.extend(filter.project_paths().iter().cloned().map(Value::Text));
            next += filter.project_paths().len();
        }
        let mut project_kinds = Vec::new();
        if filter.include_projectless() {
            project_kinds.push("root.project_kind='projectless'".to_owned());
        }
        if filter.include_unknown_project() {
            project_kinds.push("root.project_kind='unknown'".to_owned());
        }
        if !project_kinds.is_empty() {
            clauses.push(format!("({})", project_kinds.join(" OR ")));
        }
        let sql = format!(
            "SELECT q.root_session_id,q.primary_error_code,q.last_activity_at_ms,
                    root.title,root.project_name,root.project_path,
                    (SELECT COUNT(*) FROM threads child
                     WHERE child.root_session_id=q.root_session_id
                       AND child.thread_id<>q.root_session_id),
                    root.source,COALESCE(root.native_session_id,q.root_session_id)
             FROM codex_usage_session_quarantine q
             JOIN threads root ON root.thread_id=q.root_session_id
             WHERE {} ORDER BY q.root_session_id",
            clauses.join(" AND ")
        );
        let mut statement = connection.prepare(&sql).map_err(map_sql_error)?;
        statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok(SessionErrorProjection {
                    root_session_id: row.get(0)?,
                    error_code: row.get(1)?,
                    last_activity_at_ms: row.get(2)?,
                    title: row.get(3)?,
                    project_name: row.get(4)?,
                    project_path: row.get(5)?,
                    subagent_count: row.get(6)?,
                    source: row.get(7)?,
                    native_session_id: row.get(8)?,
                })
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)
    }
}

pub(crate) fn skills_usage_snapshot(
    ledger: &Ledger,
    days: &[ResolvedDay],
    filter: &UsageFilter,
) -> Result<AnalyticsSnapshot<SkillsUsage>, UsageLedgerError> {
    ledger.with_read_transaction(|transaction| {
        let (data_revision, active_epoch, active_parser): (i64, i64, i64) = transaction
            .query_row(
                "SELECT app.data_revision,sue.active_epoch,sue.active_parser_version
                 FROM app_meta app
                 JOIN source_usage_epochs sue ON sue.source='codex'
                 WHERE app.id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(StorageError::sqlite)?;
        if data_revision < 0 || active_epoch < 0 || active_parser < 0 {
            return Err(UsageLedgerError::Invalid(
                "invalid analytics snapshot metadata",
            ));
        }
        let ready = active_epoch > 0 && active_parser >= SKILL_USAGE_PARSER_VERSION;
        let source_allowed = filter.sources().is_empty()
            || filter
                .sources()
                .iter()
                .any(|source| source.as_str() == "codex");
        let mut output = Vec::with_capacity(days.len());
        for day in days {
            let range = TimeRange::new(day.start_ms, day.end_ms)?;
            let mut skills = if ready && source_allowed {
                let mut clauses = vec![
                    "se.ledger_epoch=?1".to_owned(),
                    "se.occurred_at_ms>=?2".to_owned(),
                    "se.occurred_at_ms<?3".to_owned(),
                ];
                let mut values = vec![
                    Value::Integer(active_epoch),
                    Value::Integer(range.start_ms),
                    Value::Integer(range.end_ms),
                ];
                let mut next = 4_usize;
                if !filter.models().is_empty() {
                    let placeholders = (next..next + filter.models().len())
                        .map(|value| format!("?{value}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    clauses.push(format!("se.model IN ({placeholders})"));
                    values.extend(filter.models().iter().cloned().map(Value::Text));
                    next += filter.models().len();
                }
                if !filter.project_paths().is_empty()
                    || filter.include_projectless()
                    || filter.include_unknown_project()
                {
                    let mut projects = Vec::new();
                    if !filter.project_paths().is_empty() {
                        let placeholders = (next..next + filter.project_paths().len())
                            .map(|value| format!("?{value}"))
                            .collect::<Vec<_>>()
                            .join(",");
                        projects.push(format!(
                            "(root.project_kind='project' AND root.project_path IN ({placeholders}))"
                        ));
                        values.extend(
                            filter
                                .project_paths()
                                .iter()
                                .cloned()
                                .map(Value::Text),
                        );
                        next += filter.project_paths().len();
                    }
                    if filter.include_projectless() {
                        projects.push("root.project_kind='projectless'".to_owned());
                    }
                    if filter.include_unknown_project() {
                        projects.push("root.project_kind='unknown'".to_owned());
                    }
                    clauses.push(format!("({})", projects.join(" OR ")));
                }
                let sql = format!(
                    "SELECT se.skill_name,COUNT(*)
                     FROM codex_skill_usage_events se
                     LEFT JOIN threads root ON root.thread_id=se.root_session_id
                     WHERE {} GROUP BY se.skill_name
                     ORDER BY COUNT(*) DESC,se.skill_name ASC",
                    clauses.join(" AND ")
                );
                let mut statement = transaction.prepare(&sql).map_err(StorageError::sqlite)?;
                statement
                    .query_map(params_from_iter(values.iter()), |row| {
                        Ok(SkillCount {
                            skill_name: row.get(0)?,
                            count: row.get(1)?,
                        })
                    })
                    .map_err(StorageError::sqlite)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(StorageError::sqlite)?
            } else {
                Vec::new()
            };
            skills.sort_by(|left, right| {
                right
                    .count
                    .cmp(&left.count)
                    .then_with(|| left.skill_name.cmp(&right.skill_name))
            });
            let total = skills.iter().try_fold(0_i64, |sum, row| {
                if row.count < 0 {
                    return Err(UsageLedgerError::Aggregate(
                        AggregateError::InvariantViolation,
                    ));
                }
                sum.checked_add(row.count).ok_or(UsageLedgerError::Aggregate(
                    AggregateError::ArithmeticOverflow,
                ))
            })?;
            output.push(SkillDayUsage {
                date: day.date.clone(),
                start_ms: day.start_ms,
                end_ms: day.end_ms,
                total,
                skills,
            });
        }
        Ok(AnalyticsSnapshot {
            data_revision,
            value: SkillsUsage {
                ready,
                days: output,
            },
        })
    })
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
