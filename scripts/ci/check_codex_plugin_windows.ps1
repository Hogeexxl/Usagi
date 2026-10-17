param(
    [Parameter(Mandatory = $true)]
    [string]$PluginRoot,

    [Parameter(Mandatory = $true)]
    [string]$ExpectedVersion
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$PluginRoot = [System.IO.Path]::GetFullPath($PluginRoot)
$PluginJson = Join-Path $PluginRoot "plugin.json"
$McpJson = Join-Path $PluginRoot "mcp.json"
$Binary = Join-Path $PluginRoot "bin\usagi-codex-plugin.exe"
$Dashboard = Join-Path $PluginRoot "app\usagi-dashboard.html"
$Sidebar = Join-Path $PluginRoot "assets\sidebar.svg"
$GeneratedDashboard = Join-Path $RepoRoot "frontend\dist-codex\usagi-dashboard.html"

function Fail([string]$Message) {
    throw "FAIL: $Message"
}

function Require-File([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Fail "missing required file: $Path"
    }
    if ((Get-Item -LiteralPath $Path).Length -eq 0) {
        Fail "required file is empty: $Path"
    }
}

function Read-Json([string]$Path) {
    Require-File $Path
    try {
        return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    }
    catch {
        Fail "invalid JSON in $Path`: $($_.Exception.Message)"
    }
}

function Get-Property($Object, [string]$Name, [string]$Context) {
    $Property = $Object.PSObject.Properties[$Name]
    if ($null -eq $Property) {
        Fail "$Context is missing property '$Name'"
    }
    return $Property.Value
}

function Assert-WindowsAMD64PE([string]$Path) {
    $Stream = [System.IO.File]::OpenRead($Path)
    $Reader = [System.IO.BinaryReader]::new($Stream)
    try {
        if ($Reader.ReadUInt16() -ne 0x5A4D) {
            Fail "Plugin binary does not have an MZ signature"
        }
        $Stream.Position = 0x3C
        $PeOffset = $Reader.ReadUInt32()
        if ($PeOffset + 72 -gt $Stream.Length) {
            Fail "Plugin binary has an invalid PE header offset"
        }
        $Stream.Position = $PeOffset
        if ($Reader.ReadUInt32() -ne 0x00004550) {
            Fail "Plugin binary does not have a PE signature"
        }
        $Machine = $Reader.ReadUInt16()
        if ($Machine -ne 0x8664) {
            Fail ("Plugin binary machine is 0x{0:X4}, want AMD64 0x8664" -f $Machine)
        }
        $Stream.Position = $PeOffset + 24
        if ($Reader.ReadUInt16() -ne 0x20B) {
            Fail "Plugin binary optional header is not PE32+"
        }
        $Stream.Position = $PeOffset + 24 + 68
        $Subsystem = $Reader.ReadUInt16()
        if ($Subsystem -ne 3) {
            Fail "Plugin binary subsystem is not WINDOWS_CUI"
        }
    }
    finally {
        $Reader.Dispose()
        $Stream.Dispose()
    }
}

if ($ExpectedVersion -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$') {
    Fail "invalid expected Plugin version: $ExpectedVersion"
}
if (-not (Test-Path -LiteralPath $PluginRoot -PathType Container)) {
    Fail "Plugin root does not exist: $PluginRoot"
}

Require-File $PluginJson
Require-File $McpJson
Require-File $Binary
Require-File $Dashboard
Require-File $Sidebar
Require-File $GeneratedDashboard

$Plugin = Read-Json $PluginJson
$Schema = Get-Property $Plugin '$schema' "plugin.json"
if ($Schema -ne "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json") {
    Fail "plugin.json has an unexpected schema"
}
if ((Get-Property $Plugin "name" "plugin.json") -ne "usagi") {
    Fail "plugin.json name must be usagi"
}
if ((Get-Property $Plugin "version" "plugin.json") -ne $ExpectedVersion) {
    Fail "plugin.json version does not equal expected version $ExpectedVersion"
}
if ((Get-Property $Plugin "description" "plugin.json") -ne "Local AI usage dashboard for Codex Desktop.") {
    Fail "plugin.json has an unexpected description"
}
$Extensions = Get-Property $Plugin "extensions" "plugin.json"
$OpenAI = Get-Property $Extensions "com.openai" "plugin.json extensions"
$Interface = Get-Property $OpenAI "interface" "plugin.json com.openai extension"
if ((Get-Property $Interface "displayName" "plugin.json interface") -ne "Usagi") {
    Fail "plugin.json displayName must be Usagi"
}
if ((Get-Property $Interface "logo" "plugin.json interface") -ne "./assets/sidebar.svg") {
    Fail "plugin.json logo path must be ./assets/sidebar.svg"
}

$Mcp = Read-Json $McpJson
$McpSchema = Get-Property $Mcp '$schema' "mcp.json"
if ($McpSchema -ne "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json") {
    Fail "mcp.json has an unexpected schema"
}
$McpServers = Get-Property $Mcp "mcpServers" "mcp.json"
$UsagiServer = Get-Property $McpServers "usagi" "mcp.json mcpServers"
if ((Get-Property $UsagiServer "type" "mcp.json usagi server") -ne "stdio") {
    Fail "mcp.json server type must be stdio"
}
if ((Get-Property $UsagiServer "command" "mcp.json usagi server") -ne "./bin/usagi-codex-plugin.exe") {
    Fail "mcp.json command must target bin/usagi-codex-plugin.exe"
}
if ((Get-Property $UsagiServer "cwd" "mcp.json usagi server") -ne '${PLUGIN_ROOT}') {
    Fail "mcp.json cwd must be PLUGIN_ROOT"
}

Assert-WindowsAMD64PE $Binary

$DashboardHash = (Get-FileHash -LiteralPath $Dashboard -Algorithm SHA256).Hash
$GeneratedHash = (Get-FileHash -LiteralPath $GeneratedDashboard -Algorithm SHA256).Hash
if ($DashboardHash -ne $GeneratedHash) {
    Fail "Windows Dashboard HTML does not match the current shared build:codex bundle"
}

$Html = Get-Content -LiteralPath $Dashboard -Raw
foreach ($Marker in @("@modelcontextprotocol/ext-apps", 'id="root"', ".dashboard-shell")) {
    if (-not $Html.Contains($Marker)) {
        Fail "Windows Dashboard HTML is missing required marker: $Marker"
    }
}
if ($Html -match '(?i)<iframe') {
    Fail "Windows Dashboard HTML contains an iframe"
}
if ($Html -match '(?i)http://127\.0\.0\.1|http://localhost') {
    Fail "Windows Dashboard HTML contains a localhost URL"
}
if ($Html -match '(?i)<script[^>]*\ssrc\s*=\s*[''"]?([a-z][a-z0-9.+-]*:)?//|<link[^>]*\shref\s*=\s*[''"]?([a-z][a-z0-9.+-]*:)?//') {
    Fail "Windows Dashboard HTML references an external script or stylesheet URL"
}

Push-Location $RepoRoot
try {
    & go run ./internal/codexplugin/testdata/plugin_gate_windows `
        -PluginRoot $PluginRoot `
        -Binary $Binary
    if ($LASTEXITCODE -ne 0) {
        Fail "Windows Plugin stdio/runtime Gate exited with status $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

Write-Output "PASS: Windows Codex Plugin package and runtime Gate"
