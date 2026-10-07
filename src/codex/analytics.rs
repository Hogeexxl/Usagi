//! Codex-specific read sidecars and Skills analytics.

use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    range::ResolvedDay,
    storage::{Ledger, StorageError},
    usage::{
        aggregate::{
            AggregateError, SessionCompactionProjection, SessionDetail, SessionDetailSidecar,
            SessionErrorProjection, SessionErrorSidecar, TimeRange, UsageFilter,
        },
        analytics::AnalyticsSnapshot,
        ledger::UsageLedgerError,
    },
};

pub(crate) const SKILL_USAGE_PARSER_VERSION: i64 = 11;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CompactionVisibleEvent {
    pub event_id: String,
    pub thread_id: String,
    pub root_session_id: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub occurred_at_ms: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CompactionUnknownScope {
    pub thread_id: String,
    pub root_session_id: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CompactionCandidateEvent {
    thread_id: String,
    root_session_id: String,
    model: String,
    reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactionVisibilitySignature {
    pub ready: bool,
    pub events: Vec<CompactionVisibleEvent>,
    pub unknown_scopes: Vec<CompactionUnknownScope>,
}

pub(crate) const fn compaction_ready(epoch: i64, parser_version: i64) -> bool {
    epoch > 0 && parser_version >= 12
}

/// Describes only the Compaction classification users can observe. Physical
/// occurrence counts, marker reasons, and epoch numbers do not affect it.
pub(crate) fn compaction_visibility_signature(
    connection: &Connection,
    epoch: i64,
    parser_version: i64,
) -> rusqlite::Result<CompactionVisibilitySignature> {
    compaction_visibility_signature_scoped(connection, epoch, parser_version, None)
}

pub(crate) fn compaction_visibility_signature_for_owner(
    connection: &Connection,
    epoch: i64,
    parser_version: i64,
    owning_thread_id: &str,
) -> rusqlite::Result<CompactionVisibilitySignature> {
    compaction_visibility_signature_scoped(
        connection,
        epoch,
        parser_version,
        Some(owning_thread_id),
    )
}

fn compaction_visibility_signature_scoped(
    connection: &Connection,
    epoch: i64,
    parser_version: i64,
    owner_scope: Option<&str>,
) -> rusqlite::Result<CompactionVisibilitySignature> {
    let ready = compaction_ready(epoch, parser_version);
    let mut signature = CompactionVisibilitySignature {
        ready,
        events: Vec::new(),
        unknown_scopes: Vec::new(),
    };
    if !ready {
        return Ok(signature);
    }

    let mut candidates = BTreeSet::new();
    let mut candidate_statement = connection.prepare(
        "SELECT thread_id,root_session_id,model,reasoning_effort
         FROM usage_events
         WHERE source='codex' AND source_epoch=?1 AND model IS NOT NULL
           AND (?2 IS NULL OR thread_id=?2)
         ORDER BY thread_id,root_session_id,model,reasoning_effort,event_id",
    )?;
    for row in candidate_statement.query_map(rusqlite::params![epoch, owner_scope], |row| {
        Ok(CompactionCandidateEvent {
            thread_id: row.get(0)?,
            root_session_id: row.get(1)?,
            model: row.get(2)?,
            reasoning_effort: row.get(3)?,
        })
    })? {
        candidates.insert(row?);
    }

    let mut events = BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT f.event_id,e.thread_id,e.root_session_id,e.model,e.reasoning_effort,
                e.occurred_at_ms,e.total_tokens
         FROM codex_usage_event_facts f
         JOIN usage_events e ON e.source=f.source AND e.source_epoch=f.ledger_epoch
                            AND e.event_id=f.event_id
         WHERE f.source='codex' AND f.ledger_epoch=?1 AND f.operation='compaction'
           AND (?2 IS NULL OR e.thread_id=?2)
         ORDER BY f.event_id",
    )?;
    for row in statement.query_map(rusqlite::params![epoch, owner_scope], |row| {
        Ok(CompactionVisibleEvent {
            event_id: row.get(0)?,
            thread_id: row.get(1)?,
            root_session_id: row.get(2)?,
            model: row.get(3)?,
            reasoning_effort: row.get(4)?,
            occurred_at_ms: row.get(5)?,
            total_tokens: row.get(6)?,
        })
    })? {
        events.insert(row?);
    }
    signature.events = events.into_iter().collect();

    let mut unknown = BTreeSet::new();
    let mut markers = connection.prepare(
        "SELECT m.source_file_id,m.file_generation,m.source_start_offset,m.owning_thread_id,
                m.root_session_id,m.occurred_at_ms,m.model,m.reasoning_effort,
                t.started_at_ms,t.ended_at_ms
         FROM codex_compaction_markers m
         LEFT JOIN codex_turns t
           ON t.ledger_epoch=m.ledger_epoch AND t.source_file_id=m.source_file_id
              AND t.file_generation=m.file_generation AND t.thread_id=m.owning_thread_id
              AND t.start_offset<=m.source_start_offset
              AND (t.end_offset IS NULL OR m.source_start_offset<t.end_offset)
         WHERE m.source='codex' AND m.ledger_epoch=?1 AND m.resolved_event_id IS NULL
           AND (?2 IS NULL OR m.owning_thread_id=?2)
         ORDER BY m.source_file_id,m.file_generation,m.source_start_offset,t.start_offset DESC",
    )?;
    let mut seen_markers = BTreeSet::new();
    for row in markers.query_map(rusqlite::params![epoch, owner_scope], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, Option<i64>>(9)?,
        ))
    })? {
        let (
            source_id,
            generation,
            offset,
            thread,
            root,
            occurred,
            model,
            effort,
            turn_start,
            turn_end,
        ) = row?;
        if !seen_markers.insert((source_id, generation, offset)) {
            continue;
        }
        let (start_ms, end_ms) = match occurred {
            Some(occurred) => (Some(occurred), occurred.checked_add(1)),
            None => (turn_start, turn_end.and_then(|ended| ended.checked_add(1))),
        };
        add_compaction_unknown_candidates(
            &mut unknown,
            &candidates,
            &thread,
            &root,
            model.as_deref(),
            effort.as_deref(),
            start_ms,
            end_ms,
        );
    }

    let mut sources = connection.prepare(
        "SELECT COALESCE(st.owning_thread_id,sf.thread_id),
                COALESCE(st.root_session_id,t.root_session_id),
                sf.source_file_id,sf.file_generation,
                sf.observed_size,cp.parser_version,cp.committed_offset,cp.processing_status,
                st.raw_tail_status,st.file_generation,st.usage_parser_version,
                st.resolved_through_offset,st.observed_raw_size,turn.started_at_ms
         FROM codex_source_files sf
         LEFT JOIN threads t ON t.thread_id=sf.thread_id AND t.source='codex'
         LEFT JOIN codex_source_checkpoints cp
           ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'
         LEFT JOIN codex_usage_source_states st
           ON st.ledger_epoch=?1 AND st.source_file_id=sf.source_file_id
         LEFT JOIN codex_turns turn
           ON turn.ledger_epoch=?1 AND turn.source_file_id=sf.source_file_id
              AND turn.file_generation=st.file_generation
              AND turn.thread_id=st.owning_thread_id
              AND turn.turn_key=st.active_turn_key AND turn.status='open'
         WHERE (sf.file_status='present' OR st.source_file_id IS NOT NULL)
           AND (?2 IS NULL OR COALESCE(st.owning_thread_id,sf.thread_id)=?2)
         ORDER BY sf.source_file_id",
    )?;
    for row in sources.query_map(rusqlite::params![epoch, owner_scope], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<i64>>(9)?,
            row.get::<_, Option<i64>>(10)?,
            row.get::<_, Option<i64>>(11)?,
            row.get::<_, Option<i64>>(12)?,
            row.get::<_, Option<i64>>(13)?,
        ))
    })? {
        let (
            thread,
            root,
            _source_id,
            generation,
            observed,
            parser,
            offset,
            status,
            tail,
            state_generation,
            state_parser,
            resolved_through,
            state_observed,
            turn_started_at,
        ) = row?;
        let (Some(thread), Some(root)) = (thread, root) else {
            continue;
        };
        let complete = parser == Some(parser_version)
            && status.as_deref() == Some("ready")
            && offset == Some(observed)
            && tail.as_deref() == Some("none")
            && state_generation == Some(generation)
            && state_parser == Some(parser_version)
            && resolved_through == Some(observed)
            && state_observed == Some(observed);
        if complete {
            continue;
        }
        add_compaction_unknown_candidates(
            &mut unknown,
            &candidates,
            &thread,
            &root,
            None,
            None,
            turn_started_at,
            None,
        );
    }
    signature.unknown_scopes = merge_compaction_unknown_scopes(unknown);
    Ok(signature)
}

