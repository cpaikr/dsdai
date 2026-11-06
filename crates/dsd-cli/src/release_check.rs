//! Cached, advisory comparison of the running CLI with published GitHub releases.
//!
//! The check never installs anything and never changes command success; the
//! explicit `dsd upgrade` command owns installation. Release evidence is
//! cached; the comparison is recomputed from the running version on every
//! check, so a replaced executable is compared correctly immediately. A Tokio
//! runtime starts only when a refresh actually contacts GitHub.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, SubsecRound, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Release target id from `scripts/release-targets.json` for this build, if any.
pub const TARGET: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")) {
    Some("linux-x64-gnu")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64", target_env = "gnu")) {
    Some("linux-arm64-gnu")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("darwin-arm64")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64", target_env = "msvc")) {
    Some("win32-x64")
} else {
    None
};

/// The installer the README installation procedure uses on this platform.
pub const INSTALLER: &str = if cfg!(windows) { "install.ps1" } else { "install.sh" };

#[cfg(not(feature = "fixture-origin"))]
const RELEASE_ORIGIN: &str = "https://github.com";
/// The repository path that owns releases, relative to the release origin.
pub const RELEASE_PATH: &str = "/cpaikr/dsdai/releases";
// GitHub's documented latest-release asset link. It is served through
// github.com and the release asset host, not the REST API, so the
// unauthenticated API limit (60 requests/hour/IP) does not apply. "Latest" is
// GitHub's latest stable release: drafts and prereleases are never latest.
pub const LATEST_MANIFEST_PATH: &str = "/cpaikr/dsdai/releases/latest/download/release-manifest.json";
pub const RELEASE_PAGE_PREFIX: &str = "https://github.com/cpaikr/dsdai/releases/tag/";
const CACHE_FILE: &str = "release-check-v1.json";
// Agents may run the CLI many times per hour; a daily interval keeps release
// traffic negligible even when many processes share one address.
const REFRESH_INTERVAL_SECONDS: i64 = 24 * 60 * 60;
const RETRY_COOLDOWN_SECONDS: i64 = 60 * 60;
const REFRESH_BUDGET: Duration = Duration::from_millis(1_500);
pub const MAX_MANIFEST_BYTES: usize = 64 << 10;
const OPT_OUT: &str = "Release checks are disabled by DSD_NO_UPDATE_CHECK.";

/// A plain `MAJOR.MINOR.PATCH` release version. Prerelease and build
/// identities are deliberately unrepresentable: they are not stable releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut component = || {
            parts
                .next()
                .filter(|part| {
                    !part.is_empty()
                        && part.bytes().all(|byte| byte.is_ascii_digit())
                        && (part.len() == 1 || !part.starts_with('0'))
                })
                .and_then(|part| part.parse().ok())
        };
        let version = Self { major: component()?, minor: component()?, patch: component()? };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl TryFrom<String> for Version {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or_else(|| format!("invalid release version {value:?}"))
    }
}

impl From<Version> for String {
    fn from(value: Version) -> Self {
        value.to_string()
    }
}

