Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Security -ErrorAction Stop

$script:LocalSearchExecutables = @(
    'localsearch-agent.exe',
    'localsearch-cli.exe',
    'localsearch-content-index.exe',
    'localsearch-desktop.exe',
    'localsearch-fs-service.exe',
    'localsearch-mcp.exe'
)

$script:LocalSearchPackageTools = @(
    'LocalSearch.Package.psm1',
    'export-diagnostics.ps1',
    'install-windows.ps1',
    'uninstall-windows.ps1'
)

function Get-LocalSearchExpectedExecutables {
    return @($script:LocalSearchExecutables)
}

function Get-LocalSearchExpectedBundleFiles {
    return @($script:LocalSearchExecutables + $script:LocalSearchPackageTools)
}

function Get-LocalSearchFullPath {
    param([Parameter(Mandatory)][string]$Path)
    return [IO.Path]::GetFullPath($Path)
}

function Assert-LocalSearchSafeDirectory {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Purpose
    )
    $Full = Get-LocalSearchFullPath -Path $Path
    $Root = [IO.Path]::GetPathRoot($Full)
    if (-not $Root -or $Full.TrimEnd('\', '/') -eq $Root.TrimEnd('\', '/')) {
        throw "$Purpose cannot be a filesystem root"
    }
    if ($Full.Length -lt ($Root.Length + 8)) {
        throw "$Purpose is too broad"
    }
    return $Full
}

