[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'UxActionGateContract.psm1') -Force

function New-PassingReport {
    $Commit = '0123456789abcdef0123456789abcdef01234567'
    return [pscustomobject]@{
        schema_version = 1
        gate = 'START-010-U'
        git_commit = $Commit
        dirty_tree = $false
        binary_provenance = [pscustomobject]@{ verified = $true; git_commit = $Commit }
        volume = 'X:\secret-volume'
        vhdx_path = 'C:\secret\fixture.vhdx'
        provider = 'windows_fs_usn_journal'
        fixture = [pscustomobject]@{
            files_created = 7
            files_removed = 7
            cleanup_complete = $true
            search_cleanup_complete = $true
        }
        long_name = [pscustomobject]@{
            search_found = $true
            document_id = 'secret-document-id'
            horizontal_overflow = $false
            ellipsis_triggered = $true
            layout_pass = $true
        }
        rename = [pscustomobject]@{
            identity_preserved = $true
            old_path_rejected = $true
            new_path_resolved = $true
            pass = $true
        }
        move = [pscustomobject]@{
            identity_preserved = $true
            old_path_rejected = $true
            action_uses_current_path = $true
            pass = $true
        }
        delete = [pscustomobject]@{
            stale_open_prevented = $true
            controlled_error = 'not_found'
            absent_from_search = $true
            eventual_visibility_ms = 25.0
            pass = $true
        }
        offline_volume = [pscustomobject]@{
            exercised = $true
            detected = $true
            reported_as_offline = $true
            stale_action_prevented = $true
            not_reported_as_deleted = $true
            reattached_same_volume = $true
            same_logical_object = $true
            pass = $true
        }
        acceptance = [pscustomobject]@{ pass = $true }
    }
}

$Report = New-PassingReport
$Verdict = New-UxActionGateVerdict -Report $Report -SourceReportSha256 ('a' * 64)
if ($Verdict.status -ne 'PASS' -or -not $Verdict.release_eligible) {
    throw 'Passing UX-ACTION-GATE-001 fixture was rejected'
}
$Json = $Verdict | ConvertTo-Json -Depth 12
foreach ($Forbidden in @(
        'secret-volume', 'fixture.vhdx', 'secret-document-id',
        '"volume"', '"path"', '"document_id"', '"query"'
    )) {
    if ($Json -match [regex]::Escape($Forbidden)) {
        throw "Redacted UX action verdict exposed forbidden value $Forbidden"
    }
}

$NoOffline = (New-PassingReport | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
$NoOffline.offline_volume.exercised = $false
$NoOffline.offline_volume.pass = $false
if ((New-UxActionGateVerdict $NoOffline ('b' * 64)).status -ne 'FAIL') {
    throw 'Missing offline-volume evidence passed UX-ACTION-GATE-001'
}

$StaleDelete = (New-PassingReport | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
$StaleDelete.delete.stale_open_prevented = $false
if ((New-UxActionGateVerdict $StaleDelete ('c' * 64)).status -ne 'FAIL') {
    throw 'Stale delete action passed UX-ACTION-GATE-001'
}

$WrongCommit = (New-PassingReport | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
$WrongCommit.binary_provenance.git_commit = 'fedcba9876543210fedcba9876543210fedcba98'
if ((New-UxActionGateVerdict $WrongCommit ('d' * 64)).status -ne 'FAIL') {
    throw 'Mismatched binary provenance passed UX-ACTION-GATE-001'
}

$MissingNested = (New-PassingReport | ConvertTo-Json -Depth 12 | ConvertFrom-Json)
$MissingNested.move.PSObject.Properties.Remove('pass')
$Rejected = $false
try {
    New-UxActionGateVerdict $MissingNested ('e' * 64) | Out-Null
}
catch [IO.InvalidDataException] {
    $Rejected = $true
}
if (-not $Rejected) {
    throw 'Incomplete nested UX action evidence was not rejected fail-closed'
}

$Plan = & (Join-Path $PSScriptRoot 'invoke-ux-action-gate.ps1') -PlanOnly |
    ConvertFrom-Json
if (-not $Plan.plan_only -or $Plan.release_eligible -or
    $Plan.phases.Count -ne 10 -or -not $Plan.requires_repository_vhdx) {
    throw 'UX-ACTION-GATE-001 plan contract is invalid'
}

Write-Host 'UX-ACTION-GATE-001 contract tests: PASS'
