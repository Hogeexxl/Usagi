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

scan_active_source() {
	local pattern="$1"
	shift
	grep -R -n -i -E \
		--include='*.go' --include='*.ts' --include='*.tsx' --include='*.js' --include='*.mjs' --include='*.jsx' \
		--include='*.py' --include='*.sh' --include='*.ps1' --include='*.cmd' --include='*.bat' \
		--include='*.yml' --include='*.yaml' --include='*.json' --include='*.html' --include='*.css' \
		--include='*.md' --include='*.nsi' --include='*.plist' --include='*.toml' \
		--exclude-dir=dist --exclude-dir=dist-codex --exclude-dir=node_modules --exclude-dir=.git \
		--exclude='check_repository_residue.sh' \
		-e "$pattern" "$@"
}

active_scan_roots() {
	local path
	for path in cmd internal frontend scripts packaging plugins assets .github/workflows README.md; do
		if [[ -e "$path" ]]; then
			printf '%s\n' "$path"
		fi
	done
}

expect_no_source_hits() {
	local label="$1"
	local pattern="$2"
	shift 2
	local paths=("$@")
	if [[ "${#paths[@]}" -eq 0 ]]; then
		local path
		while IFS= read -r path; do
			paths+=("$path")
		done < <(active_scan_roots)
	fi
	local matches
	local status
	if matches="$(scan_active_source "$pattern" "${paths[@]}" 2>&1)"; then
		fail_with_matches "$label found in active source or CI:" "$matches"
	else
		status=$?
		if [[ "$status" -ne 1 ]]; then
			fail_with_matches "could not scan active source for $label (scan exit $status):" "$matches"
		fi
	fi
}

expect_no_workflow_hits() {
	local label="$1"
	local pattern="$2"
	local matches
	local status
	if [[ ! -d .github/workflows ]]; then
		fail "current CI workflow directory is missing"
	fi
	if matches="$(scan_active_source "$pattern" .github/workflows 2>&1)"; then
		fail_with_matches "$label found in active CI or release workflows:" "$matches"
	else
		status=$?
		if [[ "$status" -ne 1 ]]; then
			fail_with_matches "could not scan active workflows for $label (scan exit $status):" "$matches"
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
	scripts/ci/check_spec05_residue.sh \
	scripts/dev/build_rust_schema_fixtures.py \
	scripts/dev/build_antigravity_wal_fixture.py \
	tools/generate_litellm_claude_catalog.py \
	tools/generate_litellm_openai_catalog.py; do
	assert_absent "$path"
done
printf 'PASS: Rust backend and legacy Go paths are absent\n'

expect_no_source_hits 'S06 old query cache symbols' '\b(usageStore|dayBucket|dayFetch|warmDays|idleTTL|lastUsed|inflight|snapshotMu|refreshMu|sessionSnapshot|newSessionLookups|generation)\b' internal/query
expect_no_source_hits 'S06 old query entry points and result types' '\b(UsageQuery|UsageRowsQuery|UsageRowsResult|UsageResult|UsageGroupBy|SessionResult|SessionRefreshFact|RefreshUsageRows|UsageSnapshot|RefreshSessions)\b|func[[:space:]]+\(s[[:space:]]+\*Service\)[[:space:]]+(Usage|UsageRows|Sessions)[[:space:]]*\(' internal/query
assert_absent internal/query/store.go
assert_absent internal/query/store_test.go
expect_no_source_hits 'S06 conflicting parent error' 'ErrConflictingThreadParent'
expect_no_source_hits 'S06 old model sorting branch' '^[[:space:]]*\|[[:space:]]*"model"([[:space:]]*[,;])?$|DEFAULT_SORT_BY[[:space:]]*:[^;]*"model"|sortBy[[:space:]]*===?[[:space:]]*"model"|sort_by[[:space:]]*:[[:space:]]*"model"|case[[:space:]]*"model"' frontend/src/dashboard/session frontend/src/data/types.ts
printf 'PASS: S06 query cache, compatibility entry points, conflicting parent error, and model sorting are absent\n'

expect_no_source_hits 'updater endpoint residue' '/api/update/'
expect_no_source_hits 'fixed 3210 port residue' '127[.]0[.]0[.]1:3210|(^|[^0-9]):3210([^0-9]|$)'
expect_no_source_hits 'legacy scanner/storage fallback residue' 'cmd/storage-check|internal/(storage|source|ingestion|codex|domain|usage)/|legacy_profiles|file_identity(_unix|_windows)?[.]go|\b(legacy|old)[[:space:]_-]+(scanner|storage|parser|sqlite)\b|\b(scanner|storage|parser|sqlite)[[:space:]_-]+(fallback|legacy|old)\b|\b(fallback|legacy|old)[[:space:]_-]+(scanner|storage|parser|sqlite)\b|\b(LegacyScanner|ScannerFallback|FallbackScanner|LegacyStorage|StorageFallback|LegacyParser|ParserFallback|FallbackParser)\b|canonical ledger'
printf 'PASS: updater, fixed-port, and legacy scanner/storage references are absent from active source and CI\n'

expect_no_workflow_hits 'Rust toolchain or legacy packaging residue' 'cargo[[:space:]]+(check|test|build|fmt|clippy|packager)|cargo-packager|rustfmt|codesign|notarization|rust[[:space:]]+oracle|windows[[:space:]]+release[[:space:]]+smoke'
printf 'PASS: CI and release workflows contain no Rust toolchain or legacy packaging residue\n'

active_files="$(
	find . \
		\( -path './.git' -o -path './docs' -o -path '*/node_modules' -o -path '*/dist' -o -path '*/dist-codex' \) -prune \
		-o \( -type f -o -type l \) \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) -print |
		sort
)"
if [[ -n "$active_files" ]]; then
	fail_with_matches 'Rust source or manifest files remain in the active repository surface:' "$active_files"
fi
expect_no_source_hits 'Rust production runtime residue' '\b(rust|cargo|rustfmt|cargo-packager)\b'
expect_no_source_hits 'MiniUsage runtime residue' '\bMiniUsage\b|\bMini[[:space:]_-]?Usage\b|mu[.]sqlite3?'
expect_no_source_hits 'Tray runtime residue' '\bTray\b|tray-measure|tray-icon'
assert_absent frontend/src/tray
assert_absent assets/windows
expect_no_source_hits 'README legacy runtime or direct-scanner claims' 'Rust|Cargo|MiniUsage|mu[.]sqlite3?|CODEX_HOME|rollout|state_5[.]sqlite|session_index[.]jsonl|~/.codex' README.md
printf 'PASS: Spec06 Rust, MiniUsage, Tray, and README residue checks pass\n'
