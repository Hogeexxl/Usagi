param(
    [Parameter(Mandatory = $true)]
    [string]$PythonExecutable
)
$ErrorActionPreference = 'Stop'
if (-not [System.IO.Path]::IsPathFullyQualified($PythonExecutable) -or
    -not (Test-Path -LiteralPath $PythonExecutable -PathType Leaf)) {
    throw 'PythonExecutable must be an existing absolute file path'
}
Set-Location (Join-Path $PSScriptRoot '../..')

pwsh -File scripts/ci/check_oracle_baseline.ps1
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:GOTOOLCHAIN = 'local'
$GoVersion = go env GOVERSION
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($GoVersion -cne 'go1.27.1') { throw "Expected go1.27.1, got $GoVersion" }
& $PythonExecutable -c 'import sys; assert sys.version_info[:3] == (3, 14, 8)'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$Unformatted = @(gofmt -l cmd internal tools)
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Unformatted.Count -ne 0) { throw "Unformatted Go files: $Unformatted" }
go vet ./...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/platform/... ./internal/domain/... ./internal/usage/... ./internal/storage/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/storage/... -run TestRustV14SemanticSchemaParity
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $PythonExecutable scripts/dev/build_rust_schema_fixtures.py --check-profiles
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/storage/... -run 'TestLegacyConversionV(0[1-9]|1[0-4])ToV14|TestLegacyConversionV11AssistVariant'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/storage/... -run 'TestCheckpointBusy|TestConversionBlocksIndependentSecondWriter|TestLegacyImportReplacesFreshSeeds|TestTempTargetUsesDeleteJournal|TestForeignKeyImportBoundary|TestBackupExistsBeforeActiveCommit|TestCrash|TestBackupPartialFileRecovery|TestLegacyHousekeepingPartialRetry|TestExplicitOpenRetriesMatchingMarker|TestConvertLegacyReentry|TestConvertLegacyRejectsCurrentWithoutMarker|TestSourceIdentityRecheckedAfterLock|TestHousekeepingRejectsReplacedSource|TestWindowsSourceHandlesClosedBeforeHousekeeping|TestPostCommitMarkerFailureStillSucceeds|TestOrphan'
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test -race ./internal/storage/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$TempDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
$null = New-Item -ItemType Directory -Path $TempDirectory
try {
    $OutputPath = Join-Path $TempDirectory 'stdout'
    go run ./cmd/storage-check --db (Join-Path $TempDirectory 'usagi.sqlite3') > $OutputPath
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $Lines = @(Get-Content -LiteralPath $OutputPath)
    $Lines | Write-Output
    if ($Lines.Count -ne 3 -or
        $Lines[0] -cne 'schema_generation=1' -or
        $Lines[1] -cne 'schema_version=1' -or
        $Lines[2] -cne 'validation=ok') {
        throw 'storage-check stdout must be exactly the three contract lines'
    }
} finally {
    Remove-Item -LiteralPath $TempDirectory -Recurse -Force
}
