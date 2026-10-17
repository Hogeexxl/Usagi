$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-SmokeCondition {
    param(
        [Parameter(Mandatory = $true)]
        [bool]$Condition,
        [Parameter(Mandatory = $true)]
        [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Invoke-SmokeHttp {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Uri,
        [ValidateSet("GET", "POST")]
        [string]$Method = "GET",
        [int]$TimeoutSeconds = 5
    )

    $Request = [System.Net.HttpWebRequest]::Create($Uri)
    $Request.Method = $Method
    $Request.Proxy = $null
    $Request.KeepAlive = $false
    $Request.Timeout = $TimeoutSeconds * 1000
    $Request.ReadWriteTimeout = $TimeoutSeconds * 1000

    try {
        $Response = $Request.GetResponse()
    }
    catch [System.Net.WebException] {
        if ($null -eq $_.Exception.Response) {
            throw
        }
        $Response = $_.Exception.Response
    }

    $Reader = $null
    try {
        $Reader = New-Object System.IO.StreamReader($Response.GetResponseStream())
        $Content = $Reader.ReadToEnd()
        $Headers = @{}
        foreach ($Name in $Response.Headers.AllKeys) {
            $Headers[$Name] = $Response.Headers[$Name]
        }
        return [pscustomobject]@{
            StatusCode = [int]$Response.StatusCode
            Content = $Content
            Headers = $Headers
        }
    }
    finally {
        if ($null -ne $Reader) {
            $Reader.Dispose()
        }
        $Response.Close()
    }
}

function Wait-SmokeCondition {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Condition,
        [Parameter(Mandatory = $true)]
        [string]$Description,
        [int]$TimeoutSeconds = 30,
        [int]$PollMilliseconds = 100
    )

    $Deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $LastError = $null
    while ([DateTime]::UtcNow -lt $Deadline) {
        try {
            if (& $Condition) {
                return
            }
        }
        catch {
            $LastError = $_
        }
        Start-Sleep -Milliseconds $PollMilliseconds
    }

    $Suffix = if ($null -ne $LastError) { ": $($LastError.Exception.Message)" } else { "" }
    throw "timed out waiting for $Description$Suffix"
}

function Wait-UsagiDescriptor {
    param(
        [Parameter(Mandatory = $true)]
        [string]$DescriptorPath,
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$RuntimeProcess,
        [int]$TimeoutSeconds = 30
    )

    Wait-SmokeCondition -Description "runtime descriptor $DescriptorPath" -TimeoutSeconds $TimeoutSeconds -Condition {
        $RuntimeProcess.Refresh()
        if ($RuntimeProcess.HasExited) {
            throw "runtime exited before writing its descriptor with code $($RuntimeProcess.ExitCode)"
        }
        if (-not (Test-Path -LiteralPath $DescriptorPath -PathType Leaf)) {
            return $false
        }
        if ((Get-Item -LiteralPath $DescriptorPath).Length -eq 0) {
            return $false
        }
        $Descriptor = Get-Content -LiteralPath $DescriptorPath -Raw | ConvertFrom-Json
        Assert-SmokeCondition ($Descriptor.schema_version -eq 1) "runtime descriptor schema is not 1"
        Assert-SmokeCondition ($Descriptor.pid -eq $RuntimeProcess.Id) "runtime descriptor PID does not match the launched process"
        Assert-SmokeCondition ($Descriptor.runtime_mode -eq "production") "runtime descriptor mode is not production"
        Assert-SmokeCondition ($Descriptor.port -ge 37891 -and $Descriptor.port -le 37922) "runtime descriptor port is outside 37891...37922"
        Assert-SmokeCondition ($Descriptor.base_url -eq "http://127.0.0.1:$($Descriptor.port)") "runtime descriptor base_url does not match its port"
        return $true
    }

    return Get-Content -LiteralPath $DescriptorPath -Raw | ConvertFrom-Json
}

function Wait-UsagiRuntimeState {
    param(
        [Parameter(Mandatory = $true)]
        [string]$BaseUrl,
        [Parameter(Mandatory = $true)]
        [string]$ExpectedState,
        [int]$TimeoutSeconds = 30
    )

    Wait-SmokeCondition -Description "Usagi runtime state $ExpectedState" -TimeoutSeconds $TimeoutSeconds -Condition {
        $Response = Invoke-SmokeHttp -Uri "$BaseUrl/api/status"
        if ($Response.StatusCode -ne 200) {
            return $false
        }
        $Status = $Response.Content | ConvertFrom-Json
        return $Status.runtime_state -eq $ExpectedState
    }
}

function Start-UsagiRuntime {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath,
        [Parameter(Mandatory = $true)]
        [string]$WorkingDirectory,
        [hashtable]$Environment = @{}
    )

    $Previous = @{}
    foreach ($Name in $Environment.Keys) {
        $Previous[$Name] = [Environment]::GetEnvironmentVariable($Name, "Process")
        [Environment]::SetEnvironmentVariable($Name, [string]$Environment[$Name], "Process")
    }
    try {
        return Start-Process -FilePath $ExecutablePath -WorkingDirectory $WorkingDirectory -PassThru
    }
    finally {
        foreach ($Name in $Environment.Keys) {
            [Environment]::SetEnvironmentVariable($Name, $Previous[$Name], "Process")
        }
    }
}

