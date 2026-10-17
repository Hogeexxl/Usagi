[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ConfigPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Write-Host "worker started: PID=$PID user=$([System.Security.Principal.WindowsIdentity]::GetCurrent().Name) inherited profile=$env:USERPROFILE"
. (Join-Path $PSScriptRoot "windows_smoke_common.ps1")

$Config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
$ResultPath = [string]$Config.result_path
$LogPath = [string]$Config.log_path
$Script:ActiveRuntime = $null
$Script:ActiveBaseUrl = ""
$Script:InstallDir = ""
$Script:UninstallerPath = ""
$Script:PortBlocker = $null
$Script:ProfileVerified = $false

function Assert-TemporaryUserIdentity {
    $Identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $Principal = New-Object System.Security.Principal.WindowsPrincipal($Identity)
    Assert-SmokeCondition ($Identity.Name -ieq "$env:COMPUTERNAME\$($Config.username)") "worker is not running as $($Config.username)"
    Assert-SmokeCondition (-not $Principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) "temporary user unexpectedly has administrator rights"

    # PowerShell 5.1 compiles Add-Type through the process temp directory.
    $env:TEMP = $PSScriptRoot
    $env:TMP = $PSScriptRoot
    # Resolve with the worker's token, independently of inherited runner environment variables.
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SmokeKnownFolders {
    [DllImport("shell32.dll")]
    private static extern int SHGetKnownFolderPath(ref Guid folder, uint flags, IntPtr token, out IntPtr path);
    public static string GetPath(string folderId) {
        Guid folder = new Guid(folderId);
        IntPtr path;
        Marshal.ThrowExceptionForHR(SHGetKnownFolderPath(ref folder, 0, IntPtr.Zero, out path));
        try { return Marshal.PtrToStringUni(path); }
        finally { Marshal.FreeCoTaskMem(path); }
    }
}
'@
    $KnownProfile = [SmokeKnownFolders]::GetPath("5E6C858F-0E22-4760-9AFE-EA3317B67173")
    $KnownLocalAppData = [SmokeKnownFolders]::GetPath("F1B32785-6FBA-4FCF-9D55-7B8E7F157091")
    $KnownRoamingAppData = [SmokeKnownFolders]::GetPath("3EB685DB-65F9-4CF6-A03A-E3EF65729F3D")
    $ExpectedProfile = Join-Path $env:SystemDrive "Users\$($Config.username)"
    Assert-SmokeCondition ($KnownProfile -ieq $ExpectedProfile) "Known Folder profile is not the temporary user's real profile"
    Assert-SmokeCondition ($KnownLocalAppData.StartsWith($KnownProfile + "\", [StringComparison]::OrdinalIgnoreCase)) "LocalApplicationData is outside the temporary user's profile"
    Assert-SmokeCondition ($KnownRoamingAppData.StartsWith($KnownProfile + "\", [StringComparison]::OrdinalIgnoreCase)) "RoamingApplicationData is outside the temporary user's profile"
    $env:USERPROFILE = $KnownProfile
    $env:LOCALAPPDATA = $KnownLocalAppData
    $env:APPDATA = $KnownRoamingAppData
    Assert-SmokeCondition ($env:USERPROFILE -ieq $ExpectedProfile) "worker did not load the temporary user's real profile"
    Assert-SmokeCondition ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData) -ieq $KnownLocalAppData) "LocalApplicationData does not match the worker token's Known Folder"
    Assert-SmokeCondition ($env:LOCALAPPDATA -ieq $KnownLocalAppData) "LOCALAPPDATA does not match the Known Folder"
    Write-Host "profile: $env:USERPROFILE"
    Write-Host "LocalApplicationData: $KnownLocalAppData"
    $Script:ProfileVerified = $true
}

function Get-SmokeDataRoot {
    return Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)) "Usagi"
}

function Install-Usagi {
    param(
        [Parameter(Mandatory = $true)]
        [string]$InstallerPath
    )

    Assert-SmokeCondition (Test-Path -LiteralPath $InstallerPath -PathType Leaf) "missing installer: $InstallerPath"
    $Installer = Start-Process -FilePath $InstallerPath -ArgumentList "/S" -Wait -PassThru
    Assert-SmokeCondition ($Installer.ExitCode -eq 0) "installer exited with code $($Installer.ExitCode)"

    $LocalAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    $Script:InstallDir = Join-Path $LocalAppData "Programs\Usagi"
    $Executable = Join-Path $Script:InstallDir "usagi.exe"
    $Script:UninstallerPath = Join-Path $Script:InstallDir "Uninstall.exe"
    Assert-SmokeCondition (Test-Path -LiteralPath $Executable -PathType Leaf) "installer did not create usagi.exe at $Executable"
    Assert-SmokeCondition (Test-Path -LiteralPath $Script:UninstallerPath -PathType Leaf) "installer did not create Uninstall.exe"

    $Shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Usagi.lnk"
    Assert-SmokeCondition (Test-Path -LiteralPath $Shortcut -PathType Leaf) "installer did not create the Start Menu shortcut"
}

function Restore-InstallDirectoryAcl {
    if ([string]::IsNullOrWhiteSpace($Script:InstallDir) -or -not (Test-Path -LiteralPath $Script:InstallDir)) {
        return
    }
    & icacls.exe $Script:InstallDir /remove:d $env:USERNAME /T /C | Out-Null
}

function Deny-InstallDirectoryWrites {
    & icacls.exe $Script:InstallDir /deny "$($env:USERNAME):(OI)(CI)(WD,AD,WEA,WA)" /T /C | Out-Null
    Assert-SmokeCondition ($LASTEXITCODE -eq 0) "failed to make install directory read-only"

    $ProbePath = Join-Path $Script:InstallDir "write-probe.txt"
    $WriteFailed = $false
    try {
        Set-Content -LiteralPath $ProbePath -Value "unexpected" -ErrorAction Stop
    }
    catch {
        $WriteFailed = $true
    }
    if (Test-Path -LiteralPath $ProbePath) {
        Remove-Item -LiteralPath $ProbePath -Force
    }
    Assert-SmokeCondition ($WriteFailed -and -not (Test-Path -LiteralPath $ProbePath)) "temporary user can still write to the install directory"
}

function Get-UsagiProcesses {
    $Executable = Join-Path $Script:InstallDir "usagi.exe"
    $Processes = @(
        Get-CimInstance Win32_Process -Filter "Name = 'usagi.exe'" -ErrorAction SilentlyContinue |
            Where-Object { $null -ne $_.ExecutablePath -and $_.ExecutablePath -ieq $Executable } |
            ForEach-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }
    )
    return $Processes
}

