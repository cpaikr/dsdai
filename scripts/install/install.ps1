# Install dsd @@VERSION@@ from a downloaded release archive.
# Download this installer, your archive, and SHA256SUMS from the same release:
# https://github.com/cpaikr/dsdai/releases/tag/v@@VERSION@@
param(
    [Parameter(Mandatory=$true)][string]$Archive,
    [Parameter(Mandatory=$true)][string]$Checksums,
    [string]$BinDirectory = (Join-Path $env:LOCALAPPDATA 'dsd\bin')
)
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT' -or
    [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -ne 'X64') {
    throw 'This installer requires Windows x64.'
}
if (-not ("DsdInstallerNative" -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

public static class DsdInstallerNative
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandle(
        SafeFileHandle hFile,
        StringBuilder lpszFilePath,
        uint cchFilePath,
        uint dwFlags);

    public static string GetFinalPath(string path)
    {
        using (var stream = new FileStream(path, FileMode.Open, FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete))
        {
            var capacity = 512;
            while (true)
            {
                var buffer = new StringBuilder(capacity);
                var length = GetFinalPathNameByHandle(stream.SafeFileHandle, buffer,
                    (uint)buffer.Capacity, 0);
                if (length == 0)
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(),
                        "GetFinalPathNameByHandle failed.");
                }
                if (length < buffer.Capacity)
                {
                    return buffer.ToString();
                }
                capacity = checked((int)length + 1);
            }
        }
    }
}
'@
}

function ConvertToComparableWindowsPath([string] $Path) {
    if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        return '\\' + $Path.Substring(8)
    }
    if ($Path.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) {
        return $Path.Substring(4)
    }
    return [IO.Path]::GetFullPath($Path)
}

# A shell started by a packaged (MSIX) app can redirect writes to private
# storage that other programs cannot see; refuse such a destination.
function AssertInstallationPathIsVisible([string] $Path) {
    $probe = Join-Path $Path ('.dsd-visibility-' + [Guid]::NewGuid().ToString('N'))
    try {
        [IO.File]::WriteAllText($probe, 'dsd installer visibility probe')
        try {
            $advertised = ConvertToComparableWindowsPath $probe
            $physical = ConvertToComparableWindowsPath ([DsdInstallerNative]::GetFinalPath($probe))
        } catch {
            throw "Could not verify the physical installation path for '$Path': $($_.Exception.Message) Run this installer from an independently launched normal PowerShell session, or choose a local filesystem directory with -BinDirectory."
        }
        if (-not [String]::Equals($advertised, $physical, [StringComparison]::OrdinalIgnoreCase)) {
            throw @"
The selected installation directory is redirected or otherwise resolves to a different physical path.
Advertised path: $advertised
Physical path:   $physical
No existing installation was changed. Run this installer from an independently launched normal PowerShell session, or rerun it with -BinDirectory `"$env:USERPROFILE\.local\bin`" (or another non-redirected directory).
Do not add the advertised path to PATH unless an independent terminal can see that exact path.
"@
        }
    } finally {
        Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue
    }
}

$Archive = (Resolve-Path -LiteralPath $Archive).Path
$archiveName = '@@WINDOWS_ARCHIVE@@'
if ([IO.Path]::GetFileName($Archive) -ne $archiveName) { throw "Expected $archiveName for this host." }
$matches = @(Get-Content -LiteralPath $Checksums | Where-Object { $_ -match ('^[0-9a-f]{64}  ' + [regex]::Escape($archiveName) + '$') })
if ($matches.Count -ne 1) { throw 'Missing or duplicate archive checksum.' }
$expected = $matches[0].Substring(0, 64)
if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
    throw 'Archive checksum mismatch; existing installation was not changed.'
}
# Use the Windows-provided tar: another tar earlier in PATH, such as Git's,
# can misread drive-letter paths.
$tar = Join-Path $env:SystemRoot 'System32\tar.exe'
if (-not (Test-Path -LiteralPath $tar -PathType Leaf)) { throw "This installer requires the Windows-provided tar at $tar." }
# The archive holds exactly one regular file: the executable.
$entries = @(& $tar -tzf $Archive)
if ($LASTEXITCODE -ne 0 -or ($entries -join "`n") -ne 'dsd.exe') { throw 'Unexpected archive contents.' }
$types = @(& $tar -tvzf $Archive)
if ($LASTEXITCODE -ne 0 -or @($types | Where-Object { -not $_.StartsWith('-') }).Count -ne 0) { throw 'Archive must contain only regular files.' }
$null = New-Item -ItemType Directory -Path $BinDirectory -Force
$BinDirectory = (Resolve-Path -LiteralPath $BinDirectory).Path
AssertInstallationPathIsVisible $BinDirectory
$stage = Join-Path $BinDirectory ('.dsd-install-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $stage
try {
    & $tar -xzf $Archive -C $stage
    if ($LASTEXITCODE -ne 0) { throw 'Archive extraction failed.' }
    $candidate = Join-Path $stage 'dsd.exe'
    $reported = (& $candidate --version) -join ' '
    if ($LASTEXITCODE -ne 0 -or $reported -ne 'dsd @@VERSION@@') {
        throw 'The archived executable does not report dsd @@VERSION@@; existing installation was not changed.'
    }
    $destination = Join-Path $BinDirectory 'dsd.exe'
    # The receipt lets `dsd upgrade` manage exactly this executable.
    $receipt = [ordered]@{
        schemaVersion = 1
        manager = 'standalone'
        version = '@@VERSION@@'
        target = '@@WINDOWS_TARGET@@'
        executable = $destination
        releaseRepository = 'cpaikr/dsdai'
        releaseTag = 'v@@VERSION@@'
        assetName = $archiveName
        sha256 = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $stagedReceipt = Join-Path $stage 'receipt.json'
    [IO.File]::WriteAllText($stagedReceipt, ($receipt | ConvertTo-Json -Compress) + "`n", (New-Object Text.UTF8Encoding($false)))
    if (Test-Path -LiteralPath $destination) {
        # PowerShell converts $null to an empty string for .NET string
        # parameters, which File.Replace rejects as a backup path.
        [IO.File]::Replace($candidate, $destination, [NullString]::Value)
    } else {
        [IO.File]::Move($candidate, $destination)
    }
    $receiptPath = Join-Path $BinDirectory '.dsd-receipt.json'
    try {
        if (Test-Path -LiteralPath $receiptPath) {
            [IO.File]::Replace($stagedReceipt, $receiptPath, [NullString]::Value)
        } else {
            [IO.File]::Move($stagedReceipt, $receiptPath)
        }
    } catch {
        throw "dsd @@VERSION@@ was installed, but its upgrade receipt could not be written; rerun the installer. $($_.Exception.Message)"
    }
    Write-Output "Installed dsd @@VERSION@@ to $destination"
    Write-Output "Next, verify from an independent terminal: & `"$destination`" --version"
    Write-Output "Add $BinDirectory to the user PATH if needed; a new terminal is required for persistent PATH changes."
} finally {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
