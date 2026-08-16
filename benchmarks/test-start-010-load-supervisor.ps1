[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'Start010LoadSupervisor.psm1') -Force
$PowerShell = (Get-Process -Id $PID).Path

$Success = Invoke-Start010JsonProcess -Executable $PowerShell -Arguments @(
    '-NoProfile',
    '-NonInteractive',
    '-Command',
    '[Console]::Out.Write(''{"ok":true,"value":"argument with spaces"}'')'
) -TimeoutMilliseconds 5000
if (-not $Success.ok -or $Success.value -ne 'argument with spaces') {
    throw 'Supervisor corrupted successful JSON output or quoted arguments'
}

$InvalidJsonRejected = $false
try {
    Invoke-Start010JsonProcess -Executable $PowerShell -Arguments @(
        '-NoProfile', '-NonInteractive', '-Command', '[Console]::Out.Write(''not-json'')'
    ) -TimeoutMilliseconds 5000
}
catch [IO.InvalidDataException] {
    $InvalidJsonRejected = $true
}
if (-not $InvalidJsonRejected) {
    throw 'Supervisor accepted invalid JSON output'
}

$TimeoutRejected = $false
$Timer = [Diagnostics.Stopwatch]::StartNew()
try {
    Invoke-Start010JsonProcess -Executable $PowerShell -Arguments @(
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        'Start-Sleep -Seconds 5; [Console]::Out.Write(''{"late":true}'')'
    ) -TimeoutMilliseconds 200
}
catch [TimeoutException] {
    $TimeoutRejected = $true
}
$Timer.Stop()
if (-not $TimeoutRejected) {
    throw 'Supervisor failed to reject a child process that exceeded its deadline'
}
if ($Timer.ElapsedMilliseconds -gt 3000) {
    throw 'Supervisor timeout was not bounded'
}

Write-Host "START-010-L supervisor self-test: PASS ($($Timer.ElapsedMilliseconds) ms timeout path)"
