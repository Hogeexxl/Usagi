[CmdletBinding()]
param(
    [string]$Version
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($Version)) {
    throw "missing required -Version argument"
}
if ($Version -notmatch '^(?:[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?|0\.0\.0-ci)$') {
    throw "invalid version: $Version"
}

$RepoRoot = Split-Path -Parent $PSScriptRoot
$OutputDirectory = Join-Path $RepoRoot "dist\windows-x64"
$BinaryPath = Join-Path $OutputDirectory "usagi.exe"
$BuildManifestPath = Join-Path $OutputDirectory "usagi.build.json"
$VerifiedManifestPath = Join-Path $OutputDirectory "usagi.pe.json"
$InstallerDefinition = Join-Path $RepoRoot "packaging\windows\installer.nsi"
$InstallerPath = Join-Path $RepoRoot "dist\Usagi-v$Version-windows-x64-setup.exe"

if (Test-Path -LiteralPath $InstallerPath) {
    Remove-Item -LiteralPath $InstallerPath -Force
}
foreach ($required in @($BinaryPath, $BuildManifestPath, $VerifiedManifestPath, $InstallerDefinition)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "missing required packaging input: $required"
    }
}
if ((Split-Path -Leaf $BinaryPath) -cne "usagi.exe") {
    throw "runtime artifact must be named usagi.exe"
}

$BuildManifest = Get-Content -LiteralPath $BuildManifestPath -Raw | ConvertFrom-Json
$VerifiedManifest = Get-Content -LiteralPath $VerifiedManifestPath -Raw | ConvertFrom-Json
if ($BuildManifest.version -ne $Version -or $VerifiedManifest.version -ne $Version) {
    throw "runtime build, PE verification, and package versions must match $Version"
}
$Digest = (Get-FileHash -LiteralPath $BinaryPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($BuildManifest.sha256 -ne $Digest -or $VerifiedManifest.sha256 -ne $Digest) {
    throw "runtime binary does not match its build and PE verification manifests"
}
if (
    $VerifiedManifest.machine -ne "amd64" -or
    $VerifiedManifest.optional_header -ne "pe32+" -or
    $VerifiedManifest.subsystem -ne "windowsgui"
) {
    throw "runtime has not passed the required AMD64 PE32+ Windows GUI gate"
}

$MakensisCommand = Get-Command "makensis.exe" -ErrorAction SilentlyContinue
if ($null -eq $MakensisCommand) {
    throw "makensis.exe was not found; install and verify NSIS 3.13.0 before packaging"
}
$Makensis = $MakensisCommand.Source
$MakensisVersion = (& $Makensis /VERSION 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "makensis.exe /VERSION failed with exit code $LASTEXITCODE"
}
if ($MakensisVersion -cne "v3.13") {
    throw "NSIS version is '$MakensisVersion', want v3.13"
}

& $Makensis @(
    "/V2",
    "/DVERSION=$Version",
    "/DAPP_EXE=$BinaryPath",
    "/DOUTPUT=$InstallerPath",
    $InstallerDefinition
)
if ($LASTEXITCODE -ne 0) {
    throw "makensis.exe failed with exit code $LASTEXITCODE"
}
if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf) -or (Get-Item -LiteralPath $InstallerPath).Length -eq 0) {
    throw "installer packaging did not produce $InstallerPath"
}

Write-Host "created $InstallerPath"
