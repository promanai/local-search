Set-StrictMode -Version Latest

function Test-LoadGateReport {
    [CmdletBinding()]
    param([Parameter(Mandatory)][object]$Report)

    $Required = @(
        'schema_version', 'gate', 'git_commit', 'dirty_tree', 'binary_provenance',
        'duration_seconds', 'driver', 'workload', 'cli_search', 'content_search',
        'desktop_search', 'hotkey', 'projection', 'convergence', 'storage',
        'resources', 'supervisor', 'ui', 'fixture', 'acceptance'
    )
    foreach ($Property in $Required) {
        if ($Property -notin $Report.PSObject.Properties.Name) {
            throw [IO.InvalidDataException]::new(
                "LOAD-GATE-001 source report is missing $Property"
            )
        }
    }
    if ([int]$Report.schema_version -ne 1 -or $Report.gate -ne 'START-010-L') {
        throw [IO.InvalidDataException]::new(
            'LOAD-GATE-001 source report contract mismatch'
        )
    }
    if ([string]$Report.git_commit -notmatch '^[0-9a-f]{40}$') {
        throw [IO.InvalidDataException]::new('LOAD-GATE-001 source commit is invalid')
    }
    if ($Report.dirty_tree -isnot [bool] -or [bool]$Report.dirty_tree) {
        throw [IO.InvalidDataException]::new(
            'LOAD-GATE-001 requires clean-source evidence'
        )
    }

    $Restarts = @($Report.projection.restarts)
    $Checks = [ordered]@{
        duration_at_least_15_minutes = ([int]$Report.duration_seconds -ge 900)
        clean_verified_binary_provenance = (
            [bool]$Report.binary_provenance.verified -and
            [string]$Report.binary_provenance.git_commit -eq
                [string]$Report.git_commit
        )
        filesystem_churn_recorded = (
            [int64]$Report.workload.filesystem_operations -gt 0 -and
            [int64]$Report.workload.provider_events -gt 0
        )
        catalog_search_sla = (
            [int]$Report.cli_search.samples -ge 100 -and
            [int]$Report.cli_search.errors -eq 0 -and
            [double]$Report.cli_search.p95_ms -le 75
        )
        content_search_sla = (
            [bool]$Report.content_search.enabled -and
            [int]$Report.content_search.samples -ge 100 -and
            [int]$Report.content_search.errors -eq 0 -and
            [double]$Report.content_search.p95_ms -le 150 -and
            [double]$Report.content_search.p99_ms -le 300
        )
        desktop_and_hotkey_sla = (
            [int]$Report.desktop_search.non_cancellation_failures -eq 0 -and
            [int]$Report.desktop_search.stale_results_rendered -eq 0 -and
            [double]$Report.hotkey.p95_ms -lt 100 -and
            [int]$Report.ui.stalls_over_100_ms -eq 0
        )
        two_restart_recoveries = (
            [int]$Report.driver.agent_restart_count -ge 2 -and
            $Restarts.Count -eq [int]$Report.driver.agent_restart_count -and
            @($Restarts | Where-Object {
                -not [bool]$_.pass -or
                [int64]$_.backlog_before_recovery -le 0 -or
                [int64]$_.backlog_after_recovery -ne 0
            }).Count -eq 0
        )
        backlog_bounded_and_drained = (
            [int64]$Report.projection.maximum_backlog_mutations -le 10000 -and
            [int64]$Report.projection.final_backlog_mutations -eq 0 -and
            [int64]$Report.projection.final_drain_milliseconds -le
                ([int64]$Report.driver.drain_timeout_seconds * 1000)
        )
        exact_catalog_convergence = (
            [bool]$Report.convergence.converged -and
            [bool]$Report.convergence.payloads_match -and
            [int64]$Report.convergence.duplicate_documents -eq 0 -and
            [int64]$Report.convergence.desired_documents -eq
                [int64]$Report.convergence.indexed_documents
        )
        storage_within_declared_limits = (
            [int64]$Report.storage.maximum_graph_bytes -le 10737418240 -and
            [int64]$Report.storage.maximum_content_index_bytes -le 10737418240 -and
            [int64]$Report.storage.maximum_observed.graph_bytes -le
                [int64]$Report.storage.maximum_graph_bytes -and
            [int64]$Report.storage.maximum_observed.content_bytes -le
                [int64]$Report.storage.maximum_content_index_bytes
        )
        telemetry_and_governor_safe = (
            [int]$Report.resources.unavailable_samples -eq 0 -and
            [int]$Report.supervisor.unsafe_idle_boost_transitions -eq 0 -and
            -not [bool]$Report.supervisor.churn_deadline_exceeded
        )
        cleanup_complete = [bool]$Report.fixture.cleanup_complete
        source_acceptance_passed = [bool]$Report.acceptance.pass
    }
    $Pass = @($Checks.Values | Where-Object { -not $_ }).Count -eq 0
    return [pscustomobject]@{ pass = $Pass; checks = $Checks }
}