fn merge_compaction_unknown_scopes(
    unknown: BTreeSet<CompactionUnknownScope>,
) -> Vec<CompactionUnknownScope> {
    let mut merged: Vec<CompactionUnknownScope> = Vec::new();
    for scope in unknown {
        if let Some(previous) = merged.last_mut() {
            let same_block = previous.thread_id == scope.thread_id
                && previous.root_session_id == scope.root_session_id
                && previous.model == scope.model
                && previous.reasoning_effort == scope.reasoning_effort;
            let overlaps = previous.end_ms.is_none()
                || scope.start_ms.is_none()
                || scope.start_ms <= previous.end_ms;
            if same_block && overlaps {
                previous.end_ms = match (previous.end_ms, scope.end_ms) {
                    (None, _) | (_, None) => None,
                    (Some(previous_end), Some(scope_end)) => Some(previous_end.max(scope_end)),
                };
                continue;
            }
        }
        merged.push(scope);
    }
    merged
}

fn add_compaction_unknown_candidates(
    unknown: &mut BTreeSet<CompactionUnknownScope>,
    candidates: &BTreeSet<CompactionCandidateEvent>,
    thread_id: &str,
    root_session_id: &str,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) {
    for candidate in candidates {
        if candidate.thread_id != thread_id
            || candidate.root_session_id != root_session_id
            || model.is_some_and(|value| candidate.model != value)
            || reasoning_effort
                .is_some_and(|value| candidate.reasoning_effort.as_deref() != Some(value))
        {
            continue;
        }
        unknown.insert(CompactionUnknownScope {
            thread_id: candidate.thread_id.clone(),
            root_session_id: candidate.root_session_id.clone(),
            model: candidate.model.clone(),
            reasoning_effort: candidate.reasoning_effort.clone(),
            start_ms,
            end_ms,
        });
    }
}

