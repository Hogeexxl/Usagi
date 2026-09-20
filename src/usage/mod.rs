//! Usage ingestion, epoch rebuild, carry, and read-only aggregate seams.

pub mod adapters;
pub mod aggregate;
pub mod analytics;
pub mod ledger;
pub mod normalized;
pub mod pipeline;
pub mod processor;
pub mod rebuild;
pub mod skills;

pub use normalized::{
    NormalizedTokenUsage, USAGE_CANONICAL_ALGORITHM_VERSION, USAGE_PARSER_VERSION,
    canonical_algorithm_for,
};
pub use skills::SkillUsageEvent;

pub use aggregate::{
    AggregateError, AggregateReader, FilterOptions, MAX_SESSION_ROWS, MainModelUsage,
    MainSessionDetail, ModelFilterOption, ModelUsageRow, ModelUsageRows, ProjectFilterOption,
    SessionCursor, SessionDetail, SessionPageRequest, SessionSnapshot, SessionSortField,
    SessionSortIndexItem, SessionSortOrder, SessionUsagePage, SessionUsageRow, SourceFilterOption,
    SubagentDetail, SubagentModelUsage, SummaryQuery, TimeRange, TokenTotals, UsageFilter,
    UsageSummary,
};
pub use pipeline::{
    CheckpointExpectation, ClassifiedOversizedUsageLine, ClassifiedUsageItem, ClassifiedUsageLine,
    FixedViewTail, PipelineDisposition, PipelineError, PlanAction, SourceContinuationState,
    SourceStateProof, TailStatus, UsagePipeline, UsagePipelinePlan, UsageSourceCommitDto,
};
pub use processor::{
    Anomaly, AnomalyCode, ClosedTurn, EventKind, GapKind, Occurrence, Ownership, ProcessResult,
    ProcessorError, TurnEndStatus, TurnModelState, TurnState, UsageContext, UsageEvent,
    UsageProcessor, UsageRecord, UsageSourceState, UsageValue,
};
pub use rebuild::{
    ActivationOutcome, BuildSnapshot, CompletionStatus, ManifestEntry, ProgressOutcome,
    RebuildError, RebuildLedger, SourceProgress, TailProof,
};

/// Compare the user-visible usage projection that would be selected by two
/// epochs for one source. Epoch identity itself is deliberately ignored.
pub(crate) fn usage_epochs_visible_equal(
    connection: &rusqlite::Connection,
    source: &str,
    active_epoch: i64,
    active_parser_version: i64,
    build_epoch: i64,
    build_parser_version: i64,
) -> rusqlite::Result<bool> {
    let columns = "event_id,event_kind,occurred_at_ms,thread_id,root_session_id,turn_key,model,
                   reasoning_effort,estimated_cost_nanos_usd,input_tokens,cached_tokens,
                   cache_write_tokens,output_tokens,reasoning_tokens,total_tokens,quality_status";
    let canonical_sql = format!(
        "SELECT NOT EXISTS(
             SELECT {columns} FROM usage_events
             WHERE source=?1 AND source_epoch=?2
             EXCEPT
             SELECT {columns} FROM usage_events
             WHERE source=?1 AND source_epoch=?3
         ) AND NOT EXISTS(
             SELECT {columns} FROM usage_events
             WHERE source=?1 AND source_epoch=?3
             EXCEPT
             SELECT {columns} FROM usage_events
             WHERE source=?1 AND source_epoch=?2
         )"
    );
    let canonical_equal: i64 = connection.query_row(
        &canonical_sql,
        rusqlite::params![source, active_epoch, build_epoch],
        |row| row.get(0),
    )?;
    if canonical_equal == 0 || source != "codex" {
        return Ok(canonical_equal != 0);
    }

    // Skills are Codex-private today, but they are part of the dashboard's
    // active-epoch projection. A parser transition can also change the visible
    // ready state even when both epochs contain no skill rows.
    let active_skills_ready =
        active_epoch > 0 && active_parser_version >= analytics::SKILL_USAGE_PARSER_VERSION;
    let build_skills_ready =
        build_epoch > 0 && build_parser_version >= analytics::SKILL_USAGE_PARSER_VERSION;
    if active_skills_ready != build_skills_ready {
        return Ok(false);
    }
    if active_skills_ready {
        let skills_equal: i64 = connection.query_row(
            "SELECT NOT EXISTS(
                 SELECT occurred_at_ms,root_session_id,model,skill_name,COUNT(*)
                 FROM skill_usage_events WHERE ledger_epoch=?1
                 GROUP BY occurred_at_ms,root_session_id,model,skill_name
                 EXCEPT
                 SELECT occurred_at_ms,root_session_id,model,skill_name,COUNT(*)
                 FROM skill_usage_events WHERE ledger_epoch=?2
                 GROUP BY occurred_at_ms,root_session_id,model,skill_name
             ) AND NOT EXISTS(
                 SELECT occurred_at_ms,root_session_id,model,skill_name,COUNT(*)
                 FROM skill_usage_events WHERE ledger_epoch=?2
                 GROUP BY occurred_at_ms,root_session_id,model,skill_name
                 EXCEPT
                 SELECT occurred_at_ms,root_session_id,model,skill_name,COUNT(*)
                 FROM skill_usage_events WHERE ledger_epoch=?1
                 GROUP BY occurred_at_ms,root_session_id,model,skill_name
             )",
            rusqlite::params![active_epoch, build_epoch],
            |row| row.get(0),
        )?;
        if skills_equal == 0 {
            return Ok(false);
        }
    }

    // Quarantine rows are surfaced as error sessions in summary/session APIs.
    // Proof rows and timestamps that are not returned to readers stay private.
    let quarantine_equal: i64 = connection.query_row(
        "SELECT NOT EXISTS(
             SELECT root_session_id,primary_error_code,last_activity_at_ms
             FROM usage_session_quarantine WHERE ledger_epoch=?1
             EXCEPT
             SELECT root_session_id,primary_error_code,last_activity_at_ms
             FROM usage_session_quarantine WHERE ledger_epoch=?2
         ) AND NOT EXISTS(
             SELECT root_session_id,primary_error_code,last_activity_at_ms
             FROM usage_session_quarantine WHERE ledger_epoch=?2
             EXCEPT
             SELECT root_session_id,primary_error_code,last_activity_at_ms
             FROM usage_session_quarantine WHERE ledger_epoch=?1
         )",
        rusqlite::params![active_epoch, build_epoch],
        |row| row.get(0),
    )?;
    Ok(quarantine_equal != 0)
}

