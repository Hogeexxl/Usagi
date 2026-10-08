$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '../..')

$env:GOTOOLCHAIN = 'local'
if ($env:GOTOOLCHAIN -cne 'local') { throw "Expected GOTOOLCHAIN=local, got $env:GOTOOLCHAIN" }
$GoVersion = go env GOVERSION
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($GoVersion -cne 'go1.27.1') { throw "Expected go1.27.1, got $GoVersion" }

git diff --exit-code 6c477c85c905b9bc398c754510a65522e5f44fe5 -- src/source src/storage/lifecycle.rs src/ingestion
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$Unformatted = @(gofmt -l internal)
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Unformatted.Count -ne 0) { throw "Unformatted Go files: $Unformatted" }
go vet ./...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/platform/... ./internal/domain/... ./internal/usage/... ./internal/storage/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/source/... ./internal/storage/... -run 'Test(Source|RunContext|Private|Usage|Revision|Active|NoRevision|Adapter|Availability|Registry|FirstUsage|Copy|Begin|Retarget|Ensure|Build|Inactive|Thread|Nil)'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/storage/... -run 'Test(DirectScan|Followup|ScanComplete|ScanAggregate|ExplicitScan|SourceScan|Lifecycle|ScanSnapshot|TerminalScan)'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/ingestion/... -count=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/ingestion/... -run TestSpec02FakeAdapterEndToEnd -count=1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test -race ./internal/storage/... ./internal/source/... ./internal/ingestion/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/ingestion/... -run 'Test(OnlyOneActiveWorker|TerminalPersistenceRetriesBusyAndInternal|ShutdownDurableBeforeAck|ShutdownCallerTimeoutDoesNotAbortCleanup|ShutdownDuringRecoveryPreventsNewScanStart|ShutdownTerminalPendingOwnership|ShutdownCommandMustInstallReplyBeforeStopped|RequestLoopDoneDoesNotHang|ShutdownLoopDoneDoesNotHang|StartCommitRaceKeepsRustParity|CancellationDuringAvailabilityMatchesRust|RunScanCancellationRustPrecedence|LateWorkerFinishedAfterLoopExitDoesNotLeak|SourceReportsReplacedAfterCancelledRun)' -count=20
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go mod tidy
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
git diff --exit-code -- go.mod go.sum
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
