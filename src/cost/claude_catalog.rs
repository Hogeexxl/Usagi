// Generated from the pinned LiteLLM snapshot; do not edit by hand.
// Source: https://raw.githubusercontent.com/BerriAI/litellm/29b4f20572d5c66de81a8ee4d893490720e83767/model_prices_and_context_window.json
// Retrieved: 2026-10-06
// Snapshot SHA-256: 3f106d57876f27b83badd6f4f193009b7858796379e1dd8cfc10c7c314732279
// cache_creation_input_token_cost is the 5-minute cache write rate.

pub const ANTHROPIC_STANDARD_PRICING_CATALOG: &[ModelPricing] = &[
    ModelPricing {
        canonical_model_id: "claude-opus-5-5",
        effective_from_ms: i64::MIN,
        effective_to_ms: None,
        short_context: TokenRates::new(4_000, 200, Some(5_000), 20_000),
        long_context: None,
    },
    ModelPricing {
        canonical_model_id: "claude-sonnet-5-5",
        effective_from_ms: i64::MIN,
        effective_to_ms: None,
        short_context: TokenRates::new(2_000, 200, Some(2_500), 10_000),
        long_context: None,
    },
    ModelPricing {
        canonical_model_id: "claude-fable-5-1",
        effective_from_ms: i64::MIN,
        effective_to_ms: None,
        short_context: TokenRates::new(10_000, 250, Some(12_500), 50_000),
        long_context: None,
    },
];