/// The fields release checks need from a release's `release-manifest.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: String,
    tag: String,
    targets: Vec<ManifestTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestTarget {
    target: String,
    runtime_certified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distribution {
    Complete,
    IncompleteDistribution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Newer,
    Equal,
    Ahead,
    NoStableRelease,
    Uncomparable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    Unavailable,
}

/// Latest stable release observed at `observed_at`, assessed for this target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    observed_at: DateTime<Utc>,
    latest: Option<LatestRelease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestRelease {
    version: Version,
    url: String,
    distribution: Distribution,
    runtime_certified: Option<bool>,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheRecord {
    target: Option<String>,
    evidence: Option<Evidence>,
    last_failure_at: Option<DateTime<Utc>>,
}

/// The release report shared by `dsd version --check` and incidental advisories.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseCheck {
    current_version: &'static str,
    target: Option<&'static str>,
    comparison: Option<Comparison>,
    latest_version: Option<Version>,
    release_url: Option<String>,
    distribution: Option<Distribution>,
    runtime_certified: Option<bool>,
    freshness: Freshness,
    observed_at: Option<DateTime<Utc>>,
    age_seconds: Option<i64>,
    last_refresh_failed_at: Option<DateTime<Utc>>,
    problems: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Incidental,
    Explicit,
}

struct Settings {
    disabled: Option<&'static str>,
    manifest_url: String,
    cache_directory: Result<PathBuf, String>,
}

impl Settings {
    fn from_environment() -> Self {
        let origin = release_origin();
        Self {
            disabled: if opted_out() { Some(OPT_OUT) } else { origin.as_ref().err().copied() },
            manifest_url: format!("{}{LATEST_MANIFEST_PATH}", origin.unwrap_or_default()),
            cache_directory: cache_directory(),
        }
    }
}

/// `DSD_NO_UPDATE_CHECK` set to anything but empty or `0`.
fn opted_out() -> bool {
    std::env::var_os("DSD_NO_UPDATE_CHECK").is_some_and(|value| !value.is_empty() && value != "0")
}

/// The GitHub release origin, or why this execution must not contact it.
#[cfg(not(feature = "fixture-origin"))]
#[allow(clippy::unnecessary_wraps)] // Fixture-origin builds can refuse.
pub fn release_origin() -> Result<String, &'static str> {
    Ok(RELEASE_ORIGIN.to_owned())
}

/// Fixture-origin builds never contact GitHub; tests supply releases explicitly.
#[cfg(feature = "fixture-origin")]
pub fn release_origin() -> Result<String, &'static str> {
    std::env::var("DSD_FIXTURE_RELEASE_ORIGIN")
        .map(|origin| origin.trim_end_matches('/').to_owned())
        .map_err(|_| "Release checks are disabled for fixture-origin builds without DSD_FIXTURE_RELEASE_ORIGIN.")
}

fn cache_directory() -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("DSD_CACHE_DIR").filter(|value| !value.is_empty()) {
        let directory = PathBuf::from(directory);
        return if directory.is_absolute() {
            Ok(directory)
        } else {
            Err("DSD_CACHE_DIR must be an absolute directory path.".to_owned())
        };
    }
    default_cache_directory().ok_or_else(|| "No user cache directory is available; set DSD_CACHE_DIR.".to_owned())
}

fn default_cache_directory() -> Option<PathBuf> {
    let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
    if cfg!(windows) {
        absolute("LOCALAPPDATA").map(|root| root.join("dsd").join("cache"))
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|root| root.join("Library/Caches/dsd"))
    } else {
        absolute("XDG_CACHE_HOME")
            .map(|root| root.join("dsd"))
            .or_else(|| absolute("HOME").map(|root| root.join(".cache/dsd")))
    }
}

/// Runs `future` on a single-threaded runtime started for this network work.
pub fn block_on<F: Future>(future: F) -> Result<F::Output, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Failed to start the release HTTP runtime.".to_owned())?;
    let output = runtime.block_on(future);
    // A timed-out request can leave a blocking DNS lookup behind; dropping the
    // runtime would wait for it, so let process exit reclaim it instead.
    runtime.shutdown_background();
    Ok(output)
}

/// The advisory to attach after a successful command, if any.
pub fn incidental_advisory() -> Option<Value> {
    let settings = Settings::from_environment();
    if settings.disabled.is_some() {
        return None;
    }
    run_check(&settings, Mode::Incidental, Utc::now()).advisory()
}

/// The explicit report for `dsd version --check`; evidence problems are part of it.
pub fn explicit_check() -> ReleaseCheck {
    run_check(&Settings::from_environment(), Mode::Explicit, Utc::now())
}

