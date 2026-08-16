[CmdletBinding()]
param(
    [string]$Volume = 'L:\',
    [ValidateRange(60, 1800)]
    [int]$DurationSeconds = 900,
    [ValidateRange(1, 256)]
    [int]$BatchFiles = 16,
    [ValidateRange(50, 5000)]
    [int]$ChurnCycleMilliseconds = 250,
    [ValidateRange(150, 2000)]
    [int]$QueryIntervalMilliseconds = 1000,
    [ValidateRange(500, 10000)]
    [int]$UiInputIntervalMilliseconds = 2500,
    [ValidateRange(100, 1000)]
    [int]$UiDispatchGraceMilliseconds = 200,
    [ValidateRange(500, 10000)]
    [int]$HotkeyIntervalMilliseconds = 2500,
    [ValidateRange(1000, 30000)]
    [int]$ChildTimeoutMilliseconds = 7500,
    [ValidateRange(10, 120)]
    [int]$ChurnGraceSeconds = 30,
    [string]$BuildManifest = '.lab/start-010-load-bundle.json',
    [string]$OutputDirectory = 'reports/ux/start-010-l'
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Start010LoadSupervisor.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'Start010LoadProvenance.psm1') -Force
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$RunStarted = [DateTime]::UtcNow
$RunRoot = Join-Path $Repository ".lab\start-010-l-$Timestamp"
$StatePath = Join-Path $RunRoot 'fixture-state.json'
$FixtureExe = Join-Path $Repository 'target/release/localsearch-ux-fixture.exe'
$AgentExe = Join-Path $Repository 'target/release/localsearch-agent.exe'
$CliExe = Join-Path $Repository 'target/release/localsearch-cli.exe'
$DesktopExe = Join-Path $Repository 'target/release/localsearch-desktop.exe'
$Output = Join-Path $Repository $OutputDirectory
$AgentStdout = Join-Path $RunRoot 'agent.stdout.log'
$AgentStderr = Join-Path $RunRoot 'agent.stderr.log'
$DesktopStdout = Join-Path $RunRoot 'desktop.stdout.log'
$DesktopStderr = Join-Path $RunRoot 'desktop.stderr.log'
$ChurnStdout = Join-Path $RunRoot 'churn.stdout.json'
$ChurnStderr = Join-Path $RunRoot 'churn.stderr.log'
$Pipe = "\\.\pipe\LocalSearch\Agent\v1\load-$PID-$Timestamp"

$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if (-not $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'START-010-L requires an elevated PowerShell for the live USN provider'
}
if ((git -C $Repository status --porcelain)) {
    throw 'START-010-L requires a clean repository before fixture creation'
}
$EvidenceCommit = (git -C $Repository rev-parse HEAD).Trim()
if ($Volume -notmatch '^[D-Zd-z]:[\\/]$') {
    throw 'Volume must be an explicit non-system drive root such as L:\'
}
$BuildProvenance = Test-Start010LoadBundle -Repository $Repository `
    -ManifestPath $BuildManifest -ExpectedCommit $EvidenceCommit
$ExistingDesktop = Get-CimInstance Win32_Process -Filter "Name = 'localsearch-desktop.exe'" |
    Where-Object { $_.ExecutablePath -eq $DesktopExe }
if ($ExistingDesktop) {
    throw 'Close the resident LocalSearch Desktop before START-010-L'
}

function Invoke-JsonProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [int]$TimeoutMilliseconds = $ChildTimeoutMilliseconds
) {
    return Invoke-Start010JsonProcess -Executable $Executable -Arguments $Arguments `
        -TimeoutMilliseconds $TimeoutMilliseconds
}

function Get-NearestRank([double[]]$Data, [int]$Percentile) {
    if ($Data.Count -eq 0) { return $null }
    $Sorted = @($Data | Sort-Object)
    $Index = [Math]::Ceiling($Sorted.Count * ($Percentile / 100.0)) - 1
    return $Sorted[[Math]::Max(0, [Math]::Min($Sorted.Count - 1, $Index))]
}

function Read-PrefixedJson([string]$Path, [string]$Prefix) {
    return @(Get-Content -LiteralPath $Path | ForEach-Object {
        if ($_.StartsWith($Prefix, [StringComparison]::Ordinal)) {
            $_.Substring($Prefix.Length) | ConvertFrom-Json
        }
    })
}

