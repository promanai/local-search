[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$BaselineBundlePath,
    [Parameter(Mandatory)][string]$CandidateBundlePath,
    [Parameter(Mandatory)][string]$InstallRoot,
    [Parameter(Mandatory)][string]$StateRoot,
    [Parameter(Mandatory)][string]$OutputPath,
    [string]$AuthorizedLogonSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
    [switch]$AllowUnsignedDevelopmentBundles,
    [switch]$ConfirmDisposableMachine,
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force

$Install = Assert-LocalSearchSafeDirectory -Path $InstallRoot -Purpose 'OPS gate install root'
$State = Assert-LocalSearchSafeDirectory -Path $StateRoot -Purpose 'OPS gate state root'
$Sid = Assert-LocalSearchSid -Sid $AuthorizedLogonSid
$Output = [IO.Path]::GetFullPath($OutputPath)
foreach ($OwnedRoot in @($Install, $State)) {
    $Prefix = $OwnedRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if ($Output.Equals($OwnedRoot, [StringComparison]::OrdinalIgnoreCase) -or
        $Output.StartsWith($Prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'OPS gate output must stay outside disposable install and state roots'
    }
}

$RequireSignature = -not $AllowUnsignedDevelopmentBundles
$Baseline = Test-LocalSearchBundle -BundlePath $BaselineBundlePath `
    -RequireAuthenticodeSignature:$RequireSignature
$Candidate = Test-LocalSearchBundle -BundlePath $CandidateBundlePath `
    -RequireAuthenticodeSignature:$RequireSignature
if ($Baseline.git_commit -eq $Candidate.git_commit) {
    throw 'OPS gate requires distinct baseline and candidate commits'
}

$StepNames = @(
    'fresh-install',
    'repair',
    'rollback-after-payload-copy',
    'rollback-after-runtime-registration',
    'upgrade',
    'uninstall-keep-indexes',
    'reinstall',
    'uninstall-remove-indexes'
)
$Plan = [ordered]@{
    schema_version = 1
    product = 'LocalSearch'
    gate = 'OPS-GATE-001'
    baseline_commit = [string]$Baseline.git_commit
    candidate_commit = [string]$Candidate.git_commit
    signed_bundles_required = $RequireSignature
    current_user_policy = $true
    elevated_broker_enabled = $false
    disposable_confirmation_required = $true
    steps = @($StepNames)
    privacy = [ordered]@{
        paths_included = $false
        filenames_included = $false
        queries_included = $false
        content_included = $false
    }
}
if ($PlanOnly) {
    $Plan | ConvertTo-Json -Depth 8
    return
}
if (-not $ConfirmDisposableMachine) {
    throw 'OPS gate execution requires ConfirmDisposableMachine'
}
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if (-not $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'OPS gate execution requires an elevated PowerShell'
}
if ($Identity.User.Value -ne $Sid) {
    throw 'AuthorizedLogonSid must match the elevated interactive user'
}
if ((Test-Path -LiteralPath $Install) -or (Test-Path -LiteralPath $State)) {
    throw 'OPS gate requires absent disposable install and state roots'
}
foreach ($TaskName in @('LocalSearch Agent', 'LocalSearch Desktop')) {
    if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        throw 'OPS gate refuses to replace an existing LocalSearch scheduled task'
    }
}
if (Get-Service -Name 'LocalSearchWinFS' -ErrorAction SilentlyContinue) {
    throw 'OPS gate refuses to replace an existing LocalSearch service'
}

function Invoke-GateInstall {
    param(
        [Parameter(Mandatory)][string]$Bundle,
        [ValidateSet('None', 'AfterPayloadCopy', 'AfterRuntimeRegistration')]
        [string]$FailurePoint = 'None'
    )
    $Arguments = @{
        BundlePath = $Bundle
        InstallRoot = $Install
        StateRoot = $State
        AuthorizedLogonSid = $Sid
        LifecycleFailurePoint = $FailurePoint
        Confirm = $false
    }
    if ($AllowUnsignedDevelopmentBundles) {
        $Arguments.AllowUnsignedDevelopmentBundle = $true
    }
    if ($FailurePoint -ne 'None') {
        $Arguments.AllowLifecycleFailureInjection = $true
    }
    & (Join-Path $PSScriptRoot 'install-windows.ps1') @Arguments
}

function Invoke-GateUninstall {
    param(
        [Parameter(Mandatory)][ValidateSet('KeepIndexes', 'RemoveIndexes')]
        [string]$Retention
    )
    & (Join-Path $PSScriptRoot 'uninstall-windows.ps1') `
        -InstallRoot $Install -StateRoot $State -AuthorizedLogonSid $Sid `
        -Retention $Retention -Confirm:$false
}

function Assert-InstalledState {
    param([Parameter(Mandatory)]$ExpectedManifest)
    $Marker = Test-LocalSearchMarker -Root $Install -Kind install -OwnerSid $Sid
    if ($Marker.git_commit -ne $ExpectedManifest.git_commit) {
        throw 'Installed marker commit does not match the expected bundle'
    }
    $InstalledManifest = Test-LocalSearchBundle -BundlePath $Install
    if ($InstalledManifest.git_commit -ne $ExpectedManifest.git_commit) {
        throw 'Installed payload commit does not match the expected bundle'
    }
    foreach ($TaskName in @('LocalSearch Agent', 'LocalSearch Desktop')) {
        $Task = Get-ScheduledTask -TaskName $TaskName -ErrorAction Stop
        if ($Task.Principal.UserId -ne $Sid -or [string]$Task.Principal.RunLevel -ne 'Limited') {
            throw 'Scheduled task principal escaped the owner/limited policy'
        }
    }
    if (Get-Service -Name 'LocalSearchWinFS' -ErrorAction SilentlyContinue) {
        throw 'Public current-user OPS gate unexpectedly installed the elevated broker'
    }
    $StateMarker = Test-LocalSearchMarker -Root $State -Kind state -OwnerSid $Sid
    if (-not $StateMarker) { throw 'State marker validation failed' }
    $Acl = Get-Acl -LiteralPath $State
    if (-not $Acl.AreAccessRulesProtected) {
        throw 'State ACL inherits broad access rules'
    }
    $ActualSids = @($Acl.Access | ForEach-Object {
        $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
    } | Sort-Object -Unique)
    $ExpectedSids = @('S-1-5-18', $Sid) | Sort-Object -Unique
    if (($ActualSids -join ',') -ne ($ExpectedSids -join ',')) {
        throw 'State ACL grants an unexpected identity'
    }
}

function Assert-UninstalledState {
    param([Parameter(Mandatory)][bool]$ExpectState)
    foreach ($TaskName in @('LocalSearch Agent', 'LocalSearch Desktop')) {
        if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
            throw 'Uninstall left an orphan scheduled task'
        }
    }
    if (Get-Service -Name 'LocalSearchWinFS' -ErrorAction SilentlyContinue) {
        throw 'Uninstall left an orphan service'
    }
    if (Test-Path -LiteralPath $Install) {
        throw 'Uninstall left the marked install root'
    }
    if ([bool](Test-Path -LiteralPath $State) -ne $ExpectState) {
        throw 'Uninstall retention result did not match policy'
    }
    if ($ExpectState) {
        Test-LocalSearchMarker -Root $State -Kind state -OwnerSid $Sid | Out-Null
    }
}

