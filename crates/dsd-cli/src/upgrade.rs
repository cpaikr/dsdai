//! Receipt-managed upgrade of a standalone installation.
//!
//! The release installers write `.dsd-receipt.json` beside the executable.
//! `dsd upgrade` replaces only an executable whose bytes match that receipt.
//! It verifies the candidate against the release manifest, `SHA256SUMS`, the
//! exact archive entry set, and the candidate's own `--version`, then publishes
//! the executable before the receipt. A failure before both are durable restores
//! the previous installation or reports the exact paths that remain.

use std::env::consts::EXE_SUFFIX;
use std::fs::{self, File};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::envelope::Failure;
use crate::release_check::{self, Version};

const RECEIPT_FILE: &str = ".dsd-receipt.json";
const LOCK_FILE: &str = ".dsd-upgrade.lock";
const RECEIPT_SCHEMA_VERSION: u32 = 1;
const RELEASE_REPOSITORY: &str = "cpaikr/dsdai";
/// The only entry of a release archive; `scripts/release.py` packs the same.
const EXECUTABLE: &str = if cfg!(windows) { "dsd.exe" } else { "dsd" };
/// Prefixes of files an upgrade creates beside the executable.
const SCRATCH_PREFIXES: [&str; 4] = [".dsd-new-", ".dsd-old-", ".dsd-receipt-new-", ".dsd-receipt-old-"];
const MAX_RECEIPT_BYTES: u64 = 64 << 10;
const MAX_CHECKSUMS_BYTES: usize = 64 << 10;
// Release archives are a few megabytes; the bounds leave room for growth while
// stopping a corrupt or hostile response early.
const MAX_ARCHIVE_BYTES: usize = 64 << 20;
const MAX_EXECUTABLE_BYTES: u64 = 128 << 20;
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(300);
const STAGED_VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const REINSTALL_HINT: &str = "Reinstall with the release installer as described in the README; it records the receipt that dsd upgrade requires.";

/// Runs `dsd upgrade`, returning its result or failure.
pub fn run(check: bool) -> Result<Value, Failure> {
    let mut progress = Progress::for_stderr();
    upgrade(check, &mut progress).map(|outcome| outcome.result()).map_err(UpgradeError::into_failure)
}

fn upgrade(check: bool, progress: &mut Progress) -> Result<Outcome, UpgradeError> {
    let target = release_check::TARGET.ok_or_else(|| {
        UpgradeError::new(
            "upgrade_unsupported",
            "This build is not a release target, so no release archive can replace it.",
        )
    })?;
    let running = running_version()?;
    let executable = std::env::current_exe().and_then(fs::canonicalize).map_err(|_| unmanaged())?;
    // Reject an unmanaged installation before creating any file beside it or
    // contacting GitHub.
    let mut installation = Installation::at(executable.clone(), running, target)?;
    let origin = release_check::release_origin().map_err(|reason| UpgradeError::new("upgrade_unsupported", reason))?;
    // Only a real upgrade changes files. It holds the lock through publication
    // and inspects again under it, since another upgrade may have finished.
    let _lock = if check {
        None
    } else {
        let lock = lock_installation(&executable)?;
        installation = Installation::at(executable, running, target)?;
        Some(lock)
    };
    progress.stage(format_args!("Checking for the latest dsd release..."));
    let client = release_check::http_client().map_err(source_unavailable)?;
    let network = Network { client: &client, origin: &origin };
    release_check::block_on(network.upgrade(check, progress, &installation, running, target))
        .map_err(source_unavailable)?
}

struct Network<'a> {
    client: &'a reqwest::Client,
    origin: &'a str,
}

