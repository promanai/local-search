Set-StrictMode -Version Latest

function ConvertTo-Start010ProcessArgument {
    param([AllowEmptyString()][string]$Argument)

    if ($Argument.Length -gt 0 -and $Argument -notmatch '[\s"]') {
        return $Argument
    }

    $Builder = [Text.StringBuilder]::new()
    [void]$Builder.Append('"')
    $Backslashes = 0
    foreach ($Character in $Argument.ToCharArray()) {
        if ($Character -eq [char]92) {
            $Backslashes++
            continue
        }
        if ($Character -eq [char]34) {
            if ($Backslashes -gt 0) {
                [void]$Builder.Append(('\' * ($Backslashes * 2)))
            }
            [void]$Builder.Append('\"')
        }
        else {
            if ($Backslashes -gt 0) {
                [void]$Builder.Append(('\' * $Backslashes))
            }
            [void]$Builder.Append($Character)
        }
        $Backslashes = 0
    }
    if ($Backslashes -gt 0) {
        [void]$Builder.Append(('\' * ($Backslashes * 2)))
    }
    [void]$Builder.Append('"')
    return $Builder.ToString()
}

function Invoke-Start010JsonProcess {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]
        [string]$Executable,
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [string[]]$Arguments,
        [ValidateRange(100, 300000)]
        [int]$TimeoutMilliseconds = 10000
    )

    $Info = [Diagnostics.ProcessStartInfo]::new()
    $Info.FileName = $Executable
    $Info.Arguments = (($Arguments | ForEach-Object {
        ConvertTo-Start010ProcessArgument $_
    }) -join ' ')
    $Info.UseShellExecute = $false
    $Info.CreateNoWindow = $true
    $Info.RedirectStandardOutput = $true
    $Info.RedirectStandardError = $true

    $Process = [Diagnostics.Process]::new()
    $Process.StartInfo = $Info
    try {
        if (-not $Process.Start()) {
            throw [InvalidOperationException]::new('bounded child process did not start')
        }
        $StdoutTask = $Process.StandardOutput.ReadToEndAsync()
        $StderrTask = $Process.StandardError.ReadToEndAsync()
        if (-not $Process.WaitForExit($TimeoutMilliseconds)) {
            try {
                $Process.Kill()
                $Process.WaitForExit()
            }
            catch {
                # The process may have exited between the timeout and the bounded kill.
            }
            throw [TimeoutException]::new('bounded child process exceeded its supervisor deadline')
        }
        $Process.WaitForExit()
        $Stdout = $StdoutTask.GetAwaiter().GetResult()
        [void]$StderrTask.GetAwaiter().GetResult()
        if ($Process.ExitCode -ne 0) {
            throw [InvalidOperationException]::new(
                "bounded child process failed with exit code $($Process.ExitCode)"
            )
        }
        try {
            return $Stdout | ConvertFrom-Json -ErrorAction Stop
        }
        catch {
            throw [IO.InvalidDataException]::new('bounded child process returned invalid JSON')
        }
    }
    finally {
        $Process.Dispose()
    }
}

Export-ModuleMember -Function Invoke-Start010JsonProcess
