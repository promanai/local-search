Set-StrictMode -Version Latest

function Get-Start010ExpectedExecutables {
    return @(
        'target/release/localsearch-agent.exe',
        'target/release/localsearch-cli.exe',
        'target/release/localsearch-content-index.exe',
        'target/release/localsearch-desktop.exe',
        'target/release/localsearch-ux-fixture.exe'
    )
}

function Test-Start010LoadBundle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Repository,
        [Parameter(Mandatory)]
        [string]$ManifestPath,
        [Parameter(Mandatory)]
        [ValidatePattern('^[0-9a-fA-F]{40}$')]
        [string]$ExpectedCommit
    )

    $RepositoryRoot = [IO.Path]::GetFullPath($Repository).TrimEnd('\', '/')
    $ManifestFullPath = if ([IO.Path]::IsPathRooted($ManifestPath)) {
        [IO.Path]::GetFullPath($ManifestPath)
    }
    else {
        [IO.Path]::GetFullPath((Join-Path $RepositoryRoot $ManifestPath))
    }
    $RepositoryPrefix = $RepositoryRoot + [IO.Path]::DirectorySeparatorChar
    if (-not $ManifestFullPath.StartsWith($RepositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw [IO.InvalidDataException]::new('build manifest must stay inside the repository')
    }
    if (-not (Test-Path -LiteralPath $ManifestFullPath -PathType Leaf)) {
        throw [IO.FileNotFoundException]::new('START-010-L build manifest is missing')
    }

    try {
        $Manifest = Get-Content -LiteralPath $ManifestFullPath -Raw |
            ConvertFrom-Json -ErrorAction Stop
    }
    catch {
        throw [IO.InvalidDataException]::new('START-010-L build manifest is invalid JSON')
    }
    $RequiredProperties = @(
        'schema_version', 'gate', 'built_at_utc', 'git_commit', 'dirty_tree',
        'build_elevated', 'toolchain', 'executables'
    )
    foreach ($Property in $RequiredProperties) {
        if ($Property -notin $Manifest.PSObject.Properties.Name) {
            throw [IO.InvalidDataException]::new('START-010-L build manifest is incomplete')
        }
    }
    if ([int]$Manifest.schema_version -ne 1 -or $Manifest.gate -ne 'START-010-L-BUILD') {
        throw [IO.InvalidDataException]::new('START-010-L build manifest contract mismatch')
    }
    if ($Manifest.dirty_tree -isnot [bool] -or $Manifest.build_elevated -isnot [bool] -or
        [bool]$Manifest.dirty_tree -or [bool]$Manifest.build_elevated) {
        throw [IO.InvalidDataException]::new('release bundle was not produced from a clean non-elevated build')
    }
    if ($Manifest.git_commit -ne $ExpectedCommit) {
        throw [IO.InvalidDataException]::new('release bundle commit does not match current HEAD')
    }

    $Expected = @(Get-Start010ExpectedExecutables | Sort-Object)
    $Entries = @($Manifest.executables)
    $Actual = @($Entries | ForEach-Object { [string]$_.path } | Sort-Object -Unique)
    if ($Entries.Count -ne $Expected.Count -or $Actual.Count -ne $Expected.Count -or
        (Compare-Object -ReferenceObject $Expected -DifferenceObject $Actual)) {
        throw [IO.InvalidDataException]::new('release bundle executable allowlist mismatch')
    }

    $Verified = foreach ($Entry in $Entries) {
        foreach ($Property in @('path', 'length_bytes', 'sha256')) {
            if ($Property -notin $Entry.PSObject.Properties.Name) {
                throw [IO.InvalidDataException]::new('release bundle executable entry is incomplete')
            }
        }
        $RelativePath = [string]$Entry.path
        $FullPath = [IO.Path]::GetFullPath((Join-Path $RepositoryRoot $RelativePath))
        if (-not $FullPath.StartsWith($RepositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw [IO.InvalidDataException]::new('release bundle executable escaped the repository')
        }
        if (-not (Test-Path -LiteralPath $FullPath -PathType Leaf)) {
            throw [IO.FileNotFoundException]::new('release bundle executable is missing')
        }
        $File = Get-Item -LiteralPath $FullPath
        if (($File.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw [IO.InvalidDataException]::new('release bundle executable cannot be a reparse point')
        }
        $Hash = (Get-FileHash -LiteralPath $FullPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ([int64]$Entry.length_bytes -ne [int64]$File.Length -or
            [string]$Entry.sha256 -ne $Hash) {
            throw [IO.InvalidDataException]::new('release bundle executable hash or length mismatch')
        }
        [pscustomobject]@{
            path = $RelativePath
            length_bytes = [int64]$File.Length
            sha256 = $Hash
        }
    }

    return [pscustomobject]@{
        verified = $true
        schema_version = 1
        gate = 'START-010-L-BUILD'
        git_commit = [string]$Manifest.git_commit
        built_at_utc = [string]$Manifest.built_at_utc
        toolchain = $Manifest.toolchain
        executables = @($Verified)
    }
}

Export-ModuleMember -Function Get-Start010ExpectedExecutables, Test-Start010LoadBundle