impl Network<'_> {
    async fn upgrade(
        &self,
        check: bool,
        progress: &mut Progress,
        installation: &Installation,
        running: Version,
        target: &'static str,
    ) -> Result<Outcome, UpgradeError> {
        let release = self.latest_release(target).await?;
        let mut outcome = Outcome {
            running,
            latest: release.version,
            target,
            release_url: format!("{}{}", release_check::RELEASE_PAGE_PREFIX, release.tag),
            executable: installation.receipt.executable.clone(),
            installable: release.entry.is_some(),
            updated: false,
        };
        if release.version <= running {
            progress.stage(format_args!("dsd {running} is up to date."));
            return Ok(outcome);
        }
        if check {
            progress.stage(format_args!("{}", outcome.next_step()));
            return Ok(outcome);
        }
        let entry = release.entry.as_ref().ok_or_else(|| release_invalid(outcome.next_step()))?;
        installation.remove_leftovers();
        progress.stage(format_args!("Downloading {}...", entry.archive));
        let executable = self.download_verified(&release, entry, progress).await?;
        progress.stage(format_args!("Installing dsd {}...", release.version));
        installation.replace(&executable, release.version, target)?;
        progress.stage(format_args!("Upgraded dsd {running} to {}.", release.version));
        outcome.updated = true;
        Ok(outcome)
    }

    async fn latest_release(&self, target: &str) -> Result<Release, UpgradeError> {
        let url = format!("{}{}", self.origin, release_check::LATEST_MANIFEST_PATH);
        let body = self
            .fetch(&url, release_check::MAX_MANIFEST_BYTES, "release manifest", METADATA_TIMEOUT)
            .await?
            .ok_or_else(|| release_invalid("No published dsd release provides a release manifest."))?;
        parse_release(&body, target)
    }

    async fn fetch(
        &self,
        url: &str,
        limit: usize,
        label: &str,
        timeout: Duration,
    ) -> Result<Option<Vec<u8>>, UpgradeError> {
        tokio::time::timeout(timeout, release_check::fetch_bounded(self.client, url, limit, label))
            .await
            .map_err(|_| source_unavailable(format!("The {label} download exceeded {} seconds.", timeout.as_secs())))?
            .map_err(source_unavailable)
    }

    /// Downloads this target's archive and returns its verified executable.
    async fn download_verified(
        &self,
        release: &Release,
        entry: &ManifestTarget,
        progress: &mut Progress,
    ) -> Result<Vec<u8>, UpgradeError> {
        let base = format!("{}{}/download/{}", self.origin, release_check::RELEASE_PATH, release.tag);
        let checksums = self
            .fetch(&format!("{base}/SHA256SUMS"), MAX_CHECKSUMS_BYTES, "release checksum list", METADATA_TIMEOUT)
            .await?
            .ok_or_else(|| release_invalid("The release has no SHA256SUMS asset."))?;
        let expected = checksum_entry(&checksums, &entry.archive)?;
        if expected != entry.sha256.to_ascii_lowercase() {
            return Err(verification_failed(format!(
                "The release manifest and SHA256SUMS disagree about {}.",
                entry.archive
            )));
        }
        let archive = self
            .fetch(&format!("{base}/{}", entry.archive), MAX_ARCHIVE_BYTES, "release archive", ARCHIVE_TIMEOUT)
            .await?
            .ok_or_else(|| release_invalid(format!("The release has no {} asset.", entry.archive)))?;
        progress.stage(format_args!("Verifying {} ({})...", entry.archive, display_size(archive.len())));
        if sha256_hex(&archive) != expected {
            return Err(verification_failed("The downloaded archive does not match its published SHA-256 checksum."));
        }
        executable_from_archive(&archive)
    }
}

/// The version this process upgrades from.
fn running_version() -> Result<Version, UpgradeError> {
    // Fixture-origin tests run the current build as an older installation.
    #[cfg(feature = "fixture-origin")]
    if let Ok(version) = std::env::var("DSD_FIXTURE_RUNNING_VERSION") {
        return Version::parse(&version)
            .ok_or_else(|| UpgradeError::new("upgrade_unsupported", "Invalid fixture running version."));
    }
    Version::parse(release_check::CURRENT_VERSION).ok_or_else(|| {
        UpgradeError::new(
            "upgrade_unsupported",
            format!(
                "Running version {} is not a release version; only release installations can upgrade.",
                release_check::CURRENT_VERSION
            ),
        )
    })
}

struct Outcome {
    running: Version,
    latest: Version,
    target: &'static str,
    release_url: String,
    executable: PathBuf,
    /// The latest release has an archive for this target.
    installable: bool,
    updated: bool,
}

impl Outcome {
    fn next_step(&self) -> String {
        if self.installable {
            format!("Run dsd upgrade to install dsd {}.", self.latest)
        } else {
            format!(
                "dsd {} has no {} archive in its release manifest; keep {} until the release is complete.",
                self.latest, self.target, self.running
            )
        }
    }

