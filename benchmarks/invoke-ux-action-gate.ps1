[CmdletBinding()]
param(
    [string]$Volume = 'L:\',
    [string]$VhdxPath = '',
    [string]$BuildManifest = '.lab/start-010-load-bundle.json',
    [string]$OutputDirectory = 'reports/ux/start-010-u',
    [switch]$ConfirmDisposableVolume,
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Import-Module (Join-Path $PSScriptRoot 'UxActionGateContract.psm1') -Force

$Plan = [ordered]@{
    schema_version = 1
    gate = 'UX-ACTION-GATE-001'
    plan_only = [bool]$PlanOnly
    release_eligible = $false
    requires_elevation = $true
    requires_disposable_ntfs_volume = $true
    requires_repository_vhdx = $true
    phases = @(
        'clean-provenance-preflight',
        'controlled-real-filesystem-fixture',
        'long-name-layout',
        'rename-current-identity',
        'move-current-identity',
        'delete-fail-closed',
        'vhdx-offline-fail-closed',
        'vhdx-reattach-recovery',
        'bounded-cleanup',
        'redacted-verdict'
    )
}
if ($PlanOnly) {
    $Plan | ConvertTo-Json -Depth 6
    return
}

$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if (-not $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'UX-ACTION-GATE-001 requires an elevated PowerShell for live USN/VHDX evidence'
}
if (-not $ConfirmDisposableVolume) {
    throw 'UX-ACTION-GATE-001 requires -ConfirmDisposableVolume'
}
if ($Volume -notmatch '^[D-Zd-z]:[\\/]$') {
    throw 'Volume must be an explicit non-system drive root such as L:\'
}
if ([string]::IsNullOrWhiteSpace($VhdxPath)) {
    throw 'UX-ACTION-GATE-001 requires -VhdxPath for offline/reattach evidence'
}
if (git -C $Repository status --porcelain) {
    throw 'UX-ACTION-GATE-001 requires a clean repository'
}

$OutputRoot = [IO.Path]::GetFullPath((Join-Path $Repository $OutputDirectory))
$RepositoryPrefix = $Repository.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if (-not $OutputRoot.StartsWith($RepositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'UX-ACTION-GATE-001 output must stay inside the repository'
}
$LabRoot = [IO.Path]::GetFullPath((Join-Path $Repository '.lab'))
$ResolvedVhdx = [IO.Path]::GetFullPath($VhdxPath)
if (-not $ResolvedVhdx.StartsWith(
        $LabRoot + [IO.Path]::DirectorySeparatorChar,
        [StringComparison]::OrdinalIgnoreCase
    ) -or
    [IO.Path]::GetExtension($ResolvedVhdx) -ne '.vhdx' -or
    -not (Test-Path -LiteralPath $ResolvedVhdx -PathType Leaf)) {
    throw 'UX-ACTION-GATE-001 accepts only an existing .vhdx below repository .lab'
}

$Started = [DateTime]::UtcNow
$RunnerParameters = @{
    Volume = $Volume
    VhdxPath = $ResolvedVhdx
    BuildManifest = $BuildManifest
    OutputDirectory = $OutputDirectory
}
& (Join-Path $PSScriptRoot 'run-start-010-real-fs-ux.ps1') @RunnerParameters
if ($LASTEXITCODE -ne 0) {
    throw "UX-ACTION-GATE-001 live runner failed with exit code $LASTEXITCODE"
}

$SourceReport = Get-ChildItem -LiteralPath $OutputRoot -Filter 'start-010-u-*.json' -File |
    Where-Object { $_.LastWriteTimeUtc -ge $Started } |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
if (-not $SourceReport) {
    throw 'UX-ACTION-GATE-001 live runner did not produce a source report'
}
$Report = Get-Content -LiteralPath $SourceReport.FullName -Raw |
    ConvertFrom-Json -ErrorAction Stop
$Hash = (Get-FileHash -LiteralPath $SourceReport.FullName -Algorithm SHA256).Hash
$Verdict = New-UxActionGateVerdict -Report $Report -SourceReportSha256 $Hash
$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$VerdictPath = Join-Path $OutputRoot "ux-action-gate-001-$Timestamp.json"
$Verdict | ConvertTo-Json -Depth 10 |
    Set-Content -LiteralPath $VerdictPath -Encoding utf8
Write-Host "UX-ACTION-GATE-001 verdict: $VerdictPath"
Write-Host "status=$($Verdict.status); commit=$($Verdict.source_commit)"
if ($Verdict.status -ne 'PASS') {
    throw 'UX-ACTION-GATE-001 acceptance failed'
}