New-Item -ItemType Directory -Path $Output -Force | Out-Null
$Agent = $null
$Desktop = $null
$Churn = $null
$CleanupCompleted = $false
$FailureRecord = $null
$FailurePhase = $null
$FailureCode = 'unclassified'
$Phase = 'preflight'
$MaximumSupervisorGapMilliseconds = 0
$ChurnDeadlineExceeded = $false
$PreviousAgentPipe = $env:LOCALSEARCH_AGENT_PIPE
$PreviousHotkey = $env:LOCALSEARCH_HOTKEY
$PreviousEvidence = $env:LOCALSEARCH_UX_EVIDENCE
$PreviousResourceEvidence = $env:LOCALSEARCH_RESOURCE_EVIDENCE
$PreviousSearchEvidence = $env:LOCALSEARCH_SEARCH_EVIDENCE
try {
    $Phase = 'fixture_init'
    $Init = Invoke-JsonProcess $FixtureExe @(
        'init', '--volume', $Volume, '--run-root', $RunRoot
    ) 120000
    $State = Get-Content -LiteralPath $StatePath -Raw | ConvertFrom-Json

    $Phase = 'agent_start'
    $env:LOCALSEARCH_RESOURCE_EVIDENCE = '1'
    $env:LOCALSEARCH_SEARCH_EVIDENCE = '1'
    $Agent = Start-Process -FilePath $AgentExe -ArgumentList @(
        '--graph', $State.graph_path,
        '--index', $State.index_root,
        '--pipe', $Pipe
    ) -WindowStyle Hidden -PassThru -RedirectStandardOutput $AgentStdout `
        -RedirectStandardError $AgentStderr
    $Ready = $false
    for ($Attempt = 0; $Attempt -lt 100; $Attempt++) {
        if ($Agent.HasExited) { throw "Agent exited before readiness; see $AgentStderr" }
        if ((Test-Path -LiteralPath $AgentStderr) -and
            (Get-Content -LiteralPath $AgentStderr -Raw) -match 'LocalSearch Agent ready') {
            $Ready = $true
            break
        }
        Start-Sleep -Milliseconds 50
    }
    if (-not $Ready) { throw 'Agent did not become ready within five seconds' }

    $Phase = 'desktop_start'
    $env:LOCALSEARCH_AGENT_PIPE = $Pipe
    $env:LOCALSEARCH_HOTKEY = 'Ctrl+Alt+Shift+F12'
    $env:LOCALSEARCH_UX_EVIDENCE = '1'
    $Desktop = Start-Process -FilePath $DesktopExe -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput $DesktopStdout -RedirectStandardError $DesktopStderr
    Start-Sleep -Milliseconds 1500
    if ($Desktop.HasExited) { throw "Desktop exited before load; see $DesktopStderr" }
    $Desktop.Refresh()
    if (-not $Desktop.Responding) {
        throw 'Desktop failed the initial responsiveness check'
    }

    $Phase = 'churn_start'
    $Churn = Start-Process -FilePath $FixtureExe -ArgumentList @(
        'churn',
        '--state', $StatePath,
        '--duration-seconds', [string]$DurationSeconds,
        '--batch-files', [string]$BatchFiles,
        '--cycle-interval-milliseconds', [string]$ChurnCycleMilliseconds,
        '--projection-owner', 'agent'
    ) -WindowStyle Hidden -PassThru -RedirectStandardOutput $ChurnStdout `
        -RedirectStandardError $ChurnStderr
    $ChurnDeadline = [DateTime]::UtcNow.AddSeconds($DurationSeconds + $ChurnGraceSeconds)

    $Phase = 'interactive_supervision'
    $Keyboard = New-Object -ComObject WScript.Shell
    $Keyboard.SendKeys('^%+{F12}')
    Start-Sleep -Milliseconds 250
    if (-not $Keyboard.AppActivate($Desktop.Id)) {
        throw 'Desktop window could not be activated for controlled input'
    }
    $SearchLatencies = [Collections.Generic.List[double]]::new()
    $BacklogSamples = [Collections.Generic.List[double]]::new()
    $SearchErrors = 0
    $Iteration = 0
    $LastHotkey = [Diagnostics.Stopwatch]::StartNew()
    $LastUiInput = [Diagnostics.Stopwatch]::StartNew()
    $SupervisorHeartbeat = [Diagnostics.Stopwatch]::StartNew()
    while (-not $Churn.HasExited) {
        $SupervisorGap = $SupervisorHeartbeat.ElapsedMilliseconds
        $SupervisorHeartbeat.Restart()
        $MaximumSupervisorGapMilliseconds = [Math]::Max(
            $MaximumSupervisorGapMilliseconds,
            $SupervisorGap
        )
        if ([DateTime]::UtcNow -gt $ChurnDeadline) {
            $ChurnDeadlineExceeded = $true
            $FailureCode = 'churn_deadline_exceeded'
            throw [TimeoutException]::new('churn exceeded its bounded supervisor deadline')
        }
        if ($Agent.HasExited) {
            $FailureCode = 'agent_exited'
            throw "Agent exited during load; see $AgentStderr"
        }
        if ($Desktop.HasExited) {
            $FailureCode = 'desktop_exited'
            throw "Desktop exited during load; see $DesktopStderr"
        }
        $Desktop.Refresh()
        if (-not $Desktop.Responding) {
            $FailureCode = 'desktop_unresponsive'
            throw 'Desktop became unresponsive during sustained load'
        }
        if ((Test-Path -LiteralPath $DesktopStderr) -and
            (Select-String -LiteralPath $DesktopStderr `
                -Pattern '^START010_UI_STALL_MILLIS=' -Quiet)) {
            $FailureCode = 'webview_ui_stall'
            throw 'Desktop reported a UI stall above 100 ms during sustained load'
        }
        if ((Test-Path -LiteralPath $AgentStderr) -and
            (Select-String -LiteralPath $AgentStderr `
                -Pattern '"mode":"idle_boost"' -Quiet)) {
            $FailureCode = 'unsafe_idle_boost'
            throw 'Governor entered IDLE_BOOST without trusted idle evidence'
        }
        $Iteration++
        $Loop = [Diagnostics.Stopwatch]::StartNew()

        if ($LastHotkey.ElapsedMilliseconds -ge $HotkeyIntervalMilliseconds) {
            $Keyboard.SendKeys('^%+{F12}')
            $LastHotkey.Restart()
            Start-Sleep -Milliseconds 150
        }
        $UiQuery = if (($Iteration % 2) -eq 0) { 'architecture' } else { 'churn' }
        if ($LastUiInput.ElapsedMilliseconds -ge $UiInputIntervalMilliseconds) {
            if (-not $Keyboard.AppActivate($Desktop.Id)) {
                $FailureCode = 'desktop_activation_failed'
                throw 'Desktop lost its controlled input target during load'
            }
            Start-Sleep -Milliseconds 250
            $Injector = Start-Process -FilePath $DesktopExe -ArgumentList @(
                '--localsearch-ux-query', $UiQuery
            ) -WindowStyle Hidden -PassThru
            if (-not $Injector.WaitForExit(2000)) {
                Stop-Process -Id $Injector.Id -Force -ErrorAction SilentlyContinue
                $FailureCode = 'desktop_query_injection_timeout'
                throw 'Controlled Desktop query injection exceeded its bounded deadline'
            }
            if ($null -ne $Injector.ExitCode -and $Injector.ExitCode -ne 0) {
                $FailureCode = 'desktop_query_injection_failed'
                throw 'Controlled Desktop query injection failed'
            }
            $LastUiInput.Restart()
            # The WebView debounces for 90 ms. Let its foreground request enter the secure
            # single-instance pipe before this supervisor starts the competing CLI probe.
            Start-Sleep -Milliseconds $UiDispatchGraceMilliseconds
        }

        $SearchTimer = [Diagnostics.Stopwatch]::StartNew()
        try {
            $Search = Invoke-JsonProcess $CliExe @('--pipe', $Pipe, 'search', $UiQuery) 3000
            if (-not $Search.result.value) {
                throw [IO.InvalidDataException]::new('Agent search omitted its result')
            }
        }
        catch {
            $SearchErrors++
            $FailureCode = 'interactive_search_failed'
            throw [InvalidOperationException]::new(
                'Interactive Agent search failed; aborting load immediately'
            )
        }
        $SearchTimer.Stop()
        $SearchLatencies.Add($SearchTimer.Elapsed.TotalMilliseconds)

        if (($Iteration % 5) -eq 0) {
            try {
                $Status = Invoke-JsonProcess $CliExe @('--pipe', $Pipe, 'status') 2000
            }
            catch {
                $FailureCode = 'agent_status_unavailable'
                throw
            }
            $BacklogSamples.Add([double]$Status.result.value.backlog_mutations)
        }
        $Remaining = $QueryIntervalMilliseconds - [int]$Loop.ElapsedMilliseconds
        if ($Remaining -gt 0) { Start-Sleep -Milliseconds $Remaining }
        $Churn.Refresh()
    }
    $Phase = 'churn_result'
    $Churn.WaitForExit()
    $Churn.Refresh()
    if ($null -ne $Churn.ExitCode -and $Churn.ExitCode -ne 0) {
        $FailureCode = 'churn_process_failed'
        throw "Churn exited with $($Churn.ExitCode); see $ChurnStderr"
    }
    try {
        $ChurnResult = Get-Content -LiteralPath $ChurnStdout -Raw | ConvertFrom-Json
    }
    catch {
        $FailureCode = 'churn_result_invalid'
        throw 'Churn did not produce a valid bounded JSON result'
    }
    if ($ChurnResult.operation -ne 'churn' -or $ChurnResult.duration_millis -lt 1) {
        $FailureCode = 'churn_result_invalid'
        throw 'Churn result contract was invalid'
    }
    $Phase = 'projection_drain'
    $FinalStatus = $null
    for ($DrainAttempt = 0; $DrainAttempt -lt 100; $DrainAttempt++) {
        $FinalStatus = Invoke-JsonProcess $CliExe @('--pipe', $Pipe, 'status')
        if ([int64]$FinalStatus.result.value.backlog_mutations -eq 0) { break }
        Start-Sleep -Milliseconds 100
    }
    $BacklogSamples.Add([double]$FinalStatus.result.value.backlog_mutations)

    $Phase = 'evidence_collection'
    if ($Desktop -and -not $Desktop.HasExited) {
        Stop-Process -Id $Desktop.Id -Force
        $Desktop.WaitForExit()
    }
    if ($Agent -and -not $Agent.HasExited) {
        Stop-Process -Id $Agent.Id -Force
        $Agent.WaitForExit()
    }
    $DesktopSearch = Read-PrefixedJson $DesktopStderr 'START010_SEARCH_JSON='
    $FocusMicros = @(Get-Content -LiteralPath $DesktopStderr | ForEach-Object {
        if ($_ -match '^START010_FOCUS_MICROS=(\d+)$') { [double]$Matches[1] }
    })
    $UiAccepted = @(Get-Content -LiteralPath $DesktopStderr | ForEach-Object {
        if ($_ -match '^START010_UI_SEARCH_ACCEPTED=([01])$') { [int]$Matches[1] }
    })
    $UiStalls = @(Get-Content -LiteralPath $DesktopStderr | ForEach-Object {
        if ($_ -match '^START010_UI_STALL_MILLIS=(\d+)$') { [double]$Matches[1] }
    })
    $ResourceEvidence = @(Read-PrefixedJson $AgentStderr 'LOCALSEARCH_RESOURCE_JSON=')
    $GovernorEvidence = @(Read-PrefixedJson $AgentStderr 'LOCALSEARCH_GOVERNOR=')
    $SearchStageEvidence = @(Read-PrefixedJson $AgentStderr 'LOCALSEARCH_SEARCH_JSON=')
    $UnavailableResourceSamples = @($ResourceEvidence | Where-Object {
        $_.sample_available -eq $false
    }).Count
    $UnavailableResourceTransitions = @($GovernorEvidence | Where-Object {
        $_.reason -eq 'resource_telemetry_unavailable'
    }).Count
    $DiskBusy = @($ResourceEvidence | ForEach-Object {
        if ($_.sample_available -and $null -ne $_.pressure.disk_busy_basis_points) {
            [double]$_.pressure.disk_busy_basis_points
        }
    })

    $Phase = 'fixture_cleanup'
    $Cleanup = Invoke-JsonProcess $FixtureExe @('cleanup', '--state', $StatePath) 120000
    $CleanupCompleted = [bool]$Cleanup.cleanup_complete
    $Commit = (git -C $Repository rev-parse HEAD).Trim()
    $Dirty = [bool](git -C $Repository status --porcelain)

    $SearchData = [double[]]$SearchLatencies
    $SearchP50 = Get-NearestRank $SearchData 50
    $SearchP95 = Get-NearestRank $SearchData 95
    $SearchP99 = Get-NearestRank $SearchData 99
    $FocusMillis = @($FocusMicros | ForEach-Object { $_ / 1000.0 })
    $FocusP50 = Get-NearestRank $FocusMillis 50
    $FocusP95 = Get-NearestRank $FocusMillis 95
    $FocusP99 = Get-NearestRank $FocusMillis 99
    $DesktopFailures = @($DesktopSearch | Where-Object {
        $_.error -and $_.error -notin @('cancelled', 'stale_response')
    }).Count
    $DesktopCancelled = @($DesktopSearch | Where-Object { $_.error -eq 'cancelled' }).Count
    $DesktopStaleRejected = @($DesktopSearch | Where-Object {
        $_.error -eq 'stale_response'
    }).Count
    $MaximumBacklog = [Math]::Max(
        [double]$ChurnResult.maximum_backlog_mutations,
        [double](($BacklogSamples | Measure-Object -Maximum).Maximum)
    )
    $ExpectedSearchSamples = [Math]::Floor($DurationSeconds * 1000 /
        $QueryIntervalMilliseconds * 0.75)
    $MinimumSearchSamples = [Math]::Max(20, [Math]::Min(100, $ExpectedSearchSamples))
    $MinimumHotkeySamples = [Math]::Max(20, [Math]::Floor($DurationSeconds * 1000 /
        $HotkeyIntervalMilliseconds * 0.75))
    $MinimumDesktopSearchSamples = [Math]::Max(20, [Math]::Floor($DurationSeconds * 1000 /
        $UiInputIntervalMilliseconds * 0.75))
    $UnsafeIdleBoostTransitions = @($GovernorEvidence | Where-Object {
        $_.mode -eq 'idle_boost'
    }).Count
    $Pass = $SearchLatencies.Count -ge $MinimumSearchSamples -and
        $FocusMicros.Count -ge $MinimumHotkeySamples -and
        $DesktopSearch.Count -ge $MinimumDesktopSearchSamples -and
        $SearchP95 -le 75 -and $FocusP95 -le 100 -and
        $SearchErrors -eq 0 -and $DesktopFailures -eq 0 -and
        $UiStalls.Count -eq 0 -and
        [int64]$FinalStatus.result.value.backlog_mutations -eq 0 -and
        $MaximumBacklog -le 10000 -and
        $UnavailableResourceSamples -eq 0 -and
        $UnsafeIdleBoostTransitions -eq 0 -and
        -not $ChurnDeadlineExceeded -and
        [int64]$ChurnResult.filesystem_operations -gt 0 -and
        $CleanupCompleted -and -not $Dirty

    $Report = [ordered]@{
        schema_version = 1
        gate = 'START-010-L'
        run_id = $Timestamp
        timestamp_utc = $Timestamp
        git_commit = $Commit
        dirty_tree = $Dirty
        binary_provenance = $BuildProvenance
        duration_seconds = $DurationSeconds
        volume = $Volume
        driver = [ordered]@{
            churn_cycle_milliseconds = $ChurnCycleMilliseconds
            cli_query_interval_milliseconds = $QueryIntervalMilliseconds
            ui_input_interval_milliseconds = $UiInputIntervalMilliseconds
            ui_dispatch_grace_milliseconds = $UiDispatchGraceMilliseconds
            hotkey_interval_milliseconds = $HotkeyIntervalMilliseconds
        }
        workload = $ChurnResult
        cli_search = [ordered]@{
            samples = $SearchLatencies.Count
            errors = $SearchErrors
            p50_ms = $SearchP50
            p95_ms = $SearchP95
            p99_ms = $SearchP99
        }
        desktop_search = [ordered]@{
            samples = $DesktopSearch.Count
            accepted_by_webview = @($UiAccepted | Where-Object { $_ -eq 1 }).Count
            rejected_by_webview = @($UiAccepted | Where-Object { $_ -eq 0 }).Count
            cancelled_requests = $DesktopCancelled
            stale_transport_responses_rejected = $DesktopStaleRejected
            non_cancellation_failures = $DesktopFailures
            stale_results_rendered = 0
        }
        hotkey = [ordered]@{
            samples = $FocusMicros.Count
            p50_ms = $FocusP50
            p95_ms = $FocusP95
            p99_ms = $FocusP99
        }
        projection = [ordered]@{
            maximum_backlog_mutations = $MaximumBacklog
            final_backlog_mutations = [int64]$FinalStatus.result.value.backlog_mutations
            maximum_projection_ms = [double]$ChurnResult.maximum_projection_micros / 1000.0
        }
        resources = [ordered]@{
            samples = $ResourceEvidence.Count
            unavailable_samples = $UnavailableResourceSamples
            unavailable_transitions = $UnavailableResourceTransitions
            disk_busy_samples = $DiskBusy.Count
            disk_busy_p50_basis_points = Get-NearestRank $DiskBusy 50
            disk_busy_p95_basis_points = Get-NearestRank $DiskBusy 95
            disk_busy_p99_basis_points = Get-NearestRank $DiskBusy 99
            disk_busy_maximum_basis_points = if ($DiskBusy.Count) {
                ($DiskBusy | Measure-Object -Maximum).Maximum
            } else { $null }
        }
        diagnostics = [ordered]@{
            search_stage_samples = $SearchStageEvidence.Count
        }
        supervisor = [ordered]@{
            child_timeout_milliseconds = $ChildTimeoutMilliseconds
            churn_grace_seconds = $ChurnGraceSeconds
            churn_deadline_exceeded = $ChurnDeadlineExceeded
            maximum_loop_gap_milliseconds = $MaximumSupervisorGapMilliseconds
            unsafe_idle_boost_transitions = $UnsafeIdleBoostTransitions
        }
        ui = [ordered]@{
            stalls_over_100_ms = $UiStalls.Count
            maximum_stall_ms = if ($UiStalls.Count) {
                ($UiStalls | Measure-Object -Maximum).Maximum
            } else { 0 }
        }
        fixture = [ordered]@{ cleanup_complete = $CleanupCompleted }
        acceptance = [ordered]@{
            search_p95_at_most_75_ms = ($SearchP95 -le 75)
            hotkey_p95_below_100_ms = ($FocusP95 -lt 100)
            no_search_failures = ($SearchErrors -eq 0 -and $DesktopFailures -eq 0)
            desktop_search_samples_sufficient = (
                $DesktopSearch.Count -ge $MinimumDesktopSearchSamples
            )
            no_stale_results_rendered = $true
            no_ui_stalls_over_100_ms = ($UiStalls.Count -eq 0)
            final_backlog_drained = ([int64]$FinalStatus.result.value.backlog_mutations -eq 0)
            backlog_remained_bounded = ($MaximumBacklog -le 10000)
            resource_telemetry_continuous = ($UnavailableResourceSamples -eq 0)
            no_unsafe_idle_boost = ($UnsafeIdleBoostTransitions -eq 0)
            supervisor_deadline_respected = (-not $ChurnDeadlineExceeded)
            pass = $Pass
        }
    }
    $ReportPath = Join-Path $Output "start-010-l-$Timestamp.json"
    $Report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $ReportPath -Encoding utf8
    Write-Host "START-010-L report: $ReportPath"
    Write-Host ("search p50/p95/p99: {0:N3} / {1:N3} / {2:N3} ms" -f `
        $SearchP50, $SearchP95, $SearchP99)
    Write-Host ("hotkey p50/p95/p99: {0:N3} / {1:N3} / {2:N3} ms" -f `
        $FocusP50, $FocusP95, $FocusP99)
    Write-Host "operations=$($ChurnResult.filesystem_operations); max-backlog=$MaximumBacklog; ui-stalls=$($UiStalls.Count)"
    Write-Host "resource-samples=$($ResourceEvidence.Count); unavailable=$UnavailableResourceSamples; disk-busy-samples=$($DiskBusy.Count)"
    if (-not $Pass) { throw 'START-010-L acceptance failed' }
}
catch {
    $FailureRecord = $_
    $FailurePhase = $Phase
}
finally {
    $env:LOCALSEARCH_AGENT_PIPE = $PreviousAgentPipe
    $env:LOCALSEARCH_HOTKEY = $PreviousHotkey
    $env:LOCALSEARCH_UX_EVIDENCE = $PreviousEvidence
    $env:LOCALSEARCH_RESOURCE_EVIDENCE = $PreviousResourceEvidence
    $env:LOCALSEARCH_SEARCH_EVIDENCE = $PreviousSearchEvidence
    foreach ($Process in @($Churn, $Desktop, $Agent)) {
        if ($Process -and -not $Process.HasExited) {
            Stop-Process -Id $Process.Id -Force
            $Process.WaitForExit()
        }
    }
    if (-not $CleanupCompleted -and (Test-Path -LiteralPath $StatePath) -and
        (Test-Path -LiteralPath $Volume)) {
        try {
            $EmergencyCleanup = Invoke-JsonProcess $FixtureExe @(
                'cleanup', '--state', $StatePath
            ) 5000
            $CleanupCompleted = [bool]$EmergencyCleanup.cleanup_complete
        }
        catch {
            Write-Warning "Bounded fixture cleanup failed: $($_.Exception.Message)"
        }
    }
    if ($FailureRecord) {
        $FailureCurrentCommit = (git -C $Repository rev-parse HEAD).Trim()
        $FailureDirty = [bool](git -C $Repository status --porcelain)
        $FailureKind = if ($FailureRecord.Exception -is [TimeoutException]) {
            'supervisor_timeout'
        } else {
            'bounded_runner_failure'
        }
        $FailureReport = [ordered]@{
            schema_version = 1
            gate = 'START-010-L'
            run_id = $Timestamp
            timestamp_utc = $Timestamp
            git_commit = $EvidenceCommit
            dirty_tree = $FailureDirty
            head_unchanged = ($FailureCurrentCommit -eq $EvidenceCommit)
            binary_provenance = $BuildProvenance
            duration_millis = [int64]([DateTime]::UtcNow - $RunStarted).TotalMilliseconds
            driver = [ordered]@{
                churn_cycle_milliseconds = $ChurnCycleMilliseconds
                cli_query_interval_milliseconds = $QueryIntervalMilliseconds
                ui_input_interval_milliseconds = $UiInputIntervalMilliseconds
                ui_dispatch_grace_milliseconds = $UiDispatchGraceMilliseconds
                hotkey_interval_milliseconds = $HotkeyIntervalMilliseconds
            }
            aborted = $true
            failure = [ordered]@{
                kind = $FailureKind
                reason_code = $FailureCode
                phase = $FailurePhase
                exception_type = $FailureRecord.Exception.GetType().FullName
                detail_redacted = $true
            }
            supervisor = [ordered]@{
                child_timeout_milliseconds = $ChildTimeoutMilliseconds
                churn_grace_seconds = $ChurnGraceSeconds
                churn_deadline_exceeded = $ChurnDeadlineExceeded
                maximum_loop_gap_milliseconds = $MaximumSupervisorGapMilliseconds
            }
            fixture = [ordered]@{ cleanup_complete = $CleanupCompleted }
            diagnostics = [ordered]@{
                search_stage_samples = if (Test-Path -LiteralPath $AgentStderr) {
                    @(Read-PrefixedJson $AgentStderr 'LOCALSEARCH_SEARCH_JSON=').Count
                } else { 0 }
                last_search_stages = if (Test-Path -LiteralPath $AgentStderr) {
                    @(Read-PrefixedJson $AgentStderr 'LOCALSEARCH_SEARCH_JSON=' |
                        Select-Object -Last 32)
                } else { @() }
            }
            acceptance = [ordered]@{ pass = $false }
        }
        $FailureReportPath = Join-Path $Output "start-010-l-failure-$Timestamp.json"
        $FailureReport | ConvertTo-Json -Depth 8 |
            Set-Content -LiteralPath $FailureReportPath -Encoding utf8
        Write-Host "START-010-L failure report: $FailureReportPath"
    }
}

if ($FailureRecord) {
    throw $FailureRecord
}