fn run_check(settings: &Settings, mode: Mode, now: DateTime<Utc>) -> ReleaseCheck {
    let now = now.trunc_subsecs(0);
    if let Some(reason) = settings.disabled {
        return report(None, now, vec![reason.to_owned()]);
    }
    let mut problems = Vec::new();
    let directory = match &settings.cache_directory {
        Ok(directory) => Some(directory.as_path()),
        Err(problem) => {
            problems.push(problem.clone());
            None
        }
    };
    let mut record = directory
        .map(|directory| {
            load(directory).unwrap_or_else(|problem| {
                problems.push(problem);
                CacheRecord::default()
            })
        })
        .unwrap_or_default();
    if refresh_due(&record, now) {
        // Record the attempt before any network work. It doubles as the failure
        // cooldown if this process dies mid-refresh and proves the cache
        // persists: incidental checks refresh only then, so an unreadable or
        // unwritable cache cannot turn every invocation into GitHub requests.
        let attempt = CacheRecord {
            target: TARGET.map(str::to_owned),
            evidence: record.evidence.clone(),
            last_failure_at: Some(now),
        };
        let persisted = directory
            .is_some_and(|directory| save(directory, &attempt).map_err(|problem| problems.push(problem)).is_ok());
        if persisted || mode == Mode::Explicit {
            record = attempt;
            refresh(settings, directory.filter(|_| persisted), &mut record, &mut problems, now);
        }
    }
    report(Some(&record), now, problems)
}

fn refresh(
    settings: &Settings,
    directory: Option<&Path>,
    record: &mut CacheRecord,
    problems: &mut Vec<String>,
    now: DateTime<Utc>,
) {
    // The timer must be created inside the runtime that drives it.
    let refreshed = block_on(async {
        tokio::time::timeout(REFRESH_BUDGET, fetch_evidence(&settings.manifest_url, now))
            .await
            .unwrap_or_else(|_| Err(format!("Release refresh exceeded its {} ms budget.", REFRESH_BUDGET.as_millis())))
    })
    .and_then(|refreshed| refreshed);
    match refreshed {
        Ok(evidence) => {
            record.evidence = Some(evidence);
            record.last_failure_at = None;
        }
        Err(problem) => {
            problems.push(problem);
            // A concurrent process may have saved fresh evidence since this one
            // loaded the cache; keep it rather than overwrite it with a failure.
            if let Some(directory) = directory
                && let Ok(current) = load(directory)
                && current
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| within(evidence.observed_at, now, REFRESH_INTERVAL_SECONDS))
            {
                *record = current;
                return;
            }
            // Otherwise the attempt already recorded `now` as the last failure.
        }
    }
    if let Some(directory) = directory
        && let Err(problem) = save(directory, record)
    {
        problems.push(problem);
    }
}

fn refresh_due(record: &CacheRecord, now: DateTime<Utc>) -> bool {
    let evidence_fresh =
        record.evidence.as_ref().is_some_and(|evidence| within(evidence.observed_at, now, REFRESH_INTERVAL_SECONDS));
    let cooling_down = record.last_failure_at.is_some_and(|failure| within(failure, now, RETRY_COOLDOWN_SECONDS));
    !evidence_fresh && !cooling_down
}

// A timestamp in the future is not trusted; it would otherwise suppress refreshes.
fn within(then: DateTime<Utc>, now: DateTime<Utc>, seconds: i64) -> bool {
    then <= now && now - then < TimeDelta::seconds(seconds)
}