    fn result(&self) -> Value {
        let update_available = self.latest > self.running;
        json!({
            "name": "dsd",
            "runningVersion": self.running,
            "latestVersion": self.latest,
            "installedVersion": if self.updated { self.latest } else { self.running },
            "updateAvailable": update_available,
            "updated": self.updated,
            "distribution": if self.installable { "complete" } else { "incomplete_distribution" },
            "target": self.target,
            "releaseUrl": self.release_url,
            "executable": self.executable,
            "nextStep": (update_available && !self.updated).then(|| self.next_step()),
        })
    }
}

#[derive(Debug)]
struct UpgradeError {
    code: &'static str,
    message: String,
    hint: Option<String>,
}

impl UpgradeError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), hint: None }
    }

    #[must_use]
    fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn into_failure(self) -> Failure {
        Failure::cli(self.code, self.message, self.hint)
    }
}

fn unmanaged() -> UpgradeError {
    UpgradeError::new(
        "unmanaged_installation",
        format!(
            "This executable has no {RECEIPT_FILE} installation receipt beside it, so dsd upgrade does not manage it."
        ),
    )
    .hint(REINSTALL_HINT)
}

fn receipt_mismatch(message: impl Into<String>) -> UpgradeError {
    UpgradeError::new("upgrade_receipt_mismatch", message).hint(REINSTALL_HINT)
}

fn source_unavailable(message: String) -> UpgradeError {
    UpgradeError::new("upgrade_source_unavailable", message).hint(
        "Retry later; upgrades need github.com and GitHub's release asset host. The installation was not changed.",
    )
}

fn release_invalid(message: impl Into<String>) -> UpgradeError {
    UpgradeError::new("upgrade_release_invalid", message)
}

fn verification_failed(message: impl Into<String>) -> UpgradeError {
    UpgradeError::new("upgrade_verification_failed", message)
}

/// The installation is unchanged or was restored.
fn upgrade_failed(message: impl Into<String>) -> UpgradeError {
    UpgradeError::new("upgrade_failed", message)
}

/// Stage lines for a person watching in a terminal.
///
/// stdout carries only the JSON envelope, so stages go to stderr, and only when
/// stderr is interactive: pipes, CI, and agents keep an empty stderr.
struct Progress(Option<std::io::Stderr>);

impl Progress {
    fn for_stderr() -> Self {
        let stderr = std::io::stderr();
        Self(stderr.is_terminal().then_some(stderr))
    }

    fn stage(&mut self, message: std::fmt::Arguments<'_>) {
        if let Some(sink) = &mut self.0 {
            // Progress is advisory; a closed terminal must not fail the upgrade.
            let _ = writeln!(sink, "{message}");
        }
    }
}

fn display_size(bytes: usize) -> String {
    let tenths = bytes.div_ceil(100_000);
    format!("{}.{} MB", tenths / 10, tenths % 10)
}

/// The fields `dsd upgrade` needs from a release's `release-manifest.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: String,
    tag: String,
    targets: Vec<ManifestTarget>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestTarget {
    target: String,
    archive: String,
    sha256: String,
}

#[derive(Debug)]
struct Release {
    version: Version,
    tag: String,
    /// This target's archive, absent while the release is incomplete for it.
    entry: Option<ManifestTarget>,
}

fn parse_release(body: &[u8], target: &str) -> Result<Release, UpgradeError> {
    let manifest: Manifest = serde_json::from_slice(body)
        .map_err(|_| release_invalid("The latest release manifest is not valid release-manifest JSON."))?;
    let version = Version::parse(&manifest.version)
        .filter(|version| manifest.tag == format!("v{version}"))
        .ok_or_else(|| release_invalid("The latest release manifest does not name a stable v<version> release."))?;
    let mut entries = manifest.targets.into_iter().filter(|entry| entry.target == target);
    let entry = entries.next();
    if entries.next().is_some()
        || entry
            .as_ref()
            .is_some_and(|entry| entry.archive != archive_name(version, target) || !is_sha256(&entry.sha256))
    {
        return Err(release_invalid(format!(
            "The latest release manifest does not list exactly one valid {target} archive."
        )));
    }
    Ok(Release { version, tag: manifest.tag, entry })
}

