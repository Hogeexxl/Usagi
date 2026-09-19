$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$installer = Join-Path $env:GITHUB_WORKSPACE "target/release/Usagi-v$env:TAG_VERSION-windows-x64-setup.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "Windows installer was not found: $installer"
}

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace Usagi.Acceptance
{
    public sealed class AlternateUserProcess : IDisposable
    {
        private IntPtr processHandle;
        private IntPtr threadHandle;

        internal AlternateUserProcess(IntPtr processHandle, IntPtr threadHandle, uint processId)
        {
            this.processHandle = processHandle;
            this.threadHandle = threadHandle;
            ProcessId = processId;
        }

        public uint ProcessId { get; private set; }

        public int WaitForExit(int milliseconds)
        {
            if (processHandle == IntPtr.Zero)
                throw new ObjectDisposedException(nameof(AlternateUserProcess));

            uint wait = NativeMethods.WaitForSingleObject(processHandle, (uint)milliseconds);
            if (wait == NativeMethods.WAIT_TIMEOUT)
                return int.MinValue;
            if (wait != NativeMethods.WAIT_OBJECT_0)
                throw new Win32Exception(Marshal.GetLastWin32Error(), "WaitForSingleObject failed");

            if (!NativeMethods.GetExitCodeProcess(processHandle, out uint exitCode))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "GetExitCodeProcess failed");
            return unchecked((int)exitCode);
        }

        public void Dispose()
        {
            if (threadHandle != IntPtr.Zero)
            {
                NativeMethods.CloseHandle(threadHandle);
                threadHandle = IntPtr.Zero;
            }
            if (processHandle != IntPtr.Zero)
            {
                NativeMethods.CloseHandle(processHandle);
                processHandle = IntPtr.Zero;
            }
            GC.SuppressFinalize(this);
        }
    }

    public static class NativeLogon
    {
        private const int LOGON_WITH_PROFILE = 0x00000001;
        private const uint CREATE_NO_WINDOW = 0x08000000;

        public static AlternateUserProcess Start(
            string userName,
            string domain,
            string password,
            string applicationName,
            string arguments,
            string currentDirectory)
        {
            var startupInfo = new NativeMethods.STARTUPINFO();
            startupInfo.cb = Marshal.SizeOf(typeof(NativeMethods.STARTUPINFO));

            string command = "\"" + applicationName + "\"";
            if (!String.IsNullOrWhiteSpace(arguments))
                command += " " + arguments;
            var commandLine = new StringBuilder(command, Math.Max(1024, command.Length + 1));

            if (!NativeMethods.CreateProcessWithLogonW(
                    userName,
                    domain,
                    password,
                    LOGON_WITH_PROFILE,
                    applicationName,
                    commandLine,
                    CREATE_NO_WINDOW,
                    IntPtr.Zero,
                    currentDirectory,
                    ref startupInfo,
                    out NativeMethods.PROCESS_INFORMATION processInfo))
            {
                int error = Marshal.GetLastWin32Error();
                throw new Win32Exception(error, "CreateProcessWithLogonW(LOGON_WITH_PROFILE) failed");
            }

            return new AlternateUserProcess(
                processInfo.hProcess,
                processInfo.hThread,
                processInfo.dwProcessId);
        }
    }

    internal static class NativeMethods
    {
        internal const uint WAIT_OBJECT_0 = 0x00000000;
        internal const uint WAIT_TIMEOUT = 0x00000102;

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        internal struct STARTUPINFO
        {
            internal int cb;
            internal string lpReserved;
            internal string lpDesktop;
            internal string lpTitle;
            internal int dwX;
            internal int dwY;
            internal int dwXSize;
            internal int dwYSize;
            internal int dwXCountChars;
            internal int dwYCountChars;
            internal int dwFillAttribute;
            internal int dwFlags;
            internal short wShowWindow;
            internal short cbReserved2;
            internal IntPtr lpReserved2;
            internal IntPtr hStdInput;
            internal IntPtr hStdOutput;
            internal IntPtr hStdError;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct PROCESS_INFORMATION
        {
            internal IntPtr hProcess;
            internal IntPtr hThread;
            internal uint dwProcessId;
            internal uint dwThreadId;
        }

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool CreateProcessWithLogonW(
            string lpUsername,
            string lpDomain,
            string lpPassword,
            int dwLogonFlags,
            string lpApplicationName,
            StringBuilder lpCommandLine,
            uint dwCreationFlags,
            IntPtr lpEnvironment,
            string lpCurrentDirectory,
            ref STARTUPINFO lpStartupInfo,
            out PROCESS_INFORMATION lpProcessInformation);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool CloseHandle(IntPtr hObject);

        [DllImport("kernel32.dll", SetLastError = true)]
        internal static extern uint WaitForSingleObject(IntPtr hHandle, uint dwMilliseconds);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool GetExitCodeProcess(IntPtr hProcess, out uint lpExitCode);
    }
}
'@

