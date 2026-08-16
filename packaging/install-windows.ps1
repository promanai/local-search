[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)][string]$BundlePath,
    [string]$InstallRoot = "$env:ProgramFiles\LocalSearch",
    [string]$StateRoot = "$env:LOCALAPPDATA\LocalSearch",
    [string]$AuthorizedLogonSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
    [string]$BrokerPipe = '\\.\pipe\LocalSearch\WinFS\v1\default',
    [string[]]$ObserveRoot = @(),
    [switch]$EnableBrokerObservation,
    [switch]$AllowElevatedMetadataDevelopmentMode,
    [switch]$EnableContent,
    [switch]$AllowUnsignedDevelopmentBundle,
    [switch]$AllowLifecycleFailureInjection,
    [ValidateSet('None', 'AfterPayloadCopy', 'AfterRuntimeRegistration')]
    [string]$LifecycleFailurePoint = 'None',
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force
if ($LifecycleFailurePoint -ne 'None' -and -not $AllowLifecycleFailureInjection) {
    throw 'Lifecycle failure injection requires AllowLifecycleFailureInjection'
}
if ($LifecycleFailurePoint -eq 'None' -and $AllowLifecycleFailureInjection) {
    throw 'AllowLifecycleFailureInjection requires an explicit LifecycleFailurePoint'
}

