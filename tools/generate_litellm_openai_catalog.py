#!/usr/bin/env python3
"""Generate the bounded OpenAI LiteLLM catalog update.

The checked-in catalog is intentionally a local projection rather than a
full re-generation of the upstream file.  This script therefore validates
the pinned input and changes only the four models covered by this update.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


SOURCE_REF = "eeb7732fc11fd47762ca84cc3fb7cc74235d7097"
SOURCE_URL = (
    "https://raw.githubusercontent.com/BerriAI/litellm/"
    f"{SOURCE_REF}/model_prices_and_context_window.json"
)
SOURCE_SHA256 = "f68d88c12610ea31ab355a1293fde55aeed6fa78a1f4b182c67be47d80b1d202"
VERIFIED_AT = "2026-09-12"
NANODOLLARS_PER_DOLLAR = Decimal("1000000000")
THRESHOLD_INPUT_TOKENS = 272_000

TARGET_MODELS = (
    "gpt-6-astra",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
)

JSON_KEYS = (
    (
        "input_cost_per_token",
        "input_cost_per_token_above_272k_tokens",
    ),
    (
        "cache_read_input_token_cost",
        "cache_read_input_token_cost_above_272k_tokens",
    ),
    (
        "cache_creation_input_token_cost",
        "cache_creation_input_token_cost_above_272k_tokens",
    ),
    (
        "output_cost_per_token",
        "output_cost_per_token_above_272k_tokens",
    ),
)

# Values are nanodollars/token, ordered input, cached input, cache write,
# output for short and long context respectively.
EXPECTED_RATES: dict[str, tuple[tuple[int, int, int, int], tuple[int, int, int, int]]] = {
    "gpt-6-astra": (
        (10_000, 1_000, 12_500, 50_000),
        (20_000, 2_000, 25_000, 75_000),
    ),
    "gpt-5.6-sol": (
        (4_000, 400, 5_000, 20_000),
        (8_000, 800, 10_000, 30_000),
    ),
    "gpt-5.6-terra": (
        (2_000, 200, 2_500, 12_000),
        (4_000, 400, 5_000, 18_000),
    ),
    "gpt-5.6-luna": (
        (200, 20, 250, 1_200),
        (400, 40, 500, 1_800),
    ),
}

MODEL_CONSTANTS = {
    "gpt-6-astra": "SNAPSHOT_GPT_6_ASTRA_PRICING",
    "gpt-5.6-sol": "SNAPSHOT_GPT_5_6_SOL_PRICING",
    "gpt-5.6-terra": "SNAPSHOT_GPT_5_6_TERRA_PRICING",
    "gpt-5.6-luna": "SNAPSHOT_GPT_5_6_LUNA_PRICING",
}

MODEL_IDS_RE = re.compile(
    r"(?ms)^pub const LITELLM_SNAPSHOT_MODEL_IDS: &\[&str\] = &\[\n"
    r"(?P<body>.*?)^\];"
)
CATALOG_RE = re.compile(
    r"(?ms)^pub const LITELLM_OPENAI_PRICING_CATALOG: &\[ModelPricing\] = &\[\n"
    r"(?P<body>.*?)^\];"
)
CONSTANT_RE_TEMPLATE = (
    r"(?ms)^pub const {name}: ModelPricing = ModelPricing \{{\n"
    r".*?^\}};\n?"
)
COUNT_COMMENT_RE = re.compile(
    r"(?m)^// (?:Projection counts:|Local projection counts after this round:).*$"
)


class GenerationError(Exception):
    """An input or catalog invariant required for a safe local update failed."""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate the four-model Usagi LiteLLM catalog candidate."
    )
    parser.add_argument("--input", required=True, type=Path, help="pinned LiteLLM JSON")
    parser.add_argument("--catalog", required=True, type=Path, help="current Rust catalog")
    parser.add_argument("--output", required=True, type=Path, help="candidate Rust catalog")
    return parser.parse_args()


def read_pinned_json(path: Path) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise GenerationError(f"cannot read input JSON {path}: {exc}") from exc

    actual_sha256 = hashlib.sha256(raw).hexdigest()
    if actual_sha256 != SOURCE_SHA256:
        raise GenerationError(
            "input JSON SHA-256 mismatch: "
            f"expected {SOURCE_SHA256}, got {actual_sha256}"
        )

    try:
        decoded = json.loads(
            raw.decode("utf-8"), parse_float=Decimal, parse_int=Decimal
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GenerationError(f"cannot parse input JSON {path}: {exc}") from exc

    if not isinstance(decoded, dict):
        raise GenerationError("input JSON root must be an object")
    return decoded


def to_nanodollars(value: Any, model: str, key: str) -> int:
    if isinstance(value, bool) or not isinstance(value, (Decimal, int)):
        raise GenerationError(f"{model}.{key} is not a JSON decimal number")

    try:
        decimal_value = value if isinstance(value, Decimal) else Decimal(value)
        scaled = decimal_value * NANODOLLARS_PER_DOLLAR
        integral = scaled.to_integral_exact()
    except (InvalidOperation, ValueError, TypeError) as exc:
        raise GenerationError(f"{model}.{key} cannot be converted exactly") from exc

    if not scaled.is_finite() or scaled != integral:
        raise GenerationError(f"{model}.{key} cannot be converted exactly")

    result = int(integral)
    if result < 0:
        raise GenerationError(f"{model}.{key} must not be negative")
    return result


def read_target_rates(data: dict[str, Any]) -> dict[str, tuple[tuple[int, int, int, int], tuple[int, int, int, int]]]:
    rates: dict[str, tuple[tuple[int, int, int, int], tuple[int, int, int, int]]] = {}
    for model in TARGET_MODELS:
        entry = data.get(model)
        if not isinstance(entry, dict):
            raise GenerationError(f"target model {model!r} is missing from input JSON")

        short_values: list[int] = []
        long_values: list[int] = []
        for short_key, long_key in JSON_KEYS:
            if short_key not in entry:
                raise GenerationError(f"target field {model}.{short_key} is missing")
            if long_key not in entry:
                raise GenerationError(f"target field {model}.{long_key} is missing")
            short_values.append(to_nanodollars(entry[short_key], model, short_key))
            long_values.append(to_nanodollars(entry[long_key], model, long_key))

        actual = (tuple(short_values), tuple(long_values))
        expected = EXPECTED_RATES[model]
        if actual != expected:
            raise GenerationError(
                f"{model} pricing matrix mismatch: expected {expected}, got {actual}"
            )
        rates[model] = actual
    return rates


def model_block(model: str, rates: tuple[tuple[int, int, int, int], tuple[int, int, int, int]]) -> str:
    constant = MODEL_CONSTANTS[model]
    short, long = rates
    short_input, short_cached, short_write, short_output = short
    long_input, long_cached, long_write, long_output = long
    return (
        f"pub const {constant}: ModelPricing = ModelPricing {{\n"
        f'    canonical_model_id: "{model}",\n'
        "    effective_from_ms: i64::MIN,\n"
        "    effective_to_ms: None,\n"
        "    short_context: TokenRates::new("
        f"{short_input}, {short_cached}, Some({short_write}), {short_output}),\n"
        "    long_context: Some(LongContextPolicy::new(\n"
        f"        {THRESHOLD_INPUT_TOKENS:,}, TokenRates::new("
        f"{long_input}, {long_cached}, Some({long_write}), {long_output}),\n"
        "    )),\n"
        "};\n"
    ).replace(f"{THRESHOLD_INPUT_TOKENS:,}", "272_000")


def update_header(source: str) -> str:
    lines = source.splitlines(keepends=True)
    header_end = None
    for index, line in enumerate(lines):
        if index == 0 and not line.startswith("//"):
            raise GenerationError("catalog must start with a source header")
        if index > 0 and (line.strip() == "" or not line.startswith("//")):
            header_end = index
            break
    if header_end is None:
        raise GenerationError("catalog source header is missing its terminating blank line")

    newline = "\r\n" if "\r\n" in source else "\n"
    metadata = {
        "LITELLM_SNAPSHOT_SOURCE_URL": f"// LITELLM_SNAPSHOT_SOURCE_URL: {SOURCE_URL}",
        "LITELLM_SNAPSHOT_SOURCE_REF": f"// LITELLM_SNAPSHOT_SOURCE_REF: {SOURCE_REF}",
        "LITELLM_SNAPSHOT_RETRIEVED_AT": (
            f"// LITELLM_SNAPSHOT_RETRIEVED_AT: {VERIFIED_AT}"
        ),
        "LITELLM_SNAPSHOT_SHA256": f"// LITELLM_SNAPSHOT_SHA256: {SOURCE_SHA256}",
        "LITELLM_SNAPSHOT_VERIFIED_AT": (
            f"// LITELLM_SNAPSHOT_VERIFIED_AT: {VERIFIED_AT}"
        ),
        "LITELLM_SNAPSHOT_SCOPE": (
            "// LITELLM_SNAPSHOT_SCOPE: this round updates only four target models "
            "(gpt-6-astra, gpt-5.6-sol, gpt-5.6-terra, gpt-5.6-luna)"
        ),
    }

    seen: set[str] = set()
    for index in range(header_end):
        match = re.match(r"^// (LITELLM_SNAPSHOT_[A-Z0-9_]+):", lines[index])
        if not match or match.group(1) not in metadata:
            continue
        key = match.group(1)
        if key in seen:
            raise GenerationError(f"catalog header contains duplicate {key}")
        seen.add(key)
        lines[index] = metadata[key] + newline

    missing = [key for key in metadata if key not in seen]
    if missing:
        lines[header_end:header_end] = [metadata[key] + newline for key in missing]
    return "".join(lines)


def replace_model_ids(source: str) -> str:
    matches = list(MODEL_IDS_RE.finditer(source))
    if len(matches) != 1:
        raise GenerationError("catalog must contain exactly one snapshot model ID list")
    match = matches[0]
    body = match.group("body")
    ids = re.findall(r'(?m)^\s*"([^"]+)",\s*$', body)

    for model in TARGET_MODELS:
        count = ids.count(model)
        if model == "gpt-6-astra":
            if count > 1:
                raise GenerationError("snapshot identity for gpt-6-astra is duplicated")
        elif count != 1:
            raise GenerationError(f"snapshot identity for {model} must occur exactly once")

    if "gpt-6-astra" not in ids:
        marker = '    "gpt-5.6-terra",\n'
        if body.count(marker) != 1:
            raise GenerationError("cannot locate insertion point for gpt-6-astra identity")
        body = body.replace(marker, marker + '    "gpt-6-astra",\n', 1)

    return source[: match.start("body")] + body + source[match.end("body") :]


def replace_target_blocks(source: str, rates: dict[str, tuple[tuple[int, int, int, int], tuple[int, int, int, int]]]) -> str:
    for model in TARGET_MODELS:
        constant = MODEL_CONSTANTS[model]
        pattern = re.compile(CONSTANT_RE_TEMPLATE.format(name=re.escape(constant)))
        matches = list(pattern.finditer(source))
        if model == "gpt-6-astra":
            if len(matches) > 1:
                raise GenerationError(f"pricing constant for {model} is duplicated")
        elif len(matches) != 1:
            raise GenerationError(f"pricing constant for {model} must occur exactly once")

        if matches:
            match = matches[0]
            source = source[: match.start()] + model_block(model, rates[model]) + source[match.end() :]

    if not re.search(
        rf"(?ms)^pub const {re.escape(MODEL_CONSTANTS['gpt-6-astra'])}: ModelPricing",
        source,
    ):
        terra_constant = MODEL_CONSTANTS["gpt-5.6-terra"]
        terra_pattern = re.compile(CONSTANT_RE_TEMPLATE.format(name=re.escape(terra_constant)))
        terra = terra_pattern.search(source)
        if terra is None:
            raise GenerationError("cannot locate insertion point for Astra pricing constant")
        source = (
            source[: terra.end()]
            + "\n"
            + model_block("gpt-6-astra", rates["gpt-6-astra"])
            + source[terra.end() :]
        )
    return source


def replace_catalog_reference(source: str) -> str:
    matches = list(CATALOG_RE.finditer(source))
    if len(matches) != 1:
        raise GenerationError("catalog must contain exactly one OpenAI pricing catalog")
    match = matches[0]
    body = match.group("body")
    astra_constant = MODEL_CONSTANTS["gpt-6-astra"]
    terra_constant = MODEL_CONSTANTS["gpt-5.6-terra"]

    for model in TARGET_MODELS:
        constant = MODEL_CONSTANTS[model]
        count = len(re.findall(rf"(?m)^\s*{re.escape(constant)},\s*$", body))
        if model == "gpt-6-astra":
            if count > 1:
                raise GenerationError(f"pricing catalog entry for {model} is duplicated")
        elif count != 1:
            raise GenerationError(f"pricing catalog entry for {model} must occur exactly once")

    if not re.search(rf"(?m)^\s*{re.escape(astra_constant)},\s*$", body):
        marker = f"    {terra_constant},\n"
        if body.count(marker) != 1:
            raise GenerationError("cannot locate insertion point for Astra catalog entry")
        body = body.replace(marker, marker + f"    {astra_constant},\n", 1)

    return source[: match.start("body")] + body + source[match.end("body") :]


def update_count_comment(source: str) -> str:
    id_list = MODEL_IDS_RE.search(source)
    catalog = CATALOG_RE.search(source)
    if id_list is None or catalog is None:
        raise GenerationError("catalog structures disappeared during generation")

    ids = re.findall(r'(?m)^\s*"([^"]+)",\s*$', id_list.group("body"))
    body = catalog.group("body")
    inline_entries = len(re.findall(r"(?m)^\s*ModelPricing \{\s*$", body))
    constant_entries = len(
        re.findall(r"(?m)^\s*SNAPSHOT_[A-Z0-9_]+_PRICING,\s*$", body)
    )
    count_line = (
        f"// Local projection counts after this round: {len(ids)} model IDs, "
        f"{inline_entries + constant_entries} priced entries."
    )
    updated, count = COUNT_COMMENT_RE.subn(count_line, source, count=1)
    if count == 0:
        raise GenerationError("catalog projection count comment is missing")
    return updated


def generate(data: dict[str, Any], source: str) -> str:
    rates = read_target_rates(data)
    updated = update_header(source)
    updated = replace_model_ids(updated)
    updated = replace_target_blocks(updated, rates)
    updated = replace_catalog_reference(updated)
    return update_count_comment(updated)


def main() -> int:
    args = parse_args()
    data = read_pinned_json(args.input)
    try:
        source = args.catalog.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        print(f"error: cannot read catalog {args.catalog}: {exc}", file=sys.stderr)
        return 1

    try:
        candidate = generate(data, source)
    except GenerationError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    try:
        args.output.write_bytes(candidate.encode("utf-8"))
    except OSError as exc:
        print(f"error: cannot write candidate {args.output}: {exc}", file=sys.stderr)
        return 1

    print(
        f"generated {args.output} from {SOURCE_REF} "
        f"(SHA-256 {SOURCE_SHA256}; {len(TARGET_MODELS)} target models)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