pub struct CodexSessionDetailSidecar;

impl SessionDetailSidecar for CodexSessionDetailSidecar {
    fn compaction_usage(
        &self,
        connection: &Connection,
        range: TimeRange,
        filter: &UsageFilter,
        detail: &SessionDetail,
    ) -> Result<Vec<SessionCompactionProjection>, AggregateError> {
        if detail.source != "codex"
            || (!filter.sources().is_empty()
                && !filter
                    .sources()
                    .iter()
                    .any(|source| source.as_str() == "codex"))
        {
            return Ok(Vec::new());
        }

        let mut targets =
            BTreeMap::<(String, String, Option<String>), CompactionCandidateEvent>::new();
        if detail.main.source == "codex" {
            for model in &detail.main.model_usage {
                if filter.models().is_empty() || filter.models().contains(&model.model) {
                    let candidate = CompactionCandidateEvent {
                        thread_id: detail.main.thread_id.clone(),
                        root_session_id: detail.root_session_id.clone(),
                        model: model.model.clone(),
                        reasoning_effort: model.reasoning_effort.clone(),
                    };
                    targets.insert(
                        (
                            candidate.thread_id.clone(),
                            candidate.model.clone(),
                            candidate.reasoning_effort.clone(),
                        ),
                        candidate,
                    );
                }
            }
        }
        for subagent in &detail.subagents {
            if subagent.source != "codex" {
                continue;
            }
            for model in &subagent.model_usage {
                if filter.models().is_empty() || filter.models().contains(&model.model) {
                    let candidate = CompactionCandidateEvent {
                        thread_id: subagent.thread_id.clone(),
                        root_session_id: detail.root_session_id.clone(),
                        model: model.model.clone(),
                        reasoning_effort: model.reasoning_effort.clone(),
                    };
                    targets.insert(
                        (
                            candidate.thread_id.clone(),
                            candidate.model.clone(),
                            candidate.reasoning_effort.clone(),
                        ),
                        candidate,
                    );
                }
            }
        }
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        let mut projections = targets
            .keys()
            .map(
                |(thread_id, model, reasoning_effort)| SessionCompactionProjection {
                    thread_id: thread_id.clone(),
                    model: model.clone(),
                    reasoning_effort: reasoning_effort.clone(),
                    compaction_tokens: None,
                },
            )
            .collect::<Vec<_>>();
        let active = connection
            .query_row(
                "SELECT active_epoch,active_parser_version FROM source_usage_epochs
                 WHERE source='codex'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()
            .map_err(map_sql_error)?;
        let Some((epoch, Some(parser_version))) = active else {
            return Ok(projections);
        };
        if !compaction_ready(epoch, parser_version) {
            return Ok(projections);
        }

        let target_rows = (0..targets.len())
            .map(|index| {
                let start = index * 3 + 1;
                format!("(?{start},?{},?{})", start + 1, start + 2)
            })
            .collect::<Vec<_>>()
            .join(",");
        let target_values = targets
            .values()
            .flat_map(|target| {
                [
                    Value::Text(target.thread_id.clone()),
                    Value::Text(target.model.clone()),
                    target
                        .reasoning_effort
                        .clone()
                        .map(Value::Text)
                        .unwrap_or(Value::Null),
                ]
            })
            .collect::<Vec<_>>();

        let epoch_parameter = target_values.len() + 1;
        let root_parameter = epoch_parameter + 1;
        let start_parameter = root_parameter + 1;
        let end_parameter = start_parameter + 1;
        let mut event_values = target_values.clone();
        event_values.extend([
            Value::Integer(epoch),
            Value::Text(detail.root_session_id.clone()),
            Value::Integer(range.start_ms),
            Value::Integer(range.end_ms),
        ]);
        let event_sql = format!(
            "WITH targets(thread_id,model,reasoning_effort) AS (VALUES {target_rows})
             SELECT e.event_id,e.thread_id,e.model,e.reasoning_effort,e.total_tokens
             FROM targets target
             JOIN usage_events e ON e.thread_id=target.thread_id AND e.model=target.model
                                AND e.reasoning_effort IS target.reasoning_effort
             JOIN codex_usage_event_facts f
               ON f.source=e.source AND f.ledger_epoch=e.source_epoch AND f.event_id=e.event_id
             WHERE e.source='codex' AND e.source_epoch=?{epoch_parameter}
               AND f.source='codex' AND f.ledger_epoch=?{epoch_parameter}
               AND f.operation='compaction' AND e.root_session_id=?{root_parameter}
               AND e.occurred_at_ms>=?{start_parameter} AND e.occurred_at_ms<?{end_parameter}
             ORDER BY e.thread_id,e.model,e.reasoning_effort,e.event_id"
        );
        let mut event_statement = connection.prepare(&event_sql).map_err(map_sql_error)?;
        let event_rows = event_statement
            .query_map(params_from_iter(event_values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        let mut totals = BTreeMap::<(String, String, Option<String>), i64>::new();
        let mut seen_events = BTreeSet::new();
        for (event_id, thread_id, model, reasoning_effort, tokens) in event_rows {
            if !seen_events.insert(event_id) || tokens < 0 {
                return Err(AggregateError::InvariantViolation);
            }
            let total = totals
                .entry((thread_id, model, reasoning_effort))
                .or_default();
            *total = total
                .checked_add(tokens)
                .ok_or(AggregateError::ArithmeticOverflow)?;
        }
        for projection in &mut projections {
            projection.compaction_tokens = Some(
                totals
                    .get(&(
                        projection.thread_id.clone(),
                        projection.model.clone(),
                        projection.reasoning_effort.clone(),
                    ))
                    .copied()
                    .unwrap_or(0),
            );
        }

        let mut unknown_scopes = BTreeSet::new();
        let owner_ids = targets
            .keys()
            .map(|(thread_id, _, _)| thread_id.clone())
            .collect::<BTreeSet<_>>();
        let candidates = targets.values().cloned().collect::<BTreeSet<_>>();
        let owner_parameters = (0..owner_ids.len())
            .map(|index| format!("?{}", target_values.len() + index + 5))
            .collect::<Vec<_>>()
            .join(",");
        let mut marker_values = target_values;
        marker_values.extend([
            Value::Integer(epoch),
            Value::Text(detail.root_session_id.clone()),
            Value::Integer(range.start_ms),
            Value::Integer(range.end_ms),
        ]);
        marker_values.extend(owner_ids.iter().cloned().map(Value::Text));
        let marker_epoch_parameter = marker_values.len() - owner_ids.len() - 3;
        let marker_root_parameter = marker_epoch_parameter + 1;
        let marker_start_parameter = marker_root_parameter + 1;
        let marker_end_parameter = marker_start_parameter + 1;
        let marker_sql = format!(
            "WITH targets(thread_id,model,reasoning_effort) AS (VALUES {target_rows})
             SELECT m.source_file_id,m.file_generation,m.source_start_offset,m.owning_thread_id,
                    m.root_session_id,m.occurred_at_ms,m.model,m.reasoning_effort,
                    t.started_at_ms,t.ended_at_ms
             FROM codex_compaction_markers m
             LEFT JOIN codex_turns t
               ON t.ledger_epoch=m.ledger_epoch AND t.source_file_id=m.source_file_id
                  AND t.file_generation=m.file_generation AND t.thread_id=m.owning_thread_id
                  AND t.start_offset<=m.source_start_offset
                  AND (t.end_offset IS NULL OR m.source_start_offset<t.end_offset)
             WHERE m.source='codex' AND m.ledger_epoch=?{marker_epoch_parameter}
               AND m.root_session_id=?{marker_root_parameter}
               AND m.resolved_event_id IS NULL AND m.owning_thread_id IN ({owner_parameters})
               AND EXISTS (
                   SELECT 1 FROM targets candidate
                   WHERE candidate.thread_id=m.owning_thread_id
                     AND (m.model IS NULL OR m.model=candidate.model)
                     AND (m.reasoning_effort IS NULL
                          OR m.reasoning_effort=candidate.reasoning_effort)
               )
               AND ((m.occurred_at_ms IS NOT NULL
                     AND m.occurred_at_ms>=?{marker_start_parameter}
                     AND m.occurred_at_ms<?{marker_end_parameter})
                    OR (m.occurred_at_ms IS NULL
                        AND (t.started_at_ms IS NULL OR t.started_at_ms<?{marker_end_parameter})
                        AND (t.ended_at_ms IS NULL OR t.ended_at_ms>=?{marker_start_parameter})))
             ORDER BY m.source_file_id,m.file_generation,m.source_start_offset,t.start_offset DESC"
        );
        let mut marker_statement = connection.prepare(&marker_sql).map_err(map_sql_error)?;
        let marker_rows = marker_statement
            .query_map(params_from_iter(marker_values.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        let mut seen_markers = BTreeSet::new();
        for (
            source_id,
            generation,
            offset,
            thread_id,
            root_session_id,
            occurred_at_ms,
            model,
            reasoning_effort,
            turn_start,
            turn_end,
        ) in marker_rows
        {
            if !seen_markers.insert((source_id, generation, offset)) {
                continue;
            }
            let (start_ms, end_ms) = match occurred_at_ms {
                Some(occurred) => (Some(occurred), occurred.checked_add(1)),
                None => (turn_start, turn_end.and_then(|ended| ended.checked_add(1))),
            };
            add_compaction_unknown_candidates(
                &mut unknown_scopes,
                &candidates,
                &thread_id,
                &root_session_id,
                model.as_deref(),
                reasoning_effort.as_deref(),
                start_ms,
                end_ms,
            );
        }

        let source_values = std::iter::once(Value::Integer(epoch))
            .chain(std::iter::once(Value::Text(detail.root_session_id.clone())))
            .chain(std::iter::once(Value::Integer(range.end_ms)))
            .chain(owner_ids.iter().cloned().map(Value::Text))
            .collect::<Vec<_>>();
        let source_epoch_parameter = 1;
        let source_root_parameter = 2;
        let source_end_parameter = 3;
        let source_owner_parameters = (0..owner_ids.len())
            .map(|index| format!("?{}", index + 4))
            .collect::<Vec<_>>()
            .join(",");
        let source_sql = format!(
            "SELECT COALESCE(st.owning_thread_id,sf.thread_id),
                    COALESCE(st.root_session_id,thread.root_session_id),
                    sf.file_generation,sf.observed_size,cp.parser_version,cp.committed_offset,
                    cp.processing_status,st.raw_tail_status,st.file_generation,
                    st.usage_parser_version,st.resolved_through_offset,st.observed_raw_size,
                    turn.started_at_ms
             FROM codex_source_files sf
             LEFT JOIN threads thread ON thread.thread_id=sf.thread_id AND thread.source='codex'
             LEFT JOIN codex_source_checkpoints cp
               ON cp.source_file_id=sf.source_file_id AND cp.consumer_kind='usage'
             LEFT JOIN codex_usage_source_states st
               ON st.ledger_epoch=?{source_epoch_parameter} AND st.source_file_id=sf.source_file_id
             LEFT JOIN codex_turns turn
               ON turn.ledger_epoch=?{source_epoch_parameter}
                  AND turn.source_file_id=sf.source_file_id
                  AND turn.file_generation=st.file_generation
                  AND turn.thread_id=st.owning_thread_id
                  AND turn.turn_key=st.active_turn_key AND turn.status='open'
             WHERE (sf.file_status='present' OR st.source_file_id IS NOT NULL)
               AND COALESCE(st.owning_thread_id,sf.thread_id) IN ({source_owner_parameters})
               AND COALESCE(st.root_session_id,thread.root_session_id)=?{source_root_parameter}
               AND (turn.started_at_ms IS NULL OR turn.started_at_ms<?{source_end_parameter})
             ORDER BY sf.source_file_id"
        );
        let mut source_statement = connection.prepare(&source_sql).map_err(map_sql_error)?;
        let source_rows = source_statement
            .query_map(params_from_iter(source_values.iter()), |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                ))
            })
            .map_err(map_sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(map_sql_error)?;
        for (
            thread_id,
            root_session_id,
            generation,
            observed_size,
            checkpoint_parser,
            checkpoint_offset,
            checkpoint_status,
            tail_status,
            state_generation,
            state_parser,
            resolved_through,
            state_observed_size,
            turn_start,
        ) in source_rows
        {
            let (Some(thread_id), Some(root_session_id)) = (thread_id, root_session_id) else {
                continue;
            };
            let complete = checkpoint_parser == Some(parser_version)
                && checkpoint_status.as_deref() == Some("ready")
                && checkpoint_offset == Some(observed_size)
                && tail_status.as_deref() == Some("none")
                && state_generation == Some(generation)
                && state_parser == Some(parser_version)
                && resolved_through == Some(observed_size)
                && state_observed_size == Some(observed_size);
            if !complete {
                add_compaction_unknown_candidates(
                    &mut unknown_scopes,
                    &candidates,
                    &thread_id,
                    &root_session_id,
                    None,
                    None,
                    turn_start,
                    None,
                );
            }
        }

        for scope in merge_compaction_unknown_scopes(unknown_scopes) {
            if compaction_scope_intersects(scope.start_ms, scope.end_ms, range) {
                if let Some(projection) = projections.iter_mut().find(|projection| {
                    projection.thread_id == scope.thread_id
                        && projection.model == scope.model
                        && projection.reasoning_effort == scope.reasoning_effort
                }) {
                    projection.compaction_tokens = None;
                }
            }
        }
        Ok(projections)
    }
}

fn compaction_scope_intersects(
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    range: TimeRange,
) -> bool {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) => start < range.end_ms && range.start_ms < end,
        (Some(start), None) => start < range.end_ms,
        (None, Some(end)) => range.start_ms < end,
        (None, None) => true,
    }
}

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
        let sources = filter.sources();
        let codex_source_included = sources.is_empty()
            || sources
                .iter()
                .any(|source| source == &crate::source::SourceId::CODEX);
        if filter.models().iter().next().is_some() || !codex_source_included {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{source::SourceId, storage::migrate, usage::AggregateReader};
    use rusqlite::params;

    #[test]
    fn td_p4_sidecar_or_01_filter_table_driven() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&mut conn, 0).unwrap();

