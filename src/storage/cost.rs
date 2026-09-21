//! Derived usage-event cost persistence and historical backfill.

use crate::{
    cost::{
        BundledPricingRepository, COST_ALGORITHM_VERSION, CostEstimateOutcome, CostEstimator,
        PRICING_CATALOG_VERSION, UnknownCostReason, UsageCostGranularity,
    },
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
    model: &str,
    occurred_at_ms: i64,
    granularity: UsageCostGranularity,
    usage: &NormalizedTokenUsage,
) -> StorageResult<Option<i64>> {
    let pricing = repository.resolve(model, occurred_at_ms);
    let outcome = match pricing {
        Some(pricing) => estimator
            .estimate_with(usage, pricing, granularity)
            .map_err(|_| StorageError::invalid_state("usage cost estimation failed"))?,
        None => CostEstimateOutcome::Unknown(UnknownCostReason::UnknownModel),
    };
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
        let estimated_cost = estimate_event_cost(
            &repository,
            &estimator,
            &model,
            occurred_at_ms,
            granularity,
            &usage,
        )?;
        updates.push((
            row.get::<_, String>(0)?,
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
