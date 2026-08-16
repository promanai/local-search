[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Start010LoadProvenance.psm1') -Force
$Scripts = @(
    'prepare-start-010-load.ps1',
    'run-start-010-load.ps1',
    'invoke-load-gate.ps1',
    'LoadGateContract.psm1'
)
foreach ($Script in $Scripts) {
    $Tokens = $null
    $Errors = $null
    [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot $Script),
        [ref]$Tokens,
        [ref]$Errors
    ) | Out-Null
    if ($Errors.Count -gt 0) {
        throw "PowerShell parser rejected $Script"
    }
}
$Root = Join-Path ([IO.Path]::GetTempPath()) "localsearch-start010-provenance-$PID"
$Commit = '0123456789abcdef0123456789abcdef01234567'
try {
    New-Item -ItemType Directory -Path $Root -Force | Out-Null
    $Entries = foreach ($RelativePath in @(Get-Start010ExpectedExecutables)) {
        $FullPath = Join-Path $Root $RelativePath
        New-Item -ItemType Directory -Path (Split-Path -Parent $FullPath) -Force | Out-Null
        Set-Content -LiteralPath $FullPath -Value "bounded fixture for $RelativePath" -Encoding ascii
        $File = Get-Item -LiteralPath $FullPath
        [ordered]@{
            path = $RelativePath
            length_bytes = [int64]$File.Length
            sha256 = (Get-FileHash -LiteralPath $FullPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $ManifestPath = Join-Path $Root '.lab/start-010-load-bundle.json'
    New-Item -ItemType Directory -Path (Split-Path -Parent $ManifestPath) -Force | Out-Null
    $Manifest = [ordered]@{
        schema_version = 1
        gate = 'START-010-L-BUILD'
        built_at_utc = '20260815T000000Z'
        git_commit = $Commit
        dirty_tree = $false
        build_elevated = $false
        toolchain = [ordered]@{ rustc = 'test'; cargo = 'test'; host = 'test' }
        executables = @($Entries)
    }
    $Manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $ManifestPath -Encoding utf8

    $Verified = Test-Start010LoadBundle -Repository $Root -ManifestPath $ManifestPath `
        -ExpectedCommit $Commit
    if (-not $Verified.verified -or $Verified.executables.Count -ne 6) {
        throw 'Valid release bundle did not pass provenance verification'
    }

    Add-Content -LiteralPath (Join-Path $Root $Entries[0].path) -Value 'tampered' -Encoding ascii
    $TamperRejected = $false
    try {
        Test-Start010LoadBundle -Repository $Root -ManifestPath $ManifestPath `
            -ExpectedCommit $Commit | Out-Null
    }
    catch [IO.InvalidDataException] {
        $TamperRejected = $true
    }
    if (-not $TamperRejected) {
        throw 'Tampered release executable passed provenance verification'
    }

    $WrongCommitRejected = $false
    try {
        Test-Start010LoadBundle -Repository $Root -ManifestPath $ManifestPath `
            -ExpectedCommit 'abcdef0123456789abcdef0123456789abcdef01' | Out-Null
    }
    catch [IO.InvalidDataException] {
        $WrongCommitRejected = $true
    }
    if (-not $WrongCommitRejected) {
        throw 'Mismatched release commit passed provenance verification'
    }

    Write-Host 'START-010-L provenance self-test: PASS'
}
finally {
    if (Test-Path -LiteralPath $Root) {
        Remove-Item -LiteralPath $Root -Recurse -Force
    }
}
