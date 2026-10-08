#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."
base=6c477c85c905b9bc398c754510a65522e5f44fe5
git diff --exit-code "$base" -- src/storage/migrations.rs src/storage/schema/
printf 'Rust Oracle baseline: PASS\n'
test -f tools/rust-schema-oracle/Cargo.lock
cargo metadata --manifest-path tools/rust-schema-oracle/Cargo.toml --locked --format-version 1 > /dev/null
printf 'Rust Oracle lockfile: PASS\n'