function Get-FixtureProcesses {
    $Processes = @(
        Get-CimInstance Win32_Process -Filter "Name = 'magpie-windows-amd64.exe'" -ErrorAction SilentlyContinue |
            Where-Object { $null -ne $_.ExecutablePath -and $_.ExecutablePath -ieq $Config.fixture_exe } |
            ForEach-Object { Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }
    )
    return $Processes
}

function Assert-FixtureProcess {
    Wait-SmokeCondition -Description "native Magpie fixture web process" -TimeoutSeconds 30 -Condition {
        return (@(Get-FixtureProcesses).Count -ge 1)
    }
    $FixtureRuntime = @(Get-FixtureProcesses)[0]
    Assert-NoVisibleWindow -Process $FixtureRuntime -Label "Magpie fixture"
    return $FixtureRuntime
}

function Assert-NoBrowserProcess {
    $BrowserNames = @("msedge.exe", "chrome.exe", "firefox.exe")
    $BrowserProcesses = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $BrowserNames -contains $_.Name }
    foreach ($BrowserProcess in $BrowserProcesses) {
        $Owner = Invoke-CimMethod -InputObject $BrowserProcess -MethodName GetOwner -ErrorAction SilentlyContinue
        if ($null -ne $Owner -and $Owner.ReturnValue -eq 0 -and $Owner.User -ieq $Config.username) {
            throw "USAGI_DISABLE_BROWSER=1 still launched browser process $($BrowserProcess.Name) as $($Config.username)"
        }
    }
}

function Start-InstalledRuntime {
    param(
        [Parameter(Mandatory = $true)]
        [string]$WorkingDirectory,
        [Parameter(Mandatory = $true)]
        [hashtable]$Environment
    )

    $Executable = Join-Path $Script:InstallDir "usagi.exe"
    Assert-SmokeCondition (Test-Path -LiteralPath $Executable -PathType Leaf) "missing installed runtime: $Executable"
    $Script:ActiveRuntime = Start-UsagiRuntime -ExecutablePath $Executable -WorkingDirectory $WorkingDirectory -Environment $Environment
    Assert-NoVisibleWindow -Process $Script:ActiveRuntime -Label "Usagi runtime"
    return $Script:ActiveRuntime
}

