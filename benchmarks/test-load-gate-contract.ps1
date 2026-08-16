[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'LoadGateContract.psm1') -Force

function New-PassingReport {
    $Commit = '0123456789abcdef0123456789abcdef01234567'
    return [pscustomobject]@{
        schema_version = 1
        gate = 'START-010-L'
        git_commit = $Commit
        dirty_tree = $false
        binary_provenance = [pscustomobject]@{
            verified = $true
            git_commit = $Commit
        }
        duration_seconds = 900
        volume = 'X:\secret-volume'
        driver = [pscustomobject]@{
            agent_restart_count = 2
            drain_timeout_seconds = 120
        }
        workload = [pscustomobject]@{
            filesystem_operations = 100000
            provider_events = 80000
            operations_per_second = 111.1
        }
        cli_search = [pscustomobject]@{
            samples = 800
            errors = 0
            p95_ms = 20.0
            p99_ms = 32.0
        }
        content_search = [pscustomobject]@{
            enabled = $true
            samples = 800
            errors = 0
            p95_ms = 45.0
            p99_ms = 70.0
        }
        desktop_search = [pscustomobject]@{
            non_cancellation_failures = 0
            stale_results_rendered = 0
        }
        hotkey = [pscustomobject]@{ p95_ms = 8.0 }
        projection = [pscustomobject]@{
            maximum_backlog_mutations = 400
            final_backlog_mutations = 0
            final_drain_milliseconds = 2500
            restarts = @(
                [pscustomobject]@{
                    pass = $true
                    backlog_before_recovery = 200
                    backlog_after_recovery = 0
                    recovery_headroom = 1.85
                },
                [pscustomobject]@{
                    pass = $true
                    backlog_before_recovery = 240
                    backlog_after_recovery = 0
                    recovery_headroom = 2.2
                }
            )
        }
        convergence = [pscustomobject]@{
            converged = $true
            payloads_match = $true
            duplicate_documents = 0
            desired_documents = 1000000
            indexed_documents = 1000000
        }
        storage = [pscustomobject]@{
            maximum_graph_bytes = 10737418240
            maximum_content_index_bytes = 10737418240
            final = [pscustomobject]@{
                graph_bytes = 1000000000
                catalog_bytes = 400000000
                content_bytes = 600000000
            }
            maximum_observed = [pscustomobject]@{
                graph_bytes = 1000000000
                catalog_bytes = 400000000
                content_bytes = 600000000
            }
            graph_growth_bytes = 10000000
            catalog_growth_bytes = 5000000
            content_growth_bytes = 7000000
        }
        resources = [pscustomobject]@{ unavailable_samples = 0 }
        supervisor = [pscustomobject]@{
            unsafe_idle_boost_transitions = 0
            churn_deadline_exceeded = $false
        }
        ui = [pscustomobject]@{ stalls_over_100_ms = 0 }
        fixture = [pscustomobject]@{ cleanup_complete = $true }
        acceptance = [pscustomobject]@{ pass = $true }
    }
}

$Report = New-PassingReport
$Verdict = New-LoadGateVerdict -Report $Report -SourceReportSha256 ('a' * 64)
if ($Verdict.status -ne 'PASS' -or -not $Verdict.release_eligible) {
    throw 'Passing LOAD-GATE-001 fixture was rejected'
}
if ($Verdict.recovery.headroom_target_met) {
    throw 'Recovery headroom KPI was incorrectly promoted to a hard pass'
}
$Json = $Verdict | ConvertTo-Json -Depth 12
foreach ($Forbidden in @('secret-volume', 'X:\\', '"volume"', '"path"', '"query"')) {
    if ($Json -match [regex]::Escape($Forbidden)) {
        throw "Redacted verdict exposed forbidden value $Forbidden"
    }
}

$SlowContent = (New-PassingReport | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
$SlowContent.content_search.p95_ms = 151
$SlowVerdict = New-LoadGateVerdict -Report $SlowContent -SourceReportSha256 ('b' * 64)
if ($SlowVerdict.status -ne 'FAIL' -or $SlowVerdict.release_eligible) {
    throw 'Content SLA regression passed LOAD-GATE-001'
}

$NoRecovery = (New-PassingReport | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
$NoRecovery.projection.restarts[0].backlog_before_recovery = 0
$RecoveryVerdict = New-LoadGateVerdict -Report $NoRecovery -SourceReportSha256 ('c' * 64)
if ($RecoveryVerdict.status -ne 'FAIL') {
    throw 'A restart without demonstrated backlog recovery passed LOAD-GATE-001'
}

$Plan = & (Join-Path $PSScriptRoot 'invoke-load-gate.ps1') -PlanOnly |
    ConvertFrom-Json
if (-not $Plan.plan_only -or $Plan.release_eligible -or
    $Plan.phases.Count -ne 10 -or -not $Plan.content_search) {
    throw 'LOAD-GATE-001 plan contract is invalid'
}

Write-Host 'LOAD-GATE-001 contract tests: PASS'
