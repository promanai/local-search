[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [string]$InstallRoot = "$env:ProgramFiles\LocalSearch",
    [string]$StateRoot = "$env:LOCALAPPDATA\LocalSearch",
    [string]$AuthorizedLogonSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
    [ValidateSet('KeepIndexes', 'RemoveIndexes')][string]$Retention = 'KeepIndexes',
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force
$Install = Assert-LocalSearchSafeDirectory -Path $InstallRoot -Purpose 'Install root'
$State = Assert-LocalSearchSafeDirectory -Path $StateRoot -Purpose 'State root'
$Sid = Assert-LocalSearchSid -Sid $AuthorizedLogonSid
$Plan = [ordered]@{
    schema_version = 1
    product = 'LocalSearch'
    install_root = $Install
    state_root = $State
    owner_sid = $Sid
    retention = $Retention
    service = 'LocalSearchWinFS'
    tasks = @('LocalSearch Agent', 'LocalSearch Desktop')
}
if ($PlanOnly) {
    $Plan | ConvertTo-Json -Depth 6
    return
}
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if (-not $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'LocalSearch uninstall requires an elevated PowerShell'
}
if ($Identity.User.Value -ne $Sid) { throw 'AuthorizedLogonSid must match the current user' }

if (Test-Path -LiteralPath $Install -PathType Container) {
    Test-LocalSearchMarker -Root $Install -Kind install -OwnerSid $Sid | Out-Null
}
if ($Retention -eq 'RemoveIndexes' -and (Test-Path -LiteralPath $State -PathType Container)) {
    Test-LocalSearchMarker -Root $State -Kind state -OwnerSid $Sid | Out-Null
}
if ($PSCmdlet.ShouldProcess('LocalSearch service and scheduled tasks', 'Stop and unregister')) {
    foreach ($Task in $Plan.tasks) {
        Stop-ScheduledTask -TaskName $Task -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $Task -Confirm:$false -ErrorAction SilentlyContinue
    }
    if (Get-Service -Name $Plan.service -ErrorAction SilentlyContinue) {
        Stop-Service -Name $Plan.service -Force -ErrorAction SilentlyContinue
        & sc.exe delete $Plan.service | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'WinFS service removal failed' }
        $Deadline = (Get-Date).AddSeconds(15)
        while ((Get-Service -Name $Plan.service -ErrorAction SilentlyContinue) -and
            (Get-Date) -lt $Deadline) {
            Start-Sleep -Milliseconds 200
        }
        if (Get-Service -Name $Plan.service -ErrorAction SilentlyContinue) {
            throw 'WinFS service did not leave SCM before the deadline'
        }
    }
}
if (Test-Path -LiteralPath $Install -PathType Container) {
    if ($PSCmdlet.ShouldProcess($Install, 'Remove marked LocalSearch installation')) {
        Remove-Item -LiteralPath $Install -Recurse -Force
    }
}
if ($Retention -eq 'RemoveIndexes' -and (Test-Path -LiteralPath $State -PathType Container)) {
    if ($PSCmdlet.ShouldProcess($State, 'Remove marked LocalSearch indexes and state')) {
        Remove-Item -LiteralPath $State -Recurse -Force
    }
}
Write-Host "LocalSearch uninstall complete; retention=$Retention"
