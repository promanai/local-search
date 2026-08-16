[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force

function Assert-Equal {
    param($Expected, $Actual, [Parameter(Mandatory)][string]$Message)
    if ($Expected -ne $Actual) {
        throw "$Message (expected=[$Expected], actual=[$Actual])"
    }
}

function Assert-True {
    param([bool]$Condition, [Parameter(Mandatory)][string]$Message)
    if (-not $Condition) { throw $Message }
}

function Assert-Fails {
    param([Parameter(Mandatory)][scriptblock]$Action, [Parameter(Mandatory)][string]$Message)
    try {
        & $Action
    } catch {
        return
    }
    throw $Message
}

$TemporaryParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
$TestRoot = Join-Path $TemporaryParent "localsearch-package-tests-$PID-$([Guid]::NewGuid().ToString('N'))"
$Bundle = Join-Path $TestRoot 'bundle'
$Install = Join-Path $TestRoot 'installed-product'
$State = Join-Path $TestRoot 'private-state'
$WrongRoot = Join-Path $TestRoot 'wrong-product-root'
$Sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$Commit = '0123456789abcdef0123456789abcdef01234567'

try {
    New-Item -ItemType Directory -Path $Bundle, $Install, $State, $WrongRoot | Out-Null
    $Entries = foreach ($Name in Get-LocalSearchExpectedBundleFiles) {
        $Path = Join-Path $Bundle $Name
        Set-Content -LiteralPath $Path -Value "fixture:$Name" -Encoding utf8
        $File = Get-Item -LiteralPath $Path
        [ordered]@{
            path = $Name
            length_bytes = [int64]$File.Length
            sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $Manifest = [ordered]@{
        schema_version = 1
        product = 'LocalSearch'
        version = 'test'
        git_commit = $Commit
        dirty_tree = $false
        build_elevated = $false
        profile = 'release'
        authenticode_signed = $false
        files = @($Entries)
    }
    $Manifest | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath (Join-Path $Bundle 'manifest.json') -Encoding utf8

    $Verified = Test-LocalSearchBundle -BundlePath $Bundle
    Assert-Equal 1 $Verified.schema_version 'Valid fixture bundle was not accepted'
    Assert-Fails { Test-LocalSearchBundle -BundlePath $Bundle -RequireAuthenticodeSignature } `
        'Unsigned fixture passed signature enforcement'

    $Tampered = Join-Path $Bundle 'localsearch-agent.exe'
    Add-Content -LiteralPath $Tampered -Value 'tamper' -Encoding utf8
    Assert-Fails { Test-LocalSearchBundle -BundlePath $Bundle } `
        'Tampered bundle passed hash verification'
    Set-Content -LiteralPath $Tampered -Value 'fixture:localsearch-agent.exe' -Encoding utf8

    Assert-Fails {
        Assert-LocalSearchSafeDirectory -Path ([IO.Path]::GetPathRoot($TestRoot)) -Purpose test
    } 'Filesystem root passed destructive-directory policy'
    Assert-Fails {
        Assert-LocalSearchVolumeRoot -Path $TestRoot
    } 'Non-volume path passed observation-root policy'

    $VolumeRoot = [IO.Path]::GetPathRoot($TestRoot)
    Assert-Fails {
        New-LocalSearchInstallPlan -BundlePath $Bundle -InstallRoot $Install -StateRoot $State `
            -AuthorizedLogonSid $Sid -EnableBrokerObservation
    } 'Broker observation without explicit roots passed planning'
    $Plan = New-LocalSearchInstallPlan -BundlePath $Bundle -InstallRoot $Install -StateRoot $State `
        -AuthorizedLogonSid $Sid -EnableBrokerObservation -ObserveRoot $VolumeRoot -EnableContent
    Assert-Equal $VolumeRoot $Plan.observed_roots[0] 'Observed volume root was not preserved'
    Assert-True ($Plan.agent.arguments -contains '--content-index') `
        'Content index was not included in the Agent plan'

    Assert-Equal '""' (ConvertTo-LocalSearchCommandArgument -Value '') `
        'Empty command argument quoting is wrong'
    Assert-Equal '"a b"' (ConvertTo-LocalSearchCommandArgument -Value 'a b') `
        'Whitespace command argument quoting is wrong'
    Assert-Equal '"a\\"' (ConvertTo-LocalSearchCommandArgument -Value 'a\') `
        'Trailing slash command argument quoting is wrong'
    Assert-Equal '"a\"b"' (ConvertTo-LocalSearchCommandArgument -Value 'a"b') `
        'Quote escaping is wrong'

    Write-LocalSearchMarker -Root $Install -Kind install -OwnerSid $Sid -GitCommit $Commit
    Test-LocalSearchMarker -Root $Install -Kind install -OwnerSid $Sid | Out-Null
    Copy-Item -LiteralPath (Join-Path $Install '.localsearch-install.json') `
        -Destination (Join-Path $WrongRoot '.localsearch-install.json')
    Assert-Fails {
        Test-LocalSearchMarker -Root $WrongRoot -Kind install -OwnerSid $Sid
    } 'A copied marker authorized a different deletion root'
    Assert-Fails {
        Test-LocalSearchMarker -Root $Install -Kind install -OwnerSid 'S-1-5-18'
    } 'A marker authorized the wrong owner SID'

    $AclRoot = Join-Path $TestRoot 'private-acl-state'
    New-Item -ItemType Directory -Path $AclRoot | Out-Null
    Set-LocalSearchPrivateStateAcl -Root $AclRoot -OwnerSid $Sid
    $AppliedAcl = Get-Acl -LiteralPath $AclRoot
    Assert-Equal $true $AppliedAcl.AreAccessRulesProtected 'State ACL still inherits broad rules'
    $AppliedSids = @($AppliedAcl.Access | ForEach-Object {
        $_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
    } | Sort-Object -Unique)
    $ExpectedSids = @('S-1-5-18', $Sid) | Sort-Object -Unique
    Assert-Equal ($ExpectedSids -join ',') ($AppliedSids -join ',') `
        'State ACL grants an unexpected identity'

    Set-Content -LiteralPath (Join-Path $State 'TOP-SECRET-file-name.txt') `
        -Value 'TOP-SECRET-query-and-content' -Encoding utf8
    $Diagnostics = New-LocalSearchDiagnosticsDocument -InstallRoot $Install -StateRoot $State
    $DiagnosticsJson = $Diagnostics | ConvertTo-Json -Depth 10 -Compress
    Assert-True (-not $DiagnosticsJson.Contains('TOP-SECRET')) `
        'Diagnostics leaked a filename, query, or content fixture'
    Assert-Equal $false $Diagnostics.privacy.paths_included 'Diagnostics path policy is wrong'
    Assert-Equal $false $Diagnostics.privacy.filenames_included 'Diagnostics filename policy is wrong'

    $InstallPlanJson = & (Join-Path $PSScriptRoot 'install-windows.ps1') -BundlePath $Bundle `
        -InstallRoot $Install -StateRoot $State -AuthorizedLogonSid $Sid `
        -AllowUnsignedDevelopmentBundle -PlanOnly
    $InstallPlan = $InstallPlanJson | ConvertFrom-Json
    Assert-Equal $false $InstallPlan.signature_required `
        'Development plan did not record its unsigned exception'
    Assert-Fails {
        & (Join-Path $PSScriptRoot 'install-windows.ps1') -BundlePath $Bundle `
            -InstallRoot $Install -StateRoot $State -AuthorizedLogonSid $Sid -PlanOnly
    } 'Installer accepted an unsigned bundle without the explicit development exception'

    foreach ($Retention in @('KeepIndexes', 'RemoveIndexes')) {
        $UninstallPlanJson = & (Join-Path $PSScriptRoot 'uninstall-windows.ps1') `
            -InstallRoot $Install -StateRoot $State -AuthorizedLogonSid $Sid `
            -Retention $Retention -PlanOnly
        $UninstallPlan = $UninstallPlanJson | ConvertFrom-Json
        Assert-Equal $Retention $UninstallPlan.retention 'Uninstall retention plan changed'
    }

    $DiagnosticsOutput = Join-Path $TestRoot 'diagnostics\report.json'
    & (Join-Path $PSScriptRoot 'export-diagnostics.ps1') -OutputPath $DiagnosticsOutput `
        -InstallRoot $Install -StateRoot $State
    $Exported = Get-Content -LiteralPath $DiagnosticsOutput -Raw -Encoding utf8
    Assert-True (-not $Exported.Contains('TOP-SECRET')) 'Exported diagnostics leaked private data'

    $Scripts = @(Get-ChildItem -LiteralPath $PSScriptRoot -File |
        Where-Object Extension -in @('.ps1', '.psm1'))
    foreach ($Script in $Scripts) {
        $Tokens = $null
        $Errors = $null
        [void][Management.Automation.Language.Parser]::ParseFile(
            $Script.FullName, [ref]$Tokens, [ref]$Errors
        )
        Assert-Equal 0 @($Errors).Count "PowerShell parser rejected $($Script.Name)"
    }

    Write-Host 'Windows package contracts: PASS'
    Write-Host 'bundle signatures/hashes, safe roots, private ACL, markers, plans, retention, diagnostics: PASS'
} finally {
    $ResolvedRoot = [IO.Path]::GetFullPath($TestRoot)
    $ExpectedPrefix = $TemporaryParent + [IO.Path]::DirectorySeparatorChar
    $Leaf = Split-Path -Leaf $ResolvedRoot
    if ($ResolvedRoot.StartsWith($ExpectedPrefix, [StringComparison]::OrdinalIgnoreCase) -and
        $Leaf -match '^localsearch-package-tests-\d+-[0-9a-f]{32}$' -and
        (Test-Path -LiteralPath $ResolvedRoot)) {
        Remove-Item -LiteralPath $ResolvedRoot -Recurse -Force
    }
}
