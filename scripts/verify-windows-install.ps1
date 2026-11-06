# Verify a Windows dsd installation from a process outside the caller's
# process tree, then print the evidence. See docs/release.md.
#
# A shell spawned by a packaged (MSIX) desktop app can see redirected files
# that no other process can. Without -Out, this script relaunches itself
# through the Windows management service, whose child is not a descendant of
# the calling app, and reports what that independent process observed.
param(
    [string]$BinDirectory = (Join-Path $env:LOCALAPPDATA 'dsd\bin'),
    [string]$ExpectedVersion,
    [string]$Out
)
$ErrorActionPreference = 'Stop'

if (-not $Out) {
    # The independent process does not share this shell's working directory,
    # so resolve a relative directory here.
    $BinDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($BinDirectory)
    # The user profile root is not subject to AppData redirection.
    $Out = Join-Path $env:USERPROFILE ('.dsd-verify-' + [Guid]::NewGuid().ToString('N') + '.txt')
    # The inbox Windows PowerShell host is never a packaged app, unlike the
    # caller's own host when PowerShell came from the Microsoft Store.
    $shell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    if (-not (Test-Path -LiteralPath $shell -PathType Leaf)) { throw "Windows PowerShell was not found at $shell." }
    # Windows argument parsing reads a backslash before a closing quote as an
    # escaped quote, so double any trailing backslashes (for example 'D:\').
    $quoted = @($shell, $PSCommandPath, $BinDirectory, $ExpectedVersion, $Out) |
        ForEach-Object { '"' + ($_ -replace '(\\+)$', '$1$1') + '"' }
    $command = '{0} -NoProfile -ExecutionPolicy Bypass -File {1} -BinDirectory {2} -ExpectedVersion {3} -Out {4}' -f $quoted
    $created = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $command }
    if ($created.ReturnValue -ne 0) { throw "Could not start an independent process (Win32_Process.Create returned $($created.ReturnValue))." }
    try {
        $deadline = (Get-Date).AddSeconds(60)
        while ((Get-Date) -lt $deadline -and (Get-Process -Id $created.ProcessId -ErrorAction SilentlyContinue)) {
            Start-Sleep -Milliseconds 250
        }
        if (-not (Test-Path -LiteralPath $Out -PathType Leaf)) { throw 'The independent process produced no report.' }
        $report = @(Get-Content -LiteralPath $Out)
    } finally {
        Remove-Item -LiteralPath $Out -Force -ErrorAction SilentlyContinue
    }
    $report
    if ($report[-1] -ne 'result: PASS') { exit 1 }
    exit 0
}

$lines = New-Object System.Collections.Generic.List[string]
$failures = 0
function Say([string] $text) { $lines.Add($text) }
function Check([string] $name, [bool] $ok, [string] $detail) {
    if (-not $ok) { $script:failures++ }
    Say ("{0}: {1}{2}" -f $name, $(if ($ok) { 'ok' } else { 'FAILED' }), $(if ($detail) { " ($detail)" } else { '' }))
}

try {
    Say "time: $((Get-Date).ToUniversalTime().ToString('o'))"
    Say "host: PowerShell $($PSVersionTable.PSVersion) on $([Environment]::OSVersion.VersionString)"
    $ancestry = @()
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$PID"
    $packaged = $false
    for ($depth = 0; $depth -lt 12 -and $current; $depth++) {
        $ancestry += "$($current.Name)($($current.ProcessId))"
        if ($current.ExecutablePath -like '*\WindowsApps\*') { $packaged = $true }
        $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($current.ParentProcessId)" -ErrorAction SilentlyContinue
    }
    Say "process ancestry: $($ancestry -join ' <- ')"
    Check 'independent of packaged apps' (-not $packaged) ''

    $exe = [IO.Path]::GetFullPath((Join-Path $BinDirectory 'dsd.exe'))
    $visible = Test-Path -LiteralPath $exe -PathType Leaf
    Check 'file visible' $visible $exe
    if ($visible) {
        Say "sha256: $((Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant())"
        $null = & $exe --help
        Check 'full-path --help' ($LASTEXITCODE -eq 0) "exit $LASTEXITCODE"
        $version = (& $exe --version) -join ' '
        $versionOk = $LASTEXITCODE -eq 0 -and (-not $ExpectedVersion -or $version -eq "dsd $ExpectedVersion")
        Check 'full-path --version' $versionOk $version
    }

    # Resolve the command as a newly opened terminal would, from the persisted PATH.
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
    $commands = @(Get-Command dsd -All -ErrorAction SilentlyContinue)
    Say "Get-Command dsd -All: $(($commands | ForEach-Object { "$($_.CommandType):$($_.Source)" }) -join ' | ')"
    $first = $commands | Select-Object -First 1
    $resolves = $null -ne $first -and $first.CommandType -eq 'Application' -and $first.Source -ieq $exe
    Check 'command resolves to this executable' $resolves ''
    if ($resolves) {
        $null = dsd --help
        Check 'command-name --help' ($LASTEXITCODE -eq 0) "exit $LASTEXITCODE"
    }
} catch {
    $failures++
    Say "error: $($_.Exception.Message)"
}
Say ("result: {0}" -f $(if ($failures -eq 0) { 'PASS' } else { 'FAIL' }))
[IO.File]::WriteAllLines($Out, $lines)