function Assert-LocalSearchVolumeRoot {
    param([Parameter(Mandatory)][string]$Path)
    $Full = Get-LocalSearchFullPath -Path $Path
    $Root = [IO.Path]::GetPathRoot($Full)
    if (-not $Root -or $Full.TrimEnd('\', '/') -ne $Root.TrimEnd('\', '/')) {
        throw "Observed root must be a complete mounted volume root: $Path"
    }
    return $Root
}

function Assert-LocalSearchPipeName {
    param([Parameter(Mandatory)][string]$PipeName)
    if ($PipeName -notmatch '^\\\\\.\\pipe\\LocalSearch\\WinFS\\v1\\[A-Za-z0-9._-]{1,64}$') {
        throw 'Broker pipe must stay inside \\.\pipe\LocalSearch\WinFS\v1\ with a bounded leaf'
    }
    return $PipeName
}

function Assert-LocalSearchSid {
    param([Parameter(Mandatory)][string]$Sid)
    if ($Sid -notmatch '^S-\d-(?:\d+-){1,14}\d+$') {
        throw 'Authorized logon SID is invalid'
    }
    return $Sid
}

function Test-LocalSearchBundle {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$BundlePath,
        [string]$ManifestPath,
        [switch]$RequireAuthenticodeSignature
    )
    $Bundle = (Resolve-Path -LiteralPath $BundlePath).Path
    if (-not $ManifestPath) {
        $ManifestPath = Join-Path $Bundle 'manifest.json'
    }
    $ManifestFull = (Resolve-Path -LiteralPath $ManifestPath).Path
    $ManifestSignerThumbprint = $null
    if ($RequireAuthenticodeSignature) {
        $DetachedSignaturePath = Join-Path $Bundle 'manifest.p7s'
        if (-not (Test-Path -LiteralPath $DetachedSignaturePath -PathType Leaf)) {
            throw [Security.SecurityException]::new('Detached bundle manifest signature is missing')
        }
        $ManifestBytes = [IO.File]::ReadAllBytes($ManifestFull)
        $ContentInfo = [Security.Cryptography.Pkcs.ContentInfo]::new($ManifestBytes)
        $SignedManifest = [Security.Cryptography.Pkcs.SignedCms]::new($ContentInfo, $true)
        $SignedManifest.Decode([IO.File]::ReadAllBytes($DetachedSignaturePath))
        $SignedManifest.CheckSignature($true)
        if ($SignedManifest.SignerInfos.Count -ne 1 -or
            $null -eq $SignedManifest.SignerInfos[0].Certificate) {
            throw [Security.SecurityException]::new('Detached manifest signature has no unique signer')
        }
        $ManifestSignerThumbprint = $SignedManifest.SignerInfos[0].Certificate.Thumbprint
    }
    $Manifest = Get-Content -LiteralPath $ManifestFull -Raw -Encoding utf8 | ConvertFrom-Json
    if ($Manifest.schema_version -ne 1 -or $Manifest.product -ne 'LocalSearch') {
        throw [IO.InvalidDataException]::new('Unsupported LocalSearch bundle manifest')
    }
    if ($Manifest.git_commit -notmatch '^[0-9a-f]{40}$' -or $Manifest.dirty_tree -ne $false) {
        throw [IO.InvalidDataException]::new('Bundle provenance is incomplete')
    }
    if ($Manifest.profile -ne 'release' -or $Manifest.build_elevated -ne $false) {
        throw [IO.InvalidDataException]::new('Bundle was not produced by the non-elevated release path')
    }
    if ($RequireAuthenticodeSignature -and
        ($Manifest.authenticode_signed -ne $true -or
        [string]$Manifest.signer_thumbprint -ne $ManifestSignerThumbprint)) {
        throw [Security.SecurityException]::new('Manifest signer provenance does not match its signature')
    }
    $Entries = @($Manifest.files)
    $Expected = @(Get-LocalSearchExpectedBundleFiles | Sort-Object)
    $Actual = @($Entries | ForEach-Object { [string]$_.path } | Sort-Object)
    if (($Expected -join "`n") -ne ($Actual -join "`n")) {
        throw [IO.InvalidDataException]::new('Bundle executable allowlist mismatch')
    }
    foreach ($Entry in $Entries) {
        $Relative = [string]$Entry.path
        if ([IO.Path]::IsPathRooted($Relative) -or $Relative -ne [IO.Path]::GetFileName($Relative) -or
            $Relative.Contains('..') -or $Relative.Contains('/') -or $Relative.Contains('\')) {
            throw [IO.InvalidDataException]::new('Bundle contains an unsafe relative path')
        }
        $Full = Join-Path $Bundle $Relative
        if (-not (Test-Path -LiteralPath $Full -PathType Leaf)) {
            throw [IO.InvalidDataException]::new("Bundle file is missing: $Relative")
        }
        $File = Get-Item -LiteralPath $Full
        if ([int64]$Entry.length_bytes -ne [int64]$File.Length) {
            throw [IO.InvalidDataException]::new("Bundle length mismatch: $Relative")
        }
        $ExpectedHash = [string]$Entry.sha256
        if ($ExpectedHash -notmatch '^[0-9a-fA-F]{64}$') {
            throw [IO.InvalidDataException]::new("Bundle hash is malformed: $Relative")
        }
        $Hash = (Get-FileHash -LiteralPath $Full -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($Hash -ne $ExpectedHash.ToLowerInvariant()) {
            throw [IO.InvalidDataException]::new("Bundle hash mismatch: $Relative")
        }
        if ($RequireAuthenticodeSignature) {
            $Signature = Get-AuthenticodeSignature -LiteralPath $Full
            if ($Signature.Status -ne 'Valid' -or $null -eq $Signature.SignerCertificate) {
                throw [Security.SecurityException]::new("Bundle signature is not trusted: $Relative")
            }
            if ($Signature.SignerCertificate.Thumbprint -ne $ManifestSignerThumbprint) {
                throw [Security.SecurityException]::new("Bundle payload signer mismatch: $Relative")
            }
        }
    }
    return $Manifest
}

function ConvertTo-LocalSearchCommandArgument {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Value)
    if ($Value.Length -gt 32760) { throw 'Command argument exceeds Windows policy' }
    $Builder = [Text.StringBuilder]::new()
    [void]$Builder.Append('"')
    $Backslashes = 0
    foreach ($Character in $Value.ToCharArray()) {
        if ($Character -eq '\') {
            $Backslashes++
            continue
        }
        if ($Character -eq '"') {
            [void]$Builder.Append(('\' * (($Backslashes * 2) + 1)))
            [void]$Builder.Append('"')
            $Backslashes = 0
            continue
        }
        if ($Backslashes -gt 0) {
            [void]$Builder.Append(('\' * $Backslashes))
            $Backslashes = 0
        }
        [void]$Builder.Append($Character)
    }
    if ($Backslashes -gt 0) { [void]$Builder.Append(('\' * ($Backslashes * 2))) }
    [void]$Builder.Append('"')
    return $Builder.ToString()
}

function Join-LocalSearchCommandLine {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [string[]]$ArgumentList = @()
    )
    $Parts = @(ConvertTo-LocalSearchCommandArgument -Value $Executable)
    $Parts += @($ArgumentList | ForEach-Object { ConvertTo-LocalSearchCommandArgument -Value $_ })
    return ($Parts -join ' ')
}

function New-LocalSearchInstallPlan {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$BundlePath,
        [Parameter(Mandatory)][string]$InstallRoot,
        [Parameter(Mandatory)][string]$StateRoot,
        [Parameter(Mandatory)][string]$AuthorizedLogonSid,
        [string]$BrokerPipe = '\\.\pipe\LocalSearch\WinFS\v1\default',
        [string[]]$ObserveRoot = @(),
        [switch]$EnableBrokerObservation,
        [switch]$EnableContent
    )
    $Bundle = (Resolve-Path -LiteralPath $BundlePath).Path
    $Install = Assert-LocalSearchSafeDirectory -Path $InstallRoot -Purpose 'Install root'
    $State = Assert-LocalSearchSafeDirectory -Path $StateRoot -Purpose 'State root'
    $Sid = Assert-LocalSearchSid -Sid $AuthorizedLogonSid
    $Pipe = Assert-LocalSearchPipeName -PipeName $BrokerPipe
    $Roots = @($ObserveRoot | ForEach-Object { Assert-LocalSearchVolumeRoot -Path $_ } | Sort-Object -Unique)
    if ($EnableBrokerObservation -and $Roots.Count -eq 0) {
        throw 'Broker observation requires at least one explicit mounted volume root'
    }
    if (-not $EnableBrokerObservation -and $Roots.Count -gt 0) {
        throw 'ObserveRoot requires EnableBrokerObservation'
    }
    # Keep the package layout identical to the content workspace layout so metadata observation,
    # catalog projection, and explicitly scoped content projection share one authoritative graph.
    $Graph = Join-Path $State 'graph.sqlite3'
    $Catalog = Join-Path $State 'catalog'
    $Content = Join-Path $State 'content-index-v1'
    $AgentArguments = @('--graph', $Graph, '--index', $Catalog)
    if ($EnableContent) { $AgentArguments += @('--content-index', $Content) }
    if ($EnableBrokerObservation) {
        $AgentArguments += @('--observe-usn', '--broker-pipe', $Pipe)
        foreach ($Root in $Roots) { $AgentArguments += @('--observe-root', $Root) }
    }
    $ServiceArguments = @(
        '--windows-service', '--pipe', $Pipe, '--authorized-logon-sid', $Sid
    )
    return [pscustomobject][ordered]@{
        schema_version = 1
        product = 'LocalSearch'
        bundle_root = $Bundle
        install_root = $Install
        state_root = $State
        owner_sid = $Sid
        broker_enabled = [bool]$EnableBrokerObservation
        content_enabled = [bool]$EnableContent
        observed_roots = @($Roots)
        service = [pscustomobject][ordered]@{
            name = 'LocalSearchWinFS'
            executable = Join-Path $Install 'localsearch-fs-service.exe'
            arguments = @($ServiceArguments)
        }
        agent = [pscustomobject][ordered]@{
            task_name = 'LocalSearch Agent'
            executable = Join-Path $Install 'localsearch-agent.exe'
            arguments = @($AgentArguments)
        }
        desktop = [pscustomobject][ordered]@{
            task_name = 'LocalSearch Desktop'
            executable = Join-Path $Install 'localsearch-desktop.exe'
            arguments = @()
        }
    }
}

function Write-LocalSearchMarker {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][ValidateSet('install', 'state')][string]$Kind,
        [Parameter(Mandatory)][string]$OwnerSid,
        [Parameter(Mandatory)][string]$GitCommit
    )
    $Full = Assert-LocalSearchSafeDirectory -Path $Root -Purpose "$Kind root"
    New-Item -ItemType Directory -Path $Full -Force | Out-Null
    $Marker = [ordered]@{
        schema_version = 1
        product = 'LocalSearch'
        kind = $Kind
        owner_sid = $OwnerSid
        root = $Full
        git_commit = $GitCommit
    }
    $Marker | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $Full ".localsearch-$Kind.json") -Encoding utf8
}

function Test-LocalSearchMarker {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][ValidateSet('install', 'state')][string]$Kind,
        [Parameter(Mandatory)][string]$OwnerSid
    )
    $Full = Assert-LocalSearchSafeDirectory -Path $Root -Purpose "$Kind root"
    $MarkerPath = Join-Path $Full ".localsearch-$Kind.json"
    if (-not (Test-Path -LiteralPath $MarkerPath -PathType Leaf)) {
        throw [IO.InvalidDataException]::new("LocalSearch $Kind marker is missing")
    }
    $Marker = Get-Content -LiteralPath $MarkerPath -Raw -Encoding utf8 | ConvertFrom-Json
    if ($Marker.schema_version -ne 1 -or $Marker.product -ne 'LocalSearch' -or
        $Marker.kind -ne $Kind -or $Marker.owner_sid -ne $OwnerSid -or
        [IO.Path]::GetFullPath([string]$Marker.root) -ne $Full) {
        throw [IO.InvalidDataException]::new("LocalSearch $Kind marker does not authorize this root")
    }
    return $Marker
}

function Set-LocalSearchPrivateStateAcl {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$OwnerSid
    )
    $Full = Assert-LocalSearchSafeDirectory -Path $Root -Purpose 'State root'
    $Sid = Assert-LocalSearchSid -Sid $OwnerSid
    if (-not (Test-Path -LiteralPath $Full -PathType Container)) {
        throw 'State root must exist before its ACL is applied'
    }
    $Owner = [Security.Principal.SecurityIdentifier]::new($Sid)
    $System = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
    $Acl = [Security.AccessControl.DirectorySecurity]::new()
    $Acl.SetAccessRuleProtection($true, $false)
    foreach ($Identity in @($Owner, $System)) {
        $Acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            $Identity, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'
        ))
    }
    $Acl.SetOwner($Owner)
    Set-Acl -LiteralPath $Full -AclObject $Acl
}