function Get-Descriptor {
    param(
        [Parameter(Mandatory = $true)]
        [string]$DescriptorPath,
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$RuntimeProcess
    )

    $Descriptor = Wait-UsagiDescriptor -DescriptorPath $DescriptorPath -RuntimeProcess $RuntimeProcess
    $Script:ActiveBaseUrl = [string]$Descriptor.base_url
    return $Descriptor
}

function Stop-CurrentRuntime {
    param(
        [string]$DescriptorPath = ""
    )

    if ($null -eq $Script:ActiveRuntime) {
        return
    }
    Stop-UsagiRuntime -RuntimeProcess $Script:ActiveRuntime -BaseUrl $Script:ActiveBaseUrl
    Assert-SmokeCondition ($Script:ActiveRuntime.HasExited) "Usagi runtime did not exit"
    if (-not [string]::IsNullOrWhiteSpace($DescriptorPath)) {
        Assert-SmokeCondition (-not (Test-Path -LiteralPath $DescriptorPath)) "runtime descriptor was not removed after shutdown"
    }
    $Script:ActiveRuntime = $null
    $Script:ActiveBaseUrl = ""
}

function Assert-InstalledVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$BaseUrl
    )

    $Health = Invoke-SmokeHttp -Uri "$BaseUrl/api/health"
    Assert-SmokeCondition ($Health.StatusCode -eq 204) "health endpoint returned $($Health.StatusCode)"
    Assert-SmokeCondition ($Health.Headers.ContainsKey("X-Usagi-App") -and $Health.Headers["X-Usagi-App"] -eq "Usagi") "health endpoint identity marker is invalid"

    $InfoResponse = Invoke-SmokeHttp -Uri "$BaseUrl/api/v1/info"
    Assert-SmokeCondition ($InfoResponse.StatusCode -eq 200) "info endpoint returned $($InfoResponse.StatusCode)"
    $Info = $InfoResponse.Content | ConvertFrom-Json
    Assert-SmokeCondition ($Info.app_version -eq $Config.expected_version) "installed runtime app_version '$($Info.app_version)' does not equal '$($Config.expected_version)'"
    Assert-SmokeCondition ($Info.runtime_mode -eq "production") "installed runtime mode is not production"

    $Dashboard = Invoke-SmokeHttp -Uri "$BaseUrl/"
    Assert-SmokeCondition ($Dashboard.StatusCode -eq 200 -and $Dashboard.Content.ToLowerInvariant().Contains("<!doctype html")) "Dashboard root is not embedded production HTML"
}

function Assert-SQLiteDatabase {
    param(
        [Parameter(Mandatory = $true)]
        [string]$DataRoot
    )

    $DatabasePath = Join-Path $DataRoot "usagi.sqlite"
    Assert-SmokeCondition (Test-Path -LiteralPath $DatabasePath -PathType Leaf) "SQLite database is missing from the user data root"
    $Bytes = New-Object byte[] 16
    $Stream = [System.IO.File]::Open(
        $DatabasePath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::ReadWrite
    )
    try {
        Assert-SmokeCondition ($Stream.Read($Bytes, 0, $Bytes.Length) -eq $Bytes.Length) "SQLite database header is incomplete"
    }
    finally {
        $Stream.Dispose()
    }
    $Header = [System.Text.Encoding]::ASCII.GetString($Bytes)
    Assert-SmokeCondition ($Header.StartsWith("SQLite format 3")) "user data file is not a SQLite database"
    Assert-SmokeCondition (-not (Test-Path -LiteralPath (Join-Path $Script:InstallDir "usagi.sqlite"))) "runtime data was written into the install directory"
    Assert-SmokeCondition (-not (Test-Path -LiteralPath (Join-Path $Script:InstallDir "runtime.json"))) "runtime descriptor was written into the install directory"
}

