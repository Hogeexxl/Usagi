//! Source-neutral model and project usage distributions.

use rusqlite::{params_from_iter, types::Value};

use crate::{
    storage::{Ledger, StorageError},
    usage::ledger::UsageLedgerError,
};

use super::aggregate::{AggregateError, TimeRange, UsageFilter};

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
pub struct DistributionUsage {
    pub total_tokens: i64,
    pub estimated_cost_nanos_usd: Option<i64>,
    pub cost_status: DistributionCostStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDistributionRow {
    pub model: String,
    pub usage: DistributionUsage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectDistributionIdentity {
    Project {
        project_name: String,
        project_path: String,
    },
    Projectless,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectDistributionRow {
    pub identity: ProjectDistributionIdentity,
    pub usage: DistributionUsage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyticsSnapshot<T> {
    pub data_revision: i64,
    pub value: T,
}

fn snapshot_meta(connection: &rusqlite::Connection) -> Result<i64, UsageLedgerError> {
    let value = connection
        .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(StorageError::sqlite)?;
    if value < 0 {
        return Err(UsageLedgerError::Invalid(
            "invalid analytics snapshot metadata",
        ));
    }
    Ok(value)
}

fn scoped_where(
    event_alias: &str,
    root_alias: &str,
    range: TimeRange,
    filter: &UsageFilter,
) -> (String, Vec<Value>) {
    let mut values = vec![Value::Integer(range.start_ms), Value::Integer(range.end_ms)];
    let mut clauses = vec![
        format!("{event_alias}.occurred_at_ms>=?1"),
        format!("{event_alias}.occurred_at_ms<?2"),
    ];
    if !filter.sources().is_empty() {
        let mut placeholders = Vec::new();
        for source in filter.sources() {
            values.push(Value::Text(source.as_str().to_owned()));
            placeholders.push(format!("?{}", values.len()));
        }
        let source_col = if event_alias == "se" {
            format!("{root_alias}.source")
        } else {
            format!("{event_alias}.source")
        };
        clauses.push(format!("{source_col} IN ({})", placeholders.join(",")));
    }
    if !filter.models().is_empty() {
        let mut placeholders = Vec::new();
        for model in filter.models() {
            values.push(Value::Text(model.clone()));
            placeholders.push(format!("?{}", values.len()));
        }
        clauses.push(format!(
            "{event_alias}.model IN ({})",
            placeholders.join(",")
        ));
    }
    let mut projects = Vec::new();
    if !filter.project_paths().is_empty() {
        let mut placeholders = Vec::new();
        for path in filter.project_paths() {
            values.push(Value::Text(path.clone()));
            placeholders.push(format!("?{}", values.len()));
        }
        projects.push(format!(
            "({root_alias}.project_kind='project' AND {root_alias}.project_path IN ({}))",
            placeholders.join(",")
        ));
    }
    if filter.include_projectless() {
        projects.push(format!("{root_alias}.project_kind='projectless'"));
    }
    if filter.include_unknown_project() {
        projects.push(format!(
            "({root_alias}.project_kind='unknown' OR {root_alias}.thread_id IS NULL)"
        ));
    }
    if !projects.is_empty() {
        clauses.push(format!("({})", projects.join(" OR ")));
    }
    (clauses.join(" AND "), values)
}

pub(crate) fn model_distribution_query(
    range: TimeRange,
    filter: &UsageFilter,
) -> (String, Vec<Value>) {
    let (where_clause, values) = scoped_where("ue", "root", range, filter);
    let sql = format!(
        "SELECT ue.model,COALESCE(SUM(ue.total_tokens),0),SUM(ue.estimated_cost_nanos_usd),
                SUM(CASE WHEN ue.estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),COUNT(*)
         FROM source_usage_epochs sue
         CROSS JOIN usage_events ue
         LEFT JOIN threads root ON root.thread_id=ue.root_session_id
         WHERE sue.source=ue.source AND sue.active_epoch=ue.source_epoch
           AND {where_clause} GROUP BY ue.model ORDER BY ue.model"
    );
    (sql, values)
}

pub(crate) fn project_distribution_query(
    range: TimeRange,
    filter: &UsageFilter,
) -> (String, Vec<Value>) {
    let (where_clause, values) = scoped_where("ue", "root", range, filter);
    let sql = format!(
        "WITH scoped AS (
           SELECT CASE
                    WHEN root.project_kind='project' AND root.project_name IS NOT NULL AND root.project_path IS NOT NULL THEN 'project'
                    WHEN root.project_kind='projectless' THEN 'projectless'
                    ELSE 'unknown'
                  END AS kind,
                  CASE WHEN root.project_kind='project' AND root.project_name IS NOT NULL AND root.project_path IS NOT NULL THEN root.project_name END AS project_name,
                  CASE WHEN root.project_kind='project' AND root.project_name IS NOT NULL AND root.project_path IS NOT NULL THEN root.project_path END AS project_path,
                  ue.total_tokens,ue.estimated_cost_nanos_usd
           FROM source_usage_epochs sue
           CROSS JOIN usage_events ue
           LEFT JOIN threads root ON root.thread_id=ue.root_session_id
           WHERE sue.source=ue.source AND sue.active_epoch=ue.source_epoch
             AND {where_clause}
         )
         SELECT kind,project_name,project_path,COALESCE(SUM(total_tokens),0),SUM(estimated_cost_nanos_usd),
                SUM(CASE WHEN estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),COUNT(*)
         FROM scoped GROUP BY kind,project_name,project_path ORDER BY kind,project_path"
    );
    (sql, values)
}

fn distribution_usage(
    total_tokens: i64,
    cost: Option<i64>,
    unknown_count: i64,
    event_count: i64,
) -> Result<DistributionUsage, UsageLedgerError> {
    if total_tokens < 0 || unknown_count < 0 || event_count <= 0 || unknown_count > event_count {
        return Err(UsageLedgerError::Aggregate(
            AggregateError::InvariantViolation,
        ));
    }
    let (estimated_cost_nanos_usd, cost_status) = if unknown_count == 0 {
        (Some(cost.unwrap_or(0)), DistributionCostStatus::Complete)
    } else if unknown_count < event_count {
        (cost, DistributionCostStatus::Partial)
    } else {
        (None, DistributionCostStatus::Unknown)
    };
    if estimated_cost_nanos_usd.is_some_and(|value| value < 0) {
        return Err(UsageLedgerError::Aggregate(
            AggregateError::InvariantViolation,
        ));
    }
    Ok(DistributionUsage {
        total_tokens,
        estimated_cost_nanos_usd,
        cost_status,
    })
}

pub fn model_distribution_snapshot(
    ledger: &Ledger,
    range: TimeRange,
    filter: &UsageFilter,
) -> Result<AnalyticsSnapshot<Vec<ModelDistributionRow>>, UsageLedgerError> {
    ledger.with_read_transaction(|transaction| {
        let data_revision = snapshot_meta(transaction)?;
        let (sql, values) = model_distribution_query(range, filter);
        let mut statement = transaction.prepare(&sql).map_err(StorageError::sqlite)?;
        let rows = statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(StorageError::sqlite)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StorageError::sqlite)?;
        let value = rows
            .into_iter()
            .map(|(model, tokens, cost, unknown, count)| {
                Ok(ModelDistributionRow {
                    model,
                    usage: distribution_usage(tokens, cost, unknown, count)?,
                })
            })
            .collect::<Result<Vec<_>, UsageLedgerError>>()?;
        drop(statement);
        Ok(AnalyticsSnapshot {
            data_revision,
            value,
        })
    })
}

pub fn project_distribution_snapshot(
    ledger: &Ledger,
    range: TimeRange,
    filter: &UsageFilter,
) -> Result<AnalyticsSnapshot<Vec<ProjectDistributionRow>>, UsageLedgerError> {
    ledger.with_read_transaction(|transaction| {
        let data_revision = snapshot_meta(transaction)?;
        let (sql, values) = project_distribution_query(range, filter);
        let mut statement = transaction.prepare(&sql).map_err(StorageError::sqlite)?;
        let rows = statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(StorageError::sqlite)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StorageError::sqlite)?;
        let value = rows
            .into_iter()
            .map(|(kind, name, path, tokens, cost, unknown, count)| {
                let identity = match (kind.as_str(), name, path) {
                    ("project", Some(project_name), Some(project_path)) => {
                        ProjectDistributionIdentity::Project {
                            project_name,
                            project_path,
                        }
                    }
                    ("projectless", _, _) => ProjectDistributionIdentity::Projectless,
                    _ => ProjectDistributionIdentity::Unknown,
                };
                Ok(ProjectDistributionRow {
                    identity,
                    usage: distribution_usage(tokens, cost, unknown, count)?,
                })
            })
            .collect::<Result<Vec<_>, UsageLedgerError>>()?;
        drop(statement);
        Ok(AnalyticsSnapshot {
            data_revision,
            value,
        })
    })
}
