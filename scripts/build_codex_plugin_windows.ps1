$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$FrontendRoot = Join-Path $RepoRoot "frontend"
$SourcePluginRoot = Join-Path $RepoRoot "plugins\usagi"
$DashboardSource = Join-Path $FrontendRoot "dist-codex\usagi-dashboard.html"
$StagingParent = Join-Path $RepoRoot "dist\codex-plugin\windows-x64"
$StagingRoot = Join-Path $StagingParent "usagi"
$StagingBinary = Join-Path $StagingRoot "bin\usagi-codex-plugin.exe"

function Fail([string]$Message) {
    throw "FAIL: $Message"
}

function Require-File([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "missing required file: $Path"
    }
}

function Write-Utf8Json([string]$Path, $Value) {
    $json = $Value | ConvertTo-Json -Depth 20
    [System.IO.File]::WriteAllText(
        $Path,
        $json + [Environment]::NewLine,
        [System.Text.UTF8Encoding]::new($false)
    )
}

Push-Location $RepoRoot
try {
    if (Test-Path -LiteralPath $DashboardSource) {
        Remove-Item -LiteralPath $DashboardSource -Force
    }
    if (Test-Path -LiteralPath (Split-Path -Parent $DashboardSource)) {
        Remove-Item -LiteralPath (Split-Path -Parent $DashboardSource) -Recurse -Force
    }

    & npm --prefix $FrontendRoot run build:codex
    if ($LASTEXITCODE -ne 0) {
        Fail "frontend build:codex exited with status $LASTEXITCODE"
    }
    if (-not (Test-Path -LiteralPath $DashboardSource -PathType Leaf) -or
        (Get-Item -LiteralPath $DashboardSource).Length -eq 0) {
        Fail "build:codex did not produce a non-empty Dashboard HTML file"
    }

    Require-File (Join-Path $SourcePluginRoot "plugin.json")
    Require-File (Join-Path $SourcePluginRoot "mcp.json")
    Require-File (Join-Path $SourcePluginRoot "assets\sidebar.svg")

    if (Test-Path -LiteralPath $StagingRoot) {
        Remove-Item -LiteralPath $StagingRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path `
        (Join-Path $StagingRoot "bin"), `
        (Join-Path $StagingRoot "app"), `
        (Join-Path $StagingRoot "assets") | Out-Null

    Copy-Item -LiteralPath (Join-Path $SourcePluginRoot "plugin.json") `
        -Destination (Join-Path $StagingRoot "plugin.json")
    Copy-Item -LiteralPath (Join-Path $SourcePluginRoot "assets\sidebar.svg") `
        -Destination (Join-Path $StagingRoot "assets\sidebar.svg")
    Copy-Item -LiteralPath $DashboardSource `
        -Destination (Join-Path $StagingRoot "app\usagi-dashboard.html")

    $mcp = Get-Content -LiteralPath (Join-Path $SourcePluginRoot "mcp.json") -Raw |
        ConvertFrom-Json
    $mcp.mcpServers.usagi.command = "./bin/usagi-codex-plugin.exe"
    Write-Utf8Json (Join-Path $StagingRoot "mcp.json") $mcp

    $HadGOOS = Test-Path Env:GOOS
    $HadGOARCH = Test-Path Env:GOARCH
    $PreviousGOOS = $env:GOOS
    $PreviousGOARCH = $env:GOARCH
    try {
        $env:GOOS = "windows"
        $env:GOARCH = "amd64"
        & go build -trimpath -o $StagingBinary ./cmd/usagi-codex-plugin
        if ($LASTEXITCODE -ne 0) {
            Fail "Windows Plugin build exited with status $LASTEXITCODE"
        }
    }
    finally {
        if ($HadGOOS) {
            $env:GOOS = $PreviousGOOS
        }
        else {
            Remove-Item Env:GOOS -ErrorAction SilentlyContinue
        }
        if ($HadGOARCH) {
            $env:GOARCH = $PreviousGOARCH
        }
        else {
            Remove-Item Env:GOARCH -ErrorAction SilentlyContinue
        }
    }

    if (-not (Test-Path -LiteralPath $StagingBinary -PathType Leaf) -or
        (Get-Item -LiteralPath $StagingBinary).Length -eq 0) {
        Fail "Windows Plugin binary is missing or empty: $StagingBinary"
    }

    Write-Output "PASS: built Windows Codex Plugin staging at $StagingRoot"
}
finally {
    Pop-Location
}
