param(
    [UInt64]$Seed = 20260814,
    [UInt32]$Samples = 50,
    [UInt32]$CandidateLimit = 300,
    [UInt32]$PressureFalseCandidates = 350,
    [UInt64]$WriterHeapBytes = 134217728,
    [UInt32]$FiveMillionTrials = 3,
    [UInt64]$MemoryBytes = 34045902848,
    [string]$Storage = "Micron 2650 1TB NVMe SSD",
    [string]$Power = "High performance; AC power"
)

$ErrorActionPreference = "Stop"
if ((git status --porcelain).Count -ne 0) {
    throw "START-003-R accepted evidence requires a clean tree"
}

function Invoke-RecallRun {
    param(
        [UInt64]$Records,
        [UInt64]$BaselineBytes,
        [string]$RunId,
        [string[]]$Strategies
    )
    if ((Get-PSDrive -Name C).Free -lt 2GB) {
        throw "START-003-R aborted: C: free space is below 2 GiB"
    }
    $arguments = @(
        "run", "--locked", "--release", "-p", "localsearch-substring-spike",
        "--bin", "start_003_recall", "--",
        "--run-id", $RunId,
        "--records", $Records,
        "--seed", $Seed,
        "--samples", $Samples,
        "--candidate-limit", $CandidateLimit,
        "--pressure-false-candidates", $PressureFalseCandidates,
        "--baseline-index-bytes", $BaselineBytes,
        "--writer-heap-bytes", $WriterHeapBytes,
        "--memory-bytes", $MemoryBytes,
        "--storage", $Storage,
        "--power", $Power,
        "--output", "reports/spikes/start-003-r"
    )
    foreach ($strategy in $Strategies) {
        $arguments += @("--strategy", $strategy)
    }
    cargo @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "START-003-R failed for $Records records"
    }
}

$timestamp = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
Invoke-RecallRun `
    -Records 100000 `
    -BaselineBytes 7839410 `
    -RunId "$timestamp-100k-comparison" `
    -Strategies @("trigram", "token_prefix_limited_trigram", "bounded_fourgram")

for ($trial = 1; $trial -le $FiveMillionTrials; $trial++) {
    $timestamp = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
    Invoke-RecallRun `
        -Records 5000000 `
        -BaselineBytes 355921361 `
        -RunId "$timestamp-5m-trigram-trial$trial" `
        -Strategies @("trigram")
}
