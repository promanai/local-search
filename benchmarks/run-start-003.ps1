param(
    [UInt64]$Seed = 20260814,
    [UInt32]$Samples = 30,
    [UInt32]$CandidateLimit = 300,
    [UInt64]$WriterHeapBytes = 134217728,
    [UInt32]$Trials = 3,
    [UInt64]$MemoryBytes = 34045902848,
    [string]$Storage = "Micron 2650 1TB NVMe SSD",
    [string]$Power = "High performance; BatteryStatus=2 (AC power); 10%"
)

$ErrorActionPreference = "Stop"
$recordCounts = @(100000, 1000000, 5000000)

for ($trial = 1; $trial -le $Trials; $trial++) {
    $timestamp = (Get-Date).ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
    $runId = "$timestamp-trial$trial"
    foreach ($recordCount in $recordCounts) {
        $freeBytes = (Get-PSDrive -Name C).Free
        if ($freeBytes -lt 2GB) {
            throw "START-003 aborted: C: free space is below 2 GiB"
        }
        $scale = switch ($recordCount) {
            100000 { "100k" }
            1000000 { "1m" }
            5000000 { "5m" }
        }
        cargo run --locked --release -p localsearch-substring-spike --bin start-003-bench -- `
            --run-id $runId `
            --records $recordCount `
            --seed $Seed `
            --samples $Samples `
            --candidate-limit $CandidateLimit `
            --writer-heap-bytes $WriterHeapBytes `
            --memory-bytes $MemoryBytes `
            --storage $Storage `
            --power $Power `
            --output "reports/spikes/start-003/$scale"
        if ($LASTEXITCODE -ne 0) {
            throw "START-003 failed for trial $trial and $recordCount records"
        }
    }
}
