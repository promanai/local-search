[CmdletBinding()]
param(
    [int]$Samples = 40,
    [string]$OutputDirectory = 'reports/ux/start-010',
    [string]$Query = '',
    [switch]$RequireResultLayout,
    [switch]$RequireLongContent
)

$ErrorActionPreference = 'Stop'
if ($Samples -lt 20 -or $Samples -gt 200) {
    throw 'Samples must be between 20 and 200'
}
if ($Query -and $Query -notmatch '^[\p{L}\p{Nd} _.-]{1,64}$') {
    throw 'Query must contain only letters, digits, spaces, underscore, dot, or hyphen'
}
if ($RequireResultLayout -and -not $Query) {
    throw 'RequireResultLayout requires a non-empty Query'
}
if ($RequireLongContent -and -not $RequireResultLayout) {
    throw 'RequireLongContent requires RequireResultLayout'
}

$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Executable = Join-Path $Repository 'target/release/localsearch-desktop.exe'
$ResolvedOutput = Join-Path $Repository $OutputDirectory
$LogDirectory = Join-Path $env:TEMP 'localsearch-start010'
$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$LogPath = Join-Path $LogDirectory "desktop-$Timestamp.stderr.log"
$StdoutPath = Join-Path $LogDirectory "desktop-$Timestamp.stdout.log"
$SecondLogPath = Join-Path $LogDirectory "desktop-second-$Timestamp.stderr.log"
$SecondStdoutPath = Join-Path $LogDirectory "desktop-second-$Timestamp.stdout.log"

New-Item -ItemType Directory -Path $LogDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $ResolvedOutput -Force | Out-Null

if (-not (Test-Path -LiteralPath $Executable)) {
    throw "Release executable not found: $Executable"
}

$Existing = Get-CimInstance Win32_Process -Filter "Name = 'localsearch-desktop.exe'" |
    Where-Object { $_.ExecutablePath -eq $Executable }
if ($Existing) {
    throw 'Close the existing LocalSearch Desktop process before running the UX benchmark'
}

$PreviousHotkey = $env:LOCALSEARCH_HOTKEY
$PreviousEvidence = $env:LOCALSEARCH_UX_EVIDENCE
$env:LOCALSEARCH_HOTKEY = 'Ctrl+Alt+Shift+F12'
$env:LOCALSEARCH_UX_EVIDENCE = '1'

try {
    $Process = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden `
        -RedirectStandardError $LogPath -RedirectStandardOutput $StdoutPath
    Start-Sleep -Milliseconds 1500
    if ($Process.HasExited) {
        throw "Desktop exited before benchmark; see $LogPath"
    }

    $Keyboard = New-Object -ComObject WScript.Shell
    for ($Index = 0; $Index -lt $Samples; $Index++) {
        $Keyboard.SendKeys('^%+{F12}')
        Start-Sleep -Milliseconds 120
    }
    Start-Sleep -Milliseconds 750

    if ($Query) {
        $Keyboard.SendKeys('^a')
        $Keyboard.SendKeys($Query)
        Start-Sleep -Milliseconds 1750
    }

    $Values = @(Get-Content -LiteralPath $LogPath |
        ForEach-Object {
            if ($_ -match '^START010_FOCUS_MICROS=(\d+)$') {
                [long]$Matches[1]
            }
        })
    if ($Values.Count -ne $Samples) {
        throw "Expected $Samples acknowledged hotkeys, observed $($Values.Count); see $LogPath"
    }

    $SecondProcess = Start-Process -FilePath $Executable -PassThru -WindowStyle Hidden `
        -RedirectStandardError $SecondLogPath -RedirectStandardOutput $SecondStdoutPath
    $SingleInstancePass = $SecondProcess.WaitForExit(5000)
    if (-not $SingleInstancePass) {
        Stop-Process -Id $SecondProcess.Id -Force
        throw 'Second desktop process did not exit through the single-instance boundary'
    }
}
finally {
    if ($Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force
        $Process.WaitForExit()
    }
    $env:LOCALSEARCH_HOTKEY = $PreviousHotkey
    $env:LOCALSEARCH_UX_EVIDENCE = $PreviousEvidence
}

