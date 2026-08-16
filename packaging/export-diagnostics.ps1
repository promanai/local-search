[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputPath,
    [string]$InstallRoot = "$env:ProgramFiles\LocalSearch",
    [string]$StateRoot = "$env:LOCALAPPDATA\LocalSearch"
)

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'LocalSearch.Package.psm1') -Force
$Output = [IO.Path]::GetFullPath($OutputPath)
$Parent = Split-Path -Parent $Output
if (-not $Parent) { throw 'OutputPath must have a parent directory' }
New-Item -ItemType Directory -Path $Parent -Force | Out-Null
$Document = New-LocalSearchDiagnosticsDocument -InstallRoot $InstallRoot -StateRoot $StateRoot
$Document | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $Output -Encoding utf8
Write-Host "Redacted LocalSearch diagnostics: $Output"