$testUser = "mu-ci-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
$plainPassword = "Mu!9aA$([Guid]::NewGuid().ToString('N'))"
$securePassword = ConvertTo-SecureString $plainPassword -AsPlainText -Force
$testUserCreated = $false
$testUserSid = $null
$profilePath = $null
$workRoot = Join-Path $env:ProgramData "Usagi-S12-$([Guid]::NewGuid().ToString('N'))"

function Grant-TestUserModify {
    param([Parameter(Mandatory = $true)][string]$Path)

    New-Item -ItemType Directory -Force -Path $Path | Out-Null
    & (Join-Path $env:SystemRoot 'System32\icacls.exe') $Path /grant "${testUser}:(OI)(CI)M" /T /C | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to grant isolated Windows user modify access to $Path"
    }
}

function Start-IsolatedUserProcess {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationName,
        [Parameter(Mandatory = $true)][string]$Arguments,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    return [Usagi.Acceptance.NativeLogon]::Start(
        $testUser,
        '.',
        $plainPassword,
        $ApplicationName,
        $Arguments,
        $WorkingDirectory)
}

function Wait-ForFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][Usagi.Acceptance.AlternateUserProcess]$Process,
        [int]$TimeoutSeconds = 30
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            return
        }
        $exit = $Process.WaitForExit(0)
        if ($exit -ne [int]::MinValue) {
            throw "Isolated Windows process $($Process.ProcessId) exited with $exit before producing $Path"
        }
        Start-Sleep -Milliseconds 250
    }
    throw "Timed out waiting for isolated Windows process $($Process.ProcessId) to produce $Path"
}

function Stop-IsolatedProcessTree {
    param([Parameter(Mandatory = $true)][Usagi.Acceptance.AlternateUserProcess]$Process)

    if ($Process.WaitForExit(0) -eq [int]::MinValue) {
        & (Join-Path $env:SystemRoot 'System32\taskkill.exe') /PID $Process.ProcessId /T /F | Out-Null
        $deadline = (Get-Date).AddSeconds(10)
        while ((Get-Date) -lt $deadline) {
            if ($Process.WaitForExit(250) -ne [int]::MinValue) {
                break
            }
        }
    }
}

