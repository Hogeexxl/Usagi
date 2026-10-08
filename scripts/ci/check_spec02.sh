#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

export GOTOOLCHAIN=local
test "$GOTOOLCHAIN" = local
test "$(go env GOVERSION)" = go1.27.1

git diff --exit-code 6c477c85c905b9bc398c754510a65522e5f44fe5 -- \
    src/source src/storage/lifecycle.rs src/ingestion
UNFORMATTED=$(gofmt -l internal)
test -z "$UNFORMATTED"
go vet ./...
go test ./internal/platform/... ./internal/domain/... ./internal/usage/... ./internal/storage/...
go test ./internal/source/... ./internal/storage/... -run 'Test(Source|RunContext|Private|Usage|Revision|Active|NoRevision|Adapter|Availability|Registry|FirstUsage|Copy|Begin|Retarget|Ensure|Build|Inactive|Thread|Nil)'
go test ./internal/storage/... -run 'Test(DirectScan|Followup|ScanComplete|ScanAggregate|ExplicitScan|SourceScan|Lifecycle|ScanSnapshot|TerminalScan)'
go test ./internal/ingestion/... -count=1
go test ./internal/ingestion/... -run TestSpec02FakeAdapterEndToEnd -count=1
go test -race ./internal/storage/... ./internal/source/... ./internal/ingestion/...
go test ./internal/ingestion/... -run 'Test(OnlyOneActiveWorker|TerminalPersistenceRetriesBusyAndInternal|ShutdownDurableBeforeAck|ShutdownCallerTimeoutDoesNotAbortCleanup|ShutdownDuringRecoveryPreventsNewScanStart|ShutdownTerminalPendingOwnership|ShutdownCommandMustInstallReplyBeforeStopped|RequestLoopDoneDoesNotHang|ShutdownLoopDoneDoesNotHang|StartCommitRaceKeepsRustParity|CancellationDuringAvailabilityMatchesRust|RunScanCancellationRustPrecedence|LateWorkerFinishedAfterLoopExitDoesNotLeak|SourceReportsReplacedAfterCancelledRun)' -count=20
go mod tidy
git diff --exit-code -- go.mod go.sum
