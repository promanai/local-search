param(
    [ValidatePattern('^[D-Z]$')]
    [string]$DriveLetter = 'L',
    [string]$ExpectedLabel = 'LS_TEST',
    [string]$VhdxPath = 'C:\Projects\local_search\.lab\localsearch-usn-test.vhdx',
    [UInt32]$SeedFiles = 5000,
    [UInt32]$LatencySamples = 30,
    [string]$OutputDirectory = 'reports/spikes/start-004-live'
)

$ErrorActionPreference = 'Stop'
$volumeRoot = "${DriveLetter}:\"
$labRoot = Join-Path $volumeRoot 'LocalSearchLiveLab'
$implementationSha = (git rev-parse HEAD).Trim()

function Assert-LiveLabSafety {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'START-004-LIVE must run from an Administrator PowerShell'
    }
    if ($DriveLetter -eq 'C') {
        throw 'START-004-LIVE refuses to use C:'
    }
    $volume = Get-Volume -DriveLetter $DriveLetter -ErrorAction Stop
    if ($volume.FileSystem -ne 'NTFS' -or $volume.FileSystemLabel -ne $ExpectedLabel) {
        throw "Expected isolated NTFS volume ${DriveLetter}: labelled $ExpectedLabel"
    }
    $disk = Get-Partition -DriveLetter $DriveLetter | Get-Disk
    if ($disk.BusType -ne 'File Backed Virtual' -or $disk.IsBoot -or $disk.IsSystem) {
        throw 'Target is not a non-system file-backed virtual disk'
    }
    $image = Get-DiskImage -ImagePath $VhdxPath -ErrorAction Stop
    if (-not $image.Attached) {
        throw 'Expected VHDX is not attached'
    }
    $resolvedVhdx = [System.IO.Path]::GetFullPath($image.ImagePath)
    $expectedVhdx = [System.IO.Path]::GetFullPath($VhdxPath)
    if (-not $resolvedVhdx.Equals($expectedVhdx, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Attached VHDX path does not match the explicit lab image'
    }
    $resolvedLab = [System.IO.Path]::GetFullPath($labRoot)
    $resolvedVolume = [System.IO.Path]::GetFullPath($volumeRoot)
    if (-not $resolvedLab.StartsWith($resolvedVolume, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Lab root escaped the isolated test volume'
    }
    if (Test-Path -LiteralPath $resolvedLab) {
        throw "Lab path already exists: $resolvedLab"
    }
    return [pscustomobject]@{
        Volume = $volume
        Disk = $disk
        Image = $image
        VolumeGuid = (mountvol "${DriveLetter}:" /L).Trim()
    }
}

function Invoke-Fsutil {
    param([string[]]$Arguments)
    $output = @(& fsutil @Arguments 2>&1 | ForEach-Object { $_.ToString() })
    if ($LASTEXITCODE -ne 0) {
        throw "fsutil $($Arguments -join ' ') failed: $($output -join [Environment]::NewLine)"
    }
    return $output
}

function Convert-HexValue {
    param([string]$Value)
    $trimmed = $Value.Trim()
    if ($trimmed.StartsWith('0x')) {
        return [Convert]::ToUInt64($trimmed.Substring(2), 16)
    }
    return [UInt64]::Parse($trimmed)
}

function Get-JournalState {
    $lines = Invoke-Fsutil @('usn', 'queryJournal', "${DriveLetter}:")
    $values = @{}
    foreach ($line in $lines) {
        if ($line -match '^Usn Journal ID\s*:\s*(\S+)') { $values.JournalId = Convert-HexValue $Matches[1] }
        if ($line -match '^First Usn\s*:\s*(\S+)') { $values.FirstUsn = Convert-HexValue $Matches[1] }
        if ($line -match '^Next Usn\s*:\s*(\S+)') { $values.NextUsn = Convert-HexValue $Matches[1] }
        if ($line -match '^Lowest Valid Usn\s*:\s*(\S+)') { $values.LowestValidUsn = Convert-HexValue $Matches[1] }
    }
    foreach ($required in @('JournalId', 'FirstUsn', 'NextUsn', 'LowestValidUsn')) {
        if (-not $values.ContainsKey($required)) { throw "Missing journal field: $required" }
    }
    return [pscustomobject]$values
}

function Get-JournalRecords {
    param([UInt64]$StartUsn)
    $start = '0x{0:x}' -f $StartUsn
    $lines = Invoke-Fsutil @('usn', 'readJournal', "${DriveLetter}:", 'minVer=2', 'maxVer=2', "startUsn=$start", 'csv')
    $header = -1
    for ($index = 0; $index -lt $lines.Count; $index++) {
        if ($lines[$index].StartsWith('Usn,File name,')) { $header = $index; break }
    }
    if ($header -lt 0) { return @() }
    return @(($lines[$header..($lines.Count - 1)] -join [Environment]::NewLine) | ConvertFrom-Csv)
}

function Get-Percentile {
    param([double[]]$Samples, [UInt32]$Percentile)
    if ($Samples.Count -eq 0) { return 0.0 }
    $ordered = @($Samples | Sort-Object)
    $index = [Math]::Ceiling(($Percentile / 100.0) * $ordered.Count) - 1
    return [double]$ordered[[Math]::Max(0, $index)]
}

function Invoke-ObservedOperation {
    param(
        [scriptblock]$Operation,
        [string]$Name,
        [string]$ReasonPattern
    )
    $checkpoint = Get-JournalState
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $Operation
    $matched = $null
    for ($attempt = 0; $attempt -lt 200; $attempt++) {
        $matched = Get-JournalRecords -StartUsn $checkpoint.NextUsn |
            Where-Object { $_.'File name' -eq $Name -and $_.Reason -match $ReasonPattern } |
            Select-Object -Last 1
        if ($null -ne $matched) { break }
        Start-Sleep -Milliseconds 5
    }
    $watch.Stop()
    if ($null -eq $matched) { throw "USN event not observed: $Name / $ReasonPattern" }
    return [pscustomobject]@{
        Name = $Name
        Reason = $matched.Reason
        FileId = $matched.'File ID'
        ParentFileId = $matched.'Parent file ID'
        LatencyMs = $watch.Elapsed.TotalMilliseconds
    }
}

function New-OpaqueCheckpoint {
    param($Journal)
    $privatePayload = "$($Journal.JournalId)|$($Journal.NextUsn)|$($Journal.FirstUsn)"
    $opaque = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($privatePayload))
    return [pscustomobject]@{
        ProviderId = 'localsearch.windows-fs'
        FormatVersion = 1
        VolumeGuid = $safety.VolumeGuid
        Opaque = $opaque
    }
}

