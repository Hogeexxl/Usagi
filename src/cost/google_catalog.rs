// LiteLLM projection snapshot, matched to Google's published Gemini Standard prices.
// Source: https://raw.githubusercontent.com/BerriAI/litellm/40ec84caa2c33539dcb6dc4b38d288370a2b921f/model_prices_and_context_window.json
// Retrieved: 2026-09-23
// Snapshot SHA-256: 83cc2d6257437025ef7f8a56533d596159e915a199647ba1e3a37f3f706bc734
// Official prices: https://ai.google.dev/gemini-api/docs/pricing

const GOOGLE_FLASH_RATES: TokenRates = TokenRates::new(750, 75, None, 3_750);

pub const GOOGLE_STANDARD_PRICING_CATALOG: &[ModelPricing] = &[
    ModelPricing {
        canonical_model_id: "gemini-3.7-flash",
        effective_from_ms: i64::MIN,
        effective_to_ms: None,
        short_context: GOOGLE_FLASH_RATES,
        long_context: None,
    },
    ModelPricing {
        canonical_model_id: "gemini-3.8-flash",
        effective_from_ms: i64::MIN,
        effective_to_ms: None,
        short_context: GOOGLE_FLASH_RATES,
        long_context: None,
    },
];
