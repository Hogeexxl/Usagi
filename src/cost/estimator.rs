//! Pure cost estimation over canonical token usage and a pricing rule.

use std::convert::TryFrom;

use crate::usage::NormalizedTokenUsage;

use super::{ContextTier, UsageCostGranularity, pricing::ModelPricing};

/// The four billable components and their total, in USD nanodollars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EstimatedCost {
    pub input_nanos_usd: i64,
    pub cached_input_nanos_usd: i64,
    pub cache_write_nanos_usd: i64,
    pub output_nanos_usd: i64,
    pub total_nanos_usd: i64,
}

/// Reasons an event cannot receive a numeric estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownCostReason {
    UnknownModel,
    MissingCacheWriteRate,
    AmbiguousLongContextGranularity,
}

/// A known estimate or an explicitly unknown estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostEstimateOutcome {
    Known(EstimatedCost),
    Unknown(UnknownCostReason),
}

/// Errors that prevent a numeric estimate from being produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostEstimationError {
    InvalidUsage,
    InvalidPricing,
    ArithmeticOverflow,
}

/// Stateless estimator for one normalized usage value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CostEstimator;

impl CostEstimator {
    pub const fn new() -> Self {
        Self
    }

    /// Estimate one usage value using the supplied model pricing rule.
    ///
    /// Model lookup intentionally stays outside this function. Callers that
    /// cannot resolve a model should return `UnknownCostReason::UnknownModel`
    /// without applying another model's rates.
    pub fn estimate(
        usage: &NormalizedTokenUsage,
        pricing: &ModelPricing,
        granularity: UsageCostGranularity,
    ) -> Result<CostEstimateOutcome, CostEstimationError> {
        if usage.validate().is_err() {
            return Err(CostEstimationError::InvalidUsage);
        }
        if pricing.short_context.has_non_negative_rates()
            && pricing.long_context.is_none_or(|policy| {
                policy.threshold_input_tokens >= 0 && policy.rates.has_non_negative_rates()
            })
        {
            // Continue with the selected rates below.
        } else {
            return Err(CostEstimationError::InvalidPricing);
        }

        let tier = match context_tier(usage, pricing, granularity) {
            Ok(tier) => tier,
            Err(reason) => return Ok(CostEstimateOutcome::Unknown(reason)),
        };
        let rates = match tier {
            ContextTier::Short => pricing.short_context,
            ContextTier::Long => {
                pricing
                    .long_context
                    .expect("long tier requires long-context policy")
                    .rates
            }
        };

        let effective_cache_write_tokens = usage.cache_write_tokens.unwrap_or(0);
        let billable_uncached_input_tokens = (usage.input_tokens as i128)
            .checked_sub(usage.cached_tokens as i128)
            .and_then(|value| value.checked_sub(effective_cache_write_tokens as i128))
            .ok_or(CostEstimationError::ArithmeticOverflow)?;
        if billable_uncached_input_tokens < 0 {
            return Err(CostEstimationError::InvalidUsage);
        }

        let input_nanos_usd =
            checked_cost(billable_uncached_input_tokens, rates.input_nanos_per_token)?;
        let cached_input_nanos_usd = checked_cost(
            usage.cached_tokens as i128,
            rates.cached_input_nanos_per_token,
        )?;

        let cache_write_nanos_usd = if effective_cache_write_tokens == 0 {
            0
        } else {
            let rate = rates
                .cache_write_nanos_per_token
                .ok_or(CostEstimateOutcome::Unknown(
                    UnknownCostReason::MissingCacheWriteRate,
                ));
            match rate {
                Ok(rate) => checked_cost(effective_cache_write_tokens as i128, rate)?,
                Err(outcome) => return Ok(outcome),
            }
        };
        let output_nanos_usd =
            checked_cost(usage.output_tokens as i128, rates.output_nanos_per_token)?;
        let total_nanos_usd = input_nanos_usd
            .checked_add(cached_input_nanos_usd)
            .and_then(|value| value.checked_add(cache_write_nanos_usd))
            .and_then(|value| value.checked_add(output_nanos_usd))
            .ok_or(CostEstimationError::ArithmeticOverflow)?;

        let estimated = EstimatedCost {
            input_nanos_usd: to_non_negative_i64(input_nanos_usd)?,
            cached_input_nanos_usd: to_non_negative_i64(cached_input_nanos_usd)?,
            cache_write_nanos_usd: to_non_negative_i64(cache_write_nanos_usd)?,
            output_nanos_usd: to_non_negative_i64(output_nanos_usd)?,
            total_nanos_usd: to_non_negative_i64(total_nanos_usd)?,
        };
        Ok(CostEstimateOutcome::Known(estimated))
    }

    /// Instance-oriented spelling for callers that keep an estimator value.
    pub fn estimate_with(
        &self,
        usage: &NormalizedTokenUsage,
        pricing: &ModelPricing,
        granularity: UsageCostGranularity,
    ) -> Result<CostEstimateOutcome, CostEstimationError> {
        Self::estimate(usage, pricing, granularity)
    }
}

