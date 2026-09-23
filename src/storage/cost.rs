//! Derived usage-event cost persistence and historical backfill.

use crate::{
    cost::{
        BundledPricingRepository, COST_ALGORITHM_VERSION, CostEstimateOutcome, CostEstimator,
        PRICING_CATALOG_VERSION, UsageCostGranularity,
    },
    source::SourceId,
    usage::normalized::NormalizedTokenUsage,
};
use rusqlite::{Connection, TransactionBehavior, params};

use super::{Result as StorageResult, StorageError};

/// Estimate the derived cost for one canonical usage event.
///
/// The repository and estimator are supplied by the caller so ingestion and
/// historical refresh use the exact same pricing/estimation path.
pub(crate) fn estimate_event_cost(
    repository: &BundledPricingRepository,
    estimator: &CostEstimator,
    source: &SourceId,
    model: &str,
    occurred_at_ms: i64,
    granularity: UsageCostGranularity,
    usage: &NormalizedTokenUsage,
) -> StorageResult<Option<i64>> {
    let outcome = crate::cost::estimate_for_source(
        repository,
        estimator,
        source,
        model,
        occurred_at_ms,
        granularity,
        usage,
    )
    .map_err(|_| StorageError::invalid_state("usage cost estimation failed"))?;
    Ok(match outcome {
        CostEstimateOutcome::Known(cost) => Some(cost.total_nanos_usd),
        CostEstimateOutcome::Unknown(_) => None,
    })
}

