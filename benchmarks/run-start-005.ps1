param(
    [UInt64]$Records = 1000000,
    [UInt64]$Seed = 20260814,
    [string]$Database = 'L:\localsearch-start005-graph.sqlite3',
    [string]$Output = 'reports\benchmarks\start-005'
)

$ErrorActionPreference = 'Stop'

if (Test-Path -LiteralPath $Database) {
    throw "Benchmark database already exists: $Database"
}

cargo run --release --locked -p localsearch-filesystem-graph --bin start_005_bench -- `
    --records $Records `
    --seed $Seed `
    --database $Database `
    --output $Output

if ($LASTEXITCODE -ne 0) {
    throw "START-005 benchmark failed with exit code $LASTEXITCODE"
}