function Invoke-InstallerScenario {
    Assert-TemporaryUserIdentity
    $DataRoot = Get-SmokeDataRoot
    Assert-SmokeCondition (-not (Test-Path -LiteralPath $DataRoot)) "fresh temporary profile unexpectedly already has Usagi data"

    Install-Usagi -InstallerPath $Config.installer_path
    Deny-InstallDirectoryWrites

    $WorkingDirectory = Join-Path $env:USERPROFILE "Usagi Smoke Work\cwd with spaces"
    New-Item -ItemType Directory -Path $WorkingDirectory -Force | Out-Null

    $Script:PortBlocker = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 37891)
    $Script:PortBlocker.Start()

    $RuntimeEnvironment = @{
        "USAGI_DISABLE_BROWSER" = "1"
        "PATH" = "$($Config.fixture_dir);$env:PATH"
        "USAGI_WINDOWS_MAGPIE_FIXTURE_DIR" = $Config.fixture_dir
        "USAGI_WINDOWS_MAGPIE_OFFLINE_FILE" = $Config.offline_file
    }
    $Runtime = Start-InstalledRuntime -WorkingDirectory $WorkingDirectory -Environment $RuntimeEnvironment
    $DescriptorPath = Join-Path $DataRoot "runtime.json"
    $Descriptor = Get-Descriptor -DescriptorPath $DescriptorPath -RuntimeProcess $Runtime
    Assert-SmokeCondition ($Descriptor.port -ge 37892 -and $Descriptor.port -le 37922) "runtime did not move past occupied production port 37891"

    $DevelopmentDescriptor = Join-Path $DataRoot "runtime-dev.json"
    '{"schema_version":1,"pid":999999,"port":37991,"base_url":"http://127.0.0.1:37991","runtime_mode":"development"}' |
        Set-Content -LiteralPath $DevelopmentDescriptor -Encoding ASCII

    Assert-InstalledVersion -BaseUrl $Script:ActiveBaseUrl
    Assert-SQLiteDatabase -DataRoot $DataRoot
    Wait-UsagiRuntimeState -BaseUrl $Script:ActiveBaseUrl -ExpectedState "ready" -TimeoutSeconds 30
    $FixtureRuntime = Assert-FixtureProcess
    Assert-NoVisibleWindow -Process $Runtime -Label "Usagi runtime"
    Assert-NoBrowserProcess

    $SecondLauncher = Start-UsagiRuntime -ExecutablePath (Join-Path $Script:InstallDir "usagi.exe") -WorkingDirectory $WorkingDirectory -Environment $RuntimeEnvironment
    Assert-SmokeCondition ($SecondLauncher.WaitForExit(20000)) "second launcher did not exit"
    Assert-SmokeCondition ($SecondLauncher.ExitCode -eq 0) "second launcher exited with code $($SecondLauncher.ExitCode)"
    $Runtime.Refresh()
    Assert-SmokeCondition (-not $Runtime.HasExited) "second launch terminated the existing runtime"
    Assert-SmokeCondition (@(Get-UsagiProcesses).Count -eq 1) "more than one long-lived installed Usagi runtime is present"

    $DescriptorAfterSecondLaunch = Get-Content -LiteralPath $DescriptorPath -Raw | ConvertFrom-Json
    Assert-SmokeCondition ($DescriptorAfterSecondLaunch.pid -eq $Runtime.Id -and $DescriptorAfterSecondLaunch.port -eq $Descriptor.port) "second launch changed the active runtime descriptor"

    Stop-CurrentRuntime -DescriptorPath $DescriptorPath
    Assert-SmokeCondition ($Runtime.ExitCode -eq 0) "runtime shutdown exited with code $($Runtime.ExitCode)"
    Assert-SmokeCondition (Test-Path -LiteralPath $DevelopmentDescriptor -PathType Leaf) "shutdown removed another mode's descriptor"
    Remove-Item -LiteralPath $DevelopmentDescriptor -Force
    $FixtureRuntime.Refresh()
    Assert-SmokeCondition ($FixtureRuntime.HasExited) "Magpie fixture remained after runtime shutdown"

    Restore-InstallDirectoryAcl
    Install-Usagi -InstallerPath $Config.installer_path
    Assert-SmokeCondition (Test-Path -LiteralPath (Join-Path $Script:InstallDir "usagi.exe") -PathType Leaf) "reinstall did not restore usagi.exe"

    $Uninstaller = Start-Process -FilePath $Script:UninstallerPath -ArgumentList "/S" -Wait -PassThru
    Assert-SmokeCondition ($Uninstaller.ExitCode -eq 0) "uninstaller exited with code $($Uninstaller.ExitCode)"
    Wait-SmokeCondition -Description "installer uninstall cleanup" -TimeoutSeconds 20 -Condition {
        return -not (Test-Path -LiteralPath $Script:InstallDir)
    }
    $Shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Usagi.lnk"
    Assert-SmokeCondition (-not (Test-Path -LiteralPath $Shortcut)) "uninstaller did not remove the Start Menu shortcut"
    Assert-SmokeCondition (Test-Path -LiteralPath (Join-Path $DataRoot "usagi.sqlite") -PathType Leaf) "uninstaller removed user data"
    $Script:UninstallerPath = ""
    Write-Host "installer smoke passed: standard user profile, installed runtime, fallback port, second instance, read-only install, reinstall, and uninstall"
}

