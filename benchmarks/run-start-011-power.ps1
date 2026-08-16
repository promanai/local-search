[CmdletBinding()]
param(
    [ValidateSet('Any', 'Ac', 'Battery')]
    [string]$ExpectedPowerSource = 'Any',
    [ValidateSet('Any', 'On', 'Off')]
    [string]$ExpectedEnergySaver = 'Any',
    [ValidateRange(10, 900)]
    [int]$DurationSeconds = 30,
    [ValidateRange(250, 5000)]
    [int]$IntervalMilliseconds = 1000,
    [ValidateRange(1, 10000000)]
    [int]$BacklogMutations = 50000,
    [string]$OutputDirectory = 'reports/resource/start-011-power'
)

$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$Executable = Join-Path $Repository 'target/release/start_011_power_probe.exe'
$ResolvedOutput = Join-Path $Repository $OutputDirectory

if (git -C $Repository status --porcelain) {
    throw 'START-011-P requires a clean repository so evidence has trustworthy provenance'
}

& cargo build --manifest-path (Join-Path $Repository 'Cargo.toml') --release `
    -p localsearch-agent --bin start_011_power_probe --locked
if ($LASTEXITCODE -ne 0) {
    throw "Power probe build failed with exit code $LASTEXITCODE"
}

$RawProbe = (& $Executable `
    --duration-seconds $DurationSeconds `
    --interval-milliseconds $IntervalMilliseconds `
    --backlog-mutations $BacklogMutations | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw "Power probe failed with exit code $LASTEXITCODE"
}
$Probe = $RawProbe | ConvertFrom-Json

$MinimumSamples = [Math]::Max(
    5,
    [Math]::Floor(($DurationSeconds * 1000.0 / $IntervalMilliseconds) * 0.75)
)
$RequiredCoverage = [Math]::Ceiling($Probe.samples.Count * 0.8)
$PowerSourceCoverage = switch ($ExpectedPowerSource) {
    'Ac' { [int]$Probe.ac_samples }
    'Battery' { [int]$Probe.battery_samples }
    default { $Probe.samples.Count }
}
$EnergySaverCoverage = switch ($ExpectedEnergySaver) {
    'On' { [int]$Probe.energy_saver_samples }
    'Off' { $Probe.samples.Count - [int]$Probe.energy_saver_samples }
    default { $Probe.samples.Count }
}
$PowerSourcePass = $ExpectedPowerSource -eq 'Any' -or $PowerSourceCoverage -ge $RequiredCoverage
$EnergySaverPass = $ExpectedEnergySaver -eq 'Any' -or $EnergySaverCoverage -ge $RequiredCoverage
$SamplesPass = $Probe.samples.Count -ge $MinimumSamples
$TelemetryPass = [int]$Probe.unavailable_samples -eq 0
$PolicyPass = [int]$Probe.policy_invariant_violations -eq 0
$Pass = $SamplesPass -and $TelemetryPass -and $PolicyPass -and `
    $PowerSourcePass -and $EnergySaverPass

$Commit = (git -C $Repository rev-parse HEAD).Trim()
$Dirty = [bool](git -C $Repository status --porcelain)
$Report = [ordered]@{
    schema_version = 1
    gate = 'START-011-P'
    timestamp_utc = $Timestamp
    git_commit = $Commit
    dirty_tree = $Dirty
    expected = [ordered]@{
        power_source = $ExpectedPowerSource.ToLowerInvariant()
        energy_saver = $ExpectedEnergySaver.ToLowerInvariant()
        coverage_percent = 80
    }
    probe = $Probe
    acceptance = [ordered]@{
        minimum_samples = $MinimumSamples
        actual_samples = $Probe.samples.Count
        samples_pass = $SamplesPass
        unavailable_samples = [int]$Probe.unavailable_samples
        telemetry_pass = $TelemetryPass
        policy_invariant_violations = [int]$Probe.policy_invariant_violations
        policy_pass = $PolicyPass
        required_state_samples = $RequiredCoverage
        matching_power_source_samples = $PowerSourceCoverage
        power_source_pass = $PowerSourcePass
        matching_energy_saver_samples = $EnergySaverCoverage
        energy_saver_pass = $EnergySaverPass
        pass = $Pass
    }
}

New-Item -ItemType Directory -Path $ResolvedOutput -Force | Out-Null
$JsonPath = Join-Path $ResolvedOutput "start-011-power-$Timestamp.json"
$Report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $JsonPath -Encoding utf8

Write-Host "START-011 power report: $JsonPath"
Write-Host ("samples={0}; AC={1}; battery={2}; saver={3}; unavailable={4}; policy violations={5}" -f `
    $Probe.samples.Count, $Probe.ac_samples, $Probe.battery_samples, `
    $Probe.energy_saver_samples, $Probe.unavailable_samples, $Probe.policy_invariant_violations)
if (-not $Pass) {
    throw 'START-011-P acceptance failed; inspect the machine-readable report'
}
