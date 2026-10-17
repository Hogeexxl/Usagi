$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$OutputDirectory = Join-Path $RepoRoot "dist\windows-x64"
$BinaryPath = Join-Path $OutputDirectory "usagi.exe"
$BuildManifestPath = Join-Path $OutputDirectory "usagi.build.json"
$VerifiedManifestPath = Join-Path $OutputDirectory "usagi.pe.json"

if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
    throw "missing Windows runtime: $BinaryPath"
}
if ((Split-Path -Leaf $BinaryPath) -cne "usagi.exe") {
    throw "Windows runtime artifact must be named usagi.exe"
}
if (-not (Test-Path -LiteralPath $BuildManifestPath -PathType Leaf)) {
    throw "missing Windows runtime build manifest: $BuildManifestPath"
}

$BuildManifest = Get-Content -LiteralPath $BuildManifestPath -Raw | ConvertFrom-Json
if ($BuildManifest.goos -ne "windows" -or $BuildManifest.goarch -ne "amd64") {
    throw "runtime build manifest is not for windows/amd64"
}
if ($BuildManifest.version -notmatch '^(?:[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?|0\.0\.0-ci)$') {
    throw "runtime build manifest has an invalid version"
}
$Digest = (Get-FileHash -LiteralPath $BinaryPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($BuildManifest.sha256 -ne $Digest) {
    throw "runtime binary does not match its build manifest"
}

$Bytes = [System.IO.File]::ReadAllBytes($BinaryPath)
if ($Bytes.Length -lt 64) {
    throw "runtime is too small to be a valid PE file"
}
if ($Bytes[0] -ne 0x4D -or $Bytes[1] -ne 0x5A) {
    throw "runtime does not have an MZ header"
}

$PEOffset = [BitConverter]::ToInt32($Bytes, 0x3C)
if ($PEOffset -lt 0 -or $PEOffset + 24 -gt $Bytes.Length) {
    throw "runtime has an invalid PE header offset"
}
if (
    $Bytes[$PEOffset] -ne 0x50 -or
    $Bytes[$PEOffset + 1] -ne 0x45 -or
    $Bytes[$PEOffset + 2] -ne 0x00 -or
    $Bytes[$PEOffset + 3] -ne 0x00
) {
    throw "runtime does not have a PE signature"
}

$Machine = [BitConverter]::ToUInt16($Bytes, $PEOffset + 4)
$OptionalHeaderSize = [BitConverter]::ToUInt16($Bytes, $PEOffset + 20)
$OptionalHeaderOffset = $PEOffset + 24
if ($OptionalHeaderSize -lt 70 -or $OptionalHeaderOffset + $OptionalHeaderSize -gt $Bytes.Length) {
    throw "runtime has an invalid PE optional header"
}

$OptionalMagic = [BitConverter]::ToUInt16($Bytes, $OptionalHeaderOffset)
$Subsystem = [BitConverter]::ToUInt16($Bytes, $OptionalHeaderOffset + 0x44)
if ($Machine -ne 0x8664) {
    throw ("runtime PE machine is 0x{0:X4}, want AMD64 (0x8664)" -f $Machine)
}
if ($OptionalMagic -ne 0x20B) {
    throw ("runtime optional header magic is 0x{0:X4}, want PE32+ (0x20B)" -f $OptionalMagic)
}
if ($Subsystem -ne 2) {
    throw ("runtime PE subsystem is {0}, want Windows GUI (2)" -f $Subsystem)
}

[ordered]@{
    version = $BuildManifest.version
    goos = "windows"
    goarch = "amd64"
    sha256 = $Digest
    machine = "amd64"
    optional_header = "pe32+"
    subsystem = "windowsgui"
} | ConvertTo-Json -Compress | Set-Content -LiteralPath $VerifiedManifestPath -Encoding ASCII

Write-Host "Windows PE check passed: AMD64 PE32+ Windows GUI ($BinaryPath)"