function Stop-UsagiRuntime {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$RuntimeProcess,
        [string]$BaseUrl = "",
        [int]$TimeoutSeconds = 15,
        [switch]$AllowForce
    )

    $RuntimeProcess.Refresh()
    if (-not $RuntimeProcess.HasExited -and -not [string]::IsNullOrWhiteSpace($BaseUrl)) {
        try {
            $Response = Invoke-SmokeHttp -Uri "$BaseUrl/api/service/stop" -Method POST -TimeoutSeconds 3
            Assert-SmokeCondition ($Response.StatusCode -eq 200) "runtime stop request returned $($Response.StatusCode)"
            $Result = $Response.Content | ConvertFrom-Json
            Assert-SmokeCondition ($Result.state -eq "stopped") "runtime stop response did not report stopped"
        }
        catch {
            if (-not $AllowForce) {
                throw
            }
            Write-Host "runtime stop request failed during cleanup: $($_.Exception.Message)"
        }
    }

    $Deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not $RuntimeProcess.HasExited -and [DateTime]::UtcNow -lt $Deadline) {
        Start-Sleep -Milliseconds 100
        $RuntimeProcess.Refresh()
    }
    if (-not $RuntimeProcess.HasExited) {
        Assert-SmokeCondition $AllowForce.IsPresent "runtime process did not exit after its service stop request"
        Stop-Process -Id $RuntimeProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if (-not $RuntimeProcess.HasExited) {
        $RuntimeProcess.WaitForExit(5000) | Out-Null
    }
}

function Assert-NoVisibleWindow {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $Process.Refresh()
    Assert-SmokeCondition (-not $Process.HasExited) "$Label process is not running"
    Assert-SmokeCondition ($Process.MainWindowHandle -eq [IntPtr]::Zero) "$Label has a visible main window"
    Assert-SmokeCondition ([string]::IsNullOrWhiteSpace($Process.MainWindowTitle)) "$Label has a visible window title"
}

function New-SmokeSessionRoot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $PublicRoot = [Environment]::GetEnvironmentVariable("PUBLIC")
    $Base = if (-not [string]::IsNullOrWhiteSpace($PublicRoot)) { $PublicRoot } else { [System.IO.Path]::GetTempPath() }
    $Root = Join-Path $Base ("UsagiSmoke-{0}-{1}" -f $Label, [Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Path $Root -Force | Out-Null
    return $Root
}

function New-TemporaryStandardUser {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root
    )

    $UserName = "usagi" + [Guid]::NewGuid().ToString("N").Substring(0, 8)
    $PasswordText = "Usagi!" + [Guid]::NewGuid().ToString("N").Substring(0, 12) + "aA1"
    $Password = ConvertTo-SecureString $PasswordText -AsPlainText -Force
    New-LocalUser -Name $UserName -Password $Password -AccountNeverExpires -PasswordNeverExpires -Description "Temporary Usagi smoke user" | Out-Null

    $UsersGroup = $null
    try {
        $UsersGroup = Get-LocalGroup -SID "S-1-5-32-545"
        Add-LocalGroupMember -Group $UsersGroup.Name -Member $UserName -ErrorAction Stop
    }
    catch {
        $JoinError = $_
        $Member = $null
        if ($null -ne $UsersGroup) {
            $Member = Get-LocalGroupMember -Group $UsersGroup.Name -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -ieq "$env:COMPUTERNAME\$UserName" -or $_.Name -ieq $UserName }
        }
        if ($null -eq $Member) {
            Remove-LocalUser -Name $UserName -ErrorAction SilentlyContinue
            throw $JoinError
        }
    }

    & icacls.exe $Root /grant "${UserName}:(OI)(CI)M" /T /C | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Remove-LocalUser -Name $UserName -ErrorAction SilentlyContinue
        throw "failed to grant the temporary user access to $Root"
    }
    $AdminGroup = Get-LocalGroup -SID "S-1-5-32-544"
    $AdminMember = Get-LocalGroupMember -Group $AdminGroup.Name -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -ieq "$env:COMPUTERNAME\$UserName" -or $_.Name -ieq $UserName }
    if ($null -ne $AdminMember) {
        Remove-LocalUser -Name $UserName -ErrorAction SilentlyContinue
        throw "temporary user unexpectedly belongs to the Administrators group"
    }

    return [pscustomobject]@{
        UserName = $UserName
        Password = $Password
        PasswordText = $PasswordText
        Root = $Root
    }
}

