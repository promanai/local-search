[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$SigningCertificateThumbprint,
    [string]$TimestampServer,
    [switch]$NoArchive
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force
$Repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$Principal = [Security.Principal.WindowsPrincipal]::new($Identity)
if ($Principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Release bundles must be built from a normal non-elevated PowerShell'
}
if (git -C $Repository status --porcelain) {
    throw 'Release bundle creation requires a clean repository'
}
$Commit = (git -C $Repository rev-parse HEAD).Trim()
$Output = [IO.Path]::GetFullPath($OutputDirectory)
$RepositoryPrefix = $Repository.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if ($Output.StartsWith($RepositoryPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'OutputDirectory must be outside the source repository'
}
if (Test-Path -LiteralPath $Output) {
    throw 'OutputDirectory must not already exist'
}
if ($SigningCertificateThumbprint -and -not $TimestampServer) {
    throw 'TimestampServer is required when signing a release bundle'
}
$Certificate = $null
if ($SigningCertificateThumbprint) {
    $Thumbprint = $SigningCertificateThumbprint.Replace(' ', '').ToUpperInvariant()
    $Certificate = Get-ChildItem Cert:\CurrentUser\My, Cert:\LocalMachine\My -CodeSigningCert |
        Where-Object Thumbprint -eq $Thumbprint |
        Select-Object -First 1
    if ($null -eq $Certificate -or -not $Certificate.HasPrivateKey -or $Certificate.NotAfter -le (Get-Date)) {
        throw 'A current code-signing certificate with a private key was not found'
    }
}

& cargo build --manifest-path (Join-Path $Repository 'Cargo.toml') --release --locked `
    -p localsearch-agent --bin localsearch-agent --bin localsearch-cli `
    -p localsearch-content-index --bin localsearch-content-index `
    -p localsearch-desktop --bin localsearch-desktop `
    -p localsearch-fs-service --bin localsearch-fs-service `
    -p localsearch-mcp --bin localsearch-mcp
if ($LASTEXITCODE -ne 0) { throw "Release build failed with exit code $LASTEXITCODE" }
if (git -C $Repository status --porcelain) { throw 'Release build changed the source tree' }
if ((git -C $Repository rev-parse HEAD).Trim() -ne $Commit) {
    throw 'Repository HEAD changed during release build'
}

New-Item -ItemType Directory -Path $Output | Out-Null
foreach ($Name in Get-LocalSearchExpectedBundleFiles) {
    $Source = if ($Name.EndsWith('.exe')) {
        Join-Path $Repository "target\release\$Name"
    } else {
        Join-Path $PSScriptRoot $Name
    }
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Release input is missing: $Name"
    }
    $Destination = Join-Path $Output $Name
    Copy-Item -LiteralPath $Source -Destination $Destination
    if ($Certificate) {
        $Signature = Set-AuthenticodeSignature -LiteralPath $Destination -Certificate $Certificate `
            -HashAlgorithm SHA256 -TimestampServer $TimestampServer
        if ($Signature.Status -ne 'Valid') {
            throw "Authenticode signing failed for ${Name}: $($Signature.StatusMessage)"
        }
    }
}
$Files = foreach ($Name in Get-LocalSearchExpectedBundleFiles) {
    $Destination = Join-Path $Output $Name
    $File = Get-Item -LiteralPath $Destination
    [ordered]@{
        path = $Name
        length_bytes = [int64]$File.Length
        sha256 = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$Rustc = (& rustc --version | Out-String).Trim()
$Cargo = (& cargo --version | Out-String).Trim()
$Manifest = [ordered]@{
    schema_version = 1
    product = 'LocalSearch'
    version = '0.1.0'
    git_commit = $Commit
    dirty_tree = $false
    build_elevated = $false
    profile = 'release'
    built_at_utc = (Get-Date).ToUniversalTime().ToString('o')
    architecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
    authenticode_signed = ($null -ne $Certificate)
    signer_thumbprint = if ($Certificate) { $Certificate.Thumbprint } else { $null }
    toolchain = [ordered]@{ rustc = $Rustc; cargo = $Cargo }
    files = @($Files)
}
$ManifestPath = Join-Path $Output 'manifest.json'
$Manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $ManifestPath -Encoding utf8
if ($Certificate) {
    $ManifestContent = [Security.Cryptography.Pkcs.ContentInfo]::new(
        [IO.File]::ReadAllBytes($ManifestPath)
    )
    $SignedManifest = [Security.Cryptography.Pkcs.SignedCms]::new($ManifestContent, $true)
    $ManifestSigner = [Security.Cryptography.Pkcs.CmsSigner]::new($Certificate)
    $ManifestSigner.IncludeOption = [Security.Cryptography.X509Certificates.X509IncludeOption]::EndCertOnly
    $SignedManifest.ComputeSignature($ManifestSigner)
    [IO.File]::WriteAllBytes((Join-Path $Output 'manifest.p7s'), $SignedManifest.Encode())
}
Test-LocalSearchBundle -BundlePath $Output -ManifestPath $ManifestPath `
    -RequireAuthenticodeSignature:($null -ne $Certificate) | Out-Null

if (-not $NoArchive) {
    $Archive = "$Output.zip"
    if (Test-Path -LiteralPath $Archive) { throw 'Bundle archive already exists' }
    Compress-Archive -Path (Join-Path $Output '*') -DestinationPath $Archive -CompressionLevel Optimal
    Write-Host "LocalSearch archive: $Archive"
}
Write-Host "LocalSearch bundle: $Output"
Write-Host "commit=$Commit; files=$($Files.Count); verified=true; signed=$($null -ne $Certificate)"
