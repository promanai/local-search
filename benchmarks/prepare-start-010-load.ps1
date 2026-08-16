[CmdletBinding()]
param(
    [string]$ManifestPath = '.lab/start-010-load-bundle.json'
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Start010LoadProvenance.psm1') -Force
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if ($Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Build START-010-L release binaries from a normal non-elevated PowerShell'
}
if (git -C $Repository status --porcelain) {
    throw 'START-010-L preparation requires a clean repository'
}
$Commit = (git -C $Repository rev-parse HEAD).Trim()
$ExpectedExecutables = @(Get-Start010ExpectedExecutables)
$ExpectedProcessNames = @($ExpectedExecutables | ForEach-Object {
    [IO.Path]::GetFileName($_)
} | Sort-Object -Unique)
$Running = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -in $ExpectedProcessNames -and (
        -not $_.ExecutablePath -or
        $_.ExecutablePath.StartsWith(
            (Join-Path $Repository 'target\release\'),
            [StringComparison]::OrdinalIgnoreCase
        )
    )
})
if ($Running.Count -gt 0) {
    $ProcessList = @($Running | ForEach-Object {
        "$($_.Name) PID $($_.ProcessId)"
    }) -join ', '
    throw "Close LocalSearch release processes before preparing the bundle: $ProcessList"
}

& cargo build --manifest-path (Join-Path $Repository 'Cargo.toml') --release --locked `
    -p localsearch-agent --bin localsearch-agent --bin localsearch-cli `
    -p localsearch-content-index --bin localsearch-content-index `
    -p localsearch-desktop --bin localsearch-desktop --example ux_action_probe `
    -p localsearch-ux-fixture --bin localsearch-ux-fixture
if ($LASTEXITCODE -ne 0) {
    throw "Release bundle build failed with exit code $LASTEXITCODE"
}
if (git -C $Repository status --porcelain) {
    throw 'Release build changed the clean source tree'
}
if ((git -C $Repository rev-parse HEAD).Trim() -ne $Commit) {
    throw 'Repository HEAD changed during release bundle preparation'
}

$RustcVerbose = @(& rustc --version --verbose)
if ($LASTEXITCODE -ne 0) { throw 'rustc version query failed' }
$CargoVersion = (& cargo --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'cargo version query failed' }
$RustcVersion = [string]$RustcVerbose[0]
$RustHostTriple = [string]($RustcVerbose | Where-Object {
    $_ -match '^host: '
} | Select-Object -First 1)
if (-not $RustHostTriple) { throw 'rustc host triple is unavailable' }
$RustHostTriple = $RustHostTriple.Substring('host: '.Length)

$Executables = foreach ($RelativePath in $ExpectedExecutables) {
    $FullPath = Join-Path $Repository $RelativePath
    if (-not (Test-Path -LiteralPath $FullPath -PathType Leaf)) {
        throw "Release build did not produce $RelativePath"
    }
    $File = Get-Item -LiteralPath $FullPath
    [ordered]@{
        path = $RelativePath.Replace('\', '/')
        length_bytes = [int64]$File.Length
        sha256 = (Get-FileHash -LiteralPath $FullPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

$Manifest = [ordered]@{
    schema_version = 1
    gate = 'START-010-L-BUILD'
    built_at_utc = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
    git_commit = $Commit
    dirty_tree = $false
    build_elevated = $false
    profile = 'release'
    toolchain = [ordered]@{
        rustc = $RustcVersion
        cargo = $CargoVersion
        host = $RustHostTriple
    }
    executables = @($Executables)
}
$ManifestFullPath = if ([IO.Path]::IsPathRooted($ManifestPath)) {
    [IO.Path]::GetFullPath($ManifestPath)
}
else {
    [IO.Path]::GetFullPath((Join-Path $Repository $ManifestPath))
}
New-Item -ItemType Directory -Path (Split-Path -Parent $ManifestFullPath) -Force | Out-Null
$Manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $ManifestFullPath -Encoding utf8
$Verified = Test-Start010LoadBundle -Repository $Repository -ManifestPath $ManifestFullPath `
    -ExpectedCommit $Commit

Write-Host "START-010-L release bundle: $ManifestFullPath"
Write-Host "commit=$($Verified.git_commit); executables=$($Verified.executables.Count); verified=$($Verified.verified)"