$Results = [Collections.Generic.List[object]]::new()
$CurrentStage = 'preflight'
$GateStarted = [Diagnostics.Stopwatch]::StartNew()
function Invoke-RecordedStep {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Action
    )
    $script:CurrentStage = $Name
    $Timer = [Diagnostics.Stopwatch]::StartNew()
    & $Action
    $Timer.Stop()
    $Results.Add([ordered]@{
        name = $Name
        pass = $true
        duration_ms = [int64]$Timer.ElapsedMilliseconds
    })
}

try {
    Invoke-RecordedStep 'fresh-install' {
        Invoke-GateInstall -Bundle $BaselineBundlePath
        Assert-InstalledState -ExpectedManifest $Baseline
    }
    Invoke-RecordedStep 'repair' {
        Invoke-GateInstall -Bundle $BaselineBundlePath
        Assert-InstalledState -ExpectedManifest $Baseline
    }
    Invoke-RecordedStep 'rollback-after-payload-copy' {
        $Failed = $false
        try {
            Invoke-GateInstall -Bundle $CandidateBundlePath -FailurePoint AfterPayloadCopy
        } catch {
            $Failed = $true
        }
        if (-not $Failed) { throw 'Payload-copy failure injection did not fail' }
        Assert-InstalledState -ExpectedManifest $Baseline
    }
    Invoke-RecordedStep 'rollback-after-runtime-registration' {
        $Failed = $false
        try {
            Invoke-GateInstall -Bundle $CandidateBundlePath `
                -FailurePoint AfterRuntimeRegistration
        } catch {
            $Failed = $true
        }
        if (-not $Failed) { throw 'Runtime failure injection did not fail' }
        Assert-InstalledState -ExpectedManifest $Baseline
    }
    Invoke-RecordedStep 'upgrade' {
        Invoke-GateInstall -Bundle $CandidateBundlePath
        Assert-InstalledState -ExpectedManifest $Candidate
    }
    Invoke-RecordedStep 'uninstall-keep-indexes' {
        Invoke-GateUninstall -Retention KeepIndexes
        Assert-UninstalledState -ExpectState $true
    }
    Invoke-RecordedStep 'reinstall' {
        Invoke-GateInstall -Bundle $CandidateBundlePath
        Assert-InstalledState -ExpectedManifest $Candidate
    }
    Invoke-RecordedStep 'uninstall-remove-indexes' {
        Invoke-GateUninstall -Retention RemoveIndexes
        Assert-UninstalledState -ExpectState $false
    }
    $GateStarted.Stop()
    $Report = [ordered]@{
        schema_version = 1
        product = 'LocalSearch'
        gate = 'OPS-GATE-001'
        generated_at_utc = (Get-Date).ToUniversalTime().ToString('o')
        pass = $true
        baseline_commit = [string]$Baseline.git_commit
        candidate_commit = [string]$Candidate.git_commit
        signed_bundles_required = $RequireSignature
        current_user_policy = $true
        elevated_broker_enabled = $false
        duration_ms = [int64]$GateStarted.ElapsedMilliseconds
        steps = @($Results)
        privacy = $Plan.privacy
    }
    $Parent = Split-Path -Parent $Output
    if ($Parent) { New-Item -ItemType Directory -Path $Parent -Force | Out-Null }
    $Report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Output -Encoding utf8
    Write-Host "OPS-GATE-001: PASS; evidence=$Output"
} catch {
    $GateStarted.Stop()
    $FailureReport = [ordered]@{
        schema_version = 1
        product = 'LocalSearch'
        gate = 'OPS-GATE-001'
        generated_at_utc = (Get-Date).ToUniversalTime().ToString('o')
        pass = $false
        failed_stage = $CurrentStage
        error_type = $_.Exception.GetType().FullName
        duration_ms = [int64]$GateStarted.ElapsedMilliseconds
        steps = @($Results)
        privacy = $Plan.privacy
    }
    $Parent = Split-Path -Parent $Output
    if ($Parent) { New-Item -ItemType Directory -Path $Parent -Force | Out-Null }
    $FailureReport | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Output -Encoding utf8
    throw
}
