[CmdletBinding()]
param(
    [string]$InstallerPath,
    [string]$ExpectedVersion
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot "windows_smoke_common.ps1")

if ([string]::IsNullOrWhiteSpace($InstallerPath)) {
    throw "missing required -InstallerPath argument"
}
if ([string]::IsNullOrWhiteSpace($ExpectedVersion)) {
    throw "missing required -ExpectedVersion argument"
}
if ($ExpectedVersion -notmatch '^(?:[0-9]+\.[0-9]+\.[0-9]+(?:-rc\.[0-9]+)?|0\.0\.0-ci)$') {
    throw "invalid expected version: $ExpectedVersion"
}

$ResolvedInstaller = (Resolve-Path -LiteralPath $InstallerPath).Path
if ((Split-Path -Leaf $ResolvedInstaller) -cne "Usagi-v$ExpectedVersion-windows-x64-setup.exe") {
    throw "installer filename does not match expected version $ExpectedVersion"
}

$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Root = New-SmokeSessionRoot -Label "magpie"
$User = $null
$Failure = $null
try {
    Push-Location $RepoRoot
    try {
        & go build -trimpath -o (Join-Path $Root "magpie-windows-amd64.exe") (Join-Path $RepoRoot "scripts\ci\fixtures\magpie_windows")
        if ($LASTEXITCODE -ne 0) {
            throw "building the native Windows Magpie fixture failed with exit code $LASTEXITCODE"
        }
        & go build -trimpath -o (Join-Path $Root "usagi-smoke-data.exe") (Join-Path $RepoRoot "scripts\ci\fixtures\usagi_smoke_data")
        if ($LASTEXITCODE -ne 0) {
            throw "building the Windows SQLite smoke helper failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }

    $CopiedInstaller = Join-Path $Root "Usagi-v$ExpectedVersion-windows-x64-setup.exe"
    Copy-Item -LiteralPath $ResolvedInstaller -Destination $CopiedInstaller -Force
    $User = New-TemporaryStandardUser -Root $Root
    $Parameters = @{
        installer_path = $CopiedInstaller
        expected_version = $ExpectedVersion
        fixture_dir = $Root
        fixture_exe = (Join-Path $Root "magpie-windows-amd64.exe")
        offline_file = (Join-Path $Root "magpie-offline")
        seed_exe = (Join-Path $Root "usagi-smoke-data.exe")
    }
    Invoke-TemporaryUserWorker -User $User -Scenario "magpie" -Parameters $Parameters -TimeoutSeconds 900
    Write-Host "Windows Magpie machine smoke passed for $ExpectedVersion"
}
catch {
    $Failure = $_
}
finally {
    if ($null -ne $User) {
        try {
            Remove-TemporaryStandardUser -User $User
        }
        catch {
            if ($null -eq $Failure) {
                $Failure = $_
            }
            else {
                Write-Warning "temporary user cleanup failed: $($_.Exception.Message)"
            }
        }
    }
    if (Test-Path -LiteralPath $Root) {
        Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($null -ne $Failure) {
    throw $Failure
}