        conn.execute(
            "INSERT INTO source_usage_epochs(source, active_epoch, build_epoch, active_parser_version, build_parser_version)
             VALUES ('codex', 1, NULL, 1, NULL)
             ON CONFLICT(source) DO UPDATE SET active_epoch=1, build_epoch=NULL, active_parser_version=1, build_parser_version=NULL",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO threads(thread_id, source, native_session_id, parent_thread_id, root_session_id, agent_role, project_kind, archived, metadata_quality_status, metadata_resolved_at_ms)
             VALUES ('root-codex-1', 'codex', 'native-1', NULL, 'root-codex-1', 'main', 'project', 0, 'complete', 100)",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO codex_usage_session_quarantine(ledger_epoch, root_session_id, primary_error_code, last_activity_at_ms, first_seen_at_ms, updated_at_ms)
             VALUES (1, 'root-codex-1', 'TEST_ERROR', 100, 100, 100)",
            [],
        ).unwrap();

        let sidecar = CodexSessionErrorSidecar;
        let range = TimeRange::new(0, 1000).unwrap();

        // 1. []
        let filter_empty = UsageFilter::default();
        let roots_empty = sidecar.error_roots(&conn, range, &filter_empty).unwrap();
        assert_eq!(roots_empty.len(), 1);
        assert_eq!(roots_empty[0].root_session_id, "root-codex-1");

