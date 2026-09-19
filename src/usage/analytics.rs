use rusqlite::{TransactionBehavior, params_from_iter, types::Value};

use crate::{
    range::ResolvedDay,
    storage::{Ledger, StorageError},
};

use super::{
    aggregate::{AggregateError, TimeRange, UsageFilter},
    ledger::UsageLedgerError,
};

pub const SKILL_USAGE_PARSER_VERSION: i64 = 11;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyticsSnapshot<T> {
    pub data_revision: i64,
    pub active_epoch: i64,
    pub value: T,
}

fn snapshot_meta(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(i64, i64, i64), UsageLedgerError> {
    let values = transaction
        .query_row(
            "SELECT data_revision,usage_active_epoch,usage_parser_version FROM app_meta WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(StorageError::sqlite)?;
    if values.0 < 0 || values.1 < 0 || values.2 < 0 {
        return Err(UsageLedgerError::Invalid(
            "invalid analytics snapshot metadata",
        ));
    }
    Ok(values)
}

fn scoped_where(
    event_alias: &str,
    root_alias: &str,
    epoch: i64,
    range: TimeRange,
    filter: &UsageFilter,
) -> (String, Vec<Value>) {
    let mut values = vec![
        Value::Integer(epoch),
        Value::Integer(range.start_ms),
        Value::Integer(range.end_ms),
    ];
    let mut clauses = vec![
        format!("{event_alias}.ledger_epoch=?1"),
        format!("{event_alias}.occurred_at_ms>=?2"),
        format!("{event_alias}.occurred_at_ms<?3"),
    ];
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
    let mut connection = ledger.connection()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(StorageError::sqlite)?;
    let (data_revision, active_epoch, _) = snapshot_meta(&transaction)?;
    let (where_clause, values) = scoped_where("ue", "root", active_epoch, range, filter);
    let sql = format!(
        "SELECT ue.model,COALESCE(SUM(ue.total_tokens),0),SUM(ue.estimated_cost_nanos_usd),
                SUM(CASE WHEN ue.estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),COUNT(*)
         FROM usage_events ue LEFT JOIN threads root ON root.thread_id=ue.root_session_id
         WHERE {where_clause} GROUP BY ue.model ORDER BY ue.model"
    );
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
    transaction.commit().map_err(StorageError::sqlite)?;
    Ok(AnalyticsSnapshot {
        data_revision,
        active_epoch,
        value,
    })
}

pub fn project_distribution_snapshot(
    ledger: &Ledger,
    range: TimeRange,
    filter: &UsageFilter,
) -> Result<AnalyticsSnapshot<Vec<ProjectDistributionRow>>, UsageLedgerError> {
    let mut connection = ledger.connection()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(StorageError::sqlite)?;
    let (data_revision, active_epoch, _) = snapshot_meta(&transaction)?;
    let (where_clause, values) = scoped_where("ue", "root", active_epoch, range, filter);
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
           FROM usage_events ue LEFT JOIN threads root ON root.thread_id=ue.root_session_id
           WHERE {where_clause}
         )
         SELECT kind,project_name,project_path,COALESCE(SUM(total_tokens),0),SUM(estimated_cost_nanos_usd),
                SUM(CASE WHEN estimated_cost_nanos_usd IS NULL THEN 1 ELSE 0 END),COUNT(*)
         FROM scoped GROUP BY kind,project_name,project_path ORDER BY kind,project_path"
    );
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
    transaction.commit().map_err(StorageError::sqlite)?;
    Ok(AnalyticsSnapshot {
        data_revision,
        active_epoch,
        value,
    })
}

pub fn skills_usage_snapshot(
    ledger: &Ledger,
    days: &[ResolvedDay],
    filter: &UsageFilter,
) -> Result<AnalyticsSnapshot<SkillsUsage>, UsageLedgerError> {
    let mut connection = ledger.connection()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(StorageError::sqlite)?;
    let (data_revision, active_epoch, active_parser) = snapshot_meta(&transaction)?;
    let ready = active_epoch > 0 && active_parser >= SKILL_USAGE_PARSER_VERSION;
    let mut output = Vec::with_capacity(days.len());
    for day in days {
        let range = TimeRange::new(day.start_ms, day.end_ms)?;
        let (where_clause, values) = scoped_where("se", "root", active_epoch, range, filter);
        let mut skills = if ready {
            let sql = format!(
                "SELECT se.skill_name,COUNT(*) FROM skill_usage_events se
                 LEFT JOIN threads root ON root.thread_id=se.root_session_id
                 WHERE {where_clause} GROUP BY se.skill_name
                 ORDER BY COUNT(*) DESC,se.skill_name ASC"
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
            sum.checked_add(row.count)
                .ok_or(UsageLedgerError::Aggregate(
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
    transaction.commit().map_err(StorageError::sqlite)?;
    Ok(AnalyticsSnapshot {
        data_revision,
        active_epoch,
        value: SkillsUsage {
            ready,
            days: output,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::storage::LedgerOptions;

    use super::*;

    fn ledger_with_active_parser(parser_version: i64) -> (Ledger, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("usagi-s07-analytics-{unique}"));
        fs::create_dir_all(&root).unwrap();
        let ledger = Ledger::open(LedgerOptions::new(
            root.join("mu.sqlite3"),
            root.join("codex"),
        ))
        .unwrap();
        ledger
            .connection()
            .unwrap()
            .execute(
                "UPDATE app_meta SET usage_active_epoch=1,usage_parser_version=?1 WHERE id=1",
                [parser_version],
            )
            .unwrap();
        (ledger, root)
    }

    #[test]
    fn t_s07_002_skills_ready_requires_parser_v11() {
        assert_eq!(SKILL_USAGE_PARSER_VERSION, 11);
        for (parser_version, expected_ready) in [(10, false), (11, true)] {
            let (ledger, root) = ledger_with_active_parser(parser_version);
            let snapshot = skills_usage_snapshot(&ledger, &[], &UsageFilter::default()).unwrap();
            assert_eq!(snapshot.value.ready, expected_ready);
            drop(ledger);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