fn report(record: Option<&CacheRecord>, now: DateTime<Utc>, problems: Vec<String>) -> ReleaseCheck {
    let evidence = record.and_then(|record| record.evidence.as_ref());
    let latest = evidence.and_then(|evidence| evidence.latest.as_ref());
    let freshness = match evidence {
        None => Freshness::Unavailable,
        Some(evidence) if within(evidence.observed_at, now, REFRESH_INTERVAL_SECONDS) => Freshness::Fresh,
        Some(_) => Freshness::Stale,
    };
    let comparison = match Version::parse(CURRENT_VERSION) {
        None => Some(Comparison::Uncomparable),
        Some(current) => evidence.map(|evidence| match &evidence.latest {
            None => Comparison::NoStableRelease,
            Some(latest) => match latest.version.cmp(&current) {
                std::cmp::Ordering::Greater => Comparison::Newer,
                std::cmp::Ordering::Equal => Comparison::Equal,
                std::cmp::Ordering::Less => Comparison::Ahead,
            },
        }),
    };
    ReleaseCheck {
        current_version: CURRENT_VERSION,
        target: TARGET,
        comparison,
        latest_version: latest.map(|latest| latest.version),
        release_url: latest.map(|latest| latest.url.clone()),
        distribution: latest.map(|latest| latest.distribution),
        runtime_certified: latest.and_then(|latest| latest.runtime_certified),
        freshness,
        observed_at: evidence.map(|evidence| evidence.observed_at),
        // A future observation has no meaningful age.
        age_seconds: evidence
            .filter(|evidence| evidence.observed_at <= now)
            .map(|evidence| (now - evidence.observed_at).num_seconds()),
        last_refresh_failed_at: record.and_then(|record| record.last_failure_at),
        problems,
    }
}

impl ReleaseCheck {
    /// How to install a newer complete release, when there is one.
    pub fn update_help(&self) -> Option<String> {
        (self.comparison == Some(Comparison::Newer) && self.distribution == Some(Distribution::Complete)).then(|| {
            format!(
                "Run dsd upgrade to install it. Without an installation receipt, download the {} archive, SHA256SUMS, and {INSTALLER} from {} and repeat the README installation procedure.",
                TARGET.unwrap_or("platform"),
                self.release_url.as_deref().unwrap_or_default()
            )
        })
    }

    /// One advisory entry, omitted for fresh equal, ahead, and no-release results.
    fn advisory(&self) -> Option<Value> {
        let latest = self.latest_version.map_or_else(String::new, |version| version.to_string());
        let url = self.release_url.as_deref().unwrap_or_default();
        let stale_note = match (self.freshness, self.age_seconds) {
            (Freshness::Stale, Some(age)) => {
                format!(" Release evidence is stale: observed {} ago and not refreshed.", describe_age(age))
            }
            (Freshness::Stale, None) => " Release evidence is stale: its observation time is in the future.".to_owned(),
            _ => String::new(),
        };
        let (code, message) = match (self.comparison, self.distribution) {
            (Some(Comparison::Uncomparable), _) => (
                "version_uncomparable",
                format!(
                    "Running version {CURRENT_VERSION} is not a plain release version and cannot be compared with published releases."
                ),
            ),
            (Some(Comparison::Newer), Some(Distribution::Complete)) => (
                "update_available",
                format!(
                    "dsd {latest} is available; this is {CURRENT_VERSION}. {}{stale_note}",
                    self.update_help().unwrap_or_default()
                ),
            ),
            (Some(Comparison::Newer), _) => (
                "update_incomplete_distribution",
                format!(
                    "dsd {latest} is published at {url}, but its release manifest has no {} archive. Keep {CURRENT_VERSION} until the release is complete.{stale_note}",
                    TARGET.unwrap_or("platform")
                ),
            ),
            // Evidence problems carry the cause and next step, such as setting DSD_CACHE_DIR.
            _ if self.freshness == Freshness::Stale => {
                ("update_check_stale", self.with_problems(stale_note.trim_start()))
            }
            _ if self.freshness == Freshness::Unavailable => (
                "update_check_unavailable",
                self.with_problems(
                    "No usable release evidence is available; dsd cannot tell whether a newer release exists.",
                ),
            ),
            _ if !self.problems.is_empty() => ("update_check_problem", self.problems.join(" ")),
            _ => return None,
        };
        Some(json!({"code": code, "message": message, "check": self}))
    }

