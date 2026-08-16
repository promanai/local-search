[CmdletBinding()]
param(
    [string]$Volume = 'L:\',
    [string]$OutputDirectory = 'reports/ux/start-010-u',
    [string]$VhdxPath = '',
    [string]$BuildManifest = '.lab/start-010-load-bundle.json'
)

$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Import-Module (Join-Path $PSScriptRoot 'Start010LoadProvenance.psm1') -Force
$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$RunRoot = Join-Path $Repository ".lab\start-010-u-$Timestamp"
$StatePath = Join-Path $RunRoot 'fixture-state.json'
$FixtureExe = Join-Path $Repository 'target/release/localsearch-ux-fixture.exe'
$AgentExe = Join-Path $Repository 'target/release/localsearch-agent.exe'
$CliExe = Join-Path $Repository 'target/release/localsearch-cli.exe'
$DesktopExe = Join-Path $Repository 'target/release/localsearch-desktop.exe'
$ActionProbeExe = Join-Path $Repository 'target/release/examples/ux_action_probe.exe'
$Output = Join-Path $Repository $OutputDirectory
$AgentStdout = Join-Path $RunRoot 'agent.stdout.log'
$AgentStderr = Join-Path $RunRoot 'agent.stderr.log'
$Pipe = "\\.\pipe\LocalSearch\Agent\v1\ux-$PID-$Timestamp"
$VolumeDetached = $false
$ExerciseOffline = -not [string]::IsNullOrWhiteSpace($VhdxPath)

