# dsdai

Agent skill for reading and editing Korean DART Editor `.dsd` documents.

| Path | Purpose |
|---|---|
| [skills/dart-dsd/](skills/dart-dsd/SKILL.md) | The skill: workflow, [format reference](skills/dart-dsd/references/format.md), and the `scripts/dsd.py` helper |
| `fixtures/public/sample.dsd` | Synthetic fixture built from `fixtures/public/sample/` by the Rust tests (`DSD_UPDATE_FIXTURES=1 cargo test` rewrites it) |
| `fixtures/private/` | Git-ignored real samples and DART schemas for local checks only |

## Helper

`skills/dart-dsd/scripts/dsd.py` is a single standard-library Python 3.9+
script:

- **Read:** `inspect`, `outline`, `read` (exact text view), `table`, `find`
- **Edit:** `set-cell`, `replace`, `copy-row`, `delete-row`, `apply`
  (JSON batch), and `extract`/`pack` for hand edits
- **Verify:** `check` (structure, images, TOC, table grids, optional XSD via
  `lxml`), `diff`

Edits replace exact byte ranges in `contents.xml` and rewrite the ZIP with the
original member order, CP949 name bytes, flags, and timestamps; untouched
members keep their original compressed bytes. An unchanged `extract`/`pack`
round trip is byte-identical on real DART Editor files.

## Rust CLI (in progress)

