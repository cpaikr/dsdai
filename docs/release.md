# Release

The `dsd` CLI is distributed as prebuilt archives on the public
[GitHub Releases of cpaikr/dsdai](https://github.com/cpaikr/dsdai/releases).
The [README](../README.md#installation) owns the consumer installation and
update procedure; this runbook owns targets, certification, and release
operations. The first release was v0.1.0; the latest is v0.1.1.

## Targets and certification

[`scripts/release-targets.json`](../scripts/release-targets.json) is the single
target inventory. It names each target's id, Rust triple, executable, and
native GitHub-hosted runner, and the Linux glibc ceiling. The CI matrix, archive
names, installer platform cases, the CLI's embedded target id, and the release
manifest all derive from it; a Rust unit test and a Python unit test fail when
they drift apart.

| Target | Runner | Notes |
|---|---|---|
| `linux-x64-gnu` | `ubuntu-22.04` | glibc floor measured from the binary; at most 2.35 |
| `linux-arm64-gnu` | `ubuntu-22.04-arm` | as above |
| `darwin-arm64` | `macos-15` | `MACOSX_DEPLOYMENT_TARGET=11.0` |
| `win32-x64` | `windows-2025` | static C runtime (`+crt-static`) |

**Every target is built and certified natively.** The repository is public, so
standard GitHub-hosted runners cost nothing, and native runners give each
claimed platform real runtime evidence instead of a cross-build that is only
format-checked. On every target's own runner, CI and Release:

1. run the Rust test suite (`cargo test --workspace --all-features --locked`),
   including the release-check and upgrade tests against a local fixture
   release origin;
2. build the production executable with Rust 1.88 (`scripts/release.py build`),
   check its binary format and architecture, refuse an executable carrying the
   test-only `fixture-origin` hooks, measure the Linux glibc floor against
   `maxGlibc`, and pack `dsd-<version>-<target>.tar.gz`;
3. certify that exact archive (`scripts/release.py certify`): install it with
   the release installer (`install.sh`, or `install.ps1` under Windows
   PowerShell) into a clean temporary directory whose path has spaces; check
   the installed bytes and the `.dsd-receipt.json` contents; run `dsd --version`,
   `--help`, `version`, and `check`/`outline` on the public fixture with an
   empty `PATH`; confirm `dsd upgrade --check` accepts the installer's receipt;
   and confirm that a corrupt checksum fails installation without changing the
   installed executable or receipt.

A bundle job then requires all four build records and certification reports
for the same source revision and compiler, and writes `SHA256SUMS` and
`release-manifest.json`. Every manifest entry therefore carries
`runtimeCertified: true`.

Archives are deterministic for a given toolchain: exactly two entries,
`LICENSE.md` (mode 0644) and `dsd` or `dsd.exe` (mode 0755), with zero
timestamps and ownership, USTAR, and a gzip header without a name or
timestamp. `pack` in `scripts/release.py`, the installers' entry checks, and
`executable_from_archive` in `crates/dsd-cli/src/upgrade.rs` enforce the same
layout and change together. Installers and `upgrade` install only the
executable.

The published release contains, for version `X.Y.Z`:

- `dsd-X.Y.Z-<target>.tar.gz` for each target;
- `install.sh` and `install.ps1`, rendered for `X.Y.Z` from
  [`scripts/install/`](../scripts/install/);
- [`verify-windows-install.ps1`](../scripts/verify-windows-install.ps1);
- `SHA256SUMS` covering all of the above;
- `release-manifest.json`: version, tag, source revision, compiler, and per
  target the archive name, its SHA-256, the executable's SHA-256, and
  `runtimeCertified`, plus the installers' SHA-256.

`dsd version --check`, incidental advisories, and `dsd upgrade` read that
manifest through GitHub's `releases/latest/download/` link.

### Windows visibility

CI certifies Windows installation from a runner process, which is not
descended from a packaged (MSIX) desktop app. On a desktop, a shell spawned by
such an app can see redirected files that no other program can. The installer
probes a file handle and refuses a destination whose physical path differs,
and `dsd upgrade` refuses a staged executable that resolves elsewhere.
[`scripts/verify-windows-install.ps1`](../scripts/verify-windows-install.ps1)
records independent-consumer evidence for a desktop installation: it relaunches
itself through the Windows management service, records its process ancestry,
and checks visibility, full-path execution, and `PATH` resolution:

```powershell
.\scripts\verify-windows-install.ps1 -BinDirectory "$env:LOCALAPPDATA\dsd\bin" -ExpectedVersion 0.1.1
```

[Windows installation and recovery](windows-installation.md) covers the
consumer side.

## Maintainer-controlled Actions

Every job, including the reusable [`standalone.yml`](../.github/workflows/standalone.yml)
jobs, requires both `github.actor` and `github.triggering_actor` to be
`sjunepark`. [`ci.yml`](../.github/workflows/ci.yml) runs on `pull_request`
(never `pull_request_target`) only for pull requests authored by `sjunepark`
from a branch of this repository, and otherwise only by manual dispatch.
Other authors' pull requests skip every job; set the repository's fork pull
request policy to require approval for all outside contributors.

CI reports the `ci/validated-source` commit status on the exact source SHA (the
PR head, not GitHub's merge commit): pending before work starts, success only
when `python scripts/check.py` on Linux and the native build, test, and
certification of all four targets succeed. Skipped, failed, or cancelled
prerequisites report failure. Only the two reporting jobs get
`statuses: write`, and they do not check out repository code. Make
`ci/validated-source` the required status on `main`; a skipped gated job would
otherwise satisfy a required check.

`main` is the only integration branch, so every maintainer PR to it runs the
full platform matrix. To run CI manually:

```sh
gh workflow run ci.yml --repo cpaikr/dsdai --ref main
```

## Version authority

`[workspace.package] version` in [`Cargo.toml`](../Cargo.toml) owns `X.Y.Z`;
both crates inherit it, and `Cargo.lock` must agree
(`python scripts/release.py version`). Its source tag is `vX.Y.Z`, a lightweight
or annotated tag on a commit of `main`. The CLI embeds the version, so
`dsd --version`, the archive names, the receipt, and the manifest all agree.
Before 1.0, compatible features and fixes advance the patch version, and
breaking changes to the CLI contract advance the minor version. Never move a
tag or reuse a published version.

## Releasing

1. **Candidate run.** Dispatch Release with an empty tag on `main` (or another
   branch via `--ref <branch>`). It builds and certifies every target and
   uploads the bundle as the `dsd-release` workflow artifact, without creating
   a tag or release. Review the artifact.

   ```sh
   gh workflow run release.yml --repo cpaikr/dsdai --ref main
   ```

2. **Version commit.** In a PR, set `[workspace.package] version`, run
   `cargo update -w` so `Cargo.lock` follows, and run
   `python scripts/check.py`. Merge after CI succeeds. Leave the README's
   release status alone until step 7, so `main` never names an unpublished
   release.

3. **Exact-commit CI.** Release requires a successful CI run dispatched by
   `sjunepark` on `main` whose head SHA is the tagged commit. PR runs do not
   count: GitHub's merge and rebase merges create new SHAs, and a PR run whose
   gated jobs were skipped must never pass as validation. Dispatch CI on
   `main` after the version commit lands and wait for success:

   ```sh
   gh workflow run ci.yml --repo cpaikr/dsdai --ref main
   ```

4. **Tag.** Verify the tag is unused remotely, then create and push only it:

   ```sh
   set -eu
   git fetch origin main --tags
   SOURCE_SHA="$(git rev-parse origin/main)"
   VERSION="$(git show "$SOURCE_SHA:Cargo.toml" | sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p')"
   SOURCE_TAG="v$VERSION"
   test -z "$(git ls-remote --refs origin "refs/tags/$SOURCE_TAG")"
   git tag "$SOURCE_TAG" "$SOURCE_SHA"
   git push origin "refs/tags/$SOURCE_TAG"
   ```

   A failed remote lookup is not evidence that a tag is unused. Pushing a tag
   triggers nothing.

5. **Publish.** Dispatch Release from `main` with the tag:

   ```sh
   gh workflow run release.yml --repo cpaikr/dsdai --ref main -f tag="$SOURCE_TAG"
   ```

   The metadata job refuses a tag that is not stable `vX.Y.Z`, does not exist
   remotely, is not on `main`, lacks a successful CI dispatch on `main` for its
   exact commit, or disagrees with
   the Cargo version. Every later job checks out the tag's commit. After
   native certification and bundle assembly, the publisher rechecks the tag,
   validates the bundle against the checkout, creates a draft with generated
   notes, uploads only missing assets after byte-comparing existing ones,
   rechecks the tag, publishes, and downloads every asset again to verify the
   published release. Only this final job has `contents: write`.

6. **Consumer check.** On at least one machine, follow the README unattended
   installation or run `dsd upgrade` from the previous release, then
   `dsd version --check`. On Windows desktops, record
   `verify-windows-install.ps1` output from an independent process.

7. **Record the release.** In a PR, point the README installation guide and
   this runbook at the published version, then merge it.

## Recovery

The release operator owns failures:

- **Source or validation failures** need a reviewed fix and a new version and
  tag. Infrastructure failures (runner outage, network) may rerun unchanged
  source.
- **Interrupted publication** leaves a draft. Rerun the failed jobs: the
  publisher reuses the same bundle, requires existing draft assets to match
  byte for byte, and uploads only what is missing.
- **A rebuilt bundle that differs from a draft's assets** stops publication.
  Prefer rerunning failed jobs of the original run (which reuse its artifact),
  or delete the unpublished draft deliberately and dispatch again; never move
  the tag.
- **A published release** is only verified, never changed. Missing, extra, or
  differing assets fail verification; corrections need a new version.
- **A moved or deleted tag, or an uncertain GitHub lookup** stops completion.
  Inspect the draft or published state before retrying.

## Local validation

```sh
python scripts/check.py
python scripts/release.py build <target> release-artifact/archive
python scripts/release.py certify <target> release-artifact/archive release-artifact/report/<target>.json
```

`check.py` runs the Rust and Python suites, including
[`scripts/tests/test_release.py`](../scripts/tests/test_release.py) for the
packager, bundle verification, and publisher (against a fake `gh`). `build`
and `certify` run the CI steps for the host's own target; `certify` refuses
another platform's archive. Neither publishes anything.