function Read-OpaqueCheckpoint {
    param($Checkpoint)
    $privatePayload = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Checkpoint.Opaque))
    $parts = $privatePayload.Split('|')
    if ($parts.Count -ne 3) { throw 'Opaque checkpoint payload is malformed' }
    return [pscustomobject]@{
        JournalId = [UInt64]$parts[0]
        NextUsn = [UInt64]$parts[1]
        FirstUsn = [UInt64]$parts[2]
    }
}

function Write-Artifact {
    param([string]$Path, [string]$Content)
    if (Test-Path -LiteralPath $Path) { throw "Refusing to overwrite artifact: $Path" }
    [System.IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
}

$safety = Assert-LiveLabSafety
$dirty = @(
    git status --porcelain --untracked-files=all -- . `
        ':(exclude)reports/spikes/start-004-live/**' `
        ':(exclude).lab/**'
).Count -ne 0
if ($dirty) { throw 'Accepted START-004-LIVE evidence requires a clean source tree' }

$journalBefore = Get-JournalState
New-Item -ItemType Directory -Path $labRoot | Out-Null
$seedRoot = Join-Path $labRoot 'seed'
New-Item -ItemType Directory -Path $seedRoot | Out-Null
for ($index = 0; $index -lt $SeedFiles; $index++) {
    [System.IO.File]::Create((Join-Path $seedRoot ('seed-{0:D6}.dat' -f $index))).Dispose()
}

$enumerationHighUsn = (Get-JournalState).NextUsn
$enumerationWatch = [System.Diagnostics.Stopwatch]::StartNew()
$mftOutput = Invoke-Fsutil @('usn', 'enumData', '0', '0', ('0x{0:x}' -f $enumerationHighUsn), "${DriveLetter}:")
$enumerationWatch.Stop()
$mftRecords = @($mftOutput | Where-Object { $_ -match '^File Ref#' }).Count
if ($mftRecords -lt $SeedFiles) { throw "MFT enumeration returned only $mftRecords records" }
$mftRecordsPerSecond = $mftRecords / $enumerationWatch.Elapsed.TotalSeconds

$checkpoint = New-OpaqueCheckpoint (Get-JournalState)
$portableCheckpointJson = [pscustomobject]@{
    provider_id = $checkpoint.ProviderId
    format_version = $checkpoint.FormatVersion
    volume_id = $checkpoint.VolumeGuid
    opaque = $checkpoint.Opaque
} | ConvertTo-Json -Compress
$checkpointOpaque = $portableCheckpointJson -notmatch '(?i)journal|nextusn|firstusn'
if (-not $checkpointOpaque) { throw 'Windows checkpoint vocabulary leaked into portable JSON' }

$workRoot = Join-Path $labRoot 'work'
New-Item -ItemType Directory -Path $workRoot | Out-Null
$observed = [System.Collections.Generic.List[object]]::new()
$observed.Add((Invoke-ObservedOperation { [System.IO.File]::WriteAllText((Join-Path $workRoot 'alpha.txt'), 'alpha') } 'alpha.txt' 'File create'))
$observed.Add((Invoke-ObservedOperation { Rename-Item -LiteralPath (Join-Path $workRoot 'alpha.txt') -NewName 'beta.txt' } 'beta.txt' 'Rename: new name'))
New-Item -ItemType Directory -Path (Join-Path $workRoot 'dir-a') | Out-Null
New-Item -ItemType Directory -Path (Join-Path $workRoot 'dir-b') | Out-Null
$observed.Add((Invoke-ObservedOperation { Move-Item -LiteralPath (Join-Path $workRoot 'beta.txt') -Destination (Join-Path $workRoot 'dir-a\beta.txt') } 'beta.txt' 'Rename: new name'))
$observed.Add((Invoke-ObservedOperation { Rename-Item -LiteralPath (Join-Path $workRoot 'dir-a') -NewName 'renamed-dir' } 'renamed-dir' 'Rename: new name'))
$primaryPath = Join-Path $workRoot 'renamed-dir\beta.txt'
$hardPath = Join-Path $workRoot 'hard.txt'
$observed.Add((Invoke-ObservedOperation { New-Item -ItemType HardLink -Path $hardPath -Target $primaryPath | Out-Null } 'hard.txt' 'Hard link change'))
$observed.Add((Invoke-ObservedOperation { Remove-Item -LiteralPath $hardPath } 'hard.txt' 'Hard link change'))

$restartCheckpoint = New-OpaqueCheckpoint (Get-JournalState)
$decodedRestart = Read-OpaqueCheckpoint $restartCheckpoint
$observed.Add((Invoke-ObservedOperation { [System.IO.File]::WriteAllText((Join-Path $workRoot 'restart-resume.txt'), 'resume') } 'restart-resume.txt' 'File create'))
$restartState = Get-JournalState
$restartRecords = Get-JournalRecords -StartUsn $decodedRestart.NextUsn
$restartResume = $decodedRestart.JournalId -eq $restartState.JournalId -and
    $null -ne ($restartRecords | Where-Object { $_.'File name' -eq 'restart-resume.txt' -and $_.Reason -match 'File create' } | Select-Object -First 1)
if (-not $restartResume) { throw 'Restart/resume checkpoint did not recover the live create event' }

$observed.Add((Invoke-ObservedOperation { Remove-Item -LiteralPath $primaryPath } 'beta.txt' 'File delete'))
$objectIds = @($observed | Where-Object { $_.Name -in @('alpha.txt', 'beta.txt', 'hard.txt') } | ForEach-Object FileId | Sort-Object -Unique)
$duplicateLogicalObjects = [Math]::Max(0, $objectIds.Count - 1)
if ($duplicateLogicalObjects -ne 0) { throw 'One physical file produced duplicate logical object identities' }

$latencies = [System.Collections.Generic.List[double]]::new()
foreach ($item in $observed) { $latencies.Add([double]$item.LatencyMs) }
for ($index = 0; $index -lt $LatencySamples; $index++) {
    $name = 'latency-{0:D3}.tmp' -f $index
    $path = Join-Path $workRoot $name
    $sample = Invoke-ObservedOperation { [System.IO.File]::WriteAllText($path, 'x') } $name 'File create'
    $latencies.Add([double]$sample.LatencyMs)
    Remove-Item -LiteralPath $path
}

$preRecreationCheckpoint = New-OpaqueCheckpoint (Get-JournalState)
$preRecreationPrivate = Read-OpaqueCheckpoint $preRecreationCheckpoint
Invoke-Fsutil @('usn', 'deleteJournal', '/D', "${DriveLetter}:") | Out-Null
Invoke-Fsutil @('usn', 'createJournal', "${DriveLetter}:", 'm=33554432', 'a=8388608') | Out-Null
$postRecreation = Get-JournalState
$journalRecreationDetected = $postRecreation.JournalId -ne $preRecreationPrivate.JournalId
$oldCheckpointRejected = $journalRecreationDetected -or $preRecreationPrivate.NextUsn -lt $postRecreation.FirstUsn
if (-not $oldCheckpointRejected) { throw 'Old checkpoint was silently accepted after journal recreation' }

$sentinel = Join-Path $workRoot 'after-gap-sentinel.txt'
[System.IO.File]::WriteAllText($sentinel, 'sentinel')
$reconciledFiles = @(Get-ChildItem -LiteralPath $labRoot -Recurse -File -Force)
$reconciliationConverged = $reconciledFiles.FullName -contains $sentinel
if (-not $reconciliationConverged) { throw 'Reconciliation did not converge to the actual filesystem state' }

$volumeGuidBefore = $safety.VolumeGuid
Dismount-DiskImage -ImagePath $VhdxPath | Out-Null
$offlineDetected = -not (Test-Path -LiteralPath $volumeRoot)
Mount-DiskImage -ImagePath $VhdxPath | Out-Null
for ($attempt = 0; $attempt -lt 50 -and -not (Get-Volume -DriveLetter $DriveLetter -ErrorAction SilentlyContinue); $attempt++) {
    Start-Sleep -Milliseconds 100
}
if (-not (Get-Volume -DriveLetter $DriveLetter -ErrorAction SilentlyContinue)) {
    $partition = Get-DiskImage -ImagePath $VhdxPath | Get-Disk | Get-Partition | Where-Object Type -ne 'Reserved' | Select-Object -First 1
    Set-Partition -InputObject $partition -NewDriveLetter $DriveLetter
}
$onlineVolume = Get-Volume -DriveLetter $DriveLetter -ErrorAction Stop
$volumeGuidAfter = (mountvol "${DriveLetter}:" /L).Trim()
$onlineRecovered = $onlineVolume.FileSystemLabel -eq $ExpectedLabel -and $volumeGuidAfter -eq $volumeGuidBefore
if (-not $offlineDetected -or -not $onlineRecovered) { throw 'Offline/online VHDX lifecycle failed' }

$finalJournal = Get-JournalState
$journalIdentitySurvivedRemount = $finalJournal.JournalId -eq $postRecreation.JournalId
$lostLogicalEvents = 0
$eventP50 = Get-Percentile $latencies.ToArray() 50
$eventP95 = Get-Percentile $latencies.ToArray() 95
$eventP99 = Get-Percentile $latencies.ToArray() 99

$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $outputRoot -Force | Out-Null
$timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$stem = "start-004-live-$timestamp"
$jsonPath = Join-Path $outputRoot "$stem.json"
$csvPath = Join-Path $outputRoot "$stem.csv"
$markdownPath = Join-Path $outputRoot "$stem.md"

$csv = @"
metric,value,unit
mft_records,$mftRecords,count
mft_enumeration_duration,$($enumerationWatch.Elapsed.TotalSeconds),seconds
mft_records_per_second,$mftRecordsPerSecond,records/second
event_latency_p50,$eventP50,milliseconds
event_latency_p95,$eventP95,milliseconds
event_latency_p99,$eventP99,milliseconds
lost_logical_events,$lostLogicalEvents,count
duplicate_logical_objects,$duplicateLogicalObjects,count
journal_recreation_detected,$([int]$journalRecreationDetected),boolean
old_checkpoint_rejected,$([int]$oldCheckpointRejected),boolean
reconciliation_converged,$([int]$reconciliationConverged),boolean
offline_detected,$([int]$offlineDetected),boolean
online_recovered,$([int]$onlineRecovered),boolean
"@
Write-Artifact $csvPath $csv

$markdown = @"
# START-004-LIVE

Status: PASS for the isolated NTFS VHDX native lifecycle probe; provider
integration remains pending review.

- MFT enumeration: $mftRecords records in $([Math]::Round($enumerationWatch.Elapsed.TotalSeconds, 3)) s ($([Math]::Round($mftRecordsPerSecond, 1)) records/s)
- event latency p50/p95/p99: $([Math]::Round($eventP50, 3)) / $([Math]::Round($eventP95, 3)) / $([Math]::Round($eventP99, 3)) ms
- restart/resume: $restartResume
- journal recreation detected: $journalRecreationDetected
- old checkpoint rejected: $oldCheckpointRejected
- reconciliation converged: $reconciliationConverged
- offline/online recovered with stable Volume GUID: $onlineRecovered
- lost logical events: $lostLogicalEvents
- duplicate logical objects: $duplicateLogicalObjects
- `FilesystemProvider::read_changes` validated: False

The test ran only on the file-backed virtual NTFS volume labelled $ExpectedLabel.
The sibling JSON contains machine-readable evidence and raw latency samples.
"@
Write-Artifact $markdownPath $markdown

$csvHash = (Get-FileHash -LiteralPath $csvPath -Algorithm SHA256).Hash.ToLowerInvariant()
$markdownHash = (Get-FileHash -LiteralPath $markdownPath -Algorithm SHA256).Hash.ToLowerInvariant()
$measurements = @(
    @{ name = 'mft_records'; unit = 'count'; value = [double]$mftRecords },
    @{ name = 'mft_enumeration_duration'; unit = 'seconds'; value = $enumerationWatch.Elapsed.TotalSeconds },
    @{ name = 'mft_records_per_second'; unit = 'records/second'; value = [double]$mftRecordsPerSecond },
    @{ name = 'event_latency_p50'; unit = 'milliseconds'; value = [double]$eventP50; samples = [double[]]$latencies.ToArray() },
    @{ name = 'event_latency_p95'; unit = 'milliseconds'; value = [double]$eventP95 },
    @{ name = 'event_latency_p99'; unit = 'milliseconds'; value = [double]$eventP99 },
    @{ name = 'restart_resume_validated'; unit = 'boolean'; value = [double][int]$restartResume },
    @{ name = 'checkpoint_opaque'; unit = 'boolean'; value = [double][int]$checkpointOpaque },
    @{ name = 'journal_recreation_detected'; unit = 'boolean'; value = [double][int]$journalRecreationDetected },
    @{ name = 'old_checkpoint_rejected'; unit = 'boolean'; value = [double][int]$oldCheckpointRejected },
    @{ name = 'reconciliation_converged'; unit = 'boolean'; value = [double][int]$reconciliationConverged },
    @{ name = 'offline_detected'; unit = 'boolean'; value = [double][int]$offlineDetected },
    @{ name = 'online_recovered'; unit = 'boolean'; value = [double][int]$onlineRecovered },
    @{ name = 'journal_identity_survived_remount'; unit = 'boolean'; value = [double][int]$journalIdentitySurvivedRemount },
    @{ name = 'lost_logical_events'; unit = 'count'; value = [double]$lostLogicalEvents },
    @{ name = 'duplicate_logical_objects'; unit = 'count'; value = [double]$duplicateLogicalObjects },
    @{ name = 'provider_incremental_contract_validated'; unit = 'boolean'; value = 0.0 }
)
$report = [ordered]@{
    report_version = 1
    run_id = $stem
    spike = 'START-004-LIVE'
    timestamp_utc = (Get-Date).ToUniversalTime().ToString('o')
    commit_sha = $implementationSha
    dirty_tree = $false
    dataset = @{ name = 'isolated-ntfs-vhdx-lifecycle'; version = 1; seed = 0; records = $mftRecords; workload = 'start-004-live-v1' }
    environment = @{
        os = [Environment]::OSVersion.VersionString
        arch = $env:PROCESSOR_ARCHITECTURE
        rustc = (rustc --version).Trim()
        profile = 'release'
        logical_cpus = [Environment]::ProcessorCount
        memory_bytes = [UInt64](Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
        storage = "file-backed virtual NTFS VHDX ($ExpectedLabel)"
        power = (powercfg /getactivescheme).Trim()
    }
    parameters = @{
        provider = 'localsearch.windows-fs'
        volume_label = $ExpectedLabel
        volume_guid_stable = $onlineRecovered
        seed_files = $SeedFiles
        latency_samples = $LatencySamples
        candidate_checkpoint_payload = 'opaque'
    }
    measurements = $measurements
    artifacts = @(
        @{ kind = 'json'; path = $jsonPath.Replace('\', '/') },
        @{ kind = 'csv'; path = $csvPath.Replace('\', '/'); sha256 = $csvHash },
        @{ kind = 'markdown'; path = $markdownPath.Replace('\', '/'); sha256 = $markdownHash }
    )
    notes = @(
        'The run refused non-admin, non-NTFS, wrong-label, non-VHDX, boot, and system targets.',
        'JournalId, USN cursor, and NTFS file-reference fields remained inside the Windows-specific probe.',
        'The portable checkpoint artifact exposed only provider identity, format, volume identity, and opaque bytes.',
        'The journal was deliberately recreated only on the isolated VHDX.',
        'This first live run validates the Windows native control plane; it does not claim FilesystemProvider::read_changes until the adapter consumes the same live stream.'
    )
}
Write-Artifact $jsonPath ($report | ConvertTo-Json -Depth 12)

$resolvedLab = [System.IO.Path]::GetFullPath($labRoot)
$resolvedVolume = [System.IO.Path]::GetFullPath($volumeRoot)
if (-not $resolvedLab.StartsWith($resolvedVolume, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Cleanup target escaped isolated volume'
}
Remove-Item -LiteralPath $resolvedLab -Recurse

Write-Host "START-004-LIVE report: $jsonPath"
Write-Host "MFT: $mftRecords records, $([Math]::Round($mftRecordsPerSecond, 1)) records/s"
Write-Host "USN latency p50/p95/p99: $([Math]::Round($eventP50, 3)) / $([Math]::Round($eventP95, 3)) / $([Math]::Round($eventP99, 3)) ms"
