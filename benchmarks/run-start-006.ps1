param(
    [UInt64]$Records = 1000000,
    [UInt64]$Seed = 20260814,
    [string]$Database = 'L:\localsearch-start006.sqlite3',
    [string]$IndexRoot = 'L:\localsearch-start006-index',
    [string]$Output = 'reports\benchmarks\start-006'
)

$ErrorActionPreference = 'Stop'

if (Test-Path -LiteralPath $Database) {
    throw "Benchmark database already exists: $Database"
}
if (Test-Path -LiteralPath $IndexRoot) {
    throw "Benchmark index root already exists: $IndexRoot"
}

cargo run --release --locked -p localsearch-catalog-index --bin start_006_bench -- `
    --records $Records `
    --seed $Seed `
    --database $Database `
    --index-root $IndexRoot `
    --output $Output

if ($LASTEXITCODE -ne 0) {
    throw "START-006 benchmark failed with exit code $LASTEXITCODE"
}