function Invoke-InstalledRuntimeSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$BinaryPath,
        [Parameter(Mandatory = $true)][string]$RuntimeRoot,
        [Parameter(Mandatory = $true)][string]$CodexHome,
        [Parameter(Mandatory = $true)][string]$Temp,
        [Parameter(Mandatory = $true)][string]$ExpectedLocalAppData
    )

    Grant-TestUserModify -Path $RuntimeRoot
    Grant-TestUserModify -Path $Temp
    Grant-TestUserModify -Path $CodexHome
    New-Item -ItemType Directory -Force -Path (Join-Path $CodexHome 'sessions'), (Join-Path $CodexHome 'archived_sessions') | Out-Null

    $launcher = Join-Path $RuntimeRoot 'launch-usagi.ps1'
    $runtimeIdentityPath = Join-Path $RuntimeRoot 'runtime-identity.json'
    $stdoutPath = Join-Path $RuntimeRoot 'stdout.log'
    $stderrPath = Join-Path $RuntimeRoot 'stderr.log'
    Remove-Item -LiteralPath $runtimeIdentityPath, $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue

    $escapedBinary = $BinaryPath.Replace("'", "''")
    $escapedRuntimeRoot = $RuntimeRoot.Replace("'", "''")
    $escapedCodexHome = $CodexHome.Replace("'", "''")
    $escapedTemp = $Temp.Replace("'", "''")
    $escapedIdentityPath = $runtimeIdentityPath.Replace("'", "''")
    $escapedStdout = $stdoutPath.Replace("'", "''")
    $escapedStderr = $stderrPath.Replace("'", "''")

    @"