/// Reprice all canonical usage rows when either derived-cost version changes.
///
/// Every read, estimate, write, metadata update, and revision increment is
/// performed in one transaction. Any malformed canonical row or estimator
/// failure therefore rolls back all changes made by this refresh.
pub(crate) fn refresh_usage_costs_if_needed(connection: &mut Connection) -> StorageResult<bool> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (cost_version, pricing_version, current_revision): (i64, i64, i64) = transaction
        .query_row(
            "SELECT cost_algorithm_version,pricing_catalog_version,data_revision
             FROM app_meta WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    if cost_version == COST_ALGORITHM_VERSION && pricing_version == PRICING_CATALOG_VERSION {
        transaction.commit()?;
        return Ok(false);
    }

    let repository = BundledPricingRepository::new();
    let estimator = CostEstimator::new();
    let mut statement = transaction.prepare(
        "SELECT source,source_epoch,event_id,event_kind,occurred_at_ms,model,
                input_tokens,cached_tokens,cache_write_tokens,output_tokens,
                reasoning_tokens,total_tokens
         FROM usage_events ORDER BY source,source_epoch,event_id",
    )?;
    let mut rows = statement.query([])?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next()? {
        let source: String = row.get(0)?;
        let source_id = match source.as_str() {
            value if value == SourceId::CODEX.as_str() => Some(SourceId::CODEX),
            value if value == SourceId::ANTIGRAVITY.as_str() => Some(SourceId::ANTIGRAVITY),
            _ => None,
        };
        let estimated_cost = if let Some(source_id) = source_id {
            let event_kind: String = row.get(3)?;
            let granularity = match event_kind.as_str() {
                "normal" | "recovered" => UsageCostGranularity::RequestScoped,
                "turn_compensation" => UsageCostGranularity::AggregateCompensation,
                _ => return Err(StorageError::invalid_state("invalid usage event kind")),
            };
            let usage = NormalizedTokenUsage::new(
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            )
            .map_err(|_| StorageError::invalid_state("invalid canonical usage row"))?;
            let model: String = row.get(5)?;
            let occurred_at_ms: i64 = row.get(4)?;
            estimate_event_cost(
                &repository,
                &estimator,
                &source_id,
                &model,
                occurred_at_ms,
                granularity,
                &usage,
            )?
        } else {
            None
        };
        updates.push((
            source,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            estimated_cost,
        ));
    }
    drop(rows);
    drop(statement);

    let mut active_cost_changed = false;
    for (source, epoch, event_id, estimated_cost) in updates {
        let old_cost: Option<i64> = transaction.query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events
             WHERE source=?1 AND source_epoch=?2 AND event_id=?3",
            params![source, epoch, event_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE usage_events SET estimated_cost_nanos_usd=?1
             WHERE source=?2 AND source_epoch=?3 AND event_id=?4",
            params![estimated_cost, source, epoch, event_id],
        )?;
        if old_cost != estimated_cost {
            let is_active: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM source_usage_epochs sue
                    WHERE sue.source=?1 AND sue.active_epoch=?2)",
                params![source, epoch],
                |row| row.get::<_, i64>(0).map(|value| value != 0),
            )?;
            active_cost_changed |= is_active;
        }
    }

    let next_revision = if active_cost_changed {
        current_revision
            .checked_add(1)
            .ok_or_else(|| StorageError::invalid_state("data revision overflow"))?
    } else {
        current_revision
    };
    let changed = transaction.execute(
        "UPDATE app_meta
         SET cost_algorithm_version=?1,pricing_catalog_version=?2,data_revision=?3
         WHERE id=1 AND data_revision=?4",
        params![
            COST_ALGORITHM_VERSION,
            PRICING_CATALOG_VERSION,
            next_revision,
            current_revision
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::invalid_state("app meta revision changed"));
    }
    transaction.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrations::migrate;

    #[test]
    fn td_p4_cost_source_01_source_aware_cost_backfill() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&mut conn, 0).unwrap();

        conn.execute(
            "INSERT INTO source_usage_epochs(source, active_epoch, build_epoch, active_parser_version, build_parser_version)
             VALUES ('antigravity', 1, NULL, 1, NULL)",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO threads(thread_id, source, native_session_id, parent_thread_id, root_session_id, agent_role, project_kind, archived, metadata_quality_status, metadata_resolved_at_ms)
             VALUES ('codex:1', 'codex', '1', NULL, 'codex:1', 'main', 'project', 0, 'complete', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO threads(thread_id, source, native_session_id, parent_thread_id, root_session_id, agent_role, project_kind, archived, metadata_quality_status, metadata_resolved_at_ms)
             VALUES ('antigravity:1', 'antigravity', '1', NULL, 'antigravity:1', 'main', 'project', 0, 'complete', 100)",
            [],
        ).unwrap();

        // Codex event: model='gpt-5.6-sol', estimated_cost_nanos_usd=NULL
        conn.execute(
            "INSERT INTO usage_events(source, source_epoch, event_id, event_kind, occurred_at_ms, thread_id, root_session_id, model, input_tokens, cached_tokens, output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms, estimated_cost_nanos_usd)
             VALUES ('codex', 1, 'ev-codex', 'normal', 100, 'codex:1', 'codex:1', 'gpt-5.6-sol', 1000, 0, 500, 0, 1500, 'complete', 100, NULL)",
            [],
        ).unwrap();

        // The same Gemini route model stays unknown when Codex is the source.
        conn.execute(
            "INSERT INTO usage_events(source, source_epoch, event_id, event_kind, occurred_at_ms, thread_id, root_session_id, model, input_tokens, cached_tokens, output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms, estimated_cost_nanos_usd)
             VALUES ('codex', 1, 'ev-codex-gemini', 'normal', 100, 'codex:1', 'codex:1', 'gemini-3.8-flash', 1000, 0, 500, 0, 1500, 'complete', 100, 123456)",
            [],
        ).unwrap();

        // An Antigravity Google model receives its matching Standard estimate.
        conn.execute(
            "INSERT INTO usage_events(source, source_epoch, event_id, event_kind, occurred_at_ms, thread_id, root_session_id, model, input_tokens, cached_tokens, output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms, estimated_cost_nanos_usd)
             VALUES ('antigravity', 1, 'ev-antigravity-flash', 'normal', 100, 'antigravity:1', 'antigravity:1', 'gemini-3.8-flash', 1000, 0, 500, 0, 1500, 'complete', 100, NULL)",
            [],
        ).unwrap();

        // Unpriced Antigravity model: historical non-NULL cost is cleared.
        conn.execute(
            "INSERT INTO usage_events(source, source_epoch, event_id, event_kind, occurred_at_ms, thread_id, root_session_id, model, input_tokens, cached_tokens, output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms, estimated_cost_nanos_usd)
             VALUES ('antigravity', 1, 'ev-antigravity', 'normal', 100, 'antigravity:1', 'antigravity:1', 'gpt-5.6-sol', 1000, 0, 500, 0, 1500, 'partial', 100, 999999)",
            [],
        ).unwrap();

        conn.execute(
            "INSERT INTO usage_events(source, source_epoch, event_id, event_kind, occurred_at_ms, thread_id, root_session_id, model, input_tokens, cached_tokens, output_tokens, reasoning_tokens, total_tokens, quality_status, created_at_ms, estimated_cost_nanos_usd)
             VALUES ('antigravity', 1, 'ev-antigravity-pro', 'normal', 100, 'antigravity:1', 'antigravity:1', 'gemini-3.8-pro', 1000, 0, 500, 0, 1500, 'complete', 100, 999999)",
            [],
        ).unwrap();

        // Deliberately set cost_algorithm_version / pricing_catalog_version to stale
        conn.execute(
            "UPDATE app_meta SET cost_algorithm_version=0, pricing_catalog_version=0 WHERE id=1",
            [],
        )
        .unwrap();

        let initial_rev: i64 = conn
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();

        // Trigger refresh
        let refreshed = refresh_usage_costs_if_needed(&mut conn).unwrap();
        assert!(refreshed, "refresh should have occurred");

        // OpenAI pricing for Codex remains unchanged.
        let codex_cost: Option<i64> = conn.query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events WHERE source='codex' AND event_id='ev-codex'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(codex_cost.is_some(), "Codex event must have computed cost");

        let codex_gemini_cost: Option<i64> = conn.query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events WHERE source='codex' AND event_id='ev-codex-gemini'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(codex_gemini_cost, None);

        let antigravity_flash_cost: Option<i64> = conn.query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events WHERE source='antigravity' AND event_id='ev-antigravity-flash'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(antigravity_flash_cost, Some(2_625_000));

        let antigravity_cost: Option<i64> = conn.query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events WHERE source='antigravity' AND event_id='ev-antigravity'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(
            antigravity_cost, None,
            "unpriced Antigravity event cost must be NULL"
        );

        let antigravity_pro_cost: Option<i64> = conn.query_row(
            "SELECT estimated_cost_nanos_usd FROM usage_events WHERE source='antigravity' AND event_id='ev-antigravity-pro'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(antigravity_pro_cost, None);

        // Revision should have bumped because active costs changed
        let new_rev: i64 = conn
            .query_row("SELECT data_revision FROM app_meta WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            new_rev,
            initial_rev + 1,
            "active cost changes must bump data_revision"
        );
    }
}
