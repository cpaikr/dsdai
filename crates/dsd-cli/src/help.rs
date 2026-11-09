//! Help text that cannot live in clap attributes directly.

/// `apply` field reference; a test keeps it equal to the library's op table.
pub const APPLY_AFTER: &str = "\
Edit objects (required fields first; ? marks optional ones):
  set-cell   table: int, row: int, col: int, text: str, unit_value?: str, flatten?: bool
  replace    old: str, new: str, expect?: int|\"any\", table?: int, except?: [str]
  copy-row   table: int, row: int, count?: int, clear?: bool, force_locked?: bool
  delete-row table: int, row: int, force_locked?: bool
  copy-col   table: int, col: int, count?: int, clear?: bool, force_locked?: bool
  delete-col table: int, col: int, force_locked?: bool
  copy-table table: int, count?: int, clear?: bool, force_locked?: bool
  delete-table table: int, force_locked?: bool
  insert-paragraph text: str, after_table?: int, before_table?: int, after_line?: int
  delete-paragraph line: int

Edits run in order and later ones see earlier results: row, column, table,
and line numbers refer to the document after earlier insertions or deletions,
so list them bottom-up or account for the shift. Nothing is written unless
every edit succeeds.

Example:
  [{\"op\": \"set-cell\", \"table\": 3, \"row\": 4, \"col\": 2, \"text\": \"1,100,000\"},
   {\"op\": \"replace\", \"old\": \"2024년\", \"new\": \"2025년\", \"expect\": 2}]";

pub const VERSION_AFTER: &str = "\
Without --check, version is local: the embedded version and release target
(null for builds that are not a release target). `dsd --version` prints
`dsd <version>` as plain text.

--check adds result.releaseCheck: the comparison with the latest stable GitHub
release (newer, equal, ahead, no_stable_release, uncomparable), whether that
release has an archive for this target, and whether the evidence is fresh,
stale, or unavailable. result.nextStep says how to install a newer release.
Evidence problems are listed in releaseCheck.problems and still exit 0.

Release evidence is fetched from github.com and GitHub's release asset host,
cached for 24 hours, and a failed refresh is retried after 1 hour; a refresh
takes at most 1.5 seconds. upgrade and upgrade --check replace the cached
evidence with the release they fetch. DSD_CACHE_DIR sets an absolute cache
directory.
DSD_NO_UPDATE_CHECK=1 disables --check and incidental advisories, not upgrade.
version never installs anything; see `dsd upgrade`.";

pub const UPGRADE_AFTER: &str = "\
Manages only an executable whose bytes match the .dsd-receipt.json receipt the
release installers write beside it; anything else fails with
unmanaged_installation or upgrade_receipt_mismatch and is left unchanged.

--check reports runningVersion, latestVersion, updateAvailable, distribution,
and nextStep without downloading an archive. upgrade downloads this target's
archive, verifies it against the release manifest and SHA256SUMS, requires it
to hold only the executable and license, and requires the new executable to report the
release version. The executable is replaced before its receipt; on failure the
previous installation is restored, or error.hint names the remaining step.

Upgrades contact github.com and GitHub's release asset host; one runs per
installation at a time (.dsd-upgrade.lock). On Windows the replaced executable
stays as .dsd-old-<pid>.exe until a later upgrade removes it. In a terminal,
stages are written to stderr; otherwise stderr stays empty.";

#[cfg(test)]
mod tests {
    #[test]
    fn apply_help_lists_the_library_ops() {
        for line in dsd::edit::op_reference() {
            assert!(super::APPLY_AFTER.contains(&format!("  {line}\n")), "{line}");
        }
    }
}