`$ErrorActionPreference = 'Stop'
`$env:PATH = "`$env:SystemRoot\System32;`$env:SystemRoot"
Remove-Item Env:CARGO_HOME, Env:RUSTUP_HOME, Env:NODE_PATH, Env:npm_config_prefix -ErrorAction SilentlyContinue
`$env:TEMP = '$escapedTemp'
`$env:TMP = '$escapedTemp'
`$env:CODEX_HOME = '$escapedCodexHome'
`$env:USAGI_WINDOWS_HEADLESS_SMOKE = '1'
`$env:USAGI_DISABLE_BROWSER = '1'
Set-Location -LiteralPath '$escapedRuntimeRoot'
[pscustomobject]@{
    UserName = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    UserSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    UserProfile = `$env:USERPROFILE
    LocalAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
} | ConvertTo-Json -Compress | Set-Content -LiteralPath '$escapedIdentityPath' -Encoding utf8 -NoNewline
`$binaryPath = '$escapedBinary'
`$stdoutPath = '$escapedStdout'
`$stderrPath = '$escapedStderr'
`$usagi = Start-Process -FilePath `$binaryPath -PassThru -Wait -RedirectStandardOutput `$stdoutPath -RedirectStandardError `$stderrPath
exit `$usagi.ExitCode
"@ | Set-Content -LiteralPath $launcher -Encoding utf8

    $pwsh = Join-Path $PSHOME 'pwsh.exe'
    $arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$launcher`""
    $process = Start-IsolatedUserProcess -ApplicationName $pwsh -Arguments $arguments -WorkingDirectory $RuntimeRoot

    try {
        Wait-ForFile -Path $runtimeIdentityPath -Process $process
        $runtimeIdentity = Get-Content -LiteralPath $runtimeIdentityPath -Raw | ConvertFrom-Json
        if ([string]$runtimeIdentity.UserSid -ne [string]$testUserSid) {
            throw "Installed runtime did not run as isolated Windows user: $($runtimeIdentity.UserName) / $($runtimeIdentity.UserSid)"
        }
        if ([string]$runtimeIdentity.LocalAppData -ine $ExpectedLocalAppData) {
            throw "Installed runtime resolved the wrong Windows LocalApplicationData known folder: $($runtimeIdentity.LocalAppData)"
        }

        $healthy = $false
        $deadline = (Get-Date).AddSeconds(30)
        while ((Get-Date) -lt $deadline) {
            if ($process.WaitForExit(0) -ne [int]::MinValue) {
                break
            }
            try {
                $response = Invoke-WebRequest -UseBasicParsing -SkipHttpErrorCheck -Uri 'http://127.0.0.1:3210/api/health' -TimeoutSec 2
                $expectedBinaryVersion = $env:CARGO_VERSION
                if ($response.StatusCode -eq 204 -and
                    [string]$response.Headers['X-Usagi-App'] -eq 'Usagi' -and
                    [string]$response.Headers['X-Usagi-Version'] -eq $expectedBinaryVersion) {
                    $healthy = $true
                    break
                }
            } catch {
                # The listener may not be ready yet.
            }
            Start-Sleep -Milliseconds 250
        }
        if (-not $healthy) {
            if (Test-Path -LiteralPath $stderrPath) {
                Get-Content -LiteralPath $stderrPath -ErrorAction SilentlyContinue | Write-Host
            }
            $runtimeExit = $process.WaitForExit(0)
            throw "Installed Windows runtime did not pass the health marker/version smoke (launcher_exit=$runtimeExit)"
        }

        $root = Invoke-WebRequest -UseBasicParsing -SkipHttpErrorCheck -Uri 'http://127.0.0.1:3210/' -TimeoutSec 5
        if ($root.StatusCode -ne 200 -or [string]$root.Headers['Content-Type'] -notmatch '(?i)^text/html(?:;|$)' -or [string]::IsNullOrWhiteSpace([string]$root.Content)) {
            throw 'Installed Windows runtime root did not return a non-empty HTML document'
        }
        $references = [regex]::Matches([string]$root.Content, '(?:src|href)="([^"]+)"') |
            ForEach-Object { $_.Groups[1].Value } |
            Where-Object { $_ -match '^/' }
        $javascriptReference = $references | Where-Object { $_ -match '(?i)\.js(?:\?|$)' } | Select-Object -First 1
        $stylesheetReference = $references | Where-Object { $_ -match '(?i)\.css(?:\?|$)' } | Select-Object -First 1
        if ($null -eq $javascriptReference -or $null -eq $stylesheetReference) {
            throw 'Installed Windows runtime index did not reference both JavaScript and CSS assets'
        }
        foreach ($assetReference in @($javascriptReference, $stylesheetReference)) {
            $assetResponse = Invoke-WebRequest -UseBasicParsing -SkipHttpErrorCheck -Uri ("http://127.0.0.1:3210" + $assetReference) -TimeoutSec 5
            if ($assetResponse.StatusCode -ne 200 -or [string]::IsNullOrWhiteSpace([string]$assetResponse.Content)) {
                throw "Installed Windows runtime asset failed: $assetReference"
            }
            $assetType = [string]$assetResponse.Headers['Content-Type']
            if ($assetReference -match '(?i)\.js(?:\?|$)' -and $assetType -notmatch '(?i)(?:application|text)/javascript') {
                throw "JavaScript asset has unexpected MIME type $assetType"
            }
            if ($assetReference -match '(?i)\.css(?:\?|$)' -and $assetType -notmatch '(?i)text/css') {
                throw "CSS asset has unexpected MIME type $assetType"
            }
        }

        $spa = Invoke-WebRequest -UseBasicParsing -SkipHttpErrorCheck -Uri 'http://127.0.0.1:3210/acceptance/spa-route' -TimeoutSec 5
        if ($spa.StatusCode -ne 200 -or [string]$spa.Headers['Content-Type'] -notmatch '(?i)^text/html(?:;|$)' -or [string]$spa.Content -notmatch '(?i)<html') {
            throw 'Installed Windows runtime did not serve the SPA fallback document'
        }
        $unknownApi = Invoke-WebRequest -UseBasicParsing -SkipHttpErrorCheck -Uri 'http://127.0.0.1:3210/api/acceptance-not-found' -TimeoutSec 5
        if ($unknownApi.StatusCode -ne 404 -or [string]$unknownApi.Headers['Content-Type'] -notmatch '(?i)application/json') {
            throw 'Installed Windows runtime unknown API did not return JSON 404'
        }
        $unknownApiBody = [string]$unknownApi.Content | ConvertFrom-Json
        if ($unknownApiBody.error.code -ne 'NOT_FOUND') {
            throw 'Installed Windows runtime unknown API returned the wrong error code'
        }
    } finally {
        Stop-IsolatedProcessTree -Process $process
        $process.Dispose()
    }
}

try {
    $secondaryLogon = Get-Service -Name 'seclogon' -ErrorAction Stop
    if ($secondaryLogon.Status -ne 'Running') {
        Start-Service -Name 'seclogon'
    }

    New-LocalUser -Name $testUser -Password $securePassword -PasswordNeverExpires -UserMayNotChangePassword | Out-Null
    $testUserCreated = $true
    $testUserSid = (Get-LocalUser -Name $testUser).SID.Value
    Grant-TestUserModify -Path $workRoot

    $probeScript = Join-Path $workRoot 'probe-profile.ps1'
    $probeResult = Join-Path $workRoot 'probe-profile.json'
    $escapedProbeResult = $probeResult.Replace("'", "''")
    @"
`$ErrorActionPreference = 'Stop'
[pscustomobject]@{
    UserName = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    UserSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    UserProfile = `$env:USERPROFILE
    LocalAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
} | ConvertTo-Json -Compress | Set-Content -LiteralPath '$escapedProbeResult' -Encoding utf8 -NoNewline
"@ | Set-Content -LiteralPath $probeScript -Encoding utf8

    $pwsh = Join-Path $PSHOME 'pwsh.exe'
    $probeArguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$probeScript`""
    $probeProcess = Start-IsolatedUserProcess -ApplicationName $pwsh -Arguments $probeArguments -WorkingDirectory $workRoot
    try {
        Wait-ForFile -Path $probeResult -Process $probeProcess
        $probeExit = $probeProcess.WaitForExit(30000)
        if ($probeExit -eq [int]::MinValue) {
            throw "Isolated Windows profile probe did not exit after writing $probeResult"
        }
        if ($probeExit -ne 0) {
            throw "Isolated Windows profile probe exited with $probeExit"
        }
        $probe = Get-Content -LiteralPath $probeResult -Raw | ConvertFrom-Json
    } finally {
        Stop-IsolatedProcessTree -Process $probeProcess
        $probeProcess.Dispose()
    }

    if ([string]$probe.UserSid -ne [string]$testUserSid) {
        throw "CreateProcessWithLogonW did not run as the isolated Windows user: $($probe.UserName) / $($probe.UserSid)"
    }
    $profilePath = [string]$probe.UserProfile
    $localAppData = [string]$probe.LocalAppData
    if ([string]::IsNullOrWhiteSpace($profilePath) -or [string]::IsNullOrWhiteSpace($localAppData)) {
        throw 'CreateProcessWithLogonW did not resolve an isolated user profile and LocalApplicationData known folder'
    }
    if (-not $localAppData.StartsWith($profilePath, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Windows LocalApplicationData escaped the isolated user profile: $localAppData"
    }

    $profile = Get-CimInstance -ClassName Win32_UserProfile | Where-Object { [string]$_.SID -eq $testUserSid } | Select-Object -First 1
    if ($null -eq $profile -or [string]::IsNullOrWhiteSpace([string]$profile.LocalPath)) {
        throw "Isolated Windows user profile was not created for SID $testUserSid"
    }
    if ([string]$profile.LocalPath -ine $profilePath) {
        throw "Windows profile registry path does not match native logon profile: $($profile.LocalPath) vs $profilePath"
    }

    $installRoot = Join-Path $workRoot 'install'
    $runtimeRoot = Join-Path $workRoot 'runtime'
    $codexHome = Join-Path $profilePath '.codex'
    $temp = Join-Path $workRoot 'temp'
    $appDataRoot = Join-Path $localAppData 'Usagi'
    $databasePath = Join-Path $appDataRoot 'mu.sqlite3'
    $sentinelPath = Join-Path $appDataRoot 'acceptance-user-data.txt'

    Grant-TestUserModify -Path $installRoot
    Grant-TestUserModify -Path $runtimeRoot
    Grant-TestUserModify -Path $temp
    Grant-TestUserModify -Path $codexHome
    New-Item -ItemType Directory -Force -Path (Join-Path $codexHome 'sessions'), (Join-Path $codexHome 'archived_sessions') | Out-Null
    if (Test-Path -LiteralPath $appDataRoot) {
        throw "Fresh isolated Windows user unexpectedly already has Usagi data: $appDataRoot"
    }

    $install = Start-Process -FilePath $installer -ArgumentList @('/S', "/D=$installRoot") -Wait -PassThru -NoNewWindow
    if ($install.ExitCode -ne 0) {
        throw "NSIS installer exited with $($install.ExitCode)"
    }
    $install.Dispose()

    $installedBinary = Get-ChildItem -Path $installRoot -Recurse -Filter 'usagi.exe' -File | Select-Object -First 1
    if ($null -eq $installedBinary) {
        throw 'Installed usagi.exe was not found'
    }
    $legacyBinary = Get-ChildItem -Path $installRoot -Recurse -Filter 'mini-usage.exe' -File -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $legacyBinary) {
        throw "Installed runtime still contains legacy mini-usage.exe: $($legacyBinary.FullName)"
    }
    if ($installedBinary.FullName.StartsWith($env:GITHUB_WORKSPACE, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Installed binary unexpectedly resolves inside the repository'
    }
    if (Get-ChildItem -Path $installRoot -Recurse -Directory | Where-Object { $_.Name -eq 'frontend' }) {
        throw 'Installed runtime unexpectedly contains a frontend directory'
    }
    $uninstallers = @(Get-ChildItem -Path $installRoot -Recurse -Filter 'uninstall*.exe' -File)
    if ($uninstallers.Count -ne 1) {
        throw "Expected exactly one NSIS uninstaller (uninstall*.exe, including packager uninstall.exe), found $($uninstallers.Count)"
    }
    $uninstallerPath = $uninstallers[0].FullName

    Invoke-InstalledRuntimeSmoke -BinaryPath $installedBinary.FullName -RuntimeRoot $runtimeRoot -CodexHome $codexHome -Temp $temp -ExpectedLocalAppData $localAppData

    if (-not (Test-Path -LiteralPath $appDataRoot -PathType Container) -or -not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
        throw "Installed runtime did not create its user database in the isolated Windows known folder: $databasePath"
    }
    $unexpectedDatabase = Get-ChildItem -Path $profilePath -Recurse -Filter 'mu.sqlite3' -File -ErrorAction SilentlyContinue |
        Where-Object { -not $_.FullName.Equals($databasePath, [System.StringComparison]::OrdinalIgnoreCase) } |
        Select-Object -First 1
    if ($null -ne $unexpectedDatabase) {
        throw "Installed runtime created a database outside the isolated default path: $($unexpectedDatabase.FullName)"
    }

    Set-Content -LiteralPath $sentinelPath -Value 'preserve across reinstall' -NoNewline
    $databaseHashBeforeReinstall = (Get-FileHash -LiteralPath $databasePath -Algorithm SHA256).Hash

    $reinstall = Start-Process -FilePath $installer -ArgumentList @('/S', "/D=$installRoot") -Wait -PassThru -NoNewWindow
    if ($reinstall.ExitCode -ne 0) {
        throw "NSIS reinstall exited with $($reinstall.ExitCode)"
    }
    $reinstall.Dispose()
    if ((Get-Content -LiteralPath $sentinelPath -Raw) -ne 'preserve across reinstall' -or -not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
        throw 'NSIS reinstall did not preserve isolated Usagi user data'
    }
    if ((Get-FileHash -LiteralPath $databasePath -Algorithm SHA256).Hash -ne $databaseHashBeforeReinstall) {
        throw 'NSIS reinstall changed the isolated Usagi database'
    }

    $installedBinary = Get-ChildItem -Path $installRoot -Recurse -Filter 'usagi.exe' -File | Select-Object -First 1
    if ($null -eq $installedBinary) {
        throw 'Reinstalled usagi.exe was not found'
    }
    $legacyBinary = Get-ChildItem -Path $installRoot -Recurse -Filter 'mini-usage.exe' -File -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($null -ne $legacyBinary) {
        throw "Reinstalled runtime still contains legacy mini-usage.exe: $($legacyBinary.FullName)"
    }
    Invoke-InstalledRuntimeSmoke -BinaryPath $installedBinary.FullName -RuntimeRoot $runtimeRoot -CodexHome $codexHome -Temp $temp -ExpectedLocalAppData $localAppData
    if ((Get-Content -LiteralPath $sentinelPath -Raw) -ne 'preserve across reinstall') {
        throw 'Installed runtime relaunch did not preserve isolated Usagi user data'
    }

    $databaseHashBeforeUninstall = (Get-FileHash -LiteralPath $databasePath -Algorithm SHA256).Hash
    $sentinelHashBeforeUninstall = (Get-FileHash -LiteralPath $sentinelPath -Algorithm SHA256).Hash
    if (-not (Test-Path -LiteralPath $uninstallerPath -PathType Leaf)) {
        throw "NSIS uninstaller disappeared before uninstall: $uninstallerPath"
    }

    $uninstall = Start-Process -FilePath $uninstallerPath -ArgumentList @('/S') -Wait -PassThru -NoNewWindow
    if ($uninstall.ExitCode -ne 0) {
        throw "NSIS uninstaller exited with $($uninstall.ExitCode)"
    }
    $uninstall.Dispose()

    $uninstallDeadline = (Get-Date).AddSeconds(30)
    while ((Test-Path -LiteralPath $installRoot) -and ((Get-Date) -lt $uninstallDeadline)) {
        Start-Sleep -Milliseconds 250
    }
    if (Test-Path -LiteralPath $installedBinary.FullName) {
        throw 'NSIS uninstall left usagi.exe in the install directory'
    }
    if (Test-Path -LiteralPath $installRoot) {
        $remainingInstalledExecutables = @(Get-ChildItem -Path $installRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -match '(?i)^usagi(?:\.exe)?$' })
        if ($remainingInstalledExecutables.Count -ne 0) {
            throw 'NSIS uninstall left an installed Usagi executable in the install directory'
        }
        throw "NSIS uninstall left the install directory in place: $installRoot"
    }
    if (-not (Test-Path -LiteralPath $sentinelPath -PathType Leaf) -or -not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
        throw 'NSIS uninstall removed isolated Usagi user data'
    }
    if ((Get-FileHash -LiteralPath $databasePath -Algorithm SHA256).Hash -ne $databaseHashBeforeUninstall) {
        throw 'NSIS uninstall changed the isolated Usagi database'
    }
    if ((Get-FileHash -LiteralPath $sentinelPath -Algorithm SHA256).Hash -ne $sentinelHashBeforeUninstall) {
        throw 'NSIS uninstall changed isolated Usagi user data'
    }
} finally {
    if ($testUserCreated) {
        if ($null -ne $testUserSid) {
            try {
                Get-CimInstance -ClassName Win32_UserProfile |
                    Where-Object { [string]$_.SID -eq $testUserSid } |
                    Remove-CimInstance -ErrorAction Stop
            } catch {
                Write-Warning "Unable to remove isolated Windows profile for $testUserSid`: $($_.Exception.Message)"
            }
        }
        try {
            Remove-LocalUser -Name $testUser -ErrorAction Stop
        } catch {
            Write-Warning "Unable to remove isolated Windows test user $testUser`: $($_.Exception.Message)"
        }
    }
    if (Test-Path -LiteralPath $workRoot) {
        Remove-Item -LiteralPath $workRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
