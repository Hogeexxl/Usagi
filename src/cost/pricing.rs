//! Bundled model pricing for the cost domain.

use super::registry::{ModelRegistry, PricingProvider};
use crate::source::SourceId;

/// Per-token rates expressed in USD nanodollars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenRates {
    pub input_nanos_per_token: i64,
    pub cached_input_nanos_per_token: i64,
    pub cache_write_nanos_per_token: Option<i64>,
    pub output_nanos_per_token: i64,
}

impl TokenRates {
    pub const fn new(
        input_nanos_per_token: i64,
        cached_input_nanos_per_token: i64,
        cache_write_nanos_per_token: Option<i64>,
        output_nanos_per_token: i64,
    ) -> Self {
        Self {
            input_nanos_per_token,
            cached_input_nanos_per_token,
            cache_write_nanos_per_token,
            output_nanos_per_token,
        }
    }

    pub(crate) fn has_non_negative_rates(self) -> bool {
        self.input_nanos_per_token >= 0
            && self.cached_input_nanos_per_token >= 0
            && self
                .cache_write_nanos_per_token
                .is_none_or(|rate| rate >= 0)
            && self.output_nanos_per_token >= 0
    }
}

/// Pricing rules used after a model's prompt crosses its context threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LongContextPolicy {
    pub threshold_input_tokens: i64,
    pub rates: TokenRates,
}

impl LongContextPolicy {
    pub const fn new(threshold_input_tokens: i64, rates: TokenRates) -> Self {
        Self {
            threshold_input_tokens,
            rates,
        }
    }
}

/// A model's versioned pricing rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelPricing {
    pub canonical_model_id: &'static str,
    pub effective_from_ms: i64,
    pub effective_to_ms: Option<i64>,
    pub short_context: TokenRates,
    pub long_context: Option<LongContextPolicy>,
}

impl ModelPricing {
    /// Returns whether the pricing rule is active at the given timestamp.
    /// The upper bound is exclusive when one is present.
    pub const fn is_effective_at(&self, occurred_at_ms: i64) -> bool {
        occurred_at_ms >= self.effective_from_ms
            && match self.effective_to_ms {
                Some(effective_to_ms) => occurred_at_ms < effective_to_ms,
                None => true,
            }
    }
}

include!("litellm_catalog.rs");
include!("google_catalog.rs");

/// The immutable bundled OpenAI Standard catalog.
pub const BUNDLED_PRICING_CATALOG: &[ModelPricing] = LITELLM_OPENAI_PRICING_CATALOG;

/// Repository backed by the fixed bundled catalog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BundledPricingRepository;

impl BundledPricingRepository {
    pub const fn new() -> Self {
        Self
    }

    pub fn resolve(&self, model: &str, occurred_at_ms: i64) -> Option<&'static ModelPricing> {
        let resolution = ModelRegistry::new().resolve(model);
        match (resolution.pricing_provider, resolution.pricing_target) {
            (Some(PricingProvider::OpenAI), Some(target)) => {
                resolve_from_catalog(BUNDLED_PRICING_CATALOG, target, occurred_at_ms)
            }
            _ => None,
        }
    }

    /// Resolve a pricing rule using both the event's source and model identity.
    pub fn resolve_for_source(
        &self,
        source: &SourceId,
        model: &str,
        occurred_at_ms: i64,
    ) -> Option<&'static ModelPricing> {
        let resolution = ModelRegistry::new().resolve_for_source(source, model);
        match (resolution.pricing_provider, resolution.pricing_target) {
            (Some(PricingProvider::OpenAI), Some(target)) => {
                resolve_from_catalog(BUNDLED_PRICING_CATALOG, target, occurred_at_ms)
            }
            (Some(PricingProvider::Google), Some(target)) => {
                resolve_from_catalog(GOOGLE_STANDARD_PRICING_CATALOG, target, occurred_at_ms)
            }
            _ => None,
        }
    }
}

