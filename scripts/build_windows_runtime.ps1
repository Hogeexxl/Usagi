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
$FrontendRoot = Join-Path $RepoRoot "frontend"
$FrontendDist = Join-Path $FrontendRoot "dist"
$EmbeddedDist = Join-Path $RepoRoot "internal\webui\dist"
$OutputDirectory = Join-Path $RepoRoot "dist\windows-x64"
$OutputBinary = Join-Path $OutputDirectory "usagi.exe"
$BuildManifest = Join-Path $OutputDirectory "usagi.build.json"
$VerifiedManifest = Join-Path $OutputDirectory "usagi.pe.json"

function Invoke-CheckedCommand {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Command,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

Push-Location $RepoRoot
try {
    Invoke-CheckedCommand -Command "npm" -Arguments @("--prefix", $FrontendRoot, "run", "build")

    $FrontendIndex = Join-Path $FrontendDist "index.html"
    if (-not (Test-Path -LiteralPath $FrontendIndex -PathType Leaf)) {
        throw "frontend production build did not produce $FrontendIndex"
    }

    if (Test-Path -LiteralPath $EmbeddedDist) {
        Remove-Item -LiteralPath $EmbeddedDist -Recurse -Force
    }
    New-Item -ItemType Directory -Path $EmbeddedDist -Force | Out-Null
    Copy-Item -Path (Join-Path $FrontendDist "*") -Destination $EmbeddedDist -Recurse -Force

    New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
    foreach ($stalePath in @($OutputBinary, $BuildManifest, $VerifiedManifest)) {
        if (Test-Path -LiteralPath $stalePath) {
            Remove-Item -LiteralPath $stalePath -Force
        }
    }

    $PreviousGOOS = [Environment]::GetEnvironmentVariable("GOOS")
    $PreviousGOARCH = [Environment]::GetEnvironmentVariable("GOARCH")
    try {
        $env:GOOS = "windows"
        $env:GOARCH = "amd64"
        $LinkerFlags = "-H=windowsgui -X github.com/Hogeexxl/Usagi/internal/api.AppVersion=$Version"
        Invoke-CheckedCommand -Command "go" -Arguments @(
            "build",
            "-tags", "production",
            "-trimpath",
            "-ldflags", $LinkerFlags,
            "-o", $OutputBinary,
            "./cmd/usagi"
        )
    }
    finally {
        if ($null -eq $PreviousGOOS) {
            Remove-Item Env:GOOS -ErrorAction SilentlyContinue
        }
        else {
            $env:GOOS = $PreviousGOOS
        }
        if ($null -eq $PreviousGOARCH) {
            Remove-Item Env:GOARCH -ErrorAction SilentlyContinue
        }
        else {
            $env:GOARCH = $PreviousGOARCH
        }
    }

    if (-not (Test-Path -LiteralPath $OutputBinary -PathType Leaf)) {
        throw "Windows runtime build did not produce $OutputBinary"
    }

    $Digest = (Get-FileHash -LiteralPath $OutputBinary -Algorithm SHA256).Hash.ToLowerInvariant()
    [ordered]@{
        version = $Version
        goos = "windows"
        goarch = "amd64"
        sha256 = $Digest
    } | ConvertTo-Json -Compress | Set-Content -LiteralPath $BuildManifest -Encoding ASCII

    Write-Host "built $OutputBinary for version $Version"
}
finally {
    Pop-Location
}
