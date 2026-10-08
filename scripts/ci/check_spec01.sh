#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 || $1 != --python || $2 != /* || ! -f $2 ]]; then
    printf 'Usage: %s --python <absolute-path-to-python>\n' "$0" >&2
    exit 1
fi
PYTHON_EXECUTABLE=$2
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

bash scripts/ci/check_oracle_baseline.sh
export GOTOOLCHAIN=local
test "$(go env GOVERSION)" = go1.27.1
"$PYTHON_EXECUTABLE" -c 'import sys; assert sys.version_info[:3] == (3, 14, 8)'
UNFORMATTED=$(gofmt -l cmd internal tools)
test -z "$UNFORMATTED"
go vet ./...
go test ./internal/platform/... ./internal/domain/... ./internal/usage/... ./internal/storage/...
go test ./internal/storage/... -run TestRustV14SemanticSchemaParity
"$PYTHON_EXECUTABLE" scripts/dev/build_rust_schema_fixtures.py --check-profiles
go test ./internal/storage/... -run 'TestLegacyConversionV(0[1-9]|1[0-4])ToV14|TestLegacyConversionV11AssistVariant'
go test ./internal/storage/... -run 'TestCheckpointBusy|TestConversionBlocksIndependentSecondWriter|TestLegacyImportReplacesFreshSeeds|TestTempTargetUsesDeleteJournal|TestForeignKeyImportBoundary|TestBackupExistsBeforeActiveCommit|TestCrash|TestBackupPartialFileRecovery|TestLegacyHousekeepingPartialRetry|TestExplicitOpenRetriesMatchingMarker|TestConvertLegacyReentry|TestConvertLegacyRejectsCurrentWithoutMarker|TestSourceIdentityRecheckedAfterLock|TestHousekeepingRejectsReplacedSource|TestWindowsSourceHandlesClosedBeforeHousekeeping|TestPostCommitMarkerFailureStillSucceeds|TestOrphan'
go test -race ./internal/storage/...

TMP_DIR=$(mktemp -d)
trap 'rm -f "$TMP_DIR"/*; rmdir "$TMP_DIR"' EXIT
go run ./cmd/storage-check --db "$TMP_DIR/usagi.sqlite3" > "$TMP_DIR/stdout"
cat "$TMP_DIR/stdout"
printf 'schema_generation=1\nschema_version=1\nvalidation=ok\n' > "$TMP_DIR/expected"
cmp "$TMP_DIR/expected" "$TMP_DIR/stdout"
