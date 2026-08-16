Set-StrictMode -Version Latest

function Assert-UxActionProperties {
    param(
        [Parameter(Mandatory)][object]$Value,
        [Parameter(Mandatory)][string[]]$Properties,
        [Parameter(Mandatory)][string]$Context
    )

    foreach ($Property in $Properties) {
        if ($null -eq $Value -or $Property -notin $Value.PSObject.Properties.Name) {
            throw [IO.InvalidDataException]::new(
                "UX-ACTION-GATE-001 $Context is missing $Property"
            )
        }
    }
}

function Test-UxActionGateReport {
    [CmdletBinding()]
    param([Parameter(Mandatory)][object]$Report)

    Assert-UxActionProperties $Report @(
        'schema_version', 'gate', 'git_commit', 'dirty_tree', 'binary_provenance',
        'provider', 'fixture', 'long_name', 'rename', 'move', 'delete',
        'offline_volume', 'acceptance'
    ) 'source report'
    if ([int]$Report.schema_version -ne 1 -or $Report.gate -ne 'START-010-U') {
        throw [IO.InvalidDataException]::new(
            'UX-ACTION-GATE-001 source report contract mismatch'
        )
    }
    if ([string]$Report.git_commit -notmatch '^[0-9a-f]{40}$') {
        throw [IO.InvalidDataException]::new('UX-ACTION-GATE-001 source commit is invalid')
    }
    if ($Report.dirty_tree -isnot [bool] -or [bool]$Report.dirty_tree) {
        throw [IO.InvalidDataException]::new(
            'UX-ACTION-GATE-001 requires clean-source evidence'
        )
    }

    Assert-UxActionProperties $Report.binary_provenance @(
        'verified', 'git_commit'
    ) 'provenance'
    Assert-UxActionProperties $Report.fixture @(
        'files_created', 'files_removed', 'cleanup_complete', 'search_cleanup_complete'
    ) 'fixture'
    Assert-UxActionProperties $Report.long_name @(
        'search_found', 'horizontal_overflow', 'ellipsis_triggered', 'layout_pass'
    ) 'long-name evidence'
    Assert-UxActionProperties $Report.rename @(
        'identity_preserved', 'old_path_rejected', 'new_path_resolved', 'pass'
    ) 'rename evidence'
    Assert-UxActionProperties $Report.move @(
        'identity_preserved', 'old_path_rejected', 'action_uses_current_path', 'pass'
    ) 'move evidence'
    Assert-UxActionProperties $Report.delete @(
        'stale_open_prevented', 'controlled_error', 'absent_from_search',
        'eventual_visibility_ms', 'pass'
    ) 'delete evidence'
    Assert-UxActionProperties $Report.offline_volume @(
        'exercised', 'detected', 'reported_as_offline', 'stale_action_prevented',
        'not_reported_as_deleted', 'reattached_same_volume', 'same_logical_object', 'pass'
    ) 'offline-volume evidence'
    Assert-UxActionProperties $Report.acceptance @('pass') 'acceptance'

    $Checks = [ordered]@{
        clean_verified_binary_provenance = (
            [bool]$Report.binary_provenance.verified -and
            [string]$Report.binary_provenance.git_commit -eq [string]$Report.git_commit
        )
        live_windows_provider = [string]$Report.provider -eq 'windows_fs_usn_journal'
        controlled_fixture_cleaned = (
            [int]$Report.fixture.files_created -eq 7 -and
            [int]$Report.fixture.files_removed -eq 7 -and
            [bool]$Report.fixture.cleanup_complete -and
            [bool]$Report.fixture.search_cleanup_complete
        )
        long_name_layout_safe = (
            [bool]$Report.long_name.search_found -and
            -not [bool]$Report.long_name.horizontal_overflow -and
            [bool]$Report.long_name.ellipsis_triggered -and
            [bool]$Report.long_name.layout_pass
        )
        rename_uses_current_identity = (
            [bool]$Report.rename.identity_preserved -and
            [bool]$Report.rename.old_path_rejected -and
            [bool]$Report.rename.new_path_resolved -and
            [bool]$Report.rename.pass
        )
        move_uses_current_identity = (
            [bool]$Report.move.identity_preserved -and
            [bool]$Report.move.old_path_rejected -and
            [bool]$Report.move.action_uses_current_path -and
            [bool]$Report.move.pass
        )
        deletion_fails_closed = (
            [bool]$Report.delete.stale_open_prevented -and
            [string]$Report.delete.controlled_error -eq 'not_found' -and
            [bool]$Report.delete.absent_from_search -and
            [double]$Report.delete.eventual_visibility_ms -le 5000 -and
            [bool]$Report.delete.pass
        )
        offline_volume_fails_closed_and_recovers = (
            [bool]$Report.offline_volume.exercised -and
            [bool]$Report.offline_volume.detected -and
            [bool]$Report.offline_volume.reported_as_offline -and
            [bool]$Report.offline_volume.stale_action_prevented -and
            [bool]$Report.offline_volume.not_reported_as_deleted -and
            [bool]$Report.offline_volume.reattached_same_volume -and
            [bool]$Report.offline_volume.same_logical_object -and
            [bool]$Report.offline_volume.pass
        )
        source_acceptance_passed = [bool]$Report.acceptance.pass
    }
    $Pass = @($Checks.Values | Where-Object { -not $_ }).Count -eq 0
    return [pscustomobject]@{ pass = $Pass; checks = $Checks }
}

function New-UxActionGateVerdict {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][object]$Report,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-f]{64}$')]
        [string]$SourceReportSha256
    )

    $Validation = Test-UxActionGateReport -Report $Report
    return [ordered]@{
        schema_version = 1
        gate = 'UX-ACTION-GATE-001'
        source_commit = [string]$Report.git_commit
        source_report_sha256 = $SourceReportSha256.ToLowerInvariant()
        status = if ($Validation.pass) { 'PASS' } else { 'FAIL' }
        release_eligible = [bool]$Validation.pass
        fixture = [ordered]@{
            files_created = [int]$Report.fixture.files_created
            files_removed = [int]$Report.fixture.files_removed
            cleanup_complete = [bool]$Report.fixture.cleanup_complete
            search_cleanup_complete = [bool]$Report.fixture.search_cleanup_complete
        }
        live_actions = [ordered]@{
            long_name_layout_pass = [bool]$Report.long_name.layout_pass
            rename_pass = [bool]$Report.rename.pass
            move_pass = [bool]$Report.move.pass
            delete_pass = [bool]$Report.delete.pass
            delete_visibility_ms = [double]$Report.delete.eventual_visibility_ms
            offline_volume_pass = [bool]$Report.offline_volume.pass
        }
        checks = $Validation.checks
    }
}

Export-ModuleMember -Function Test-UxActionGateReport, New-UxActionGateVerdict
