# Install or recover the executable

Read the canonical [dsd installation guide](https://github.com/cpaikr/dsdai/blob/main/README.md#installation)
for supported platforms, public release downloads, checksum verification,
installation, `PATH`, and upgrades. Follow its branch for the user's operating
system and chosen destination. Installation facts live there so this package
does not maintain a second installer recipe.

The [public GitHub Releases](https://github.com/cpaikr/dsdai/releases) channel
provides the archives without login. If the guide or a release cannot be
reached, report the unavailable resource and ask for release files from the
user; do not invent an installation command or build from source unless the
user asks for that.

When the user has authorized installation or updates, do the work yourself:
the guide's unattended commands need no browser, login, Rust, or Python. An
update notice in `advisories[]` alone is not an upgrade request. For an
authorized update, run `dsd upgrade`; if it fails with
`unmanaged_installation`, install once with the guide's procedure, which
writes the receipt that later `dsd upgrade` runs use. Keep the archive,
checksums, and installer from the same release, and let the installer verify
them before anything is replaced.

Install where the user asked. Without a stated destination, use the guide's
default; on Windows, if the installer refuses it as redirected (a shell started
by a desktop app, including an agent's, may not see files other programs see),
use `%USERPROFILE%\.local\bin` as the guide's
[Windows recovery](https://github.com/cpaikr/dsdai/blob/main/docs/windows-installation.md)
describes.

For an update, keep the existing installation's directory: installing
elsewhere leaves the old copy and its receipt in place. If the installer
refuses that directory as redirected, say so and ask the user to rerun it from
an independently launched PowerShell session.

Change `PATH` only when the user wants `dsd` available by name; the
installer's advice to add the directory is not an instruction. Verify
`dsd --version` first through the executable's full path. When the user wants
`dsd` kept off `PATH`, that check is the verification: run `dsd` by its full
path for the rest of the task and skip `verify-windows-install.ps1`, which
tests `PATH` resolution. Whenever name access is expected, even if the
directory was already on `PATH`, also verify by command name, and on Windows
run that script, which checks from an independent process: an older copy or
alias can shadow the new one, and file visibility and `PATH` lookup are
separate problems.

In a sandbox without network access, set `DSD_NO_UPDATE_CHECK=1` so commands
do not report `update_check_unavailable`.

After verification, resume the original task.