function Get-LocalSearchRuntimeSnapshot {
    param([Parameter(Mandatory)]$InstallPlan)
    $Tasks = foreach ($TaskName in @($InstallPlan.agent.task_name, $InstallPlan.desktop.task_name)) {
        $Task = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
        if ($Task) {
            [pscustomobject][ordered]@{
                name = $TaskName
                xml = Export-ScheduledTask -TaskName $TaskName
                running = ($Task.State -eq 'Running')
            }
        }
    }
    $Service = Get-CimInstance Win32_Service -Filter "Name='$($InstallPlan.service.name)'" `
        -ErrorAction SilentlyContinue
    $ServiceSnapshot = if ($Service) {
        [pscustomobject][ordered]@{
            name = [string]$Service.Name
            display_name = [string]$Service.DisplayName
            path_name = [string]$Service.PathName
            start_mode = [string]$Service.StartMode
            running = ([string]$Service.State -eq 'Running')
        }
    }
    return [pscustomobject][ordered]@{
        tasks = @($Tasks)
        service = $ServiceSnapshot
    }
}

function Remove-LocalSearchRuntime {
    param([Parameter(Mandatory)]$InstallPlan)
    foreach ($TaskName in @($InstallPlan.agent.task_name, $InstallPlan.desktop.task_name)) {
        Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    }
    $ExistingService = Get-Service -Name $InstallPlan.service.name -ErrorAction SilentlyContinue
    if (-not $ExistingService) { return }
    Stop-Service -Name $InstallPlan.service.name -Force -ErrorAction SilentlyContinue
    & sc.exe delete $InstallPlan.service.name | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Existing WinFS service removal failed' }
    $Deadline = (Get-Date).AddSeconds(15)
    while ((Get-Service -Name $InstallPlan.service.name -ErrorAction SilentlyContinue) -and
        (Get-Date) -lt $Deadline) {
        Start-Sleep -Milliseconds 200
    }
    if (Get-Service -Name $InstallPlan.service.name -ErrorAction SilentlyContinue) {
        throw 'Existing WinFS service did not leave SCM before the deadline'
    }
}

function Register-LocalSearchRuntime {
    param([Parameter(Mandatory)]$InstallPlan)
    if ($InstallPlan.broker_enabled) {
        $ServiceCommand = Join-LocalSearchCommandLine -Executable $InstallPlan.service.executable `
            -ArgumentList $InstallPlan.service.arguments
        New-Service -Name $InstallPlan.service.name -BinaryPathName $ServiceCommand `
            -DisplayName 'LocalSearch WinFS Metadata Broker' -StartupType Automatic | Out-Null
        & sc.exe failure $InstallPlan.service.name reset= 86400 `
            actions= restart/5000/restart/15000/""/0 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'WinFS service recovery policy failed' }
        Start-Service -Name $InstallPlan.service.name
    }
    $AgentArguments = @($InstallPlan.agent.arguments | ForEach-Object {
        ConvertTo-LocalSearchCommandArgument -Value $_
    }) -join ' '
    $AgentAction = New-ScheduledTaskAction -Execute $InstallPlan.agent.executable `
        -Argument $AgentArguments
    $DesktopAction = New-ScheduledTaskAction -Execute $InstallPlan.desktop.executable
    $Trigger = New-ScheduledTaskTrigger -AtLogOn -User $InstallPlan.owner_sid
    $PrincipalDefinition = New-ScheduledTaskPrincipal -UserId $InstallPlan.owner_sid `
        -LogonType Interactive -RunLevel Limited
    $Settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) `
        -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
    Register-ScheduledTask -TaskName $InstallPlan.agent.task_name -Action $AgentAction `
        -Trigger $Trigger -Principal $PrincipalDefinition -Settings $Settings -Force | Out-Null
    Register-ScheduledTask -TaskName $InstallPlan.desktop.task_name -Action $DesktopAction `
        -Trigger $Trigger -Principal $PrincipalDefinition -Settings $Settings -Force | Out-Null
    Start-ScheduledTask -TaskName $InstallPlan.agent.task_name
    Start-ScheduledTask -TaskName $InstallPlan.desktop.task_name
}

function Restore-LocalSearchRuntime {
    param(
        [Parameter(Mandatory)]$InstallPlan,
        [Parameter(Mandatory)]$Snapshot
    )
    if ($Snapshot.service) {
        $StartupType = switch ($Snapshot.service.start_mode) {
            'Auto' { 'Automatic' }
            'Manual' { 'Manual' }
            'Disabled' { 'Disabled' }
            default { throw "Unsupported prior service start mode: $($Snapshot.service.start_mode)" }
        }
        New-Service -Name $Snapshot.service.name -BinaryPathName $Snapshot.service.path_name `
            -DisplayName $Snapshot.service.display_name -StartupType $StartupType | Out-Null
        & sc.exe failure $Snapshot.service.name reset= 86400 `
            actions= restart/5000/restart/15000/""/0 | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Prior WinFS service recovery policy restore failed' }
        if ($Snapshot.service.running) { Start-Service -Name $Snapshot.service.name }
    }
    foreach ($Task in @($Snapshot.tasks)) {
        Register-ScheduledTask -TaskName $Task.name -Xml $Task.xml -Force | Out-Null
        if ($Task.running) { Start-ScheduledTask -TaskName $Task.name }
    }
}

$Manifest = Test-LocalSearchBundle -BundlePath $BundlePath `
    -RequireAuthenticodeSignature:(-not $AllowUnsignedDevelopmentBundle)
$Plan = New-LocalSearchInstallPlan -BundlePath $BundlePath -InstallRoot $InstallRoot `
    -StateRoot $StateRoot -AuthorizedLogonSid $AuthorizedLogonSid -BrokerPipe $BrokerPipe `
    -ObserveRoot $ObserveRoot -EnableBrokerObservation:$EnableBrokerObservation `
    -AllowElevatedMetadataDevelopmentMode:$AllowElevatedMetadataDevelopmentMode `
    -EnableContent:$EnableContent