function Invoke-SeedHelper {
    param(
        [Parameter(Mandatory = $true)]
        [ValidateSet("seed", "verify")]
        [string]$Command,
        [Parameter(Mandatory = $true)]
        [string]$DatabasePath
    )

    $Output = & $Config.seed_exe $Command $DatabasePath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "SQLite smoke helper $Command failed with code $LASTEXITCODE`: $($Output -join "`n")"
    }
    if ($Command -eq "verify") {
        $OutputText = $Output -join "`n"
        return ($OutputText | ConvertFrom-Json)
    }
}

function Invoke-MagpieScenario {
    Assert-TemporaryUserIdentity
    $DataRoot = Get-SmokeDataRoot
    $DatabasePath = Join-Path $DataRoot "usagi.sqlite"
    $DescriptorPath = Join-Path $DataRoot "runtime.json"
    Assert-SmokeCondition (-not (Test-Path -LiteralPath $DataRoot)) "fresh temporary profile unexpectedly already has Usagi data"

    Install-Usagi -InstallerPath $Config.installer_path
    $WorkingDirectory = Join-Path $env:USERPROFILE "Usagi Magpie Work\cwd with spaces"
    New-Item -ItemType Directory -Path $WorkingDirectory -Force | Out-Null
    Set-Content -LiteralPath $Config.offline_file -Value "offline" -Encoding ASCII

    $RuntimeEnvironment = @{
        "USAGI_DISABLE_BROWSER" = "1"
        "PATH" = "$($Config.fixture_dir);$env:PATH"
        "USAGI_WINDOWS_MAGPIE_FIXTURE_DIR" = $Config.fixture_dir
        "USAGI_WINDOWS_MAGPIE_OFFLINE_FILE" = $Config.offline_file
    }

    $Runtime = Start-InstalledRuntime -WorkingDirectory $WorkingDirectory -Environment $RuntimeEnvironment
    $Descriptor = Get-Descriptor -DescriptorPath $DescriptorPath -RuntimeProcess $Runtime
    Wait-UsagiRuntimeState -BaseUrl $Script:ActiveBaseUrl -ExpectedState "unavailable" -TimeoutSeconds 20
    Assert-InstalledVersion -BaseUrl $Script:ActiveBaseUrl
    Assert-SQLiteDatabase -DataRoot $DataRoot
    Assert-NoBrowserProcess

    $Loading = Invoke-SmokeHttp -Uri "$($Script:ActiveBaseUrl)/api/usage/summary?range=today"
    Assert-SmokeCondition ($Loading.StatusCode -eq 503 -and $Loading.Content.Contains("DATA_LOADING")) "new database did not retain the loading state while Magpie was unavailable"
    Stop-CurrentRuntime -DescriptorPath $DescriptorPath
    Assert-SmokeCondition ($Runtime.ExitCode -eq 0) "offline runtime shutdown exited with code $($Runtime.ExitCode)"

    Invoke-SeedHelper -Command "seed" -DatabasePath $DatabasePath | Out-Null
    $Seeded = Invoke-SeedHelper -Command "verify" -DatabasePath $DatabasePath
    Assert-SmokeCondition ($Seeded.schema_version -eq "4" -and $Seeded.initial_sync_complete -eq "1") "SQLite seed did not produce schema 4 with initial sync complete"
    Assert-SmokeCondition ($Seeded.request_count -eq 1 -and $Seeded.input -eq 10 -and $Seeded.output -eq 5) "SQLite seed data does not match expected values"

    $Runtime = Start-InstalledRuntime -WorkingDirectory $WorkingDirectory -Environment $RuntimeEnvironment
    $Descriptor = Get-Descriptor -DescriptorPath $DescriptorPath -RuntimeProcess $Runtime
    Wait-UsagiRuntimeState -BaseUrl $Script:ActiveBaseUrl -ExpectedState "unavailable" -TimeoutSeconds 20
    $OfflineSummaryResponse = Invoke-SmokeHttp -Uri "$($Script:ActiveBaseUrl)/api/usage/summary?range=today"
    Assert-SmokeCondition ($OfflineSummaryResponse.StatusCode -eq 200) "complete offline SQLite summary returned $($OfflineSummaryResponse.StatusCode)"
    $OfflineSummary = $OfflineSummaryResponse.Content | ConvertFrom-Json
    Assert-SmokeCondition ($OfflineSummary.usage.input -eq 10 -and $OfflineSummary.usage.output -eq 5) "offline summary did not read seeded SQLite data"
    $OfflineSessionsResponse = Invoke-SmokeHttp -Uri "$($Script:ActiveBaseUrl)/api/usage/sessions?range=today"
    Assert-SmokeCondition ($OfflineSessionsResponse.StatusCode -eq 200) "offline sessions endpoint returned $($OfflineSessionsResponse.StatusCode)"
    $OfflineSessions = $OfflineSessionsResponse.Content | ConvertFrom-Json
    $OfflineSessionTitles = @($OfflineSessions.items | ForEach-Object { $_.thread.title })
    Assert-SmokeCondition ($OfflineSessionTitles -contains "Seeded offline SQLite session") "offline session data is missing from SQLite"

    Remove-Item -LiteralPath $Config.offline_file -Force
    Wait-UsagiRuntimeState -BaseUrl $Script:ActiveBaseUrl -ExpectedState "ready" -TimeoutSeconds 45
    $Runtime.Refresh()
    Assert-SmokeCondition (-not $Runtime.HasExited -and $Runtime.Id -eq $Descriptor.pid) "Usagi restarted instead of recovering its Magpie supervisor"
    $FixtureRuntime = Assert-FixtureProcess
    Assert-NoVisibleWindow -Process $Runtime -Label "Usagi runtime"

    $WebArgumentsPath = Join-Path $Config.fixture_dir "web.args"
    Assert-SmokeCondition (Test-Path -LiteralPath $WebArgumentsPath -PathType Leaf) "native Magpie fixture did not record its web arguments"
    $WebArguments = Get-Content -LiteralPath $WebArgumentsPath -Raw
    Assert-SmokeCondition ($WebArguments -match '(?m)^--no-open$' -and $WebArguments -match '(?m)^--addr$' -and $WebArguments -match '127\.0\.0\.1:\d+') "native Magpie fixture was not started with web --no-open --addr"

    Wait-SmokeCondition -Description "Magpie API recovery in Usagi" -TimeoutSeconds 90 -PollMilliseconds 250 -Condition {
        $SummaryResponse = Invoke-SmokeHttp -Uri "$($Script:ActiveBaseUrl)/api/usage/summary?range=today"
        if ($SummaryResponse.StatusCode -ne 200) {
            return $false
        }
        $Summary = $SummaryResponse.Content | ConvertFrom-Json
        return $Summary.usage.input -eq 110 -and $Summary.usage.output -eq 55
    }

    $RecoveredSessionsResponse = Invoke-SmokeHttp -Uri "$($Script:ActiveBaseUrl)/api/usage/sessions?range=today"
    Assert-SmokeCondition ($RecoveredSessionsResponse.StatusCode -eq 200) "recovered sessions endpoint returned $($RecoveredSessionsResponse.StatusCode)"
    $RecoveredSessions = $RecoveredSessionsResponse.Content | ConvertFrom-Json
    $RecoveredSessionTitles = @($RecoveredSessions.items | ForEach-Object { $_.thread.title })
    Assert-SmokeCondition ($RecoveredSessionTitles -contains "Windows Magpie recovery session") "recovered Magpie session data did not reach the Dashboard API"
    $Dashboard = Invoke-SmokeHttp -Uri "$($Script:ActiveBaseUrl)/"
    Assert-SmokeCondition ($Dashboard.StatusCode -eq 200 -and $Dashboard.Content.ToLowerInvariant().Contains("<!doctype html")) "Dashboard was unavailable after Magpie recovery"

    $RequestLogPath = Join-Path $Config.fixture_dir "requests.log"
    Wait-SmokeCondition -Description "authenticated Magpie contract endpoint coverage" -TimeoutSeconds 60 -Condition {
        if (-not (Test-Path -LiteralPath $RequestLogPath -PathType Leaf)) {
            return $false
        }
        $RequestLog = Get-Content -LiteralPath $RequestLogPath -Raw
        foreach ($Path in @("/api/sessions/progress", "/api/usage", "/api/usage/requests", "/api/providers", "/api/usage/quotas")) {
            if ($RequestLog -notmatch [regex]::Escape($Path)) {
                return $false
            }
        }
        return $true
    }

    Stop-CurrentRuntime -DescriptorPath $DescriptorPath
    Assert-SmokeCondition ($Runtime.ExitCode -eq 0) "recovered runtime shutdown exited with code $($Runtime.ExitCode)"
    $FixtureRuntime.Refresh()
    Assert-SmokeCondition ($FixtureRuntime.HasExited) "Magpie fixture remained after runtime shutdown"

    $RecoveredDatabase = Invoke-SeedHelper -Command "verify" -DatabasePath $DatabasePath
    Assert-SmokeCondition ($RecoveredDatabase.schema_version -eq "4" -and $RecoveredDatabase.initial_sync_complete -eq "1") "recovered SQLite database is not schema 4 with initial sync complete"
    Assert-SmokeCondition ($RecoveredDatabase.fixture_count -eq 1 -and $RecoveredDatabase.fixture_input -eq 110 -and $RecoveredDatabase.fixture_output -eq 55) "recovered Magpie data was not persisted to SQLite"
    Write-Host "Magpie smoke passed: unavailable retention, background retry, authenticated upstream endpoints, recovery, Dashboard API, and SQLite persistence"
}