function Get-LocalSearchTreeSummary {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        return [pscustomobject]@{ exists = $false; files = 0; bytes = 0 }
    }
    $Files = @(Get-ChildItem -LiteralPath $Path -File -Recurse -Force -ErrorAction SilentlyContinue)
    $Bytes = [int64]0
    foreach ($File in $Files) { $Bytes += [int64]$File.Length }
    return [pscustomobject]@{ exists = $true; files = $Files.Count; bytes = $Bytes }
}

function New-LocalSearchDiagnosticsDocument {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$InstallRoot,
        [Parameter(Mandatory)][string]$StateRoot
    )
    $Install = Assert-LocalSearchSafeDirectory -Path $InstallRoot -Purpose 'Install root'
    $State = Assert-LocalSearchSafeDirectory -Path $StateRoot -Purpose 'State root'
    $Binaries = foreach ($Name in Get-LocalSearchExpectedExecutables) {
        $Path = Join-Path $Install $Name
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            $File = Get-Item -LiteralPath $Path
            [ordered]@{
                component = [IO.Path]::GetFileNameWithoutExtension($Name)
                length_bytes = [int64]$File.Length
                sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    }
    $Service = Get-Service -Name 'LocalSearchWinFS' -ErrorAction SilentlyContinue
    return [ordered]@{
        schema_version = 1
        product = 'LocalSearch'
        generated_at_utc = (Get-Date).ToUniversalTime().ToString('o')
        privacy = [ordered]@{
            paths_included = $false
            filenames_included = $false
            queries_included = $false
            content_included = $false
        }
        runtime = [ordered]@{
            os_version = [Environment]::OSVersion.VersionString
            process_architecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
            service_installed = ($null -ne $Service)
            service_running = ($null -ne $Service -and $Service.Status -eq 'Running')
        }
        binaries = @($Binaries)
        storage = [ordered]@{
            install = Get-LocalSearchTreeSummary -Path $Install
            state = Get-LocalSearchTreeSummary -Path $State
        }
    }
}

Export-ModuleMember -Function @(
    'Assert-LocalSearchSid',
    'Assert-LocalSearchSafeDirectory',
    'Assert-LocalSearchVolumeRoot',
    'ConvertTo-LocalSearchCommandArgument',
    'Get-LocalSearchExpectedBundleFiles',
    'Get-LocalSearchExpectedExecutables',
    'Join-LocalSearchCommandLine',
    'New-LocalSearchDiagnosticsDocument',
    'New-LocalSearchInstallPlan',
    'Set-LocalSearchPrivateStateAcl',
    'Test-LocalSearchBundle',
    'Test-LocalSearchMarker',
    'Write-LocalSearchMarker'
)