fn context_tier(
    usage: &NormalizedTokenUsage,
    pricing: &ModelPricing,
    granularity: UsageCostGranularity,
) -> Result<ContextTier, UnknownCostReason> {
    let Some(policy) = pricing.long_context else {
        return Ok(ContextTier::Short);
    };
    if usage.input_tokens <= policy.threshold_input_tokens {
        return Ok(ContextTier::Short);
    }
    match granularity {
        UsageCostGranularity::RequestScoped => Ok(ContextTier::Long),
        UsageCostGranularity::AggregateCompensation => {
            Err(UnknownCostReason::AmbiguousLongContextGranularity)
        }
    }
}

fn checked_cost(tokens: i128, rate: i64) -> Result<i128, CostEstimationError> {
    if tokens < 0 || rate < 0 {
        return Err(CostEstimationError::InvalidPricing);
    }
    tokens
        .checked_mul(rate as i128)
        .ok_or(CostEstimationError::ArithmeticOverflow)
}

fn to_non_negative_i64(value: i128) -> Result<i64, CostEstimationError> {
    if value < 0 {
        return Err(CostEstimationError::ArithmeticOverflow);
    }
    i64::try_from(value).map_err(|_| CostEstimationError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::pricing::{
        BundledPricingRepository, LongContextPolicy, ModelPricing, TokenRates,
    };

    const TEST_STANDARD_PRICING: ModelPricing = ModelPricing {
        canonical_model_id: "test-standard",
        effective_from_ms: i64::MIN,
        effective_to_ms: None,
        short_context: TokenRates::new(5_000, 500, Some(6_250), 30_000),
        long_context: Some(LongContextPolicy::new(
            272_000,
            TokenRates::new(10_000, 1_000, Some(12_500), 45_000),
        )),
    };

    fn usage(
        input_tokens: i64,
        cached_tokens: i64,
        cache_write_tokens: Option<i64>,
        output_tokens: i64,
        reasoning_tokens: i64,
    ) -> NormalizedTokenUsage {
        NormalizedTokenUsage::new(
            input_tokens,
            cached_tokens,
            cache_write_tokens,
            output_tokens,
            reasoning_tokens,
            input_tokens.checked_add(output_tokens).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn t_mu03_b02_formula_and_canonical_write_semantics() {
        let with_write = usage(1_000, 200, Some(100), 50, 20);
        let with_write_outcome = CostEstimator::estimate(
            &with_write,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        let CostEstimateOutcome::Known(cost) = with_write_outcome else {
            panic!("expected known cost");
        };
        assert_eq!(cost.input_nanos_usd, 3_500_000);
        assert_eq!(cost.cached_input_nanos_usd, 100_000);
        assert_eq!(cost.cache_write_nanos_usd, 625_000);
        assert_eq!(cost.output_nanos_usd, 1_500_000);
        assert_eq!(cost.total_nanos_usd, 5_725_000);

        let without_write = usage(1_000, 200, None, 50, 20);
        let without_write_outcome = CostEstimator::estimate(
            &without_write,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        let CostEstimateOutcome::Known(cost) = without_write_outcome else {
            panic!("expected known cost");
        };
        assert_eq!(without_write.cache_write_tokens, None);
        assert_eq!(cost.input_nanos_usd, 4_000_000);
        assert_eq!(cost.cached_input_nanos_usd, 100_000);
        assert_eq!(cost.cache_write_nanos_usd, 0);
        assert_eq!(cost.output_nanos_usd, 1_500_000);
        assert_eq!(cost.total_nanos_usd, 5_600_000);

        let no_reasoning = usage(1_000, 200, Some(100), 50, 0);
        let no_reasoning_outcome = CostEstimator::estimate(
            &no_reasoning,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        assert_eq!(with_write_outcome, no_reasoning_outcome);

        let mut missing_write_rate = TEST_STANDARD_PRICING;
        missing_write_rate.short_context.cache_write_nanos_per_token = None;
        let missing = CostEstimator::estimate(
            &with_write,
            &missing_write_rate,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        assert_eq!(
            missing,
            CostEstimateOutcome::Unknown(UnknownCostReason::MissingCacheWriteRate)
        );

        let missing_without_write = CostEstimator::estimate(
            &without_write,
            &missing_write_rate,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        assert!(matches!(
            missing_without_write,
            CostEstimateOutcome::Known(_)
        ));

        let overflowing_pricing = ModelPricing {
            short_context: TokenRates::new(i64::MAX, 0, Some(0), 0),
            ..TEST_STANDARD_PRICING
        };
        let overflowing_usage = usage(2, 0, Some(0), 0, 0);
        assert_eq!(
            CostEstimator::estimate(
                &overflowing_usage,
                &overflowing_pricing,
                UsageCostGranularity::RequestScoped,
            ),
            Err(CostEstimationError::ArithmeticOverflow)
        );
    }

    #[test]
    fn t_mu03_b03_context_boundaries_and_granularity() {
        let short = usage(272_000, 0, Some(0), 0, 0);
        let short_request = CostEstimator::estimate(
            &short,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        let short_compensation = CostEstimator::estimate(
            &short,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::AggregateCompensation,
        )
        .unwrap();
        assert!(matches!(short_request, CostEstimateOutcome::Known(_)));
        assert_eq!(short_request, short_compensation);
        let CostEstimateOutcome::Known(short_cost) = short_request else {
            unreachable!();
        };
        assert_eq!(short_cost.input_nanos_usd, 272_000 * 5_000);

        let long = usage(272_001, 0, Some(0), 0, 0);
        let long_request = CostEstimator::estimate(
            &long,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::RequestScoped,
        )
        .unwrap();
        let CostEstimateOutcome::Known(long_cost) = long_request else {
            panic!("request scoped long usage should be known");
        };
        assert_eq!(long_cost.input_nanos_usd, 272_001 * 10_000);

        let long_compensation = CostEstimator::estimate(
            &long,
            &TEST_STANDARD_PRICING,
            UsageCostGranularity::AggregateCompensation,
        )
        .unwrap();
        assert_eq!(
            long_compensation,
            CostEstimateOutcome::Unknown(UnknownCostReason::AmbiguousLongContextGranularity)
        );
    }

    #[test]
    fn t_mu04_a05_snapshot_models_without_cache_write_rate_are_unknown_only_on_write() {
        let repository = BundledPricingRepository::new();
        let with_write = usage(100, 0, Some(1), 10, 0);
        let without_write = usage(100, 0, None, 10, 0);

        for model in ["gpt-5.4", "gpt-5.5"] {
            let pricing = repository.resolve(model, 0).expect(model);
            assert_eq!(
                CostEstimator::estimate(&with_write, pricing, UsageCostGranularity::RequestScoped,),
                Ok(CostEstimateOutcome::Unknown(
                    UnknownCostReason::MissingCacheWriteRate
                ))
            );
            assert!(matches!(
                CostEstimator::estimate(
                    &without_write,
                    pricing,
                    UsageCostGranularity::RequestScoped,
                ),
                Ok(CostEstimateOutcome::Known(_))
            ));
        }
    }

    #[test]
    fn gpt_6_1_sol_cost_uses_standard_and_long_context_rates() {
        let repository = BundledPricingRepository::new();
        let pricing = repository.resolve("gpt-6.1-sol", 0).unwrap();

        for (input_tokens, expected_total) in [(272_000, 543_600_000), (272_001, 1_086_704_000)] {
            let outcome = CostEstimator::estimate(
                &usage(input_tokens, 1_000, Some(1_000), 100, 0),
                pricing,
                UsageCostGranularity::RequestScoped,
            )
            .unwrap();
            let CostEstimateOutcome::Known(cost) = outcome else {
                panic!("expected known cost");
            };
            assert_eq!(cost.total_nanos_usd, expected_total);
        }
    }

    #[test]
    fn claude_cost_separates_cached_input_and_five_minute_writes() {
        let repository = BundledPricingRepository::new();
        for (model, expected_total) in [
            ("copilot/claude-sonnet-5.5", 2_380_000),
            ("claude-opus-5-5", 4_680_000),
            ("claude-fable-5-1", 11_600_000),
        ] {
            let pricing = repository.resolve(model, 0).unwrap();
            let outcome = CostEstimator::estimate(
                &usage(1_000, 400, Some(200), 100, 20),
                pricing,
                UsageCostGranularity::RequestScoped,
            )
            .unwrap();
            let CostEstimateOutcome::Known(cost) = outcome else {
                panic!("expected known Claude cost");
            };
            assert_eq!(cost.total_nanos_usd, expected_total);
            let long = CostEstimator::estimate(
                &usage(900_000, 0, Some(0), 0, 0),
                pricing,
                UsageCostGranularity::RequestScoped,
            )
            .unwrap();
            let CostEstimateOutcome::Known(cost) = long else {
                panic!("expected standard long-context Claude cost");
            };
            assert_eq!(
                cost.total_nanos_usd,
                900_000 * pricing.short_context.input_nanos_per_token
            );
        }
    }

    #[test]
    fn t_mu04_a06_long_context_boundary_applies_to_current_gpt5_catalog() {
        let repository = BundledPricingRepository::new();
        let short = usage(272_000, 0, Some(0), 0, 0);
        let long = usage(272_001, 0, Some(0), 0, 0);

        for model in ["gpt-5.4", "gpt-5.5", "gpt-5.6-sol"] {
            let pricing = repository.resolve(model, 0).expect(model);
            assert!(matches!(
                CostEstimator::estimate(&short, pricing, UsageCostGranularity::RequestScoped,),
                Ok(CostEstimateOutcome::Known(_))
            ));
            assert!(matches!(
                CostEstimator::estimate(&long, pricing, UsageCostGranularity::RequestScoped,),
                Ok(CostEstimateOutcome::Known(_))
            ));
            assert_eq!(
                CostEstimator::estimate(
                    &long,
                    pricing,
                    UsageCostGranularity::AggregateCompensation,
                ),
                Ok(CostEstimateOutcome::Unknown(
                    UnknownCostReason::AmbiguousLongContextGranularity
                ))
            );
        }
    }
}