fn checksum_entry(checksums: &[u8], archive: &str) -> Result<String, UpgradeError> {
    let text =
        std::str::from_utf8(checksums).map_err(|_| release_invalid("The release SHA256SUMS is not UTF-8 text."))?;
    let mut matches = text.lines().filter_map(|line| line.split_once("  ")).filter(|(_, name)| *name == archive);
    match (matches.next(), matches.next()) {
        (Some((digest, _)), None) if is_sha256(digest) => Ok(digest.to_ascii_lowercase()),
        _ => Err(release_invalid(format!("The release SHA256SUMS lacks exactly one valid digest for {archive}."))),
    }
}

/// The executable from an archive holding exactly it, as a regular file.
fn executable_from_archive(archive: &[u8]) -> Result<Vec<u8>, UpgradeError> {
    let invalid =
        || verification_failed(format!("The release archive must contain exactly {EXECUTABLE} as a regular file."));
    let decoder = GzDecoder::new(archive).take(2 * MAX_EXECUTABLE_BYTES);
    let mut archive = tar::Archive::new(decoder);
    let mut executable = None;
    for entry in archive.entries().map_err(|_| invalid())? {
        let mut entry = entry.map_err(|_| invalid())?;
        if !entry.header().entry_type().is_file() || entry.size() > MAX_EXECUTABLE_BYTES {
            return Err(invalid());
        }
        let name = entry.path().ok().and_then(|path| path.to_str().map(str::to_owned)).ok_or_else(invalid)?;
        if name != EXECUTABLE || executable.is_some() {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|_| invalid())?;
        executable = Some(bytes);
    }
    executable.filter(|bytes| !bytes.is_empty()).ok_or_else(invalid)
}

fn archive_name(version: Version, target: &str) -> String {
    format!("dsd-{version}-{target}.tar.gz")
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The installation receipt written by the release installers and by upgrades.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    manager: String,
    version: String,
    target: String,
    executable: PathBuf,
    release_repository: String,
    release_tag: String,
    asset_name: String,
    sha256: String,
}

impl Receipt {
    fn new(version: Version, target: &str, executable: PathBuf, sha256: String) -> Self {
        Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            manager: "standalone".to_owned(),
            version: version.to_string(),
            target: target.to_owned(),
            executable,
            release_repository: RELEASE_REPOSITORY.to_owned(),
            release_tag: format!("v{version}"),
            asset_name: archive_name(version, target),
            sha256,
        }
    }
}

/// A standalone installation whose executable matches its receipt.
#[derive(Debug)]
struct Installation {
    /// Canonical path of the installed executable.
    executable: PathBuf,
    /// Canonical installation directory.
    directory: PathBuf,
    receipt_path: PathBuf,
    receipt: Receipt,
    receipt_bytes: Vec<u8>,
}

impl Installation {
    fn at(executable: PathBuf, running: Version, target: &str) -> Result<Self, UpgradeError> {
        let directory = executable.parent().ok_or_else(unmanaged)?.to_path_buf();
        let receipt_path = directory.join(RECEIPT_FILE);
        let receipt_bytes = read_receipt(&receipt_path)?;
        let receipt: Receipt = serde_json::from_slice(&receipt_bytes)
            .map_err(|_| receipt_mismatch("The installation receipt is not valid receipt JSON."))?;
        let expected = Receipt::new(running, target, receipt.executable.clone(), receipt.sha256.clone());
        if receipt != expected {
            return Err(receipt_mismatch(format!(
                "The installation receipt describes dsd {} for {}, but this is a standalone dsd {running} for {target}.",
                receipt.version, receipt.target
            )));
        }
        if fs::canonicalize(&receipt.executable).ok().as_ref() != Some(&executable) {
            return Err(receipt_mismatch("The installation receipt names a different executable path."));
        }
        if !is_sha256(&receipt.sha256) || file_sha256(&executable)? != receipt.sha256.to_ascii_lowercase() {
            return Err(receipt_mismatch("The executable's SHA-256 digest differs from its installation receipt."));
        }
        Ok(Self { executable, directory, receipt_path, receipt, receipt_bytes })
    }

