$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '../..')
$Base = '6c477c85c905b9bc398c754510a65522e5f44fe5'
git diff --exit-code $Base -- src/storage/migrations.rs src/storage/schema/
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output 'Rust Oracle baseline: PASS'
if (-not (Test-Path 'tools/rust-schema-oracle/Cargo.lock' -PathType Leaf)) {
    throw 'Rust Oracle Cargo.lock is missing'
}
cargo metadata --manifest-path tools/rust-schema-oracle/Cargo.toml --locked --format-version 1 | Out-Null
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output 'Rust Oracle lockfile: PASS'