fn resolve_from_catalog(
    catalog: &'static [ModelPricing],
    model: &str,
    occurred_at_ms: i64,
) -> Option<&'static ModelPricing> {
    catalog.iter().find(|pricing| {
        pricing.canonical_model_id == model && pricing.is_effective_at(occurred_at_ms)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_mu03_b01_pricing_catalog_contract() {
        let repository = BundledPricingRepository::new();

        let sol = repository.resolve("gpt-5.6-sol", 0).expect("Sol pricing");
        assert_eq!(sol.canonical_model_id, "gpt-5.6-sol");
        assert_eq!(
            sol.short_context,
            TokenRates::new(4_000, 400, Some(5_000), 20_000)
        );
        assert_eq!(
            sol.long_context.expect("Sol long pricing").rates,
            TokenRates::new(8_000, 800, Some(10_000), 30_000)
        );
        assert_eq!(
            sol.long_context
                .expect("Sol long pricing")
                .threshold_input_tokens,
            272_000
        );

        let terra = repository
            .resolve("gpt-5.6-terra", 0)
            .expect("Terra pricing");
        assert_eq!(
            terra.short_context,
            TokenRates::new(2_000, 200, Some(2_500), 12_000)
        );
        assert_eq!(
            terra.long_context.expect("Terra long pricing").rates,
            TokenRates::new(4_000, 400, Some(5_000), 18_000)
        );

        let luna = repository.resolve("gpt-5.6-luna", 0).expect("Luna pricing");
        assert_eq!(
            luna.short_context,
            TokenRates::new(200, 20, Some(250), 1_200)
        );
        assert_eq!(
            luna.long_context.expect("Luna long pricing").rates,
            TokenRates::new(400, 40, Some(500), 1_800)
        );

        let alias = repository.resolve("gpt-5.6", 0).expect("gpt-5.6 alias");
        assert_eq!(alias.canonical_model_id, "gpt-5.6-sol");
        assert_eq!(alias.short_context, sol.short_context);
        assert!(repository.resolve("gpt-5.6-s", 0).is_none());
        assert!(repository.resolve("unknown-model", 0).is_none());
    }

    #[test]
    fn t_mu04_a01_pricing_alias_matrix() {
        let repository = BundledPricingRepository::new();

        let sol = repository.resolve("gpt-5.6-sol", 0).expect("Sol pricing");
        let sol_alias = repository.resolve("gpt-5.6", 0).expect("Sol alias");
        assert_eq!(sol_alias.canonical_model_id, "gpt-5.6-sol");
        assert_eq!(
            sol.short_context,
            TokenRates::new(4_000, 400, Some(5_000), 20_000)
        );
        assert_eq!(
            sol.long_context.expect("Sol long pricing").rates,
            TokenRates::new(8_000, 800, Some(10_000), 30_000)
        );
        assert_eq!(sol_alias.short_context, sol.short_context);
        assert_eq!(sol_alias.long_context, sol.long_context);

        let luna = repository.resolve("gpt-5.6-luna", 0).expect("Luna pricing");
        let luna_alias = repository
            .resolve("codex-auto-review", 0)
            .expect("Luna alias");
        assert_eq!(luna_alias.canonical_model_id, "gpt-5.6-luna");
        assert_eq!(
            luna.short_context,
            TokenRates::new(200, 20, Some(250), 1_200)
        );
        assert_eq!(
            luna.long_context.expect("Luna long pricing").rates,
            TokenRates::new(400, 40, Some(500), 1_800)
        );
        assert_eq!(luna_alias.short_context, luna.short_context);
        assert_eq!(luna_alias.long_context, luna.long_context);

        assert!(repository.resolve("unknown-model", 0).is_none());
    }

    #[test]
    fn t_mu04_a03_local_openai_rates() {
        assert!(!LITELLM_SNAPSHOT_MODEL_IDS.contains(&"openai/container"));

        let repository = BundledPricingRepository::new();
        let expected = [
            (
                "gpt-5.1-codex-mini",
                TokenRates::new(250, 25, None, 2_000),
                None,
            ),
            (
                "gpt-5.2-codex",
                TokenRates::new(1_750, 175, None, 14_000),
                None,
            ),
            (
                "gpt-5.3-codex",
                TokenRates::new(1_750, 175, None, 14_000),
                None,
            ),
            (
                "gpt-5.4",
                TokenRates::new(2_500, 250, None, 15_000),
                Some(TokenRates::new(5_000, 500, None, 22_500)),
            ),
            ("gpt-5.4-mini", TokenRates::new(750, 75, None, 4_500), None),
            (
                "gpt-5.5",
                TokenRates::new(5_000, 500, None, 30_000),
                Some(TokenRates::new(10_000, 1_000, None, 45_000)),
            ),
            (
                "gpt-5.6-sol",
                TokenRates::new(4_000, 400, Some(5_000), 20_000),
                Some(TokenRates::new(8_000, 800, Some(10_000), 30_000)),
            ),
            (
                "gpt-5.6-terra",
                TokenRates::new(2_000, 200, Some(2_500), 12_000),
                Some(TokenRates::new(4_000, 400, Some(5_000), 18_000)),
            ),
            (
                "gpt-5.6-luna",
                TokenRates::new(200, 20, Some(250), 1_200),
                Some(TokenRates::new(400, 40, Some(500), 1_800)),
            ),
            (
                "gpt-6-astra",
                TokenRates::new(10_000, 1_000, Some(12_500), 50_000),
                Some(TokenRates::new(20_000, 2_000, Some(25_000), 75_000)),
            ),
        ];

        for (model, short_context, long_context) in expected {
            let pricing = repository.resolve(model, 0).expect(model);
            assert_eq!(pricing.canonical_model_id, model);
            assert_eq!(pricing.short_context, short_context);
            assert_eq!(
                pricing.long_context.map(|policy| policy.rates),
                long_context
            );
            assert_eq!(
                pricing
                    .long_context
                    .map(|policy| policy.threshold_input_tokens),
                long_context.map(|_| 272_000)
            );
        }
    }

    #[test]
    fn t_mu04_a05_reserve_repository_reuses_luna_pricing() {
        let repository = BundledPricingRepository::new();
        let luna = repository.resolve("gpt-5.6-luna", 0).expect("Luna pricing");
        let reserve = repository
            .resolve("gpt-reserve", 0)
            .expect("Reserve pricing target");

        assert_eq!(reserve.canonical_model_id, "gpt-5.6-luna");
        assert_eq!(reserve.short_context, luna.short_context);
        assert_eq!(reserve.long_context, luna.long_context);
        assert_eq!(
            reserve
                .long_context
                .expect("Luna long pricing")
                .threshold_input_tokens,
            272_000
        );
        assert!(repository.resolve("unknown-model", 0).is_none());
    }

    #[test]
    fn t_mu04_a04_missing_cache_read_stays_unpriced() {
        let repository = BundledPricingRepository::new();

        for model in ["gpt-3.5-turbo", "gpt-5-pro", "ft:gpt-4o-2024-11-20"] {
            assert_eq!(
                ModelRegistry::new().resolve(model).provider,
                super::super::registry::ModelProvider::OpenAI
            );
            assert!(repository.resolve(model, 0).is_none(), "{model}");
        }
    }

    #[test]
    fn google_standard_rates_require_antigravity_source_and_match_litellm_projection() {
        let repository = BundledPricingRepository::new();

        for model in ["gemini-3.7-flash", "gemini-3.8-flash"] {
            for occurred_at_ms in [0, i64::MAX] {
                let pricing = repository
                    .resolve_for_source(&SourceId::ANTIGRAVITY, model, occurred_at_ms)
                    .expect("Google Standard pricing");
                assert_eq!(pricing.canonical_model_id, model);
                assert_eq!(pricing.short_context, TokenRates::new(750, 75, None, 3_750));
            }
        }

        assert!(repository.resolve("gemini-3.8-flash", 0).is_none());
        assert!(
            repository
                .resolve_for_source(&SourceId::CODEX, "gemini-3.8-flash", 0)
                .is_none()
        );
        assert!(
            repository
                .resolve_for_source(&SourceId::ANTIGRAVITY, "gemini-3.8-pro", 0)
                .is_none()
        );
        assert!(
            repository
                .resolve_for_source(&SourceId::ANTIGRAVITY, "gpt-5.6-sol", 0)
                .is_none()
        );
    }
}
