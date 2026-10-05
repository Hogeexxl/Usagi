#!/usr/bin/env python3
"""Project the three supported Claude models from a pinned LiteLLM snapshot."""

import argparse
import hashlib
import json
from decimal import Decimal
from pathlib import Path

SOURCE_REF = "29b4f20572d5c66de81a8ee4d893490720e83767"
SOURCE_URL = (
    "https://raw.githubusercontent.com/BerriAI/litellm/"
    f"{SOURCE_REF}/model_prices_and_context_window.json"
)
SOURCE_SHA256 = "3f106d57876f27b83badd6f4f193009b7858796379e1dd8cfc10c7c314732279"
MODELS = ("claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1")
RATE_KEYS = (
    "input_cost_per_token",
    "cache_read_input_token_cost",
    "cache_creation_input_token_cost",
    "output_cost_per_token",
)


def generate(raw: bytes) -> str:
    if hashlib.sha256(raw).hexdigest() != SOURCE_SHA256:
        raise ValueError("LiteLLM snapshot SHA-256 mismatch")
    data = json.loads(raw, parse_float=Decimal, parse_int=Decimal)
    lines = [
        "// Generated from the pinned LiteLLM snapshot; do not edit by hand.",
        f"// Source: {SOURCE_URL}",
        "// Retrieved: 2026-10-06",
        f"// Snapshot SHA-256: {SOURCE_SHA256}",
        "// cache_creation_input_token_cost is the 5-minute cache write rate.",
        "",
        "pub const ANTHROPIC_STANDARD_PRICING_CATALOG: &[ModelPricing] = &[",
    ]
    for model in MODELS:
        entry = data[model]
        if entry["litellm_provider"] != "anthropic" or entry["mode"] != "chat":
            raise ValueError(f"{model}: expected Anthropic chat pricing")
        rates = []
        for key in RATE_KEYS:
            value = entry[key] * Decimal(1_000_000_000)
            if not value.is_finite() or value < 0 or value != value.to_integral_value():
                raise ValueError(f"{model}.{key}: expected exact non-negative nanodollars")
            rates.append(int(value))
        input_rate, read_rate, write_rate, output_rate = rates
        lines.extend([
            "    ModelPricing {",
            f'        canonical_model_id: "{model}",',
            "        effective_from_ms: i64::MIN,",
            "        effective_to_ms: None,",
            f"        short_context: TokenRates::new({input_rate:_}, {read_rate:_}, Some({write_rate:_}), {output_rate:_}),",
            "        long_context: None,",
            "    },",
        ])
    lines.append("];")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    args.output.write_text(generate(args.input.read_bytes()), encoding="utf-8")