$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if (-not $Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'START-010-U requires an elevated PowerShell because the live USN provider is mandatory'
}
if ((git -C $Repository status --porcelain)) {
    throw 'START-010-U requires a clean repository before fixture creation'
}
$EvidenceCommit = (git -C $Repository rev-parse HEAD).Trim()
$BuildProvenance = Test-Start010LoadBundle -Repository $Repository -ManifestPath $BuildManifest -ExpectedCommit $EvidenceCommit
if ($Volume -notmatch '^[D-Zd-z]:[\\/]$') {
    throw 'Volume must be an explicit non-system drive root such as L:\'
}
$DriveLetter = $Volume.Substring(0, 1).ToUpperInvariant()
if ($ExerciseOffline) {
    $VhdxPath = (Resolve-Path -LiteralPath $VhdxPath).Path
    $LabRoot = [IO.Path]::GetFullPath((Join-Path $Repository '.lab'))
    $Candidate = [IO.Path]::GetFullPath($VhdxPath)
    if (-not $Candidate.StartsWith($LabRoot + [IO.Path]::DirectorySeparatorChar,
            [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetExtension($Candidate) -ne '.vhdx') {
        throw 'Offline evidence accepts only an explicit .vhdx below the repository .lab directory'
    }
}
foreach ($Executable in @($FixtureExe, $AgentExe, $CliExe, $DesktopExe, $ActionProbeExe)) {
    if (-not (Test-Path -LiteralPath $Executable)) {
        throw "Required release executable is missing: $Executable"
    }
}
$ExistingDesktop = Get-CimInstance Win32_Process -Filter "Name = 'localsearch-desktop.exe'" |
    Where-Object { $_.ExecutablePath -eq $DesktopExe }
if ($ExistingDesktop) {
    throw 'Close the resident LocalSearch Desktop before START-010-U'
}

function Invoke-JsonProcess([string]$Executable, [string[]]$Arguments) {
    $Raw = (& $Executable @Arguments | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "$Executable failed with exit code $LASTEXITCODE"
    }
    return $Raw | ConvertFrom-Json
}

function Invoke-Search([string]$Query) {
    return Invoke-JsonProcess $CliExe @('--pipe', $Pipe, 'search', $Query)
}

function Find-ExactHit($Response, [string]$Name) {
    $Hit = @($Response.result.value.hits | Where-Object { $_.name -eq $Name })
    if ($Hit.Count -ne 1) {
        throw "Expected exactly one Agent hit named $Name; observed $($Hit.Count)"
    }
    return $Hit[0]
}

function Resolve-Action([string]$DocumentId) {
    return Invoke-JsonProcess $ActionProbeExe @('--pipe', $Pipe, '--document-id', $DocumentId)
}

function Resolve-RawItem([string]$DocumentId) {
    return Invoke-JsonProcess $ActionProbeExe @(
        '--pipe', $Pipe, '--document-id', $DocumentId, '--raw'
    )
}

function Invoke-VhdxDiskpart([ValidateSet('attach', 'detach')][string]$Action) {
    $DiskpartFile = Join-Path $RunRoot "diskpart-$Action.txt"
    $Lines = @(
        "select vdisk file=`"$VhdxPath`"",
        "$Action vdisk"
    )
    if ($Action -eq 'attach') {
        $Lines += @('select partition 1', "assign letter=$DriveLetter noerr")
    }
    $Lines += 'exit'
    $Lines | Set-Content -LiteralPath $DiskpartFile -Encoding ascii
    & diskpart.exe /s $DiskpartFile | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "DiskPart $Action failed with exit code $LASTEXITCODE"
    }
}

function Wait-VolumeState([bool]$Present) {
    for ($Attempt = 0; $Attempt -lt 100; $Attempt++) {
        $Observed = [bool](Get-Volume -DriveLetter $DriveLetter -ErrorAction SilentlyContinue)
        if ($Observed -eq $Present) { return }
        Start-Sleep -Milliseconds 50
    }
    throw "Volume $DriveLetter`: did not reach present=$Present within five seconds"
}

function Normalize-Path([string]$Path) {
    return $Path.Replace('\', '/').TrimEnd('/')
}

New-Item -ItemType Directory -Path $Output -Force | Out-Null
$Agent = $null
$CleanupCompleted = $false
$PreviousAgentPipe = $env:LOCALSEARCH_AGENT_PIPE
try {
    $Init = Invoke-JsonProcess $FixtureExe @('init', '--volume', $Volume, '--run-root', $RunRoot)
    $State = Get-Content -LiteralPath $StatePath -Raw | ConvertFrom-Json

    $Agent = Start-Process -FilePath $AgentExe -ArgumentList @(
        '--graph', $State.graph_path,
        '--index', $State.index_root,
        '--pipe', $Pipe
    ) -WindowStyle Hidden -PassThru -RedirectStandardOutput $AgentStdout -RedirectStandardError $AgentStderr
    $Ready = $false
    for ($Attempt = 0; $Attempt -lt 100; $Attempt++) {
        if ($Agent.HasExited) {
            throw "Agent exited before readiness; see $AgentStderr"
        }
        if ((Test-Path -LiteralPath $AgentStderr) -and
            (Get-Content -LiteralPath $AgentStderr -Raw) -match 'LocalSearch Agent ready') {
            $Ready = $true
            break
        }
        Start-Sleep -Milliseconds 50
    }
    if (-not $Ready) { throw 'Agent did not become ready within five seconds' }

    $EnglishLongName = $State.long_names[0].name
    $LongHit = Find-ExactHit (Invoke-Search 'architecture-document-name') $EnglishLongName
    $RenameHit = Find-ExactHit (Invoke-Search 'project-original.md') $State.rename.name
    $MoveHit = Find-ExactHit (Invoke-Search 'project-move.md') $State.moved.name
    $DeleteHit = Find-ExactHit (Invoke-Search 'project-delete.md') $State.deleted.name

    $env:LOCALSEARCH_AGENT_PIPE = $Pipe
    $LayoutDirectory = Join-Path $RunRoot 'layout'
    & powershell.exe -NoProfile -ExecutionPolicy Bypass `
        -File (Join-Path $Repository 'benchmarks\run-start-010-ux.ps1') `
        -Samples 40 `
        -OutputDirectory $LayoutDirectory `
        -Query 'architecture-document-name' `
        -RequireResultLayout `
        -RequireLongContent
    if ($LASTEXITCODE -ne 0) { throw "long-name layout runner failed: $LASTEXITCODE" }
    $LayoutReportPath = Get-ChildItem -LiteralPath $LayoutDirectory -Filter '*.json' |
        Sort-Object LastWriteTime | Select-Object -Last 1 -ExpandProperty FullName
    $Layout = Get-Content -LiteralPath $LayoutReportPath -Raw | ConvertFrom-Json

    $OfflineEvidence = [ordered]@{
        exercised = $false
        detected = $false
        reported_as_offline = $false
        stale_action_prevented = $false
        not_reported_as_deleted = $false
        reattached_same_volume = $false
        same_logical_object = $false
        pass = (-not $ExerciseOffline)
        reason = if ($ExerciseOffline) { $null } else { 'supply -VhdxPath to exercise physical detach/reattach' }
    }
    if ($ExerciseOffline) {
        Invoke-VhdxDiskpart 'detach'
        $VolumeDetached = $true
        Wait-VolumeState $false
        $OfflineTransition = Invoke-JsonProcess $FixtureExe @('offline', '--state', $StatePath)
        $OfflineRaw = Resolve-RawItem $LongHit.document_id
        $OfflineAction = Resolve-Action $LongHit.document_id
        $OfflineEvidence.exercised = $true
        $OfflineEvidence.detected = $OfflineTransition.operation -eq 'offline'
        $OfflineEvidence.reported_as_offline = $OfflineRaw.status -eq 'resolved' -and
            $OfflineRaw.item.availability -eq 'offline'
        $OfflineEvidence.stale_action_prevented = $OfflineAction.status -eq 'rejected' -and
            $OfflineAction.error.code -eq 'item_unavailable'
        $OfflineEvidence.not_reported_as_deleted = $OfflineRaw.status -eq 'resolved'

        Invoke-VhdxDiskpart 'attach'
        $VolumeDetached = $false
        Wait-VolumeState $true
        $OnlineTransition = Invoke-JsonProcess $FixtureExe @('online', '--state', $StatePath)
        $OnlineRaw = Resolve-RawItem $LongHit.document_id
        $OnlineAction = Resolve-Action $LongHit.document_id
        $OfflineEvidence.reattached_same_volume = $OnlineTransition.operation -eq 'online' -and
            $OnlineRaw.status -eq 'resolved' -and $OnlineRaw.item.availability -eq 'online'
        $OfflineEvidence.same_logical_object = $OnlineRaw.item.document_id -eq $LongHit.document_id -and
            $OnlineAction.status -eq 'resolved'
        $OfflineEvidence.pass = $OfflineEvidence.detected -and
            $OfflineEvidence.reported_as_offline -and
            $OfflineEvidence.stale_action_prevented -and
            $OfflineEvidence.not_reported_as_deleted -and
            $OfflineEvidence.reattached_same_volume -and
            $OfflineEvidence.same_logical_object
    }

    $RenameMutation = Invoke-JsonProcess $FixtureExe @('rename', '--state', $StatePath)
    $RenameAction = Resolve-Action $RenameHit.document_id
    $RenameCurrent = $RenameAction.item.resolved_path
    $RenamePass = $RenameAction.status -eq 'resolved' -and
        $RenameAction.item.document_id -eq $RenameHit.document_id -and
        (Normalize-Path $RenameCurrent) -eq (Normalize-Path $State.rename_target) -and
        -not (Test-Path -LiteralPath $RenameHit.resolved_path) -and
        (Test-Path -LiteralPath $RenameCurrent)

    $MoveMutation = Invoke-JsonProcess $FixtureExe @('move', '--state', $StatePath)
    $MoveAction = Resolve-Action $MoveHit.document_id
    $MoveCurrent = $MoveAction.item.resolved_path
    $MovePass = $MoveAction.status -eq 'resolved' -and
        $MoveAction.item.document_id -eq $MoveHit.document_id -and
        (Normalize-Path $MoveCurrent) -eq (Normalize-Path $State.move_target) -and
        -not (Test-Path -LiteralPath $MoveHit.resolved_path) -and
        (Test-Path -LiteralPath $MoveCurrent)

    $DeleteStarted = Get-Date
    $DeleteMutation = Invoke-JsonProcess $FixtureExe @('delete', '--state', $StatePath)
    $DeleteAction = Resolve-Action $DeleteHit.document_id
    $DeleteAbsent = $false
    for ($Attempt = 0; $Attempt -lt 100; $Attempt++) {
        $Search = Invoke-Search 'project-delete.md'
        if (@($Search.result.value.hits | Where-Object { $_.document_id -eq $DeleteHit.document_id }).Count -eq 0) {
            $DeleteAbsent = $true
            break
        }
        Start-Sleep -Milliseconds 20
    }
    $DeleteVisibilityMs = ((Get-Date) - $DeleteStarted).TotalMilliseconds
    $DeletePass = $DeleteAction.status -eq 'rejected' -and
        $DeleteAction.error.code -eq 'not_found' -and $DeleteAbsent

    $Cleanup = Invoke-JsonProcess $FixtureExe @('cleanup', '--state', $StatePath)
    $CleanupCompleted = [bool]$Cleanup.cleanup_complete
    $CleanupSearch = Invoke-Search 'architecture-document-name'
    $CleanupAbsent = @($CleanupSearch.result.value.hits |
        Where-Object { $_.document_id -eq $LongHit.document_id }).Count -eq 0

    $Commit = (git -C $Repository rev-parse HEAD).Trim()
    $Dirty = [bool](git -C $Repository status --porcelain)
    $Pass = $Layout.acceptance.pass -and
        $Layout.acceptance.long_content_exercised -and
        $RenamePass -and $MovePass -and $DeletePass -and
        $Cleanup.cleanup_complete -and $CleanupAbsent -and
        $OfflineEvidence.pass -and -not $Dirty
    $Report = [ordered]@{
        schema_version = 1
        gate = 'START-010-U'
        run_id = $Timestamp
        timestamp_utc = $Timestamp
        git_commit = $Commit
        dirty_tree = $Dirty
        binary_provenance = $BuildProvenance
        volume = $Volume
        provider = 'windows_fs_usn_journal'
        fixture = [ordered]@{
            files_created = 7
            files_removed = 7
            cleanup_complete = [bool]$Cleanup.cleanup_complete
            search_cleanup_complete = $CleanupAbsent
        }
        long_name = [ordered]@{
            search_found = $true
            document_id = $LongHit.document_id
            horizontal_overflow = [bool]$Layout.layout.results_horizontal_overflow
            ellipsis_triggered = [bool]$Layout.layout.content_overflow_exercised
            layout_pass = [bool]$Layout.layout.pass
        }
        rename = [ordered]@{
            identity_preserved = ($RenameAction.item.document_id -eq $RenameHit.document_id)
            old_path_rejected = -not (Test-Path -LiteralPath $RenameHit.resolved_path)
            new_path_resolved = ((Normalize-Path $RenameCurrent) -eq (Normalize-Path $State.rename_target))
            pass = $RenamePass
        }
        move = [ordered]@{
            identity_preserved = ($MoveAction.item.document_id -eq $MoveHit.document_id)
            old_path_rejected = -not (Test-Path -LiteralPath $MoveHit.resolved_path)
            action_uses_current_path = ((Normalize-Path $MoveCurrent) -eq (Normalize-Path $State.move_target))
            pass = $MovePass
        }
        delete = [ordered]@{
            stale_open_prevented = ($DeleteAction.status -eq 'rejected')
            controlled_error = $DeleteAction.error.code
            absent_from_search = $DeleteAbsent
            eventual_visibility_ms = $DeleteVisibilityMs
            pass = $DeletePass
        }
        offline_volume = $OfflineEvidence
        acceptance = [ordered]@{ pass = $Pass }
    }
    $ReportPath = Join-Path $Output "start-010-u-$Timestamp.json"
    $Report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $ReportPath -Encoding utf8
    Write-Host "START-010-U report: $ReportPath"
    Write-Host "long-name=$($Layout.layout.pass); rename=$RenamePass; move=$MovePass; delete=$DeletePass; cleanup=$($Cleanup.cleanup_complete)"
    if (-not $Pass) { throw 'START-010-U acceptance failed' }
}
finally {
    $env:LOCALSEARCH_AGENT_PIPE = $PreviousAgentPipe
    if ($VolumeDetached) {
        Write-Warning 'Reattaching the explicitly selected fixture VHDX after an interrupted run'
        Invoke-VhdxDiskpart 'attach'
        Wait-VolumeState $true
        $VolumeDetached = $false
    }
    if (-not $CleanupCompleted -and (Test-Path -LiteralPath $StatePath) -and
        (Test-Path -LiteralPath $Volume)) {
        try {
            $EmergencyCleanup = Invoke-JsonProcess $FixtureExe @('cleanup', '--state', $StatePath)
            $CleanupCompleted = [bool]$EmergencyCleanup.cleanup_complete
        }
        catch {
            Write-Warning "Bounded fixture cleanup failed: $($_.Exception.Message)"
        }
    }
    if ($Agent -and -not $Agent.HasExited) {
        Stop-Process -Id $Agent.Id -Force
        $Agent.WaitForExit()
    }
}
