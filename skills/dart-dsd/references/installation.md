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

Verify `dsd --version` first through the executable's full path, then by
command name in the shell that will run it: file visibility and `PATH`
lookup are separate problems. On Windows, a shell started by a desktop app may
not see files other programs see; use the guide's
`verify-windows-install.ps1` check, which tests from an independent process,
and its recovery steps for redirected paths or a stale `PATH`.

In a sandbox without network access, set `DSD_NO_UPDATE_CHECK=1` so commands
do not report `update_check_unavailable`.

After verification, resume the original task.