if ($PlanOnly) {
    $Plan | Add-Member -NotePropertyName signature_required `
        -NotePropertyValue (-not $AllowUnsignedDevelopmentBundle)
    $Plan | Add-Member -NotePropertyName lifecycle_failure_point `
        -NotePropertyValue $LifecycleFailurePoint
    if ($LifecycleFailurePoint -ne 'None') {
        $Plan.public_release_eligible = $false
    }
    $Plan | ConvertTo-Json -Depth 10
    return
}
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if (-not $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'LocalSearch installation requires an elevated PowerShell'
}
if ($Identity.User.Value -ne $Plan.owner_sid) {
    throw 'AuthorizedLogonSid must match the installing interactive user'
}
$ContentWorkspaceValidated = $false
if ($Plan.content_enabled) {
    $ContentExecutable = Join-Path $Plan.bundle_root 'localsearch-content-index.exe'
    $ContentRoot = Join-Path $Plan.state_root 'content-index-v1'
    $PreflightOutput = & $ContentExecutable search --index $ContentRoot `
        --query '__localsearch_package_preflight__' --top-k 1 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw 'EnableContent requires a complete readable content workspace in StateRoot'
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Plan.state_root 'content-workspace.json') -PathType Leaf)) {
        throw 'EnableContent requires content-workspace.json in StateRoot'
    }
    $ContentWorkspaceValidated = $true
}

$InstallExists = Test-Path -LiteralPath $Plan.install_root -PathType Container
if ($InstallExists) {
    Test-LocalSearchMarker -Root $Plan.install_root -Kind install -OwnerSid $Plan.owner_sid | Out-Null
}
$StateExists = Test-Path -LiteralPath $Plan.state_root -PathType Container
$StateMarkerPath = Join-Path $Plan.state_root '.localsearch-state.json'
$StateMarkerExisted = $StateExists -and
    (Test-Path -LiteralPath $StateMarkerPath -PathType Leaf)
$StateAclSnapshot = if ($StateExists) { Get-Acl -LiteralPath $Plan.state_root }
$AdoptContentWorkspace = $false
if ($StateExists) {
    if ($StateMarkerExisted) {
        Test-LocalSearchMarker -Root $Plan.state_root -Kind state -OwnerSid $Plan.owner_sid | Out-Null
    } elseif ($ContentWorkspaceValidated) {
        # folder-sync intentionally creates the content workspace before the product is installed.
        # A successful reader preflight plus explicit StateRoot authorizes one-time marker adoption.
        $AdoptContentWorkspace = $true
    } else {
        throw 'Existing StateRoot is not an owned LocalSearch state directory'
    }
}

$RuntimeSnapshot = Get-LocalSearchRuntimeSnapshot -InstallPlan $Plan
if (-not $InstallExists -and
    (@($RuntimeSnapshot.tasks).Count -gt 0 -or $null -ne $RuntimeSnapshot.service)) {
    throw 'Orphan LocalSearch runtime exists without an owned install root'
}

if ($PSCmdlet.ShouldProcess($Plan.install_root, 'Install or repair LocalSearch')) {
    New-Item -ItemType Directory -Path $Plan.install_root -Force | Out-Null
    New-Item -ItemType Directory -Path $Plan.state_root -Force | Out-Null
    $Backup = Join-Path ([IO.Path]::GetTempPath()) `
        "localsearch-install-backup-$PID-$([Guid]::NewGuid().ToString('N'))"
    $PayloadNames = @(
        (Get-LocalSearchExpectedBundleFiles) +
        'manifest.json' +
        'manifest.p7s' +
        '.localsearch-install.json'
    )
    $MutationStarted = $false
    try {
        if ($InstallExists) {
            New-Item -ItemType Directory -Path $Backup | Out-Null
            foreach ($Name in $PayloadNames) {
                $Existing = Join-Path $Plan.install_root $Name
                if (Test-Path -LiteralPath $Existing -PathType Leaf) {
                    Copy-Item -LiteralPath $Existing -Destination (Join-Path $Backup $Name)
                }
            }
        }
        $MutationStarted = $true
        Remove-LocalSearchRuntime -InstallPlan $Plan
        foreach ($Name in Get-LocalSearchExpectedBundleFiles) {
            Copy-Item -LiteralPath (Join-Path $Plan.bundle_root $Name) `
                -Destination (Join-Path $Plan.install_root $Name) -Force
        }
        Copy-Item -LiteralPath (Join-Path $Plan.bundle_root 'manifest.json') `
            -Destination (Join-Path $Plan.install_root 'manifest.json') -Force
        $DetachedManifest = Join-Path $Plan.bundle_root 'manifest.p7s'
        if (Test-Path -LiteralPath $DetachedManifest -PathType Leaf) {
            Copy-Item -LiteralPath $DetachedManifest `
                -Destination (Join-Path $Plan.install_root 'manifest.p7s') -Force
        } else {
            $InstalledDetachedManifest = Join-Path $Plan.install_root 'manifest.p7s'
            if (Test-Path -LiteralPath $InstalledDetachedManifest -PathType Leaf) {
                Remove-Item -LiteralPath $InstalledDetachedManifest -Force
            }
        }
        Write-LocalSearchMarker -Root $Plan.install_root -Kind install `
            -OwnerSid $Plan.owner_sid -GitCommit $Manifest.git_commit
        if (-not $StateExists -or $AdoptContentWorkspace) {
            Write-LocalSearchMarker -Root $Plan.state_root -Kind state `
                -OwnerSid $Plan.owner_sid -GitCommit $Manifest.git_commit
        }

        Set-LocalSearchPrivateStateAcl -Root $Plan.state_root -OwnerSid $Plan.owner_sid

        if ($LifecycleFailurePoint -eq 'AfterPayloadCopy') {
            throw 'Injected lifecycle failure after payload copy'
        }

        Register-LocalSearchRuntime -InstallPlan $Plan
        if ($LifecycleFailurePoint -eq 'AfterRuntimeRegistration') {
            throw 'Injected lifecycle failure after runtime registration'
        }
    }
    catch {
        $InstallFailure = $_
        if (-not $MutationStarted) { throw $InstallFailure }
        try {
            Remove-LocalSearchRuntime -InstallPlan $Plan
            if ($InstallExists) {
                foreach ($Name in $PayloadNames) {
                    $Current = Join-Path $Plan.install_root $Name
                    if (Test-Path -LiteralPath $Current -PathType Leaf) {
                        Remove-Item -LiteralPath $Current -Force
                    }
                    $Previous = Join-Path $Backup $Name
                    if (Test-Path -LiteralPath $Previous -PathType Leaf) {
                        Copy-Item -LiteralPath $Previous `
                            -Destination (Join-Path $Plan.install_root $Name) -Force
                    }
                }
                Restore-LocalSearchRuntime -InstallPlan $Plan -Snapshot $RuntimeSnapshot
            } else {
                if (Test-Path -LiteralPath $Plan.install_root -PathType Container) {
                    Remove-Item -LiteralPath $Plan.install_root -Recurse -Force
                }
                if (-not $StateExists -and
                    (Test-Path -LiteralPath $Plan.state_root -PathType Container)) {
                    Remove-Item -LiteralPath $Plan.state_root -Recurse -Force
                }
            }
            if ($StateExists) {
                if (-not $StateMarkerExisted -and
                    (Test-Path -LiteralPath $StateMarkerPath -PathType Leaf)) {
                    Remove-Item -LiteralPath $StateMarkerPath -Force
                }
                Set-Acl -LiteralPath $Plan.state_root -AclObject $StateAclSnapshot
            }
        } catch {
            throw "LocalSearch install failed and rollback also failed: install=[$($InstallFailure.Exception.Message)]; rollback=[$($_.Exception.Message)]"
        }
        throw $InstallFailure
    }
    finally {
        if (Test-Path -LiteralPath $Backup) { Remove-Item -LiteralPath $Backup -Recurse -Force }
    }
}
Write-Host "LocalSearch installation complete: commit=$($Manifest.git_commit)"