pub use ledger::{
    CarryStepOutcome, SessionDetailSnapshot, SessionRowsSnapshot, UsageBuildScanProof,
    UsageCommitOutcome, UsageLedger, UsageLedgerError, UsageScanState, UsageSourceScanPlan,
};

#[cfg(test)]
mod visibility_tests {
    use super::*;

    #[test]
    fn visibility_comparison_covers_codex_sidecars_and_skill_readiness() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE usage_events(
                    source TEXT NOT NULL, source_epoch INTEGER NOT NULL, event_id TEXT NOT NULL,
                    event_kind TEXT NOT NULL, occurred_at_ms INTEGER NOT NULL, thread_id TEXT NOT NULL,
                    root_session_id TEXT NOT NULL, turn_key TEXT, model TEXT NOT NULL,
                    reasoning_effort TEXT, estimated_cost_nanos_usd INTEGER,
                    input_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
                    cache_write_tokens INTEGER, output_tokens INTEGER NOT NULL,
                    reasoning_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL,
                    quality_status TEXT NOT NULL
                 );
                 CREATE TABLE skill_usage_events(
                    ledger_epoch INTEGER NOT NULL, occurred_at_ms INTEGER NOT NULL,
                    root_session_id TEXT NOT NULL, model TEXT, skill_name TEXT NOT NULL
                 );
                 CREATE TABLE usage_session_quarantine(
                    ledger_epoch INTEGER NOT NULL, root_session_id TEXT NOT NULL,
                    primary_error_code TEXT NOT NULL, last_activity_at_ms INTEGER NOT NULL
                 );
                 INSERT INTO usage_events VALUES
                    ('codex',1,'event','normal',10,'root','root','turn','gpt','medium',100,
                     10,1,0,5,1,15,'complete'),
                    ('codex',2,'event','normal',10,'root','root','turn','gpt','medium',100,
                     10,1,0,5,1,15,'complete');",
            )
            .unwrap();
        let parser = analytics::SKILL_USAGE_PARSER_VERSION;
        assert!(usage_epochs_visible_equal(&connection, "codex", 1, parser, 2, parser).unwrap());

        connection
            .execute(
                "INSERT INTO skill_usage_events VALUES (1,10,'root','gpt','review')",
                [],
            )
            .unwrap();
        assert!(!usage_epochs_visible_equal(&connection, "codex", 1, parser, 2, parser).unwrap());
        connection
            .execute(
                "INSERT INTO skill_usage_events VALUES (2,10,'root','gpt','review')",
                [],
            )
            .unwrap();
        assert!(usage_epochs_visible_equal(&connection, "codex", 1, parser, 2, parser).unwrap());

        connection
            .execute(
                "INSERT INTO skill_usage_events VALUES (1,10,'root','gpt','review')",
                [],
            )
            .unwrap();
        assert!(
            !usage_epochs_visible_equal(&connection, "codex", 1, parser, 2, parser).unwrap(),
            "skill comparison must preserve visible multiplicity"
        );
        connection
            .execute(
                "INSERT INTO skill_usage_events VALUES (2,10,'root','gpt','review')",
                [],
            )
            .unwrap();

        connection
            .execute(
                "INSERT INTO usage_session_quarantine VALUES (1,'root','BROKEN',10)",
                [],
            )
            .unwrap();
        assert!(!usage_epochs_visible_equal(&connection, "codex", 1, parser, 2, parser).unwrap());
        connection
            .execute(
                "INSERT INTO usage_session_quarantine VALUES (2,'root','BROKEN',10)",
                [],
            )
            .unwrap();
        assert!(usage_epochs_visible_equal(&connection, "codex", 1, parser, 2, parser).unwrap());
        assert!(
            !usage_epochs_visible_equal(&connection, "codex", 1, parser - 1, 2, parser).unwrap(),
            "a visible Skills ready-state transition must advance revision"
        );
    }
}