The `dsd` command-line tool in `crates/` is replacing the Python helper:
`crates/dsd` is the library and `crates/dsd-cli` the executable. It keeps the
helper's commands and byte-preservation rules, prints one JSON envelope per
invocation (`--format text` gives the helper's compact listings), and adds
dependency-free validation against an installed DART schema. Until it is
released and the skill moves to it, the skill keeps using `scripts/dsd.py`;
`scripts/parity.py` compares the two implementations.

## Checks

```bash
python scripts/check.py
```

Validates the skill package, runs `cargo fmt`, `clippy`, and the Rust tests
(including the public fixture's freshness against `fixtures/public/sample/`),
runs the Python unit tests, and, when `fixtures/private/` exists, runs
check/read/outline and a byte-identical round trip over each private sample
without printing document content.

## Installation

> **Not yet available.** These instructions apply from the first release,
> v0.1.0, which has not been published. Until then there is no `dsd` release
> to download; build from a checkout with `cargo build --release -p dsd-cli`
> or use `skills/dart-dsd/scripts/dsd.py`.

Download the archive for your platform, `SHA256SUMS`, and the matching
installer from the same [GitHub Release](https://github.com/cpaikr/dsdai/releases).
Installation needs no login, Rust, Python, source checkout, or GitHub CLI.

| Platform | `<target>` | Installer | Requires |
|---|---|---|---|
| Linux GNU x64 | `linux-x64-gnu` | `install.sh` | glibc 2.35 or newer (for example Ubuntu 22.04 or Debian 12) |
| Linux GNU ARM64 | `linux-arm64-gnu` | `install.sh` | glibc 2.35 or newer |
| macOS Apple Silicon | `darwin-arm64` | `install.sh` | macOS 11 or newer |
| Windows x64 | `win32-x64` | `install.ps1` | Windows 10 (1803) or newer, PowerShell 5.1 or newer |

`install.sh` needs `sh`, `tar`, `awk`, and `sha256sum` or `shasum`; the
unattended commands below also use `curl`. Alpine/musl Linux and Intel Macs
are not supported. Every archive is built, tested, and installed on its own
platform before release; the [release runbook](docs/release.md) describes
that certification.

### macOS and Linux

Place `dsd-<version>-<target>.tar.gz`, `SHA256SUMS`, and `install.sh` in the
same directory, then run:

```sh
sh install.sh "dsd-<version>-<target>.tar.gz" SHA256SUMS
"$HOME/.local/bin/dsd" --version
```

Replace `<version>` and `<target>` with the values in the downloaded file name.
The installer checks the host platform, the archive's SHA-256 checksum and
contents, and that the new executable reports the expected version before it
replaces anything. It installs to `$HOME/.local/bin` by default; an optional
third argument selects another directory. Add that directory to `PATH` to run
`dsd` from anywhere, for example in `~/.profile` or your shell's startup file:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Open a new terminal, then check that `dsd --version` prints `dsd <version>`
and `dsd --help` lists the commands.

### Windows

Download `dsd-<version>-win32-x64.tar.gz`, `SHA256SUMS`, and `install.ps1`.
Run the installer in an independently launched PowerShell session; it uses the
Windows-provided `tar` command.

```powershell
.\install.ps1 -Archive ".\dsd-<version>-win32-x64.tar.gz" -Checksums ".\SHA256SUMS"
& "$env:LOCALAPPDATA\dsd\bin\dsd.exe" --version
```

The default directory is `%LOCALAPPDATA%\dsd\bin`; `-BinDirectory` selects
another. Verify that exact path from an independent terminal before adding it
to your user `PATH`: shells launched by packaged desktop apps can redirect
writes to private storage, and the installer refuses such a destination. See
[Windows installation and recovery](docs/windows-installation.md) for path
verification, an alternative directory, and persistent or current-session
`PATH` setup. Follow your organization's script review and execution policy;
if script execution is blocked, run
`powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 ...` instead.

### Unattended installation

Agents and scripts can install or update without a browser, login, or GitHub
CLI. GitHub's latest-release links serve the current release's files, and the
release manifest names its version. On macOS and Linux, set `target` from the
table above:

```sh
base=https://github.com/cpaikr/dsdai/releases/latest/download
target=linux-x64-gnu
version=$(curl -fsSL "$base/release-manifest.json" | sed -n 's/^ *"version": "\(.*\)",$/\1/p')
for name in "dsd-$version-$target.tar.gz" SHA256SUMS install.sh; do curl -fsSLO "$base/$name"; done
sh install.sh "dsd-$version-$target.tar.gz" SHA256SUMS
```

On Windows, in PowerShell:

```powershell
$base = "https://github.com/cpaikr/dsdai/releases/latest/download"
$bin = "$env:LOCALAPPDATA\dsd\bin"
$version = (Invoke-RestMethod "$base/release-manifest.json" -UseBasicParsing).version
$archive = "dsd-$version-win32-x64.tar.gz"
foreach ($name in $archive, "SHA256SUMS", "install.ps1", "verify-windows-install.ps1") { Invoke-WebRequest "$base/$name" -OutFile $name -UseBasicParsing }
.\install.ps1 -Archive ".\$archive" -Checksums ".\SHA256SUMS" -BinDirectory $bin
```

Run these in an empty working directory. The installer still verifies the
checksum, so files from two different releases cannot be combined. To update
an installation elsewhere, pass its directory as the third `install.sh`
argument, or set `$bin` to it on Windows.

On Windows, a shell started by a packaged desktop app, including an agent's
shell, may not see the same files as other programs. The installer refuses a
redirected destination and does not change `PATH`. After a first installation,
[add `$bin` to the user `PATH`](docs/windows-installation.md#add-the-directory-to-the-user-path),
then confirm the result from a process outside the calling app with the
downloaded `verify-windows-install.ps1`:

```powershell
.\verify-windows-install.ps1 -BinDirectory $bin -ExpectedVersion $version
```

It exits nonzero unless an independent process can see and run the executable
and `dsd` resolves to it from the persisted user and machine `PATH`.

### Updates

`dsd --version` prints the installed version. `dsd version --check` compares
it with the latest stable release and reports, in `result.releaseCheck`,
whether that release is newer, equal, or older, whether it has an archive for
your platform, and how fresh the evidence is; `result.nextStep` says how to
install a newer release. Evidence problems are reported and still exit 0.

After other successful commands, `dsd` also checks for a newer stable release
and, when there is something to report, adds one entry to the JSON
`advisories` array, or writes one `dsd notice:` line to stderr after
`--format text` output. The result and exit status never change, and failed or
invalid invocations are never followed by a check. Release checks contact
`github.com` and GitHub's release asset host, cache the evidence for 24 hours,
and retry a failed refresh after 1 hour; a refresh adds at most 1.5 seconds to
a command. The cache is disposable and lives in `%LOCALAPPDATA%\dsd\cache` on
Windows, `~/Library/Caches/dsd` on macOS, and `$XDG_CACHE_HOME/dsd` or
`~/.cache/dsd` on Linux; set `DSD_CACHE_DIR` to an absolute path to override
it. Set `DSD_NO_UPDATE_CHECK=1` to disable all release checks, including
`version --check`; do so in sandboxes without network access or a writable
cache, where every command would otherwise report `update_check_unavailable`.
Release checks never install anything.

The installers write `.dsd-receipt.json` beside the executable, recording its
version, target, path, release, and SHA-256 digest. Keep it there; it lets the
CLI upgrade that exact installation:

```sh
dsd upgrade --check
dsd upgrade
```

`upgrade --check` reports whether a newer stable release exists, and whether
it has an archive for this platform, without downloading it. `upgrade`
downloads this platform's archive from the latest release, verifies it against
the release manifest and `SHA256SUMS`, requires the archive to hold only the
executable and its license, and requires the new executable to report the release version. It
then replaces the executable, followed by its receipt. If a step fails, the
previous installation is restored, or the JSON error's `hint` names the
remaining recovery step. Both commands print one JSON envelope; in a terminal
they also write progress stages to stderr. `DSD_NO_UPDATE_CHECK` does not
disable an explicit upgrade. One upgrade runs per installation at a time,
holding `.dsd-upgrade.lock` beside the executable. On Windows the replaced
executable remains as `.dsd-old-<pid>.exe` until a later upgrade removes it;
if an upgrade is killed mid-replacement and `dsd.exe` is missing, rename that
file back to `dsd.exe` or reinstall.

An executable without a matching receipt, such as one built from source, is
left unchanged: `dsd upgrade` reports `unmanaged_installation`. Update it by
repeating the installation procedure with files from a newer release, which
writes the receipt. Always use an archive, installer, and checksums from the
same release in this repository.

### Agent skill

Install the skill from the published `skills/` subpath, like the skills in
`../agent-scripts`; this repository's own `.agents/skills/` and
`.claude/skills/` placements are generated by `sjskills` from
`sjskills.toml`.

## License

[Elastic License 2.0](LICENSE.md).