    fn with_problems(&self, message: &str) -> String {
        std::iter::once(message).chain(self.problems.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
    }
}

fn describe_age(seconds: i64) -> String {
    match seconds {
        ..7_200 => format!("{} minutes", seconds / 60),
        7_200..172_800 => format!("{} hours", seconds / 3_600),
        _ => format!("{} days", seconds / 86_400),
    }
}

pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("dsd/", env!("CARGO_PKG_VERSION")))
        // Never follow a redirect off TLS; only fixture origins are plain HTTP.
        .https_only(cfg!(not(feature = "fixture-origin")))
        .build()
        .map_err(|_| "Failed to initialize the release HTTP client.".to_owned())
}

/// GET a release resource of at most `limit` bytes; `None` when GitHub answers 404.
///
/// Redirects lead from github.com to the release asset host.
pub async fn fetch_bounded(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    label: &str,
) -> Result<Option<Vec<u8>>, String> {
    let too_large = || format!("The {label} exceeds {limit} bytes.");
    let mut response = client.get(url).send().await.map_err(|_| format!("The {label} request failed."))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("The {label} request returned HTTP {}.", response.status().as_u16()));
    }
    if response.content_length().is_some_and(|length| usize::try_from(length).map_or(true, |length| length > limit)) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| format!("The {label} response was interrupted."))? {
        if body.len() + chunk.len() > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

async fn fetch_evidence(manifest_url: &str, now: DateTime<Utc>) -> Result<Evidence, String> {
    let client = http_client()?;
    // GitHub answers 404 when no published release provides a manifest.
    let Some(body) = fetch_bounded(&client, manifest_url, MAX_MANIFEST_BYTES, "release manifest").await? else {
        return Ok(Evidence { observed_at: now, latest: None });
    };
    let manifest: Manifest = serde_json::from_slice(&body)
        .map_err(|_| "The latest release manifest is not valid release-manifest JSON.".to_owned())?;
    Ok(Evidence { observed_at: now, latest: Some(assess_release(&manifest, TARGET)?) })
}

/// The release a manifest describes, assessed for `target`.
///
/// Release completion uploads and verifies every asset on a draft before
/// publishing, so a published manifest implies its listed archives, checksums,
/// and installers exist; a missing target entry is what remains incomplete.
fn assess_release(manifest: &Manifest, target: Option<&str>) -> Result<LatestRelease, String> {
    let version = Version::parse(&manifest.version)
        .filter(|version| manifest.tag == format!("v{version}"))
        .ok_or_else(|| "The latest release manifest does not name a stable v<version> release.".to_owned())?;
    let entry = manifest.targets.iter().find(|entry| Some(entry.target.as_str()) == target);
    Ok(LatestRelease {
        version,
        url: format!("{RELEASE_PAGE_PREFIX}{}", manifest.tag),
        distribution: if entry.is_some() { Distribution::Complete } else { Distribution::IncompleteDistribution },
        runtime_certified: entry.map(|entry| entry.runtime_certified),
    })
}

/// Unreadable or corrupt cache is reported and treated as absent.
fn load(directory: &Path) -> Result<CacheRecord, String> {
    let path = directory.join(CACHE_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(CacheRecord::default()),
        Err(_) => return Err(format!("Could not read the release check cache {}.", path.display())),
    };
    let record: CacheRecord = serde_json::from_slice(&bytes)
        .map_err(|_| format!("Ignored a corrupt release check cache {}; it will be replaced.", path.display()))?;
    // Evidence assessed for another target says nothing about this one.
    Ok(if record.target.as_deref() == TARGET { record } else { CacheRecord::default() })
}

/// Atomic replace: concurrent processes may refresh redundantly but never read a partial file.
fn save(directory: &Path, record: &CacheRecord) -> Result<(), String> {
    let path = directory.join(CACHE_FILE);
    let temporary = directory.join(format!("{CACHE_FILE}.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(record).expect("cache record serializes");
    let result = std::fs::create_dir_all(directory)
        .and_then(|()| std::fs::write(&temporary, bytes))
        .and_then(|()| std::fs::rename(&temporary, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|_| format!("Could not write the release check cache {}.", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNREACHABLE: &str = "http://127.0.0.1:9/unreachable";

    fn manifest(version: &str, tag: &str, target: &str, certified: bool) -> Manifest {
        Manifest {
            version: version.to_owned(),
            tag: tag.to_owned(),
            targets: vec![ManifestTarget { target: target.to_owned(), runtime_certified: certified }],
        }
    }

    fn time(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn evidence(observed_at: &str, version: Option<&str>) -> Evidence {
        Evidence {
            observed_at: time(observed_at),
            latest: version.map(|version| LatestRelease {
                version: Version::parse(version).unwrap(),
                url: format!("{RELEASE_PAGE_PREFIX}v{version}"),
                distribution: Distribution::Complete,
                runtime_certified: Some(true),
            }),
        }
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("dsd-release-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    fn record(evidence: Option<Evidence>) -> CacheRecord {
        CacheRecord { target: TARGET.map(str::to_owned), evidence, last_failure_at: None }
    }

    #[test]
    fn versions_order_numerically_and_reject_non_release_identities() {
        let parse = |text| Version::parse(text).unwrap();
        assert!(parse("0.10.0") > parse("0.9.9"));
        assert!(parse("1.0.0") > parse("0.99.99"));
        assert_eq!(parse("0.6.2").to_string(), "0.6.2");
        for invalid in ["", "1.2", "1.2.3.4", "01.2.3", "1.2.3-rc.1", "1.2.3+build", "v1.2.3", "1..3"] {
            assert_eq!(Version::parse(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn manifest_must_name_a_stable_release_with_a_matching_tag() {
        let target = Some("linux-x64-gnu");
        let release = assess_release(&manifest("1.0.0", "v1.0.0", "linux-x64-gnu", true), target).unwrap();
        assert_eq!(release.version.to_string(), "1.0.0");
        assert_eq!(release.url, "https://github.com/cpaikr/dsdai/releases/tag/v1.0.0");
        for (version, tag) in
            [("1.0.0", "v0.9.0"), ("1.0.0", "1.0.0"), ("1.0.0-rc.1", "v1.0.0-rc.1"), ("latest", "vlatest")]
        {
            assert!(assess_release(&manifest(version, tag, "linux-x64-gnu", true), target).is_err(), "{version} {tag}");
        }
    }

    #[test]
    fn distribution_requires_a_manifest_entry_for_this_target() {
        let listed = manifest("1.0.0", "v1.0.0", "linux-x64-gnu", true);
        let release = assess_release(&listed, Some("linux-x64-gnu")).unwrap();
        assert_eq!((release.distribution, release.runtime_certified), (Distribution::Complete, Some(true)));
        for target in [Some("darwin-arm64"), None] {
            let release = assess_release(&listed, target).unwrap();
            assert_eq!((release.distribution, release.runtime_certified), (Distribution::IncompleteDistribution, None));
        }
    }

    #[test]
    fn published_manifest_shape_parses() {
        let published = r#"{"version":"0.1.0","tag":"v0.1.0","sourceRevision":"0","compiler":"rustc 1.88.0",
            "targets":[{"target":"win32-x64","archive":"dsd-0.1.0-win32-x64.tar.gz","sha256":"00",
                        "executableSha256":"00","runtimeCertified":true}],
            "installers":[{"name":"install.sh","sha256":"00"}]}"#;
        let manifest: Manifest = serde_json::from_str(published).unwrap();
        let release = assess_release(&manifest, Some("win32-x64")).unwrap();
        assert_eq!(release.distribution, Distribution::Complete);
    }

    #[test]
    fn build_targets_match_the_release_inventory() {
        let inventory: Vec<Value> =
            serde_json::from_str(include_str!("../../../scripts/release-targets.json")).unwrap();
        let ids: Vec<_> = inventory.iter().map(|target| target["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["linux-x64-gnu", "linux-arm64-gnu", "darwin-arm64", "win32-x64"], "update TARGET too");
        if let Some(target) = TARGET {
            assert!(ids.contains(&target));
        }
    }

    #[test]
    fn freshness_and_comparison_are_reported_separately() {
        let now = time("2026-09-30T12:00:00Z");
        let current = Version::parse(CURRENT_VERSION).unwrap();
        let newer = Version { major: current.major + 1, ..current }.to_string();
        let older = Version { major: 0, minor: 0, patch: 0 }.to_string();

        let check = report(Some(&record(Some(evidence("2026-09-30T00:00:00Z", Some(CURRENT_VERSION))))), now, vec![]);
        assert_eq!((check.comparison, check.freshness), (Some(Comparison::Equal), Freshness::Fresh));
        assert_eq!(check.age_seconds, Some(43_200));
        assert!(check.advisory().is_none());

        let check = report(Some(&record(Some(evidence("2026-09-30T00:00:00Z", Some(&older))))), now, vec![]);
        assert_eq!(check.comparison, Some(Comparison::Ahead));
        assert!(check.advisory().is_none());

        let check = report(Some(&record(Some(evidence("2026-09-30T00:00:00Z", None)))), now, vec![]);
        assert_eq!(check.comparison, Some(Comparison::NoStableRelease));
        assert!(check.advisory().is_none());

        let check = report(Some(&record(Some(evidence("2026-09-28T00:00:00Z", Some(CURRENT_VERSION))))), now, vec![]);
        assert_eq!((check.comparison, check.freshness), (Some(Comparison::Equal), Freshness::Stale));
        assert_eq!(check.advisory().unwrap()["code"], "update_check_stale");

        let check = report(Some(&record(Some(evidence("2026-09-28T00:00:00Z", Some(&newer))))), now, vec![]);
        let advisory = check.advisory().unwrap();
        assert_eq!(advisory["code"], "update_available");
        assert!(advisory["message"].as_str().unwrap().contains("stale"));
        assert!(check.update_help().unwrap().starts_with("Run dsd upgrade"));

        let check = report(Some(&record(None)), now, vec![]);
        assert_eq!((check.comparison, check.freshness), (None, Freshness::Unavailable));
        assert_eq!(check.advisory().unwrap()["code"], "update_check_unavailable");

        let check = report(
            Some(&record(Some(evidence("2026-09-30T00:00:00Z", Some(CURRENT_VERSION))))),
            now,
            vec!["cache problem".to_owned()],
        );
        assert_eq!(check.advisory().unwrap()["code"], "update_check_problem");
    }

    #[test]
    fn refresh_waits_for_the_interval_and_the_failure_cooldown() {
        let now = time("2026-09-30T12:00:00Z");
        let record = |observed: Option<&str>, failed: Option<&str>| CacheRecord {
            target: TARGET.map(str::to_owned),
            evidence: observed.map(|observed| evidence(observed, None)),
            last_failure_at: failed.map(time),
        };
        assert!(refresh_due(&record(None, None), now));
        assert!(!refresh_due(&record(Some("2026-09-29T12:00:01Z"), None), now));
        assert!(refresh_due(&record(Some("2026-09-29T12:00:00Z"), None), now));
        assert!(!refresh_due(&record(None, Some("2026-09-30T11:00:01Z")), now));
        assert!(refresh_due(&record(None, Some("2026-09-30T11:00:00Z")), now));
        // Future timestamps are distrusted rather than suppressing refresh forever.
        assert!(refresh_due(&record(Some("2026-10-30T00:00:00Z"), None), now));
        assert!(refresh_due(&record(None, Some("2026-10-30T00:00:00Z")), now));
    }

    #[test]
    fn corrupt_cache_is_an_inspection_problem_and_saves_round_trip() {
        let directory = temporary_directory("cache");
        assert_eq!(load(&directory), Ok(CacheRecord::default()));
        let record = CacheRecord {
            target: TARGET.map(str::to_owned),
            evidence: Some(evidence("2026-09-30T00:00:00Z", Some("1.2.3"))),
            last_failure_at: Some(time("2026-09-30T01:00:00Z")),
        };
        // A refresh saves its attempt first, then replaces it with evidence.
        save(&directory, &CacheRecord::default()).unwrap();
        save(&directory, &record).unwrap();
        assert_eq!(load(&directory), Ok(record));
        std::fs::write(directory.join(CACHE_FILE), b"{\"evidence\":").unwrap();
        assert!(load(&directory).unwrap_err().contains("corrupt"));
        std::fs::write(directory.join(CACHE_FILE), br#"{"target":"other","evidence":null,"lastFailureAt":null}"#)
            .unwrap();
        assert_eq!(load(&directory), Ok(CacheRecord::default()));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn disabled_checks_do_no_cache_or_network_work() {
        let directory = temporary_directory("disabled");
        let settings = Settings {
            disabled: Some(OPT_OUT),
            manifest_url: UNREACHABLE.to_owned(),
            cache_directory: Ok(directory.clone()),
        };
        let check = run_check(&settings, Mode::Explicit, Utc::now());
        assert_eq!(check.freshness, Freshness::Unavailable);
        assert_eq!(check.problems, [OPT_OUT]);
        assert!(!directory.exists());
    }

    #[test]
    fn incidental_checks_without_a_cache_do_not_refresh() {
        let settings = Settings {
            disabled: None,
            manifest_url: UNREACHABLE.to_owned(),
            cache_directory: Err("No user cache directory is available; set DSD_CACHE_DIR.".to_owned()),
        };
        let check = run_check(&settings, Mode::Incidental, Utc::now());
        assert_eq!(check.freshness, Freshness::Unavailable);
        assert_eq!(check.problems.len(), 1, "{:?}", check.problems);
        assert!(check.last_refresh_failed_at.is_none());
        let advisory = check.advisory().unwrap();
        assert_eq!(advisory["code"], "update_check_unavailable");
        assert!(advisory["message"].as_str().unwrap().ends_with("set DSD_CACHE_DIR."));
    }

    #[test]
    fn failed_refresh_keeps_fresh_evidence_saved_concurrently() {
        let directory = temporary_directory("concurrent");
        let now = Utc::now().trunc_subsecs(0);
        let fresh = record(Some(Evidence { observed_at: now, latest: None }));
        save(&directory, &fresh).unwrap();
        // This process loaded the cache before the other one saved evidence.
        let mut attempt = CacheRecord { target: TARGET.map(str::to_owned), evidence: None, last_failure_at: Some(now) };
        let settings =
            Settings { disabled: None, manifest_url: UNREACHABLE.to_owned(), cache_directory: Ok(directory.clone()) };
        let mut problems = Vec::new();
        refresh(&settings, Some(&directory), &mut attempt, &mut problems, now);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(attempt, fresh);
        assert_eq!(load(&directory), Ok(fresh));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn incidental_checks_with_an_unwritable_cache_do_not_refresh() {
        let directory = temporary_directory("unwritable");
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("not-a-directory");
        std::fs::write(&file, b"file").unwrap();
        let settings = Settings { disabled: None, manifest_url: UNREACHABLE.to_owned(), cache_directory: Ok(file) };
        let check = run_check(&settings, Mode::Incidental, Utc::now());
        assert!(check.last_refresh_failed_at.is_none(), "{:?}", check.problems);
        assert!(check.problems.iter().all(|problem| problem.contains("cache")), "no network: {:?}", check.problems);
        // The explicit check still refreshes and reports the failure.
        let check = run_check(&settings, Mode::Explicit, Utc::now());
        assert!(check.last_refresh_failed_at.is_some());
        assert!(check.problems.iter().any(|problem| problem.contains("request") || problem.contains("budget")));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
