pub(crate) mod estimator;
pub(crate) mod pricing;
pub(crate) mod registry;

pub use estimator::{CostEstimateOutcome, CostEstimator, UnknownCostReason};
pub use pricing::BundledPricingRepository;
pub use registry::ModelRegistry;

/// Cost algorithm version used by the derived estimate.
pub const COST_ALGORITHM_VERSION: i64 = 1;

/// Bundled pricing catalog version.
pub const PRICING_CATALOG_VERSION: i64 = 5;

/// Whether usage represents one model request or a compensation over events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageCostGranularity {
    RequestScoped,
    AggregateCompensation,
}

/// Estimate the cost for a source-owned model using that source's pricing projection.
pub(crate) fn estimate_for_source(
    repository: &BundledPricingRepository,
    estimator: &CostEstimator,
    source: &crate::source::SourceId,
    model: &str,
    occurred_at_ms: i64,
    granularity: UsageCostGranularity,
    usage: &crate::usage::NormalizedTokenUsage,
) -> Result<CostEstimateOutcome, estimator::CostEstimationError> {
    let Some(pricing) = repository.resolve_for_source(source, model, occurred_at_ms) else {
        return Ok(CostEstimateOutcome::Unknown(
            UnknownCostReason::UnknownModel,
        ));
    };
    estimator.estimate_with(usage, pricing, granularity)
}

/// Map a canonical usage event kind to its estimator granularity.
pub(crate) fn granularity_for_event_kind(
    event_kind: crate::usage::event::EventKind,
) -> UsageCostGranularity {
    match event_kind {
        crate::usage::event::EventKind::Normal | crate::usage::event::EventKind::Recovered => {
            UsageCostGranularity::RequestScoped
        }
        crate::usage::event::EventKind::TurnCompensation => {
            UsageCostGranularity::AggregateCompensation
        }
    }
}

/// Context tier selected for one usage estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextTier {
    Short,
    Long,
}