        // 2. [codex]
        let filter_codex = UsageFilter::default().with_sources(vec![SourceId::CODEX]);
        let roots_codex = sidecar.error_roots(&conn, range, &filter_codex).unwrap();
        assert_eq!(roots_codex, roots_empty);

        // 3. [antigravity]
        let filter_ag = UsageFilter::default().with_sources(vec![SourceId::ANTIGRAVITY]);
        let roots_ag = sidecar.error_roots(&conn, range, &filter_ag).unwrap();
        assert!(
            roots_ag.is_empty(),
            "[antigravity] filter must not return Codex error roots"
        );

        // 4. [codex, antigravity]
        let filter_both =
            UsageFilter::default().with_sources(vec![SourceId::CODEX, SourceId::ANTIGRAVITY]);
        let roots_both = sidecar.error_roots(&conn, range, &filter_both).unwrap();
        assert_eq!(roots_both, roots_empty);
    }

    fn insert_detail_thread(
        connection: &Connection,
        thread_id: &str,
        parent_thread_id: Option<&str>,
        root_session_id: &str,
        agent_role: &str,
    ) {
        connection
            .execute(
                "INSERT INTO threads(thread_id,source,native_session_id,parent_thread_id,
                                     root_session_id,agent_role,project_kind,archived,
                                     metadata_quality_status,metadata_resolved_at_ms)
                 VALUES (?1,'codex',?1,?2,?3,?4,'project',0,'complete',1)",
                params![thread_id, parent_thread_id, root_session_id, agent_role],
            )
            .unwrap();
    }

    fn insert_detail_event(
        connection: &Connection,
        event_id: &str,
        thread_id: &str,
        root_session_id: &str,
        model: &str,
        effort: &str,
        occurred_at_ms: i64,
        total_tokens: i64,
        compaction_tokens: Option<i64>,
    ) {
        connection
            .execute(
                "INSERT INTO usage_events(source,source_epoch,event_id,event_kind,occurred_at_ms,
                     thread_id,root_session_id,turn_key,model,reasoning_effort,
                     estimated_cost_nanos_usd,input_tokens,cached_tokens,cache_write_tokens,
                     output_tokens,reasoning_tokens,total_tokens,quality_status,created_at_ms)
                 VALUES ('codex',1,?1,'normal',?2,?3,?4,NULL,?5,?6,NULL,?7,0,0,0,0,?8,
                         'complete',?2)",
                params![
                    event_id,
                    occurred_at_ms,
                    thread_id,
                    root_session_id,
                    model,
                    effort,
                    total_tokens,
                    total_tokens,
                ],
            )
            .unwrap();
        if let Some(tokens) = compaction_tokens {
            assert_eq!(tokens, total_tokens);
            connection
                .execute(
                    "INSERT INTO codex_usage_event_facts(source,ledger_epoch,event_id,
                         owning_thread_id,response_id,evidence_kind,operation)
                     VALUES ('codex',1,?1,?2,?3,'explicit','compaction')",
                    params![event_id, thread_id, format!("response-{event_id}")],
                )
                .unwrap();
            assert!(tokens <= total_tokens);
        }
    }

    fn detail_model(
        detail: &SessionDetail,
        thread_id: &str,
        model: &str,
        effort: &str,
    ) -> Option<i64> {
        if detail.main.thread_id == thread_id {
            return detail
                .main
                .model_usage
                .iter()
                .find(|block| {
                    block.model == model && block.reasoning_effort.as_deref() == Some(effort)
                })
                .and_then(|block| block.compaction_tokens);
        }
        detail
            .subagents
            .iter()
            .find(|subagent| subagent.thread_id == thread_id)?
            .model_usage
            .iter()
            .find(|block| block.model == model && block.reasoning_effort.as_deref() == Some(effort))
            .and_then(|block| block.compaction_tokens)
    }

    #[test]
    fn compaction_upgrade_readiness_requires_active_parser_12() {
        assert!(!compaction_ready(0, 12));
        assert!(!compaction_ready(1, 11));
        assert!(compaction_ready(1, 12));
        assert!(compaction_ready(2, 13));
    }

    #[test]
    fn compaction_detail_scope_uses_root_range_model_effort_and_unknown_visibility() {
        // Constructed A/B/C detail: root A, child B, and child C are synthetic rows.
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        migrate(&mut connection, 0).unwrap();
        connection
            .execute(
                "INSERT INTO source_usage_epochs(source,active_epoch,build_epoch,
                    active_parser_version,build_parser_version)
                 VALUES ('codex',1,NULL,12,NULL)
                 ON CONFLICT(source) DO UPDATE SET active_epoch=1,build_epoch=NULL,
                     active_parser_version=12,build_parser_version=NULL",
                [],
            )
            .unwrap();
        for (thread_id, parent, root_id, role) in [
            ("root-a", None, "root-a", "main"),
            ("child-b", Some("root-a"), "root-a", "subagent"),
            ("child-c", Some("root-a"), "root-a", "subagent"),
            ("other-root", None, "other-root", "main"),
        ] {
            insert_detail_thread(&connection, thread_id, parent, root_id, role);
        }

        // Each candidate has canonical usage in the queried range. The extra
        // same-owner event under another root proves root scoping.
        insert_detail_event(
            &connection,
            "a-medium-base",
            "root-a",
            "root-a",
            "gpt-6",
            "medium",
            10,
            80,
            None,
        );
        insert_detail_event(
            &connection,
            "a-medium-compaction",
            "root-a",
            "root-a",
            "gpt-6",
            "medium",
            10,
            20,
            Some(20),
        );
        insert_detail_event(
            &connection,
            "a-high-base",
            "root-a",
            "root-a",
            "gpt-6",
            "high",
            30,
            150,
            None,
        );
        insert_detail_event(
            &connection,
            "a-high-compaction",
            "root-a",
            "root-a",
            "gpt-6",
            "high",
            30,
            50,
            Some(50),
        );
        insert_detail_event(
            &connection,
            "b-high-base",
            "child-b",
            "root-a",
            "gpt-6",
            "high",
            40,
            210,
            None,
        );
        insert_detail_event(
            &connection,
            "b-high-compaction",
            "child-b",
            "root-a",
            "gpt-6",
            "high",
            40,
            90,
            Some(90),
        );
        insert_detail_event(
            &connection,
            "a-low",
            "root-a",
            "root-a",
            "gpt-6",
            "low",
            45,
            10,
            None,
        );
        insert_detail_event(
            &connection,
            "c-low",
            "child-c",
            "root-a",
            "other-model",
            "low",
            45,
            200,
            None,
        );
        insert_detail_event(
            &connection,
            "c-high",
            "child-c",
            "root-a",
            "other-model",
            "high",
            45,
            200,
            None,
        );
        insert_detail_event(
            &connection,
            "other-root-compaction",
            "root-a",
            "other-root",
            "gpt-6",
            "medium",
            10,
            800,
            Some(800),
        );

        connection
            .execute(
                "INSERT INTO codex_source_files(source_file_id,thread_id,current_path,
                    source_area,device_id,inode,file_generation,observed_size,observed_mtime_ns,
                    file_status,last_seen_at_ms)
                 VALUES (1,'child-c','/constructed/abc.jsonl','sessions',1,1,1,10,1,
                         'replaced',1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO codex_compaction_markers(source,ledger_epoch,source_file_id,
                    file_generation,source_start_offset,source_end_offset,owning_thread_id,
                    root_session_id,occurred_at_ms,model,reasoning_effort,response_id,
                    resolved_event_id,unknown_reason)
                 VALUES ('codex',1,1,1,0,1,'child-c','root-a',NULL,'other-model',NULL,NULL,
                         NULL,'time_missing')",
                [],
            )
            .unwrap();

        let sidecar = CodexSessionDetailSidecar;
        let full_range = TimeRange::new(0, 50).unwrap();
        let model_filter =
            UsageFilter::new(Vec::new(), vec!["gpt-6".into()], Vec::new(), false, false);
        let detail = AggregateReader::new(&connection, &[])
            .session_detail(full_range, &model_filter, "root-a")
            .unwrap();
        assert_eq!(detail.main.inclusive_usage.total_tokens, 1_010);
        let projections = sidecar
            .compaction_usage(&connection, full_range, &model_filter, &detail)
            .unwrap();
        let projected = projections
            .into_iter()
            .map(|projection| {
                (
                    (
                        projection.thread_id,
                        projection.model,
                        projection.reasoning_effort,
                    ),
                    projection.compaction_tokens,
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            projected,
            BTreeMap::from([
                (
                    ("root-a".into(), "gpt-6".into(), Some("high".into())),
                    Some(50)
                ),
                (
                    ("root-a".into(), "gpt-6".into(), Some("low".into())),
                    Some(0)
                ),
                (
                    ("root-a".into(), "gpt-6".into(), Some("medium".into())),
                    Some(20)
                ),
                (
                    ("child-b".into(), "gpt-6".into(), Some("high".into())),
                    Some(90)
                ),
            ])
        );

        let short_range = TimeRange::new(0, 20).unwrap();
        let short_detail = AggregateReader::new(&connection, &[])
            .session_detail(short_range, &UsageFilter::default(), "root-a")
            .unwrap();
        let short = sidecar
            .compaction_usage(
                &connection,
                short_range,
                &UsageFilter::default(),
                &short_detail,
            )
            .unwrap();
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].thread_id, "root-a");
        assert_eq!(short[0].reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(short[0].compaction_tokens, Some(20));

        let unknown_detail = AggregateReader::new(&connection, &[])
            .with_detail_sidecars(&[&sidecar])
            .session_detail(full_range, &UsageFilter::default(), "root-a")
            .unwrap();
        assert_eq!(
            unknown_detail.main.inclusive_usage.total_tokens, 1_010,
            "Compaction metadata does not change inclusive usage totals"
        );
        assert_eq!(
            detail_model(&unknown_detail, "root-a", "gpt-6", "medium"),
            Some(20)
        );
        assert_eq!(
            detail_model(&unknown_detail, "root-a", "gpt-6", "high"),
            Some(50)
        );
        assert_eq!(
            detail_model(&unknown_detail, "child-b", "gpt-6", "high"),
            Some(90)
        );
        let child_c = unknown_detail
            .subagents
            .iter()
            .find(|subagent| subagent.thread_id == "child-c")
            .unwrap();
        assert_eq!(child_c.model_usage.len(), 2);
        assert!(
            child_c
                .model_usage
                .iter()
                .all(|block| block.compaction_tokens.is_none())
        );
        assert_eq!(
            detail_model(&unknown_detail, "child-c", "other-model", "low"),
            None,
            "a marker without time or turn bounds keeps matching effort blocks unknown"
        );
        assert_eq!(
            detail_model(&unknown_detail, "child-c", "other-model", "high"),
            None,
            "an unresolved marker effort scopes all matching efforts"
        );

        connection
            .execute(
                "UPDATE source_usage_epochs
                 SET active_epoch=0,active_parser_version=12 WHERE source='codex'",
                [],
            )
            .unwrap();
        let not_ready = sidecar
            .compaction_usage(&connection, full_range, &model_filter, &detail)
            .unwrap();
        assert_eq!(not_ready.len(), 4);
        assert!(
            not_ready
                .iter()
                .all(|projection| projection.compaction_tokens.is_none())
        );
    }
}