function Invoke-TemporaryUserWorker {
    param(
        [Parameter(Mandatory = $true)]
        [pscustomobject]$User,
        [Parameter(Mandatory = $true)]
        [string]$Scenario,
        [hashtable]$Parameters = @{},
        [int]$TimeoutSeconds = 600
    )

    $CommonSource = Join-Path $PSScriptRoot "windows_smoke_common.ps1"
    $WorkerSource = Join-Path $PSScriptRoot "windows_smoke_worker.ps1"
    $CommonPath = Join-Path $User.Root "windows_smoke_common.ps1"
    $WorkerPath = Join-Path $User.Root "windows_smoke_worker.ps1"
    Copy-Item -LiteralPath $CommonSource -Destination $CommonPath -Force
    Copy-Item -LiteralPath $WorkerSource -Destination $WorkerPath -Force

    $ResultPath = Join-Path $User.Root "worker-result.json"
    $LogPath = Join-Path $User.Root "worker.log"
    $ConfigPath = Join-Path $User.Root "worker-config.json"
    $Config = [ordered]@{
        scenario = $Scenario
        username = $User.UserName
        result_path = $ResultPath
        log_path = $LogPath
    }
    foreach ($Name in $Parameters.Keys) {
        $Config[$Name] = $Parameters[$Name]
    }
    $Config | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $ConfigPath -Encoding UTF8

    $TaskName = "UsagiSmoke-" + [Guid]::NewGuid().ToString("N")
    $PowerShellPath = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
    $Arguments = '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{0}" -ConfigPath "{1}"' -f $WorkerPath, $ConfigPath
    $Action = New-ScheduledTaskAction -Execute $PowerShellPath -Argument $Arguments -WorkingDirectory $User.Root
    $TaskUser = "$env:COMPUTERNAME\$($User.UserName)"
    $Settings = New-ScheduledTaskSettingsSet `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 15) `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries

    try {
        Register-ScheduledTask `
            -TaskName $TaskName `
            -Action $Action `
            -Settings $Settings `
            -User $TaskUser `
            -Password $User.PasswordText `
            -RunLevel Limited `
            -Force | Out-Null
        Start-ScheduledTask -TaskName $TaskName

        $Deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $SeenRunning = $false
        while ([DateTime]::UtcNow -lt $Deadline) {
            if (Test-Path -LiteralPath $ResultPath -PathType Leaf) {
                try {
                    $Result = Get-Content -LiteralPath $ResultPath -Raw | ConvertFrom-Json
                }
                catch {
                    Start-Sleep -Milliseconds 250
                    continue
                }
                if (-not $Result.success) {
                    $Log = if (Test-Path -LiteralPath $LogPath) { Get-Content -LiteralPath $LogPath -Raw } else { "" }
                    throw "temporary-user worker failed: $($Result.error)`n$Log"
                }
                return
            }
            $Task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
            if ($null -eq $Task) {
                break
            }
            if ($Task.State -eq "Running") {
                $SeenRunning = $true
            }
            elseif ($Task.State -eq "Ready" -and $SeenRunning) {
                break
            }
            Start-Sleep -Milliseconds 250
        }

        $Log = if (Test-Path -LiteralPath $LogPath) { Get-Content -LiteralPath $LogPath -Raw } else { "" }
        throw "temporary-user worker did not complete within $TimeoutSeconds seconds`n$Log"
    }
    finally {
        Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    }
}

function Remove-TemporaryStandardUser {
    param(
        [Parameter(Mandatory = $true)]
        [pscustomobject]$User
    )

    $UserName = $User.UserName
    foreach ($Filter in @("USERNAME eq $UserName", "USERNAME eq $env:COMPUTERNAME\$UserName")) {
        & taskkill.exe /F /T /FI $Filter 2>$null | Out-Null
    }

    $ProfilePath = Join-Path $env:SystemDrive "Users\$UserName"
    $Deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ((Test-Path -LiteralPath $ProfilePath) -and [DateTime]::UtcNow -lt $Deadline) {
        $Profile = Get-CimInstance Win32_UserProfile -ErrorAction SilentlyContinue |
            Where-Object { $_.LocalPath -ieq $ProfilePath }
        if ($null -ne $Profile) {
            Remove-CimInstance -InputObject $Profile -ErrorAction SilentlyContinue
        }
        Start-Sleep -Milliseconds 500
    }

    Remove-LocalUser -Name $UserName -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $User.Root) {
        Remove-Item -LiteralPath $User.Root -Recurse -Force
    }

    Assert-SmokeCondition ($null -eq (Get-LocalUser -Name $UserName -ErrorAction SilentlyContinue)) "temporary user $UserName was not removed"
    Assert-SmokeCondition (-not (Test-Path -LiteralPath $ProfilePath)) "temporary profile $ProfilePath was not removed"
}

function Write-WorkerResult {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ResultPath,
        [Parameter(Mandatory = $true)]
        [bool]$Success,
        [string]$ErrorText = ""
    )

    [ordered]@{
        success = $Success
        error = $ErrorText
    } | ConvertTo-Json -Compress | Set-Content -LiteralPath $ResultPath -Encoding UTF8
}