function Invoke-WorkerCleanup {
    if (-not $Script:ProfileVerified) {
        return
    }
    try {
        if ($null -ne $Script:ActiveRuntime) {
            Stop-UsagiRuntime -RuntimeProcess $Script:ActiveRuntime -BaseUrl $Script:ActiveBaseUrl -AllowForce
            $Script:ActiveRuntime = $null
        }
    }
    catch {
        Write-Warning "stopping Usagi runtime during cleanup failed: $($_.Exception.Message)"
    }

    foreach ($FixtureRuntime in @(Get-FixtureProcesses)) {
        Stop-Process -Id $FixtureRuntime.Id -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $Script:PortBlocker) {
        $Script:PortBlocker.Stop()
        $Script:PortBlocker = $null
    }

    try {
        Restore-InstallDirectoryAcl
        if (-not [string]::IsNullOrWhiteSpace($Script:UninstallerPath) -and (Test-Path -LiteralPath $Script:UninstallerPath -PathType Leaf)) {
            $Uninstaller = Start-Process -FilePath $Script:UninstallerPath -ArgumentList "/S" -Wait -PassThru
            Write-Host "cleanup uninstaller exit code: $($Uninstaller.ExitCode)"
        }
        if (-not [string]::IsNullOrWhiteSpace($Script:InstallDir) -and (Test-Path -LiteralPath $Script:InstallDir)) {
            Remove-Item -LiteralPath $Script:InstallDir -Recurse -Force -ErrorAction SilentlyContinue
        }
        $Shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Usagi.lnk"
        if (Test-Path -LiteralPath $Shortcut) {
            Remove-Item -LiteralPath $Shortcut -Force -ErrorAction SilentlyContinue
        }
        $RegistryKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Usagi"
        if (Test-Path -LiteralPath $RegistryKey) {
            Remove-Item -LiteralPath $RegistryKey -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    catch {
        Write-Warning "installer cleanup failed: $($_.Exception.Message)"
    }
}

$Success = $false
$Failure = ""
try {
    Start-Transcript -Path $LogPath -Force | Out-Null
    switch ($Config.scenario) {
        "installer" { Invoke-InstallerScenario }
        "magpie" { Invoke-MagpieScenario }
        default { throw "unknown smoke scenario: $($Config.scenario)" }
    }
    $Success = $true
}
catch {
    $Failure = $_.Exception.ToString()
    Write-Host $Failure
}
finally {
    try {
        Invoke-WorkerCleanup
    }
    catch {
        $Failure = $_.Exception.ToString()
        $Success = $false
        Write-Host $Failure
    }
    Write-WorkerResult -ResultPath $ResultPath -Success $Success -ErrorText $Failure
    try {
        Stop-Transcript | Out-Null
    }
    catch {
    }
}

if (-not $Success) {
    exit 1
}