    /// Stages, verifies, and publishes `executable` with its new receipt.
    fn replace(&self, executable: &[u8], version: Version, target: &str) -> Result<(), UpgradeError> {
        let pid = std::process::id();
        let staged = self.directory.join(format!(".dsd-new-{pid}{EXE_SUFFIX}"));
        let staged_receipt = self.directory.join(format!(".dsd-receipt-new-{pid}.json"));
        let receipt = Receipt::new(version, target, self.receipt.executable.clone(), sha256_hex(executable));
        let result = self
            .stage(&staged, executable, version)
            .and_then(|()| {
                write_new(&staged_receipt, &receipt_json(&receipt), false).map_err(|_| {
                    upgrade_failed("Could not stage the new installation receipt; the installation was not changed.")
                })
            })
            .and_then(|()| self.publish(&staged, &staged_receipt));
        // Publication moves both staged files; anything left behind is unused.
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_file(&staged_receipt);
        result
    }

    fn stage(&self, staged: &Path, executable: &[u8], version: Version) -> Result<(), UpgradeError> {
        write_new(staged, executable, true).map_err(|_| {
            upgrade_failed(
                "Could not stage the new executable beside the installed one; the installation was not changed.",
            )
        })?;
        // A shell started by a packaged Windows app can redirect writes to private
        // storage that other programs cannot see; refuse such a replacement.
        if fs::canonicalize(staged).ok().as_deref().and_then(Path::parent) != Some(self.directory.as_path()) {
            return Err(upgrade_failed(
                "The staged executable resolves outside the installation directory, so other programs would not see the replacement; the installation was not changed.",
            )
            .hint("Run dsd upgrade from an independently launched terminal."));
        }
        verify_staged_version(staged, version)
    }

    /// Publishes the executable before the receipt, restoring both on failure.
    fn publish(&self, staged: &Path, staged_receipt: &Path) -> Result<(), UpgradeError> {
        let backup = self.directory.join(format!(".dsd-old-{}{EXE_SUFFIX}", std::process::id()));
        set_aside(&self.executable, &backup).map_err(|_| {
            upgrade_failed("Could not preserve the installed executable; the installation was not changed.")
        })?;
        if fs::rename(staged, &self.executable).is_err() {
            return Err(self.restore(&backup, false, "Could not move the new executable into place"));
        }
        if fs::rename(staged_receipt, &self.receipt_path).is_err() {
            return Err(self.restore(&backup, false, "Could not move the new installation receipt into place"));
        }
        if sync_directory(&self.directory).is_err() {
            return Err(self.restore(&backup, true, "Could not make the replacement durable"));
        }
        // Windows cannot delete the running image until this process exits; a
        // later upgrade removes it.
        let _ = fs::remove_file(&backup);
        Ok(())
    }

    fn restore(&self, backup: &Path, receipt_replaced: bool, cause: &str) -> UpgradeError {
        let executable_restored = fs::rename(backup, &self.executable).is_ok();
        // Renaming a hard link over another link to the same file succeeds without
        // removing it, so clear a backup that is still present.
        if executable_restored {
            let _ = fs::remove_file(backup);
        }
        let receipt_restored = !receipt_replaced || self.restore_receipt().is_ok();
        if executable_restored && receipt_restored && sync_directory(&self.directory).is_ok() {
            return upgrade_failed(format!("{cause}; the previous installation was restored."));
        }
        let mut steps = Vec::new();
        if !executable_restored {
            steps.push(format!("rename {} to {}", backup.display(), self.executable.display()));
        }
        if !receipt_restored {
            steps.push(format!("reinstall to rewrite {}", self.receipt_path.display()));
        }
        UpgradeError::new(
            "upgrade_recovery_required",
            format!("{cause}, and the previous installation could not be fully restored."),
        )
        .hint(if steps.is_empty() {
            "Confirm the installation with dsd --version, or reinstall with the release installer as described in the README.".to_owned()
        } else {
            format!(
                "To recover, {}; or reinstall with the release installer as described in the README.",
                steps.join(" and ")
            )
        })
    }

    fn restore_receipt(&self) -> std::io::Result<()> {
        let temporary = self.directory.join(format!(".dsd-receipt-old-{}.json", std::process::id()));
        write_new(&temporary, &self.receipt_bytes, false)?;
        fs::rename(&temporary, &self.receipt_path).inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })
    }

    /// Removes files that earlier upgrades left behind.
    ///
    /// Runs under the upgrade lock after the executable matched its receipt, so
    /// staged files and backups are no longer needed. On Windows an executable
    /// set aside while it still runs cannot be removed and stays until later.
    fn remove_leftovers(&self) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_str().is_some_and(|name| SCRATCH_PREFIXES.iter().any(|prefix| name.starts_with(prefix))) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Excludes concurrent upgrades of one installation; released when dropped.
