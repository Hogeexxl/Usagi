#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

fail_with_matches() {
	printf 'FAIL: %s\n%s\n' "$1" "$2" >&2
	exit 1
}

assert_absent() {
	local path="$1"
	if [[ -e "$path" || -L "$path" ]]; then
		fail "legacy path remains: $path"
	fi
}

expect_no_source_hits() {
	local label="$1"
	local pattern="$2"
	local matches
	local status
	if matches="$(rg -n -i --color never --glob '*.{go,ts,tsx,js,mjs,jsx,py,sh,yml,yaml,json,html,css}' --glob '!**/dist/**' --glob '!**/node_modules/**' --glob '!**/check_spec05_residue.sh' -e "$pattern" cmd internal frontend scripts .github/workflows/ci.yml 2>&1)"; then
		fail_with_matches "$label found in active source or CI:" "$matches"
	else
		status=$?
		if [[ "$status" -ne 1 ]]; then
			fail_with_matches "could not scan active source for $label (rg exit $status):" "$matches"
		fi
	fi
}

expect_no_ci_hits() {
	local label="$1"
	local pattern="$2"
	local matches
	local status
	if matches="$(rg -n -i --color never -e "$pattern" .github/workflows/ci.yml 2>&1)"; then
		fail_with_matches "$label found in current CI:" "$matches"
	else
		status=$?
		if [[ "$status" -ne 1 ]]; then
			fail_with_matches "could not scan current CI for $label (rg exit $status):" "$matches"
		fi
	fi
}

for path in \
	src \
	tests \
	Cargo.toml \
	Cargo.lock \
	cmd/storage-check \
	internal/storage \
	internal/source \
	internal/ingestion \
	internal/codex \
	internal/domain \
	internal/usage \
	internal/platform/file_identity.go \
	internal/platform/file_identity_test.go \
	internal/platform/file_identity_unix.go \
	internal/platform/file_identity_windows.go \
	internal/platform/paths.go \
	internal/platform/paths_test.go \
	tools/rust-schema-oracle \
	.github/workflows/release.yml \
	.github/workflows/s13-final-validation.yml \
	.github/scripts/s13-public-release-e2e.sh \
	.github/scripts/windows-release-smoke.ps1 \
	scripts/ci/check_codex_compaction.sh \
	scripts/ci/check_oracle_baseline.ps1 \
	scripts/ci/check_oracle_baseline.sh \
	scripts/ci/check_spec01.ps1 \
	scripts/ci/check_spec01.sh \
	scripts/ci/check_spec02.ps1 \
	scripts/ci/check_spec02.sh \
	scripts/ci/check_spec03.ps1 \
	scripts/ci/check_spec03.sh \
	scripts/ci/check_track_d_residue.sh \
	scripts/dev/build_rust_schema_fixtures.py \
	scripts/dev/build_antigravity_wal_fixture.py \
	tools/generate_litellm_claude_catalog.py \
	tools/generate_litellm_openai_catalog.py; do
	assert_absent "$path"
done
printf 'PASS: Rust backend and legacy Go paths are absent\n'

if rg -n -i 'modernc[.]org/(sqlite|libc|mathutil|memory)' go.mod go.sum; then
	fail 'SQLite driver or its transitive modules remain in Go dependencies'
else
	status=$?
	if [[ "$status" -ne 1 ]]; then
		fail "could not scan Go dependency files (rg exit $status)"
	fi
fi
printf 'PASS: SQLite dependencies are absent\n'

expect_no_source_hits 'updater endpoint residue' '/api/update/'
expect_no_source_hits 'fixed 3210 port residue' '127[.]0[.]0[.]1:3210|(^|[^0-9]):3210([^0-9]|$)'
expect_no_source_hits 'legacy scanner/storage fallback residue' 'cmd/storage-check|internal/(storage|source|ingestion|codex|domain|usage)/|modernc[.]org/|sqlite|legacy_profiles|file_identity(_unix|_windows)?[.]go|\b(legacy|old)[[:space:]_-]+(scanner|storage|parser|sqlite)\b|\b(scanner|storage|parser|sqlite)[[:space:]_-]+(fallback|legacy|old)\b|\b(fallback|legacy|old)[[:space:]_-]+(scanner|storage|parser|sqlite)\b|\b(LegacyScanner|ScannerFallback|FallbackScanner|LegacyStorage|StorageFallback|LegacyParser|ParserFallback|FallbackParser)\b|canonical ledger'
printf 'PASS: updater, fixed-port, and legacy scanner/storage references are absent from active source and CI\n'

expect_no_ci_hits 'Windows, Rust, or packaging gate' 'windows-latest|cargo[[:space:]]+(check|test|build|fmt|clippy|packager)|cargo-packager|rustfmt|codesign|notarization|(^|[^[:alnum:]_])dmg([^[:alnum:]_]|$)|nsis|windows[[:space:]]+installer|rust[[:space:]]+oracle|windows[[:space:]]+release[[:space:]]+smoke'
printf 'PASS: current CI has no Windows, Rust, or packaging steps\n'
