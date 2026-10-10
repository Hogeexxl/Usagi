$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '../..')

$env:GOTOOLCHAIN = 'local'
if ($env:GOTOOLCHAIN -cne 'local') { throw "Expected GOTOOLCHAIN=local, got $env:GOTOOLCHAIN" }
$GoVersion = go env GOVERSION
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($GoVersion -cne 'go1.27.1') { throw "Expected go1.27.1, got $GoVersion" }

$Unformatted = @(gofmt -l internal/codex internal/platform)
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Unformatted.Count -ne 0) { throw "Unformatted Go files: $Unformatted" }
go test ./internal/platform/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/codex/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go test ./internal/source/... ./internal/storage/... ./internal/usage/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
go vet ./internal/codex/... ./internal/platform/...
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