function New-LoadGateVerdict {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][object]$Report,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$SourceReportSha256
    )

    $Validation = Test-LoadGateReport -Report $Report
    $Restarts = @($Report.projection.restarts)
    $Headroom = @($Restarts | ForEach-Object {
        if ($null -ne $_.recovery_headroom) { [double]$_.recovery_headroom }
    })
    $MinimumHeadroom = if ($Headroom.Count -gt 0) {
        [double](($Headroom | Measure-Object -Minimum).Minimum)
    } else { $null }
    return [ordered]@{
        schema_version = 1
        gate = 'LOAD-GATE-001'
        source_commit = [string]$Report.git_commit
        source_report_sha256 = $SourceReportSha256.ToLowerInvariant()
        status = if ($Validation.pass) { 'PASS' } else { 'FAIL' }
        release_eligible = [bool]$Validation.pass
        workload = [ordered]@{
            duration_seconds = [int]$Report.duration_seconds
            filesystem_operations = [int64]$Report.workload.filesystem_operations
            provider_events = [int64]$Report.workload.provider_events
            operations_per_second = [double]$Report.workload.operations_per_second
        }
        interactive = [ordered]@{
            catalog_samples = [int]$Report.cli_search.samples
            catalog_p95_ms = [double]$Report.cli_search.p95_ms
            catalog_p99_ms = [double]$Report.cli_search.p99_ms
            content_samples = [int]$Report.content_search.samples
            content_p95_ms = [double]$Report.content_search.p95_ms
            content_p99_ms = [double]$Report.content_search.p99_ms
            hotkey_p95_ms = [double]$Report.hotkey.p95_ms
            ui_stalls_over_100_ms = [int]$Report.ui.stalls_over_100_ms
        }
        recovery = [ordered]@{
            restart_count = $Restarts.Count
            maximum_backlog_mutations =
                [int64]$Report.projection.maximum_backlog_mutations
            final_backlog_mutations =
                [int64]$Report.projection.final_backlog_mutations
            final_drain_milliseconds =
                [int64]$Report.projection.final_drain_milliseconds
            minimum_headroom_kpi = $MinimumHeadroom
            headroom_target_met = (
                $null -ne $MinimumHeadroom -and $MinimumHeadroom -ge 2.0
            )
        }
        correctness = [ordered]@{
            desired_documents = [int64]$Report.convergence.desired_documents
            indexed_documents = [int64]$Report.convergence.indexed_documents
            duplicate_documents = [int64]$Report.convergence.duplicate_documents
            payloads_match = [bool]$Report.convergence.payloads_match
        }
        storage = [ordered]@{
            graph_bytes = [int64]$Report.storage.final.graph_bytes
            catalog_bytes = [int64]$Report.storage.final.catalog_bytes
            content_index_bytes = [int64]$Report.storage.final.content_bytes
            maximum_graph_bytes = [int64]$Report.storage.maximum_observed.graph_bytes
            maximum_catalog_bytes = [int64]$Report.storage.maximum_observed.catalog_bytes
            maximum_content_index_bytes =
                [int64]$Report.storage.maximum_observed.content_bytes
            graph_growth_bytes = [int64]$Report.storage.graph_growth_bytes
            catalog_growth_bytes = [int64]$Report.storage.catalog_growth_bytes
            content_growth_bytes = [int64]$Report.storage.content_growth_bytes
        }
        checks = $Validation.checks
    }
}

Export-ModuleMember -Function Test-LoadGateReport, New-LoadGateVerdict
