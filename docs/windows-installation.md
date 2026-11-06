# Windows installation and recovery

Use the [README installation procedure](../README.md#windows) for a normal
installation. This guide covers filesystem visibility and command discovery
when a Windows installation appears successful but `dsd` cannot be found.
Release certification requirements belong in the [release runbook](release.md).

## Choose a visible installation directory

Prefer an independently launched PowerShell session over a shell spawned by a
packaged desktop app. MSIX apps can redirect `%LOCALAPPDATA%` writes to private
storage: a file may appear installed inside that process while an independent
terminal cannot see it at the advertised path.

The installer checks a temporary file's physical handle path before replacing
an existing executable. If the physical and advertised paths differ, it stops
and reports both paths. Do not add an invisible or redirected path to `PATH`.
Run the installer from an independent PowerShell session, or choose a directory
that is not redirected. To confirm visibility without opening another terminal,
run [`scripts/verify-windows-install.ps1`](../scripts/verify-windows-install.ps1)
with `-BinDirectory`; it checks from a process outside the calling app.

The default `%LOCALAPPDATA%\dsd\bin` is suitable in a normal independent
PowerShell session. MSIX redirection applies to `AppData`, not to the user
profile root, so `%USERPROFILE%\.local\bin` is a recovery location for that
case; it is not required on every Windows system. To choose it explicitly:

```powershell
$DsdBin = "$env:USERPROFILE\.local\bin"
.\install.ps1 -Archive ".\dsd-<version>-win32-x64.tar.gz" -Checksums ".\SHA256SUMS" -BinDirectory $DsdBin
& "$DsdBin\dsd.exe" --version
```

Pass the same `-BinDirectory` when reinstalling later. `dsd upgrade` replaces
the executable in whatever directory holds its `.dsd-receipt.json`.

## Verify the file before changing PATH

Installation, visibility from another process, persistent user `PATH`, and the
current shell's `PATH` are separate checks. In an independent terminal, replace
`$DsdBin` with the actual directory passed to `-BinDirectory` (or the default
installation directory):

```powershell
$DsdBin = "$env:USERPROFILE\.local\bin"
$DsdExe = Join-Path $DsdBin "dsd.exe"
$visible = Test-Path -LiteralPath $DsdExe -PathType Leaf
$visible
if (-not $visible) { throw "Not visible: $DsdExe" }
& $DsdExe --help
```

If `Test-Path` returns `False`, resolve the installation location or process
visibility first. Changing `PATH` cannot make that file visible.

## Add the directory to the user PATH

Set `$DsdBin` to the verified installation directory. This preserves existing
entries and avoids adding a duplicate, ignoring case and trailing backslashes.
It does not modify a PowerShell profile.

```powershell
$DsdBin = "$env:USERPROFILE\.local\bin"
$binKey = $DsdBin.TrimEnd("\")
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$userEntries = if ([string]::IsNullOrWhiteSpace($userPath)) { @() } else { @($userPath -split ";") }
$alreadyPresent = $false
foreach ($entry in $userEntries) {
    if ($entry.Trim().TrimEnd("\") -ieq $binKey) { $alreadyPresent = $true; break }
}
if (-not $alreadyPresent) {
    $separator = if ([string]::IsNullOrEmpty($userPath)) { "" } else { ";" }
    [Environment]::SetEnvironmentVariable("Path", "$userPath$separator$DsdBin", "User")
}
```

An already open PowerShell session keeps its inherited `PATH`. Open a new
terminal from a process with the updated environment, restarting the terminal
app if necessary, then [verify command resolution](#verify-command-resolution).

## Use the current PowerShell session

To check command discovery without restarting, update the current shell
separately. Use the same verified installation directory:

```powershell
$DsdBin = "$env:USERPROFILE\.local\bin"
$binKey = $DsdBin.TrimEnd("\")
$currentPath = $env:Path
$currentEntries = @($currentPath -split ";")
$alreadyPresent = $false
foreach ($entry in $currentEntries) {
    if ($entry.Trim().TrimEnd("\") -ieq $binKey) { $alreadyPresent = $true; break }
}
if (-not $alreadyPresent) {
    $env:Path = if ([string]::IsNullOrEmpty($currentPath)) { $DsdBin } else { "$DsdBin;$currentPath" }
}
```

Then [verify command resolution](#verify-command-resolution) in this session.

## Verify command resolution

Set `$DsdBin` to the verified installation directory again in the terminal
being tested. Check the command PowerShell would actually run before invoking
it: another executable earlier in `PATH`, an alias, or a function can shadow
the installed file.

```powershell
$DsdBin = "$env:USERPROFILE\.local\bin"
$DsdExe = [IO.Path]::GetFullPath((Join-Path $DsdBin "dsd.exe"))
$DsdCommand = Get-Command dsd -ErrorAction Stop
if ($DsdCommand.CommandType -ne "Application" -or $DsdCommand.Path -ine $DsdExe) {
    throw "dsd does not resolve to $DsdExe. Run Get-Command dsd -All to inspect conflicting commands."
}
dsd --help
```

Resolve any conflicting command or `PATH` entry and repeat this check. Calling
the executable by its full path verifies filesystem visibility independently
of command-name lookup.
