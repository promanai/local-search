[CmdletBinding()]
param(
    [string]$Volume = 'L:\',
    [ValidateRange(900, 1800)]
    [int]$DurationSeconds = 900,
    [ValidateRange(2, 8)]
    [int]$AgentRestartCount = 2,
    [ValidateRange(1, 30)]
    [int]$RestartOutageSeconds = 5,
    [ValidateRange(10, 600)]
    [int]$DrainTimeoutSeconds = 120,
    [string]$BuildManifest = '.lab/start-010-load-bundle.json',
    [string]$OutputDirectory = 'reports/load/load-gate-001',
    [switch]$ConfirmDisposableVolume,
    [switch]$PlanOnly
)

$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Import-Module (Join-Path $PSScriptRoot 'LoadGateContract.psm1') -Force

$Plan = [ordered]@{
    schema_version = 1
    gate = 'LOAD-GATE-001'
    plan_only = [bool]$PlanOnly
    release_eligible = $false
    duration_seconds = $DurationSeconds
    agent_restarts = $AgentRestartCount
    restart_outage_seconds = $RestartOutageSeconds
    content_search = $true
    graph_limit_bytes = 10737418240
    content_index_limit_bytes = 10737418240
    phases = @(
        'clean-provenance-preflight',
        'controlled-volume-initialization',
        'catalog-and-content-load',
        'interactive-search-and-hotkey-supervision',
        'agent-restart-recovery',
        'bounded-backlog-drain',
        'graph-catalog-convergence',
        'storage-bound-verification',
        'fixture-cleanup',
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
    throw 'LOAD-GATE-001 requires an elevated PowerShell for live USN evidence'
}
if (-not $ConfirmDisposableVolume) {
    throw 'LOAD-GATE-001 requires -ConfirmDisposableVolume'
}
if ($Volume -notmatch '^[D-Zd-z]:[\\/]$') {
    throw 'Volume must be an explicit non-system drive root such as L:\'
}
if (git -C $Repository status --porcelain) {
    throw 'LOAD-GATE-001 requires a clean repository'
}

$OutputRoot = [IO.Path]::GetFullPath((Join-Path $Repository $OutputDirectory))
$RepositoryPrefix = $Repository.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if (-not $OutputRoot.StartsWith($RepositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'LOAD-GATE-001 output must stay inside the repository'
}
$Started = [DateTime]::UtcNow
$RunnerParameters = @{
    Volume = $Volume
    DurationSeconds = $DurationSeconds
    AgentRestartCount = $AgentRestartCount
    RestartOutageSeconds = $RestartOutageSeconds
    DrainTimeoutSeconds = $DrainTimeoutSeconds
    EnableContent = $true
    MaximumGraphBytes = 10737418240
    MaximumContentIndexBytes = 10737418240
    BuildManifest = $BuildManifest
    OutputDirectory = $OutputDirectory
}
& (Join-Path $PSScriptRoot 'run-start-010-load.ps1') @RunnerParameters
if ($LASTEXITCODE -ne 0) {
    throw "LOAD-GATE-001 live runner failed with exit code $LASTEXITCODE"
}

$SourceReport = Get-ChildItem -LiteralPath $OutputRoot -Filter 'start-010-l-*.json' -File |
    Where-Object { $_.LastWriteTimeUtc -ge $Started } |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
if (-not $SourceReport) {
    throw 'LOAD-GATE-001 live runner did not produce a source report'
}
$Report = Get-Content -LiteralPath $SourceReport.FullName -Raw |
    ConvertFrom-Json -ErrorAction Stop
$Hash = (Get-FileHash -LiteralPath $SourceReport.FullName -Algorithm SHA256).Hash
$Verdict = New-LoadGateVerdict -Report $Report -SourceReportSha256 $Hash
$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$VerdictPath = Join-Path $OutputRoot "load-gate-001-$Timestamp.json"
$Verdict | ConvertTo-Json -Depth 10 |
    Set-Content -LiteralPath $VerdictPath -Encoding utf8
Write-Host "LOAD-GATE-001 verdict: $VerdictPath"
Write-Host "status=$($Verdict.status); commit=$($Verdict.source_commit)"
if ($Verdict.status -ne 'PASS') {
    throw 'LOAD-GATE-001 acceptance failed'
}
