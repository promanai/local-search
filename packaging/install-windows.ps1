[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)][string]$BundlePath,
    [string]$InstallRoot = "$env:ProgramFiles\LocalSearch",
    [string]$StateRoot = "$env:LOCALAPPDATA\LocalSearch",
    [string]$AuthorizedLogonSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
    [string]$BrokerPipe = '\\.\pipe\LocalSearch\WinFS\v1\default',
    [string[]]$ObserveRoot = @(),
    [switch]$EnableBrokerObservation,
    [switch]$EnableContent,
    [switch]$AllowUnsignedDevelopmentBundle,
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force
$Manifest = Test-LocalSearchBundle -BundlePath $BundlePath `
    -RequireAuthenticodeSignature:(-not $AllowUnsignedDevelopmentBundle)
$Plan = New-LocalSearchInstallPlan -BundlePath $BundlePath -InstallRoot $InstallRoot `
    -StateRoot $StateRoot -AuthorizedLogonSid $AuthorizedLogonSid -BrokerPipe $BrokerPipe `
    -ObserveRoot $ObserveRoot -EnableBrokerObservation:$EnableBrokerObservation `
    -EnableContent:$EnableContent
if ($PlanOnly) {
    $Plan | Add-Member -NotePropertyName signature_required `
        -NotePropertyValue (-not $AllowUnsignedDevelopmentBundle)
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
$AdoptContentWorkspace = $false
if ($StateExists) {
    $StateMarker = Join-Path $Plan.state_root '.localsearch-state.json'
    if (Test-Path -LiteralPath $StateMarker -PathType Leaf) {
        Test-LocalSearchMarker -Root $Plan.state_root -Kind state -OwnerSid $Plan.owner_sid | Out-Null
    } elseif ($ContentWorkspaceValidated) {
        # folder-sync intentionally creates the content workspace before the product is installed.
        # A successful reader preflight plus explicit StateRoot authorizes one-time marker adoption.
        $AdoptContentWorkspace = $true
    } else {
        throw 'Existing StateRoot is not an owned LocalSearch state directory'
    }
}

if ($PSCmdlet.ShouldProcess($Plan.install_root, 'Install or repair LocalSearch')) {
    New-Item -ItemType Directory -Path $Plan.install_root -Force | Out-Null
    New-Item -ItemType Directory -Path $Plan.state_root -Force | Out-Null
    $Backup = Join-Path ([IO.Path]::GetTempPath()) "localsearch-install-backup-$PID"
    try {
        foreach ($TaskName in @($Plan.agent.task_name, $Plan.desktop.task_name)) {
            Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
        }
        $ExistingService = Get-Service -Name $Plan.service.name -ErrorAction SilentlyContinue
        if ($ExistingService) {
            Stop-Service -Name $Plan.service.name -Force -ErrorAction SilentlyContinue
            & sc.exe delete $Plan.service.name | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Existing WinFS service removal failed' }
            $Deadline = (Get-Date).AddSeconds(15)
            while ((Get-Service -Name $Plan.service.name -ErrorAction SilentlyContinue) -and
                (Get-Date) -lt $Deadline) {
                Start-Sleep -Milliseconds 200
            }
            if (Get-Service -Name $Plan.service.name -ErrorAction SilentlyContinue) {
                throw 'Existing WinFS service did not leave SCM before the deadline'
            }
        }
        if ($InstallExists) {
            New-Item -ItemType Directory -Path $Backup | Out-Null
            foreach ($Name in @((Get-LocalSearchExpectedBundleFiles) + 'manifest.json' + 'manifest.p7s')) {
                $Existing = Join-Path $Plan.install_root $Name
                if (Test-Path -LiteralPath $Existing -PathType Leaf) {
                    Copy-Item -LiteralPath $Existing -Destination (Join-Path $Backup $Name)
                }
            }
        }
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
        }
        Write-LocalSearchMarker -Root $Plan.install_root -Kind install `
            -OwnerSid $Plan.owner_sid -GitCommit $Manifest.git_commit
        if (-not $StateExists -or $AdoptContentWorkspace) {
            Write-LocalSearchMarker -Root $Plan.state_root -Kind state `
                -OwnerSid $Plan.owner_sid -GitCommit $Manifest.git_commit
        }

        Set-LocalSearchPrivateStateAcl -Root $Plan.state_root -OwnerSid $Plan.owner_sid

        if ($Plan.broker_enabled) {
            $ServiceCommand = Join-LocalSearchCommandLine -Executable $Plan.service.executable `
                -ArgumentList $Plan.service.arguments
            New-Service -Name $Plan.service.name -BinaryPathName $ServiceCommand `
                -DisplayName 'LocalSearch WinFS Metadata Broker' -StartupType Automatic | Out-Null
            & sc.exe failure $Plan.service.name reset= 86400 actions= restart/5000/restart/15000/""/0 | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'WinFS service recovery policy failed' }
            Start-Service -Name $Plan.service.name
        }

        $AgentArguments = @($Plan.agent.arguments | ForEach-Object {
            ConvertTo-LocalSearchCommandArgument -Value $_
        }) -join ' '
        $AgentAction = New-ScheduledTaskAction -Execute $Plan.agent.executable -Argument $AgentArguments
        $DesktopAction = New-ScheduledTaskAction -Execute $Plan.desktop.executable
        $Trigger = New-ScheduledTaskTrigger -AtLogOn -User $Plan.owner_sid
        $PrincipalDefinition = New-ScheduledTaskPrincipal -UserId $Plan.owner_sid `
            -LogonType Interactive -RunLevel Limited
        $Settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) `
            -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
        Register-ScheduledTask -TaskName $Plan.agent.task_name -Action $AgentAction `
            -Trigger $Trigger -Principal $PrincipalDefinition -Settings $Settings -Force | Out-Null
        Register-ScheduledTask -TaskName $Plan.desktop.task_name -Action $DesktopAction `
            -Trigger $Trigger -Principal $PrincipalDefinition -Settings $Settings -Force | Out-Null
        Start-ScheduledTask -TaskName $Plan.agent.task_name
        Start-ScheduledTask -TaskName $Plan.desktop.task_name
    }
    catch {
        if (Test-Path -LiteralPath $Backup -PathType Container) {
            foreach ($Name in @((Get-LocalSearchExpectedBundleFiles) + 'manifest.json' + 'manifest.p7s')) {
                $Previous = Join-Path $Backup $Name
                if (Test-Path -LiteralPath $Previous -PathType Leaf) {
                    Copy-Item -LiteralPath $Previous -Destination (Join-Path $Plan.install_root $Name) -Force
                }
            }
        } elseif (-not $InstallExists -and (Test-Path -LiteralPath $Plan.install_root -PathType Container)) {
            foreach ($Name in @((Get-LocalSearchExpectedBundleFiles) + 'manifest.json' + 'manifest.p7s')) {
                $Partial = Join-Path $Plan.install_root $Name
                if (Test-Path -LiteralPath $Partial -PathType Leaf) {
                    Remove-Item -LiteralPath $Partial -Force
                }
            }
            $PartialMarker = Join-Path $Plan.install_root '.localsearch-install.json'
            if (Test-Path -LiteralPath $PartialMarker -PathType Leaf) {
                Remove-Item -LiteralPath $PartialMarker -Force
            }
            if (@(Get-ChildItem -LiteralPath $Plan.install_root -Force).Count -eq 0) {
                Remove-Item -LiteralPath $Plan.install_root -Force
            }
        }
        throw
    }
    finally {
        if (Test-Path -LiteralPath $Backup) { Remove-Item -LiteralPath $Backup -Recurse -Force }
    }
}
Write-Host "LocalSearch installation complete: commit=$($Manifest.git_commit)"
