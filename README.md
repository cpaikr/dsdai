# dsdai

`dsd` reads, checks, and surgically edits Korean DART Editor `.dsd` documents
(감사보고서, 검토보고서, 사업·분기·반기보고서 drafts), and the `dart-dsd` agent
skill teaches an agent to use it. Edits replace exact byte ranges of
`contents.xml` and rewrite the archive with DART Editor's original member
order, CP949 name bytes, flags, and timestamps; untouched members keep their
compressed bytes, so an unchanged `extract`/`pack` round trip is byte-identical.
DART Editor stays the authority for layout and filing.

| Path | Purpose |
|---|---|
| [skills/dart-dsd/](skills/dart-dsd/SKILL.md) | The agent skill: routing, [reading](skills/dart-dsd/references/reading.md), [editing](skills/dart-dsd/references/editing.md), [format reference](skills/dart-dsd/references/format.md) |
| [crates/dsd](crates/dsd/) | The format library |
| [crates/dsd-cli](crates/dsd-cli/) | The `dsd` executable |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Boundaries, invariants, and the process contract |
| [docs/release.md](docs/release.md) | Release runbook |
| `fixtures/public/` | Synthetic fixture; `sample.dsd` is built from `sample/` by the Rust tests (`DSD_UPDATE_FIXTURES=1 cargo test` rewrites it) |
| `fixtures/private/` | Git-ignored real samples and DART schemas for local checks only |

## Commands

- **Read:** `inspect` (no document text), `outline`, `read` (exact text of
  every block), `table` (cells with `R#`/`C#`), `find`
- **Edit:** `set-cell`, `replace`, rows (`copy-row`, `delete-row`), columns
  (`copy-col`, `delete-col`), tables (`copy-table`, `delete-table`),
  paragraphs (`insert-paragraph`, `delete-paragraph`), `apply` (JSON batch),
  and `extract`/`pack` for hand edits of `contents.xml`. Structural edits keep
  spans and sizes consistent the way DART Editor writes them.
- **Verify:** `check` (structure, images, TOC, table grids, unit values, and
  validation against an installed DART schema), `diff` (one file against its
  edit, by position)
- **Compare:** `compare` aligns two documents by content, even across forms
  and reordered tables, and reports table, row, column, cell (with numeric
  deltas), and wording differences as JSON, text, or a self-contained
  side-by-side HTML report
- **Export:** `xlsx` writes an Excel workbook for reading and footing: an
  index sheet, then one sheet per statement, per note, and per other section,
  with every table on its grid (spans merged) and figures as numbers only
  where Excel shows exactly the source text; each table's range is the
  defined name `DSD_T<n>`
- **Maintain:** `version [--check]`, `upgrade [--check]`

Every command prints one JSON object to stdout,
`{ok, command, result, warnings, error, metadata}`, and exits 0 on success,
1 on failure (including `check` finding errors), or 2 for an invalid
invocation. `outline`, `read`, `table`, `find`, `diff`, and `compare` also
take `--format text` for a compact listing; `compare --format html -o FILE`
writes the report. `dsd --help` and `dsd <command> --help`
document every option.

```sh
dsd find report.dsd 현금및현금성자산 --format text
dsd set-cell report.dsd -o report_수정.dsd --table 3 --row 4 --col 2 --text 1,100,000
dsd check report_수정.dsd --against report.dsd
dsd diff report.dsd report_수정.dsd --format text
dsd compare filed.dsd draft.dsd --format html -o compare.html
dsd xlsx report.dsd -o report.xlsx
```

Schema validation reads the DART schema installed with DART Editor
(`C:\DART\Resource\Dtd`, or `--schema-dir` / `DART_SCHEMA_DIR`); the tool
ships no schema text. `.dsd` files are usually unreleased client filings:
`dsd` works locally and never uploads them; its only network use is the
release check described under [Updates](#updates).

## Development

```sh
python scripts/check.py
```

Validates the skill package, runs `cargo fmt`, `clippy`, and the Rust tests
(with the test-only `fixture-origin` feature for release-check and upgrade
tests), the release tooling's unit tests, and a CLI smoke run (check, complete
read, outline, byte-identical extract/pack, and an xlsx export reread cell by
cell and line by line) over the public fixture and, when
`fixtures/private/` exists, each private sample without printing document
content. Building needs Rust 1.88 or newer; `cargo build --release -p dsd-cli`
produces `target/release/dsd`.

## Installation

These instructions apply to releases from
[v0.1.0](https://github.com/cpaikr/dsdai/releases/tag/v0.1.0), the first; the latest is
[v0.1.2](https://github.com/cpaikr/dsdai/releases/tag/v0.1.2).

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
redirected destination and does not change `PATH`. When it refuses a first
installation, set `$bin = "$env:USERPROFILE\.local\bin"` and run `install.ps1`
again ([why](docs/windows-installation.md#choose-a-visible-installation-directory)).
When it refuses an update, rerun it into the same directory from an
independently launched PowerShell session instead: installing elsewhere would
leave the old copy and its receipt in place. To use another directory, set
`$bin` before running the commands above; if `dsd` should not be on `PATH`,
skip the steps below and run it by its full path. After a first
installation, [add `$bin` to the user `PATH`](docs/windows-installation.md#add-the-directory-to-the-user-path),
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
a command. `upgrade` and `upgrade --check` replace the cached evidence with
the release they fetch, so a following check needs no refresh. The cache is
disposable and lives in `%LOCALAPPDATA%\dsd\cache` on Windows,
`~/Library/Caches/dsd` on macOS, and `$XDG_CACHE_HOME/dsd` or `~/.cache/dsd`
on Linux; set `DSD_CACHE_DIR` to an absolute path to override
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

The skill is separate from the executable. Install `skills/dart-dsd/` from
this repository's published `skills/` subpath with your agent's skill
installer (for example `sjskills`); it needs a working `dsd` on `PATH` and
routes to this guide when one is missing. This repository's own
`.agents/skills/` and `.claude/skills/` placements are generated by `sjskills`
from `sjskills.toml`.

## License

[Elastic License 2.0](LICENSE.md).