fn lock_installation(executable: &Path) -> Result<File, UpgradeError> {
    let directory = executable.parent().ok_or_else(unmanaged)?;
    let locked = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(LOCK_FILE))
        .and_then(|file| FileExt::try_lock_exclusive(&file).map(|locked| locked.then_some(file)));
    match locked {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(upgrade_failed(
            "Another dsd upgrade is running for this installation; the installation was not changed.",
        )
        .hint("Wait for the other upgrade to finish, then retry.")),
        Err(_) => Err(upgrade_failed(
            "Could not lock the installation directory for the upgrade; the installation was not changed.",
        )),
    }
}

fn read_receipt(path: &Path) -> Result<Vec<u8>, UpgradeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(unmanaged()),
        Err(_) => return Err(receipt_mismatch("The installation receipt cannot be read.")),
    };
    if !metadata.is_file() || metadata.len() > MAX_RECEIPT_BYTES {
        return Err(receipt_mismatch("The installation receipt is not a regular file of bounded size."));
    }
    fs::read(path).map_err(|_| receipt_mismatch("The installation receipt cannot be read."))
}

fn receipt_json(receipt: &Receipt) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(receipt).expect("receipts serialize");
    bytes.push(b'\n');
    bytes
}

fn file_sha256(path: &Path) -> Result<String, UpgradeError> {
    let unreadable = || receipt_mismatch("The installed executable cannot be read for its digest.");
    let mut hasher = Sha256::new();
    let copied = File::open(path)
        .and_then(|file| std::io::copy(&mut file.take(MAX_EXECUTABLE_BYTES + 1), &mut hasher))
        .map_err(|_| unreadable())?;
    if copied > MAX_EXECUTABLE_BYTES {
        return Err(unreadable());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Creates `path` with `bytes` and flushes it to disk; removes it on failure.
fn write_new(path: &Path, bytes: &[u8], executable: bool) -> std::io::Result<()> {
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    #[cfg(not(unix))]
    let _ = executable;
    let result = options.open(path).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

/// Requires the staged candidate to report exactly `dsd <version>`.
fn verify_staged_version(path: &Path, version: Version) -> Result<(), UpgradeError> {
    let invalid = |detail: &str| {
        verification_failed(format!("The downloaded executable {detail}; the installation was not changed."))
    };
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| invalid("could not start"))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = stdout.take(256).read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + STAGED_VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(invalid("did not report its version in time"));
            }
        }
    };
    let output = reader.join().unwrap_or_default();
    if status.success() && String::from_utf8_lossy(&output).trim_end() == format!("dsd {version}") {
        Ok(())
    } else {
        Err(invalid(&format!("does not report dsd {version}")))
    }
}

/// Keeps the installed executable reachable at `backup` for a rollback.
#[cfg(not(windows))]
fn set_aside(executable: &Path, backup: &Path) -> std::io::Result<()> {
    // A hard link leaves the installed path in place until the atomic rename
    // replaces it.
    fs::hard_link(executable, backup)
}

/// Keeps the installed executable reachable at `backup` for a rollback.
#[cfg(windows)]
fn set_aside(executable: &Path, backup: &Path) -> std::io::Result<()> {
    // Windows cannot replace a running image in place, but it can rename it
    // within the same volume.
    fs::rename(executable, backup)
}