$Sorted = @($Values | Sort-Object)
function Get-NearestRank([long[]]$Data, [int]$Percentile) {
    $Index = [Math]::Ceiling($Data.Count * ($Percentile / 100.0)) - 1
    return $Data[[Math]::Max(0, [Math]::Min($Data.Count - 1, $Index))]
}

$P50 = Get-NearestRank $Sorted 50
$P95 = Get-NearestRank $Sorted 95
$P99 = Get-NearestRank $Sorted 99
$Dpi = (Get-ItemProperty -LiteralPath 'HKCU:\Control Panel\Desktop\WindowMetrics' `
    -Name AppliedDPI -ErrorAction SilentlyContinue).AppliedDPI
if (-not $Dpi) { $Dpi = 96 }
$ScalePercent = [Math]::Round(($Dpi / 96.0) * 100)
$LayoutSamples = @(Get-Content -LiteralPath $LogPath |
    ForEach-Object {
        if ($_ -match '^START010_LAYOUT_JSON=(.+)$') {
            $Matches[1] | ConvertFrom-Json
        }
    })
$Layout = @($LayoutSamples | Where-Object { $_.reason -eq 'results' } | Select-Object -Last 1)
$LayoutPresent = $Layout.Count -eq 1
$LayoutPass = $LayoutPresent -and [bool]$Layout[0].pass
$ResultLayoutExercised = $LayoutPresent -and [int]$Layout[0].result_count -gt 0
$LongContentExercised = $LayoutPresent -and [bool]$Layout[0].content_overflow_exercised
$Commit = (git -C $Repository rev-parse HEAD).Trim()
$Dirty = [bool](git -C $Repository status --porcelain)
$LayoutRequiredPass = (-not $RequireResultLayout) -or ($LayoutPass -and $ResultLayoutExercised)
$LongContentRequiredPass = (-not $RequireLongContent) -or $LongContentExercised
$Pass = ($P50 -lt 50000 -and $P95 -lt 100000 -and $LayoutRequiredPass -and $LongContentRequiredPass)

$Report = [ordered]@{
    schema_version = 1
    gate = 'START-010-UX'
    timestamp_utc = $Timestamp
    git_commit = $Commit
    dirty_tree = $Dirty
    executable = $Executable
    hotkey = 'Ctrl+Alt+Shift+F12'
    samples = $Samples
    display_scale_percent = $ScalePercent
    single_instance_pass = $SingleInstancePass
    query = $Query
    focus_latency_micros = [ordered]@{
        p50 = $P50
        p95 = $P95
        p99 = $P99
        maximum = $Sorted[-1]
    }
    layout = if ($LayoutPresent) { $Layout[0] } else { $null }
    acceptance = [ordered]@{
        p50_below_50_ms = ($P50 -lt 50000)
        p95_below_100_ms = ($P95 -lt 100000)
        layout_required = [bool]$RequireResultLayout
        long_content_required = [bool]$RequireLongContent
        layout_sample_present = $LayoutPresent
        layout_pass = $LayoutPass
        result_layout_exercised = $ResultLayoutExercised
        long_content_exercised = $LongContentExercised
        pass = ($Pass -and $SingleInstancePass)
    }
}
$JsonPath = Join-Path $ResolvedOutput "start-010-ux-$Timestamp.json"
$Report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $JsonPath -Encoding utf8

Write-Host "START-010 UX report: $JsonPath"
Write-Host ("hotkey focus p50/p95/p99: {0:N3} / {1:N3} / {2:N3} ms" -f `
    ($P50 / 1000.0), ($P95 / 1000.0), ($P99 / 1000.0))
if ($LayoutPresent) {
    Write-Host ("layout: {0}x{1}, DPR {2}, results {3}, pass={4}" -f `
        $Layout[0].viewport_width, $Layout[0].viewport_height, `
        $Layout[0].device_pixel_ratio, $Layout[0].result_count, $Layout[0].pass)
}
if (-not $Pass) {
    throw 'START-010 UX acceptance failed'
}