/// Makes renames in `directory` durable; Windows offers no directory flush.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn temporary_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "dsd-upgrade-unit-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::canonicalize(directory).unwrap()
    }

    fn archive(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for (name, bytes, kind) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(*kind);
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            if kind.is_symlink() {
                header.set_link_name("elsewhere").unwrap();
            }
            builder.append_data(&mut header, name, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// A fake installed executable with a matching receipt.
    fn installation(directory: &Path, running: &str) -> Installation {
        let executable = directory.join(EXECUTABLE);
        fs::write(&executable, b"old executable").unwrap();
        let receipt =
            Receipt::new(version(running), "linux-x64-gnu", executable.clone(), sha256_hex(b"old executable"));
        fs::write(directory.join(RECEIPT_FILE), receipt_json(&receipt)).unwrap();
        Installation::at(executable, version(running), "linux-x64-gnu").unwrap()
    }

    fn entries(directory: &Path) -> Vec<String> {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn checksum_list_requires_one_exact_digest() {
        let digest = "a".repeat(64);
        let archive = "dsd-0.2.0-linux-x64-gnu.tar.gz";
        let list = format!("{digest}  {archive}\n{}  install.sh\n", "b".repeat(64));
        assert_eq!(checksum_entry(list.as_bytes(), archive).unwrap(), digest);
        let uppercase = format!("{}  {archive}\n", digest.to_uppercase());
        assert_eq!(checksum_entry(uppercase.as_bytes(), archive).unwrap(), digest);
        for invalid in [
            String::new(),
            format!("{digest}  {archive}\n{digest}  {archive}\n"),
            format!("{}  {archive}\n", "z".repeat(64)),
            format!("{digest} {archive}\n"),
            format!("{digest}  {archive}.sig\n"),
        ] {
            assert!(checksum_entry(invalid.as_bytes(), archive).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn archive_must_hold_exactly_the_executable() {
        let file = tar::EntryType::Regular;
        let valid = archive(&[(EXECUTABLE, b"binary", file)]);
        assert_eq!(executable_from_archive(&valid).unwrap(), b"binary");
        for invalid in [
            archive(&[]),
            archive(&[("README.md", b"readme", file)]),
            archive(&[(EXECUTABLE, b"binary", file), ("extra", b"x", file)]),
            archive(&[(EXECUTABLE, b"binary", file), (EXECUTABLE, b"binary", file)]),
            archive(&[(EXECUTABLE, b"", tar::EntryType::Symlink)]),
            archive(&[(&format!("bin/{EXECUTABLE}"), b"binary", file)]),
            archive(&[(EXECUTABLE, b"", file)]),
            b"not gzip".to_vec(),
        ] {
            assert_eq!(executable_from_archive(&invalid).unwrap_err().code, "upgrade_verification_failed");
        }
    }

    #[test]
    fn manifest_must_name_one_valid_archive_for_the_target() {
        let manifest = |tag: &str, targets: Value| {
            serde_json::to_vec(&json!({"version": "0.2.0", "tag": tag, "targets": targets})).unwrap()
        };
        let entry = |target: &str, archive: &str| json!({"target": target, "archive": archive, "sha256": "c".repeat(64), "runtimeCertified": true});
        let good = entry("linux-x64-gnu", "dsd-0.2.0-linux-x64-gnu.tar.gz");
        let release = parse_release(&manifest("v0.2.0", json!([good])), "linux-x64-gnu").unwrap();
        assert_eq!(release.version, version("0.2.0"));
        assert_eq!(release.entry.unwrap().sha256, "c".repeat(64));
        let incomplete = parse_release(&manifest("v0.2.0", json!([good])), "win32-x64").unwrap();
        assert!(incomplete.entry.is_none());
        for (tag, targets) in [
            ("v0.1.0", json!([good])),
            ("v0.2.0", json!([good, good])),
            ("v0.2.0", json!([entry("linux-x64-gnu", "dsd-0.1.0-linux-x64-gnu.tar.gz")])),
        ] {
            assert_eq!(
                parse_release(&manifest(tag, targets), "linux-x64-gnu").unwrap_err().code,
                "upgrade_release_invalid"
            );
        }
    }

    #[test]
    fn only_an_executable_matching_its_receipt_is_managed() {
        let directory = temporary_directory();
        let executable = directory.join(EXECUTABLE);
        let check = || Installation::at(executable.clone(), version("0.1.4"), "linux-x64-gnu");
        fs::write(&executable, b"old executable").unwrap();
        assert_eq!(check().unwrap_err().code, "unmanaged_installation");

        let installed = installation(&directory, "0.1.4");
        assert_eq!(installed.executable, executable);
        let old = || sha256_hex(b"old executable");
        let mismatches = [
            Receipt::new(version("0.1.3"), "linux-x64-gnu", executable.clone(), old()),
            Receipt::new(version("0.1.4"), "darwin-arm64", executable.clone(), old()),
            Receipt::new(version("0.1.4"), "linux-x64-gnu", directory.join("other"), old()),
            Receipt::new(version("0.1.4"), "linux-x64-gnu", executable.clone(), sha256_hex(b"tampered")),
        ];
        for receipt in &mismatches {
            fs::write(directory.join(RECEIPT_FILE), receipt_json(receipt)).unwrap();
            assert_eq!(check().unwrap_err().code, "upgrade_receipt_mismatch");
        }
        fs::write(directory.join(RECEIPT_FILE), b"{\"schemaVersion\": 1}").unwrap();
        assert_eq!(check().unwrap_err().code, "upgrade_receipt_mismatch");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publication_replaces_the_executable_and_then_the_receipt() {
        let directory = temporary_directory();
        let installed = installation(&directory, "0.1.4");
        let staged = directory.join("staged");
        let staged_receipt = directory.join("staged-receipt");
        fs::write(&staged, b"new executable").unwrap();
        let receipt = Receipt::new(
            version("0.2.0"),
            "linux-x64-gnu",
            installed.executable.clone(),
            sha256_hex(b"new executable"),
        );
        fs::write(&staged_receipt, receipt_json(&receipt)).unwrap();
        installed.publish(&staged, &staged_receipt).unwrap();
        assert_eq!(fs::read(&installed.executable).unwrap(), b"new executable");
        Installation::at(installed.executable.clone(), version("0.2.0"), "linux-x64-gnu").unwrap();
        assert_eq!(entries(&directory), [RECEIPT_FILE, EXECUTABLE]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_publication_restores_the_previous_installation() {
        let directory = temporary_directory();
        let installed = installation(&directory, "0.1.4");
        let staged = directory.join("staged");
        fs::write(&staged, b"new executable").unwrap();
        // A missing staged receipt fails after the executable is in place.
        let error = installed.publish(&staged, &directory.join("missing-receipt")).unwrap_err();
        assert_eq!(error.code, "upgrade_failed", "{error:?}");
        assert_eq!(fs::read(&installed.executable).unwrap(), b"old executable");
        Installation::at(installed.executable.clone(), version("0.1.4"), "linux-x64-gnu").unwrap();
        assert_eq!(entries(&directory), [RECEIPT_FILE, EXECUTABLE]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn receipt_restoration_rewrites_the_previous_receipt() {
        let directory = temporary_directory();
        let installed = installation(&directory, "0.1.4");
        let backup = directory.join("backup");
        set_aside(&installed.executable, &backup).unwrap();
        fs::write(&installed.receipt_path, b"replaced").unwrap();
        if !installed.executable.exists() {
            fs::write(&installed.executable, b"new executable").unwrap();
        }
        let error = installed.restore(&backup, true, "Simulated failure");
        assert_eq!(error.code, "upgrade_failed", "{error:?}");
        Installation::at(installed.executable.clone(), version("0.1.4"), "linux-x64-gnu").unwrap();
        assert_eq!(entries(&directory), [RECEIPT_FILE, EXECUTABLE]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unrecoverable_restoration_names_the_remaining_paths() {
        let directory = temporary_directory();
        let installed = installation(&directory, "0.1.4");
        let error = installed.restore(&directory.join("missing-backup"), false, "Simulated failure");
        assert_eq!(error.code, "upgrade_recovery_required");
        let hint = error.hint.unwrap();
        assert!(hint.contains("missing-backup"), "{hint}");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn outcome_reports_installed_version_and_next_step() {
        let outcome = |updated| {
            Outcome {
                running: version("0.1.4"),
                latest: version("0.2.0"),
                target: "linux-x64-gnu",
                release_url: "https://github.com/cpaikr/dsdai/releases/tag/v0.2.0".to_owned(),
                executable: PathBuf::from("/bin/dsd"),
                installable: true,
                updated,
            }
            .result()
        };
        let checked = outcome(false);
        assert_eq!(checked["installedVersion"], "0.1.4");
        assert_eq!(checked["updateAvailable"], true);
        assert_eq!(checked["nextStep"], "Run dsd upgrade to install dsd 0.2.0.");
        let upgraded = outcome(true);
        assert_eq!(upgraded["installedVersion"], "0.2.0");
        assert_eq!(upgraded["nextStep"], Value::Null);
        assert_eq!(display_size(4_512_345), "4.6 MB");
    }
}
