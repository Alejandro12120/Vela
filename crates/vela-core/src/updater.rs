//! Signed in-app update checking, downloading, verification and installation.
//!
//! Fetches `manifest.json` and release archives produced by
//! `.github/workflows/release.yml` and `scripts/sign-release.py`, verifies
//! OpenSSH Ed25519 (`ssh-keygen -Y verify`) signatures and SHA-256 checksums,
//! installs atomically for Linux x86_64 and macOS Apple Silicon, and relaunches
//! the updated application.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const REPO: &str = "Alejandro12120/Vela";
pub const SIGNER_IDENTITY: &str = "vela@release";
pub const SIGNER_NAMESPACE: &str = "vela-release";
pub const DEFAULT_PUBKEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAgeCkjVNnR0gfJF3QeavTEs2HlrvT0cWrBrU3oc3dko";
pub const DEFAULT_BASE_URL: &str =
    "https://github.com/Alejandro12120/Vela/releases/latest/download";

const ALLOWED_URL_PREFIXES: &[&str] = &[
    "https://github.com/Alejandro12120/Vela/releases/download/",
    "https://github.com/Alejandro12120/vSCP/releases/download/",
];

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_RELEASES_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SIG_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("Automatic updates are unavailable on this platform.")]
    UnsupportedPlatform,
    #[error("Couldn't check for updates: {0}")]
    DownloadFailed(String),
    #[error("The Ed25519 signature for {0} is invalid.")]
    SignatureInvalid(String),
    #[error("Invalid update manifest: {0}")]
    InvalidManifest(String),
    #[error("No update package is available for this platform ({0}).")]
    PlatformMissing(String),
    #[error("Unexpected file size (expected {expected}, got {actual}).")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("The downloaded package's SHA-256 checksum doesn't match.")]
    ChecksumMismatch,
    #[error("Invalid downloaded package: {0}")]
    InvalidArchive(String),
    #[error("Couldn't install the update: {0}")]
    InstallFailed(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub current_version: String,
    pub platform: Option<String>,
    /// Optional base URL override (e.g. `file:///...` or local HTTP mirror for tests/dev).
    pub base_url: Option<String>,
    pub include_prereleases: bool,
    /// Optional override for the GitHub Releases API endpoint.
    pub releases_api_url: Option<String>,
    /// OpenSSH Ed25519 public key (`ssh-ed25519 AAAA...`).
    pub pubkey: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            current_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: current_platform().map(str::to_owned),
            base_url: None,
            include_prereleases: false,
            releases_api_url: None,
            pubkey: DEFAULT_PUBKEY.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseInfo {
    pub version: String,
    pub is_prerelease: bool,
    pub is_downgrade: bool,
    pub tag: String,
    pub asset: String,
    pub url: String,
    pub sig_url: String,
    pub sha256: String,
    pub size: u64,
    pub notes_url: Option<String>,
    pub changelog: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckOutcome {
    UpToDate,
    UpdateAvailable(Box<ReleaseInfo>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Downloading,
    Verifying,
    Installing,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    /// Normalized progress in `0.0..=1.0`.
    pub fraction: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallTarget {
    Linux {
        binary_path: PathBuf,
        data_dir: Option<PathBuf>,
    },
    MacAppBundle {
        app_bundle: PathBuf,
    },
    BinaryOnly {
        binary_path: PathBuf,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelaunchTarget {
    Binary(PathBuf),
    MacAppBundle(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallOutcome {
    pub relaunch: RelaunchTarget,
}

#[derive(Debug)]
pub enum InstallEvent {
    Progress(Progress),
    Finished(Result<InstallOutcome, UpdateError>),
}

/// Streams progress and completion events from a background update installation.
pub struct InstallHandle {
    events: async_channel::Receiver<InstallEvent>,
}

impl InstallHandle {
    pub async fn next(&self) -> Option<InstallEvent> {
        self.events.recv().await.ok()
    }
}

/// Runs [`download_and_install`] on a dedicated background thread and streams progress events.
pub fn prepare_install(
    config: Config,
    release: ReleaseInfo,
    target: InstallTarget,
) -> (impl FnOnce() + Send + 'static, InstallHandle) {
    let (tx, rx) = async_channel::unbounded();
    let run = move || {
        let result = download_and_install(&config, &release, &target, |progress| {
            let _ = tx.send_blocking(InstallEvent::Progress(progress));
        });
        let _ = tx.send_blocking(InstallEvent::Finished(result));
    };
    (run, InstallHandle { events: rx })
}

pub fn spawn_install(config: Config, release: ReleaseInfo, target: InstallTarget) -> InstallHandle {
    let (run, handle) = prepare_install(config, release, target);
    thread::spawn(run);
    handle
}

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    version: String,
    tag: String,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default, deserialize_with = "optional_notes")]
    notes: Option<String>,
    #[serde(default, deserialize_with = "optional_notes")]
    changelog: Option<String>,
    platforms: HashMap<String, PlatformEntry>,
}

#[derive(Deserialize)]
struct PlatformEntry {
    asset: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Deserialize)]
struct PublishedRelease {
    tag_name: String,
    draft: bool,
    assets: Vec<ReleaseAsset>,
    #[serde(default, deserialize_with = "optional_notes")]
    body: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

impl PublishedRelease {
    fn asset_url(&self, name: &str) -> Option<&str> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.as_str())
    }
}

struct SelectedRelease {
    tag: String,
    manifest_url: String,
    signature_url: String,
    body: Option<String>,
}

/// Platform identifier matching `manifest.json` keys.
pub fn current_platform() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-aarch64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else {
        None
    }
}

impl InstallTarget {
    /// Detects the installation target for the currently running binary.
    pub fn detect_current() -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let resolved = fs::canonicalize(&exe).unwrap_or(exe);
        Ok(Self::from_exe_path(resolved, cfg!(target_os = "macos")))
    }

    fn from_exe_path(binary_path: PathBuf, is_macos: bool) -> Self {
        if is_macos {
            if let Some(app_bundle) = enclosing_app_bundle(&binary_path) {
                return Self::MacAppBundle { app_bundle };
            }
            Self::BinaryOnly { binary_path }
        } else {
            Self::Linux {
                binary_path,
                data_dir: xdg_data_dir(),
            }
        }
    }
}

fn enclosing_app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let app_bundle = contents_dir.parent()?;
    if app_bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        return None;
    }
    Some(app_bundle.to_owned())
}

fn xdg_data_dir() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("XDG_DATA_HOME")
        && !value.is_empty()
    {
        return Some(PathBuf::from(value));
    }
    std::env::home_dir().map(|home| home.join(".local/share"))
}

/// Checks for a signed update or a return to the stable channel.
pub fn check(config: &Config) -> Result<CheckOutcome, UpdateError> {
    let platform = config
        .platform
        .as_deref()
        .ok_or(UpdateError::UnsupportedPlatform)?;
    let current = semver::Version::parse(&config.current_version)
        .map_err(|err| UpdateError::InvalidManifest(format!("current version: {err}")))?;

    let custom_base = config.base_url.as_deref().map(|u| u.trim_end_matches('/'));
    let base = custom_base.unwrap_or(DEFAULT_BASE_URL);
    let strict_https = custom_base.is_none();

    let tmp = private_tempdir("vela-update-check-")?;
    let allowed_signers = write_allowed_signers(tmp.path(), &config.pubkey)?;

    let selected = if config.include_prereleases
        && (custom_base.is_none() || config.releases_api_url.is_some())
    {
        let Some(release) = latest_published_release(config, tmp.path())? else {
            return Ok(CheckOutcome::UpToDate);
        };
        Some(release)
    } else {
        None
    };
    let manifest_url = format!("{base}/manifest.json");
    let signature_url = format!("{base}/manifest.json.sshsig");
    let (manifest_url, signature_url) = match &selected {
        Some(release) => {
            if strict_https {
                validate_download_url(&release.manifest_url)?;
                validate_download_url(&release.signature_url)?;
            }
            (
                release.manifest_url.as_str(),
                release.signature_url.as_str(),
            )
        }
        None => (manifest_url.as_str(), signature_url.as_str()),
    };
    let manifest_path = tmp.path().join("manifest.json");
    let manifest_sig = tmp.path().join("manifest.json.sshsig");

    std::thread::scope(|scope| {
        let signature = scope.spawn(|| {
            download_small_file(signature_url, &manifest_sig, MAX_SIG_BYTES, strict_https)
        });
        download_small_file(
            manifest_url,
            &manifest_path,
            MAX_MANIFEST_BYTES,
            strict_https,
        )?;
        signature.join().expect("signature download panicked")
    })?;

    verify_sshsig(
        &manifest_path,
        &manifest_sig,
        &allowed_signers,
        "manifest.json",
    )?;

    let raw = fs::read_to_string(&manifest_path)?;
    let manifest: Manifest =
        serde_json::from_str(&raw).map_err(|err| UpdateError::InvalidManifest(err.to_string()))?;

    if manifest.schema != 1 {
        return Err(UpdateError::InvalidManifest(format!(
            "unsupported schema ({})",
            manifest.schema
        )));
    }

    let remote = semver::Version::parse(&manifest.version)
        .map_err(|err| UpdateError::InvalidManifest(format!("remote version: {err}")))?;

    if let Some(release) = &selected
        && (manifest.tag != release.tag
            || manifest.version != release.tag.strip_prefix('v').unwrap_or(&release.tag))
    {
        return Err(UpdateError::InvalidManifest(
            "the signed version and tag do not match the selected release".into(),
        ));
    }

    let precedence = remote.cmp_precedence(&current);
    let is_downgrade = !config.include_prereleases && !current.pre.is_empty() && precedence.is_lt();
    if !precedence.is_gt() && !is_downgrade {
        return Ok(CheckOutcome::UpToDate);
    }

    let entry = manifest
        .platforms
        .get(platform)
        .ok_or_else(|| UpdateError::PlatformMissing(platform.to_owned()))?;

    validate_asset_name(&entry.asset)?;
    validate_sha256(&entry.sha256)?;

    let (url, sig_url) = if let Some(base) = custom_base.filter(|_| selected.is_none()) {
        (
            format!("{base}/{}", entry.asset),
            format!("{base}/{}.sshsig", entry.asset),
        )
    } else {
        if strict_https {
            validate_download_url(&entry.url)?;
        }
        (entry.url.clone(), format!("{}.sshsig", entry.url))
    };

    let mut changelog = manifest.notes.or(manifest.changelog);
    if changelog.is_none() {
        changelog = match selected {
            Some(release) => release.body,
            None if strict_https => github_release_notes(&manifest.tag, tmp.path()),
            None => None,
        };
    }
    Ok(CheckOutcome::UpdateAvailable(Box::new(ReleaseInfo {
        version: manifest.version,
        is_prerelease: !remote.pre.is_empty(),
        is_downgrade,
        tag: manifest.tag,
        asset: entry.asset.clone(),
        url,
        sig_url,
        sha256: entry.sha256.clone(),
        size: entry.size,
        notes_url: manifest.notes_url,
        changelog,
    })))
}

fn latest_published_release(
    config: &Config,
    dir: &Path,
) -> Result<Option<SelectedRelease>, UpdateError> {
    let default_url = format!("https://api.github.com/repos/{REPO}/releases?per_page=20");
    let url = config.releases_api_url.as_deref().unwrap_or(&default_url);
    let mut command = curl_command(config.releases_api_url.is_none(), "60");
    command
        .args([
            "--header",
            "Accept: application/vnd.github+json",
            "--user-agent",
        ])
        .arg(format!("Vela/{}", config.current_version))
        .args(["--", url]);
    let path = dir.join("releases.json");
    download_small_response(command, &path, MAX_RELEASES_BYTES)?;
    let releases: Vec<PublishedRelease> = serde_json::from_slice(&fs::read(path)?)
        .map_err(|err| UpdateError::DownloadFailed(format!("invalid release list: {err}")))?;
    Ok(releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name);
            let version = semver::Version::parse(version).ok()?;
            let manifest_url = release.asset_url("manifest.json")?.to_owned();
            let signature_url = release.asset_url("manifest.json.sshsig")?.to_owned();
            Some((
                version,
                SelectedRelease {
                    tag: release.tag_name,
                    manifest_url,
                    signature_url,
                    body: release.body,
                },
            ))
        })
        .max_by(|(left, _), (right, _)| left.cmp_precedence(right))
        .map(|(_, release)| release))
}

// Display metadata is optional even when an older producer wrote it with
// an unexpected JSON type.
fn optional_notes<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(de)?;
    Ok(value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned))
}

fn github_release_notes(tag: &str, dir: &Path) -> Option<String> {
    // Release tags can contain slashes and other reserved URL characters.
    let tag: String = tag
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    let url = format!("https://api.github.com/repos/{REPO}/releases/tags/{tag}");
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "-fsSL",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--max-time",
        "3",
        "--header",
        "Accept: application/vnd.github+json",
        "--user-agent",
        "Vela",
        "--",
        &url,
    ]);
    let path = dir.join("release.json");
    download_small_response(command, &path, MAX_MANIFEST_BYTES).ok()?;
    parse_release_body(&fs::read(path).ok()?)
}

fn parse_release_body(raw: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct GithubRelease {
        #[serde(default, deserialize_with = "optional_notes")]
        body: Option<String>,
    }
    serde_json::from_slice::<GithubRelease>(raw).ok()?.body
}

/// Up to three short, plain-text lines suitable for a tooltip or dialog.
pub fn changelog_preview(markdown: &str) -> Vec<String> {
    let mut preview = Vec::new();
    let mut fence = None;
    let mut in_comment = false;
    for line in markdown.lines().take(100) {
        let line = line.trim();
        let line = &line[..line.floor_char_boundary(512)];
        if in_comment || line.starts_with("<!--") {
            in_comment = !line.contains("-->");
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            let marker = line.as_bytes()[0];
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if line.starts_with('#')
            && line
                .trim_matches('#')
                .trim()
                .eq_ignore_ascii_case("new contributors")
        {
            break;
        }
        if line.starts_with(['#', '<']) || line.starts_with("![") {
            continue;
        }
        let line = line.trim_start_matches(['-', '*', '+', '>']).trim_start();
        let line = line
            .split_once(". ")
            .filter(|(prefix, _)| !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()))
            .map_or(line, |(_, text)| text);
        let line = line
            .split_once(" by @")
            .filter(|(_, attribution)| attribution.contains(" in https://github.com/"))
            .map_or(line, |(text, _)| text);
        let text = strip_inline_markdown(line);
        let text = text.trim();
        if text.is_empty()
            || text.eq_ignore_ascii_case("full changelog:")
            || text.chars().all(|ch| ch.is_ascii_punctuation())
        {
            continue;
        }
        let mut characters = text.chars();
        let mut shortened: String = characters.by_ref().take(84).collect();
        if characters.next().is_some() {
            shortened.push('…');
        }
        preview.push(shortened);
        if preview.len() == 3 {
            break;
        }
    }
    preview
}

fn strip_inline_markdown(mut line: &str) -> String {
    let mut text = String::new();
    while let Some((before, rest)) = line.split_once('[') {
        text.push_str(before.trim_end_matches('!'));
        let Some((label, after)) = rest.split_once("](") else {
            text.push('[');
            line = rest;
            continue;
        };
        let Some((_, remaining)) = after.split_once(')') else {
            text.push('[');
            line = rest;
            continue;
        };
        if !before.ends_with('!') {
            text.push_str(label);
        }
        line = remaining;
    }
    text.push_str(line);
    text.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace('`', "")
        .split_whitespace()
        .filter(|word| {
            let word = word.trim_start_matches('<');
            !word.starts_with("https://") && !word.starts_with("http://")
        })
        .map(|word| word.trim_matches(['*', '_']))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Downloads, verifies (SHA-256 + Ed25519), extracts and atomically installs `release`.
pub fn download_and_install(
    config: &Config,
    release: &ReleaseInfo,
    target: &InstallTarget,
    mut on_progress: impl FnMut(Progress),
) -> Result<InstallOutcome, UpdateError> {
    validate_asset_name(&release.asset)?;
    validate_sha256(&release.sha256)?;
    let strict_https = config.base_url.is_none();
    if strict_https {
        validate_download_url(&release.url)?;
    }

    on_progress(Progress {
        stage: Stage::Downloading,
        fraction: 0.03,
    });

    let tmp = private_tempdir("vela-update-")?;
    let allowed_signers = write_allowed_signers(tmp.path(), &config.pubkey)?;

    let archive_path = tmp.path().join(&release.asset);
    let archive_sig = tmp.path().join(format!("{}.sshsig", release.asset));

    let actual_sha256 = download_archive_with_progress(
        &release.url,
        &archive_path,
        release.size,
        strict_https,
        |downloaded, total| {
            let ratio = if total > 0 {
                (downloaded as f32 / total as f32).clamp(0., 1.)
            } else {
                0.5
            };
            on_progress(Progress {
                stage: Stage::Downloading,
                fraction: 0.03 + ratio * 0.82,
            });
        },
    )?;

    on_progress(Progress {
        stage: Stage::Verifying,
        fraction: 0.87,
    });

    if actual_sha256 != release.sha256 {
        return Err(UpdateError::ChecksumMismatch);
    }

    download_small_file(&release.sig_url, &archive_sig, MAX_SIG_BYTES, strict_https)?;
    verify_sshsig(
        &archive_path,
        &archive_sig,
        &allowed_signers,
        &release.asset,
    )?;

    on_progress(Progress {
        stage: Stage::Installing,
        fraction: 0.92,
    });

    validate_tar_archive(&archive_path)?;
    let extract_dir = tmp.path().join("extract");
    fs::create_dir(&extract_dir)?;
    extract_tar_gz(&archive_path, &extract_dir)?;

    on_progress(Progress {
        stage: Stage::Installing,
        fraction: 0.96,
    });

    let relaunch = match target {
        InstallTarget::Linux {
            binary_path,
            data_dir,
        } => install_linux(&extract_dir, binary_path, data_dir.as_deref())?,
        InstallTarget::MacAppBundle { app_bundle } => {
            install_macos_bundle(&extract_dir, app_bundle)?
        }
        InstallTarget::BinaryOnly { binary_path } => {
            install_binary_only(&extract_dir, binary_path)?
        }
    };

    on_progress(Progress {
        stage: Stage::Ready,
        fraction: 1.0,
    });

    Ok(InstallOutcome { relaunch })
}

/// Spawns the newly installed application detached from the current process.
pub fn relaunch(target: &RelaunchTarget) -> io::Result<()> {
    match target {
        RelaunchTarget::Binary(binary_path) => {
            let mut command = Command::new(binary_path);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: `setsid()` is async-signal-safe per POSIX.
            unsafe {
                command.pre_exec(|| {
                    let _ = libc::setsid();
                    Ok(())
                });
            }
            command.spawn()?;
            Ok(())
        }
        RelaunchTarget::MacAppBundle(app_bundle) => {
            let pid = std::process::id().to_string();
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "pid=\"$1\"; app=\"$2\"; i=0; while [ \"$i\" -lt 100 ] && kill -0 \"$pid\" 2>/dev/null; do sleep 0.05; i=$((i + 1)); done; exec /usr/bin/open \"$app\"",
                    "_",
                    &pid,
                ])
                .arg(app_bundle)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: `setsid()` is async-signal-safe per POSIX.
            unsafe {
                command.pre_exec(|| {
                    let _ = libc::setsid();
                    Ok(())
                });
            }
            command.spawn()?;
            Ok(())
        }
    }
}

fn private_tempdir(prefix: &str) -> io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix(prefix).tempdir()?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

fn write_allowed_signers(dir: &Path, pubkey: &str) -> io::Result<PathBuf> {
    let path = dir.join("allowed_signers");
    let mut file = File::create(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    writeln!(file, "{SIGNER_IDENTITY} {}", pubkey.trim())?;
    Ok(path)
}

fn curl_command(strict_https: bool, max_time: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-q",
        "-f",
        "-s",
        "-S",
        "-L",
        "--connect-timeout",
        "15",
        "--max-time",
        max_time,
        "--retry",
        "2",
        "--retry-delay",
        "1",
    ]);
    if strict_https {
        cmd.args(["--proto", "=https", "--proto-redir", "=https", "--tlsv1.2"]);
    }
    cmd
}

fn download_small_file(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    strict_https: bool,
) -> Result<(), UpdateError> {
    let mut command = curl_command(strict_https, "60");
    command.args(["--", url]);
    download_small_response(command, dest, max_bytes)
}

fn download_small_response(
    mut command: Command,
    dest: &Path,
    max_bytes: u64,
) -> Result<(), UpdateError> {
    let mut file = File::create(dest)?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| UpdateError::DownloadFailed(format!("curl: {err}")))?;

    let stderr_handle = spawn_stderr_reader(child.stderr.take());
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| UpdateError::DownloadFailed("no output from curl".into()))?;

    let copied = io::copy(&mut stdout.take(max_bytes + 1), &mut file);
    if !matches!(copied, Ok(bytes) if bytes <= max_bytes) {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let stderr = join_stderr_reader(stderr_handle);
    if copied? > max_bytes {
        return Err(UpdateError::DownloadFailed(
            "the response exceeded the size limit".into(),
        ));
    }
    if !status.success() {
        let detail = stderr.trim();
        return Err(UpdateError::DownloadFailed(if detail.is_empty() {
            format!("curl exited with status {status}")
        } else {
            detail.to_owned()
        }));
    }
    Ok(())
}

fn download_archive_with_progress(
    url: &str,
    dest: &Path,
    expected_size: u64,
    strict_https: bool,
    mut on_chunk: impl FnMut(u64, u64),
) -> Result<String, UpdateError> {
    let mut child = curl_command(strict_https, "600")
        .args(["--", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| UpdateError::DownloadFailed(format!("curl: {err}")))?;

    let stderr_handle = spawn_stderr_reader(child.stderr.take());
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| UpdateError::DownloadFailed("no output from curl".into()))?;

    let mut file = File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_reported: u64 = 0;
    let report_step = (expected_size / 100).clamp(32 * 1024, 256 * 1024);
    let mut buf = [0u8; 64 * 1024];

    loop {
        let read = stdout.read(&mut buf)?;
        if read == 0 {
            break;
        }
        downloaded += read as u64;
        if downloaded > MAX_ARCHIVE_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return Err(UpdateError::DownloadFailed(
                "the file exceeded the 512 MiB limit".into(),
            ));
        }
        file.write_all(&buf[..read])?;
        hasher.update(&buf[..read]);
        if downloaded.saturating_sub(last_reported) >= report_step || downloaded == expected_size {
            last_reported = downloaded;
            on_chunk(downloaded, expected_size);
        }
    }
    file.flush()?;
    on_chunk(downloaded, expected_size);

    let status = child.wait()?;
    let stderr = join_stderr_reader(stderr_handle);
    if !status.success() {
        let detail = stderr.trim();
        return Err(UpdateError::DownloadFailed(if detail.is_empty() {
            format!("curl exited with status {status}")
        } else {
            detail.to_owned()
        }));
    }

    if expected_size > 0 && downloaded != expected_size {
        return Err(UpdateError::SizeMismatch {
            expected: expected_size,
            actual: downloaded,
        });
    }

    let digest = hasher.finalize();
    Ok(hex_lower(&digest))
}

fn spawn_stderr_reader(
    stderr: Option<std::process::ChildStderr>,
) -> Option<thread::JoinHandle<String>> {
    stderr.map(|mut err| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = err.read_to_string(&mut text);
            text
        })
    })
}

fn join_stderr_reader(handle: Option<thread::JoinHandle<String>>) -> String {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

fn verify_sshsig(
    file_path: &Path,
    sig_path: &Path,
    allowed_signers: &Path,
    label: &str,
) -> Result<(), UpdateError> {
    let input = File::open(file_path)?;
    let output = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-f"])
        .arg(allowed_signers)
        .args(["-I", SIGNER_IDENTITY, "-n", SIGNER_NAMESPACE, "-s"])
        .arg(sig_path)
        .stdin(Stdio::from(input))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| UpdateError::InstallFailed(format!("ssh-keygen: {err}")))?;

    if !output.status.success() {
        return Err(UpdateError::SignatureInvalid(label.to_owned()));
    }
    Ok(())
}

fn validate_asset_name(name: &str) -> Result<(), UpdateError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(UpdateError::InvalidManifest(format!(
            "invalid filename: '{name}'"
        )));
    }
    Ok(())
}

fn validate_sha256(digest: &str) -> Result<(), UpdateError> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(UpdateError::InvalidManifest(format!(
            "Invalid SHA-256 checksum: '{digest}'"
        )));
    }
    Ok(())
}

fn validate_download_url(url: &str) -> Result<(), UpdateError> {
    if ALLOWED_URL_PREFIXES
        .iter()
        .any(|prefix| url.starts_with(prefix))
    {
        Ok(())
    } else {
        Err(UpdateError::InvalidManifest(format!(
            "Disallowed download URL: '{url}'"
        )))
    }
}

fn validate_tar_archive(archive: &Path) -> Result<(), UpdateError> {
    let output = Command::new("tar")
        .arg("-tzf")
        .arg(archive)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| UpdateError::InvalidArchive(format!("tar: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::InvalidArchive(stderr.trim().to_owned()));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    validate_tar_entries(listing.lines())
}

fn validate_tar_entries<'a>(entries: impl IntoIterator<Item = &'a str>) -> Result<(), UpdateError> {
    for raw in entries {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        let path = Path::new(entry);
        if path.is_absolute()
            || path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(UpdateError::InvalidArchive(format!(
                "unsafe path in the archive: '{entry}'"
            )));
        }
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, extract_dir: &Path) -> Result<(), UpdateError> {
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(extract_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| UpdateError::InvalidArchive(format!("tar: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::InvalidArchive(stderr.trim().to_owned()));
    }
    Ok(())
}

fn find_linux_source_dir(extract_dir: &Path) -> Result<PathBuf, UpdateError> {
    if extract_dir.join("vela").is_file() {
        return Ok(extract_dir.to_owned());
    }
    for entry in fs::read_dir(extract_dir)? {
        let path = entry?.path();
        if path.is_dir() && path.join("vela").is_file() {
            return Ok(path);
        }
    }
    Err(UpdateError::InvalidArchive(
        "the package doesn't contain the 'vela' binary".into(),
    ))
}

fn atomic_replace_binary(src_bin: &Path, dest_bin: &Path) -> Result<(), UpdateError> {
    let bin_dir = dest_bin
        .parent()
        .ok_or_else(|| UpdateError::InstallFailed("couldn't locate the binary's folder".into()))?;
    let tmp_bin = bin_dir.join(format!(".vela.update.{}", std::process::id()));
    let _ = fs::remove_file(&tmp_bin);
    fs::copy(src_bin, &tmp_bin).map_err(|err| {
        UpdateError::InstallFailed(format!(
            "no write permission for {}: {err}",
            bin_dir.display()
        ))
    })?;
    fs::set_permissions(&tmp_bin, fs::Permissions::from_mode(0o755))?;
    if let Err(err) = fs::rename(&tmp_bin, dest_bin) {
        let _ = fs::remove_file(&tmp_bin);
        return Err(UpdateError::InstallFailed(format!(
            "couldn't replace {}: {err}",
            dest_bin.display()
        )));
    }
    Ok(())
}

fn install_linux(
    extract_dir: &Path,
    binary_path: &Path,
    data_dir: Option<&Path>,
) -> Result<RelaunchTarget, UpdateError> {
    let src_dir = find_linux_source_dir(extract_dir)?;
    atomic_replace_binary(&src_dir.join("vela"), binary_path)?;

    // Refresh desktop entry & hicolor icons only if an installation already exists in XDG data dir.
    if let Some(data_dir) = data_dir {
        let desktop_dest = data_dir.join("applications/vela.desktop");
        if desktop_dest.is_file() {
            let _ = refresh_linux_desktop_assets(&src_dir, binary_path, data_dir);
        }
    }

    Ok(RelaunchTarget::Binary(binary_path.to_owned()))
}

fn refresh_linux_desktop_assets(
    src_dir: &Path,
    binary_path: &Path,
    data_dir: &Path,
) -> io::Result<()> {
    let src_icons = src_dir.join("icons/hicolor");
    let dest_icons = data_dir.join("icons/hicolor");
    if src_icons.is_dir() {
        let svg_src = src_icons.join("scalable/apps/vela.svg");
        if svg_src.is_file() {
            let svg_dir = dest_icons.join("scalable/apps");
            fs::create_dir_all(&svg_dir)?;
            let svg_dst = svg_dir.join("vela.svg");
            fs::copy(&svg_src, &svg_dst)?;
            fs::set_permissions(&svg_dst, fs::Permissions::from_mode(0o644))?;
        }
        for entry in fs::read_dir(&src_icons)? {
            let entry = entry?;
            let name = entry.file_name();
            let png_src = entry.path().join("apps/vela.png");
            if entry.path().is_dir() && png_src.is_file() {
                let png_dir = dest_icons.join(name).join("apps");
                fs::create_dir_all(&png_dir)?;
                let png_dst = png_dir.join("vela.png");
                fs::copy(&png_src, &png_dst)?;
                fs::set_permissions(&png_dst, fs::Permissions::from_mode(0o644))?;
            }
        }
        let _ = Command::new("gtk-update-icon-cache")
            .args(["-q", "-f", "-t"])
            .arg(&dest_icons)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let src_desktop = src_dir.join("vela.desktop");
    if src_desktop.is_file() {
        let apps_dir = data_dir.join("applications");
        fs::create_dir_all(&apps_dir)?;
        let raw = fs::read_to_string(&src_desktop)?;
        let exec = quote_desktop_exec(&binary_path.to_string_lossy());
        let mut updated = String::with_capacity(raw.len() + 64);
        for line in raw.lines() {
            if line.starts_with("Exec=") {
                updated.push_str("Exec=");
                updated.push_str(&exec);
            } else {
                updated.push_str(line);
            }
            updated.push('\n');
        }
        let tmp_desktop = apps_dir.join(format!(".vela.desktop.tmp.{}", std::process::id()));
        fs::write(&tmp_desktop, updated)?;
        fs::set_permissions(&tmp_desktop, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp_desktop, apps_dir.join("vela.desktop"))?;
        let _ = Command::new("update-desktop-database")
            .arg(&apps_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

fn quote_desktop_exec(path: &str) -> String {
    const SPECIAL: &str = "\"'\\`$<>~|&;*?#()";
    let needs_quotes = path
        .chars()
        .any(|c| c.is_whitespace() || SPECIAL.contains(c));
    if !needs_quotes {
        return path.to_owned();
    }
    let mut out = String::with_capacity(path.len() + 4);
    out.push('"');
    for ch in path.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

fn install_macos_bundle(
    extract_dir: &Path,
    dest_app: &Path,
) -> Result<RelaunchTarget, UpdateError> {
    let src_app = extract_dir.join("Vela.app");
    if !src_app.is_dir() || !src_app.join("Contents/MacOS/vela").is_file() {
        return Err(UpdateError::InvalidArchive(
            "the package doesn't contain a valid Vela.app".into(),
        ));
    }
    let apps_dir = dest_app
        .parent()
        .ok_or_else(|| UpdateError::InstallFailed("couldn't locate the Vela.app folder".into()))?;
    let pid = std::process::id();
    let staged_app = apps_dir.join(format!(".Vela.app.new.{pid}"));
    let backup_app = apps_dir.join(format!(".Vela.app.old.{pid}"));
    let _ = fs::remove_dir_all(&staged_app);
    let _ = fs::remove_dir_all(&backup_app);

    let copy_status = Command::new("ditto")
        .arg(&src_app)
        .arg(&staged_app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();

    match copy_status {
        Ok(out) if out.status.success() => {}
        _ => {
            let fallback = Command::new("cp")
                .arg("-R")
                .arg(&src_app)
                .arg(&staged_app)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|err| UpdateError::InstallFailed(err.to_string()))?;
            if !fallback.status.success() {
                let _ = fs::remove_dir_all(&staged_app);
                return Err(UpdateError::InstallFailed(format!(
                    "no write permission for {}",
                    apps_dir.display()
                )));
            }
        }
    }

    let had_existing = dest_app.exists();
    if had_existing && let Err(err) = fs::rename(dest_app, &backup_app) {
        let _ = fs::remove_dir_all(&staged_app);
        return Err(UpdateError::InstallFailed(format!(
            "couldn't move the current package: {err}"
        )));
    }

    if let Err(err) = fs::rename(&staged_app, dest_app) {
        if had_existing {
            let _ = fs::rename(&backup_app, dest_app);
        }
        let _ = fs::remove_dir_all(&staged_app);
        return Err(UpdateError::InstallFailed(format!(
            "couldn't activate the new package: {err}"
        )));
    }

    let _ = fs::remove_dir_all(&backup_app);
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(dest_app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("touch")
        .arg(dest_app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let lsregister = Path::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    );
    if lsregister.is_file() {
        let _ = Command::new(lsregister)
            .arg("-f")
            .arg(dest_app)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    Ok(RelaunchTarget::MacAppBundle(dest_app.to_owned()))
}

fn install_binary_only(
    extract_dir: &Path,
    binary_path: &Path,
) -> Result<RelaunchTarget, UpdateError> {
    let macos_bin = extract_dir.join("Vela.app/Contents/MacOS/vela");
    let src_bin = if macos_bin.is_file() {
        macos_bin
    } else {
        find_linux_source_dir(extract_dir)?.join("vela")
    };
    atomic_replace_binary(&src_bin, binary_path)?;
    Ok(RelaunchTarget::Binary(binary_path.to_owned()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_pubkey_matches_packaging_file() {
        let file = include_str!("../../../packaging/vela-release.pub").trim();
        assert_eq!(DEFAULT_PUBKEY, file);
    }

    #[test]
    fn release_body_parsing_is_best_effort() {
        for (response, notes) in [
            (r###"{"body": "## Changes"}"###, Some("## Changes")),
            (r#"{"body": ""}"#, None),
            (r#"{"body": null}"#, None),
            (r#"{"body": 42}"#, None),
            (r#"{"message": "rate limit"}"#, None),
            ("invalid json", None),
        ] {
            assert_eq!(parse_release_body(response.as_bytes()).as_deref(), notes);
        }
    }

    #[test]
    fn changelog_preview_cleans_markdown_and_bounds_the_output() {
        let long = "é".repeat(120);
        let markdown = format!("\
## What's Changed
<!--
hidden
-->
![logo](https://example.com/logo.png)
- **Improved** [transfer speed](https://example.com) by @user in https://github.com/owner/repo/pull/1
1. _Fixed_ `SSH` reconnection
~~~sh
echo hidden
~~~
- {long}
- Fourth entry");
        assert_eq!(
            changelog_preview(&markdown),
            [
                "Improved transfer speed".into(),
                "Fixed SSH reconnection".into(),
                format!("{}…", "é".repeat(84))
            ]
        );
        assert!(changelog_preview("## Changes\n**Full Changelog**: https://example.com\n## New Contributors\n- Someone").is_empty());
    }

    #[test]
    fn validates_asset_names_and_digests_and_urls() {
        assert!(validate_asset_name("vela-0.2.0-linux-x86_64.tar.gz").is_ok());
        assert!(validate_asset_name("Vela-0.2.0-macos-aarch64.tar.gz").is_ok());
        assert!(validate_asset_name("").is_err());
        assert!(validate_asset_name("../evil.tar.gz").is_err());
        assert!(validate_asset_name("dir/evil.tar.gz").is_err());
        assert!(validate_asset_name("vela 0.2.0.tar.gz").is_err());

        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256(&"a".repeat(63)).is_err());

        let valid_url = concat!(
            "https://github.com/Alejandro12120/Vela/releases/download/",
            "v0.2.0/vela-0.2.0-linux-x86_64.tar.gz"
        );
        assert!(validate_download_url(valid_url).is_ok());
        assert!(validate_download_url("https://evil.example.com/vela.tar.gz").is_err());
    }

    #[test]
    fn rejects_tar_path_traversal() {
        assert!(validate_tar_entries(["vela-0.2.0/vela", "vela-0.2.0/LICENSE"]).is_ok());
        assert!(validate_tar_entries(["/etc/passwd"]).is_err());
        assert!(validate_tar_entries(["vela/../escape"]).is_err());
    }

    #[test]
    fn detects_macos_app_bundle_and_linux_targets() {
        let mac = InstallTarget::from_exe_path(
            PathBuf::from("/Applications/Vela.app/Contents/MacOS/vela"),
            true,
        );
        assert_eq!(
            mac,
            InstallTarget::MacAppBundle {
                app_bundle: PathBuf::from("/Applications/Vela.app")
            }
        );

        let mac_bin = InstallTarget::from_exe_path(PathBuf::from("/tmp/vela"), true);
        assert_eq!(
            mac_bin,
            InstallTarget::BinaryOnly {
                binary_path: PathBuf::from("/tmp/vela")
            }
        );

        let linux = InstallTarget::from_exe_path(PathBuf::from("/home/u/.local/bin/vela"), false);
        assert!(matches!(
            linux,
            InstallTarget::Linux { binary_path, .. }
                if binary_path == Path::new("/home/u/.local/bin/vela")
        ));
    }

    fn sign_sshsig(key_path: &Path, target: &Path) {
        let file = File::open(target).unwrap();
        let out = Command::new("ssh-keygen")
            .args(["-Y", "sign", "-f"])
            .arg(key_path)
            .args(["-n", SIGNER_NAMESPACE])
            .stdin(Stdio::from(file))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ssh-keygen sign failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        fs::write(format!("{}.sshsig", target.display()), out.stdout).unwrap();
    }

    fn create_signed_fixture(
        version: &str,
        notes: Option<(&str, &str)>,
    ) -> (tempfile::TempDir, String, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dist = tmp.path().join("dist");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&dist).unwrap();
        fs::create_dir_all(&stage).unwrap();

        let key_path = tmp.path().join("test_ed25519");
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key_path)
            .status()
            .unwrap();
        assert!(status.success());
        let pub_raw = fs::read_to_string(tmp.path().join("test_ed25519.pub")).unwrap();
        let pubkey = pub_raw
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");

        let platform = current_platform().unwrap_or("linux-x86_64");
        let asset = format!("vela-{version}-{platform}.tar.gz");
        let archive_path = dist.join(&asset);

        if platform == "macos-aarch64" {
            let app = stage.join("Vela.app");
            fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            let bin = app.join("Contents/MacOS/vela");
            fs::write(&bin, format!("#!/bin/sh\necho vela-{version}\n")).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            let status = Command::new("tar")
                .arg("-C")
                .arg(&stage)
                .arg("-czf")
                .arg(&archive_path)
                .arg("Vela.app")
                .status()
                .unwrap();
            assert!(status.success());
        } else {
            let pkg_name = format!("vela-{version}-{platform}");
            let pkg = stage.join(&pkg_name);
            fs::create_dir_all(&pkg).unwrap();
            let bin = pkg.join("vela");
            fs::write(&bin, format!("#!/bin/sh\necho vela-{version}\n")).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            let status = Command::new("tar")
                .arg("-C")
                .arg(&stage)
                .arg("-czf")
                .arg(&archive_path)
                .arg(&pkg_name)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let archive_bytes = fs::read(&archive_path).unwrap();
        let digest = hex_lower(&Sha256::digest(&archive_bytes));
        let size = archive_bytes.len() as u64;
        sign_sshsig(&key_path, &archive_path);

        let notes_url = format!("https://github.com/Alejandro12120/Vela/releases/tag/v{version}");
        let asset_url =
            format!("https://github.com/Alejandro12120/Vela/releases/download/v{version}/{asset}");
        let mut manifest = serde_json::json!({
            "schema": 1,
            "version": version,
            "tag": format!("v{version}"),
            "published_at": "2026-11-25T00:00:00Z",
            "notes_url": notes_url,
            "platforms": {
                platform: {
                    "asset": asset,
                    "url": asset_url,
                    "sha256": digest,
                    "size": size,
                    "signature": "mock"
                }
            }
        });
        if let Some((field, notes)) = notes {
            manifest[field] = serde_json::Value::String(notes.to_owned());
        }
        let manifest_path = dist.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        sign_sshsig(&key_path, &manifest_path);

        (tmp, pubkey, dist, platform.to_owned())
    }

    #[test]
    fn check_and_install_signed_update_end_to_end() {
        let (tmp, pubkey, dist, platform) = create_signed_fixture("0.2.0", None);
        let config = Config {
            current_version: "0.1.0".into(),
            platform: Some(platform.clone()),
            base_url: Some(format!("file://{}", dist.display())),
            pubkey: pubkey.clone(),
            ..Config::default()
        };

        let outcome = check(&config).unwrap();
        let release = match outcome {
            CheckOutcome::UpdateAvailable(info) => info,
            CheckOutcome::UpToDate => panic!("expected update to be available"),
        };
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.changelog, None, "custom mirrors don't query GitHub");

        // Same version reports UpToDate.
        let same_config = Config {
            current_version: "0.2.0".into(),
            ..config.clone()
        };
        assert_eq!(check(&same_config).unwrap(), CheckOutcome::UpToDate);

        // Install into a mock binary target and verify monotonic progress + replacement.
        let dest_bin = tmp.path().join("installed/vela");
        fs::create_dir_all(dest_bin.parent().unwrap()).unwrap();
        fs::write(&dest_bin, "#!/bin/sh\necho vela-0.1.0\n").unwrap();
        fs::set_permissions(&dest_bin, fs::Permissions::from_mode(0o755)).unwrap();

        let target = InstallTarget::BinaryOnly {
            binary_path: dest_bin.clone(),
        };
        let mut fractions = Vec::new();
        let installed = download_and_install(&config, &release, &target, |p| {
            fractions.push(p.fraction);
        })
        .unwrap();
        assert_eq!(installed.relaunch, RelaunchTarget::Binary(dest_bin.clone()));
        assert!(!fractions.is_empty());
        assert!((fractions.last().copied().unwrap() - 1.0).abs() < f32::EPSILON);
        assert!(fractions.windows(2).all(|w| w[0] <= w[1]));

        let out = Command::new(&dest_bin).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "vela-0.2.0");
    }

    #[test]
    fn signed_manifest_can_embed_changelog_for_a_custom_mirror() {
        for field in ["notes", "changelog"] {
            let notes = "## Changes\n- Faster reconnects";
            let (_tmp, pubkey, dist, platform) =
                create_signed_fixture("0.2.0", Some((field, notes)));
            let outcome = check(&Config {
                current_version: "0.1.0".into(),
                platform: Some(platform),
                base_url: Some(format!("file://{}", dist.display())),
                include_prereleases: true,
                pubkey,
                ..Config::default()
            })
            .unwrap();
            let CheckOutcome::UpdateAvailable(release) = outcome else {
                panic!("expected an available update");
            };
            assert_eq!(release.changelog.as_deref(), Some(notes));
        }
    }

    #[test]
    fn stable_channel_can_return_from_a_prerelease() {
        let (_tmp, pubkey, dist, platform) = create_signed_fixture("0.2.0", None);
        let mut config = Config {
            platform: Some(platform),
            base_url: Some(format!("file://{}", dist.display())),
            pubkey,
            ..Config::default()
        };
        for (current, expected_downgrade) in [
            ("0.1.3", Some(false)),
            ("0.2.0", None),
            ("0.3.0", None),
            ("0.3.0-rc.1", Some(true)),
            ("0.2.0-rc.1", Some(false)),
        ] {
            config.current_version = current.into();
            match (check(&config).unwrap(), expected_downgrade) {
                (CheckOutcome::UpToDate, None) => {}
                (CheckOutcome::UpdateAvailable(release), Some(downgrade)) => {
                    assert_eq!(release.version, "0.2.0");
                    assert!(!release.is_prerelease);
                    assert_eq!(release.is_downgrade, downgrade);
                }
                (outcome, _) => panic!("{current}: unexpected result {outcome:?}"),
            }
        }
    }

    fn release_listing(tag: &str, base: &str, draft: bool, assets: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "draft": draft,
            "body": "Release notes from GitHub",
            "assets": assets.iter().map(|name| serde_json::json!({
                "name": name,
                "browser_download_url": format!("{base}/{name}"),
            })).collect::<Vec<_>>(),
        })
    }

    const MANIFEST_ASSETS: &[&str] = &["manifest.json", "manifest.json.sshsig"];

    fn write_release_list(dir: &Path, releases: &[serde_json::Value]) -> String {
        let path = dir.join("releases.json");
        fs::write(&path, serde_json::to_vec(releases).unwrap()).unwrap();
        format!("file://{}", path.display())
    }

    #[test]
    fn prerelease_channel_selects_the_highest_complete_published_version() {
        for (current, wanted, prerelease) in [
            ("0.1.3", "0.2.0-rc.1", true),
            ("0.2.0-rc.1", "0.2.0-rc.2", true),
            ("0.2.0-rc.2", "0.2.0", false),
            ("0.2.0-rc.2", "0.2.0-rc.10", true),
        ] {
            let (tmp, pubkey, dist, platform) = create_signed_fixture(wanted, None);
            let missing = "file:///no-such-vela-release";
            let api_url = write_release_list(
                tmp.path(),
                &[
                    release_listing("v0.1.3", missing, false, MANIFEST_ASSETS),
                    release_listing("v9.0.0", missing, true, MANIFEST_ASSETS),
                    release_listing("v8.0.0", missing, false, &["manifest.json"]),
                    release_listing("v7.0.0", missing, false, &["manifest.json.sshsig"]),
                    release_listing("v6.0.0-rc.01", missing, false, MANIFEST_ASSETS),
                    release_listing(
                        &format!("v{wanted}"),
                        &format!("file://{}", dist.display()),
                        false,
                        MANIFEST_ASSETS,
                    ),
                    release_listing(&format!("v{current}"), missing, false, MANIFEST_ASSETS),
                ],
            );
            let config = Config {
                current_version: current.into(),
                platform: Some(platform),
                base_url: Some(missing.into()),
                include_prereleases: true,
                releases_api_url: Some(api_url),
                pubkey,
            };
            let CheckOutcome::UpdateAvailable(release) = check(&config).unwrap() else {
                panic!("expected {wanted} to be available");
            };
            assert_eq!(release.version, wanted);
            assert_eq!(release.is_prerelease, prerelease);
            assert!(!release.is_downgrade);
            assert_eq!(
                release.changelog.as_deref(),
                Some("Release notes from GitHub")
            );
            assert_eq!(
                check(&Config {
                    current_version: wanted.into(),
                    ..config
                })
                .unwrap(),
                CheckOutcome::UpToDate,
            );
        }
    }

    #[test]
    fn prerelease_channel_rejects_disallowed_manifest_and_signature_urls() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..2 {
            let mut release = release_listing(
                "v0.2.0-rc.1",
                &format!("{}v0.2.0-rc.1", ALLOWED_URL_PREFIXES[0]),
                false,
                MANIFEST_ASSETS,
            );
            release["assets"][index]["browser_download_url"] =
                "https://example.invalid/file".into();
            let config = Config {
                current_version: "0.1.3".into(),
                include_prereleases: true,
                releases_api_url: Some(write_release_list(dir.path(), &[release])),
                ..Config::default()
            };
            assert!(matches!(
                check(&config),
                Err(UpdateError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn prerelease_channel_rejects_tampered_signatures_and_mismatched_tags() {
        let (tmp, pubkey, dist, platform) = create_signed_fixture("0.2.0-rc.1", None);
        let base = format!("file://{}", dist.display());
        let mut config = Config {
            current_version: "0.1.3".into(),
            platform: Some(platform),
            base_url: Some(base.clone()),
            include_prereleases: true,
            releases_api_url: Some(write_release_list(
                tmp.path(),
                &[release_listing(
                    "v0.2.0-rc.2",
                    &base,
                    false,
                    MANIFEST_ASSETS,
                )],
            )),
            pubkey,
        };
        assert!(matches!(
            check(&config),
            Err(UpdateError::InvalidManifest(_))
        ));
        config.releases_api_url = Some(write_release_list(
            tmp.path(),
            &[release_listing(
                "v0.2.0-rc.1",
                &base,
                false,
                MANIFEST_ASSETS,
            )],
        ));
        fs::write(dist.join("manifest.json.sshsig"), "invalid signature").unwrap();
        assert!(matches!(
            check(&config),
            Err(UpdateError::SignatureInvalid(_))
        ));
    }

    #[test]
    fn rejects_wrong_key_and_tampered_artifacts() {
        let (tmp, pubkey, dist, platform) = create_signed_fixture("0.2.0", None);

        // 1. Wrong key fails closed.
        let wrong_key = tmp.path().join("wrong_key");
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&wrong_key)
            .status()
            .unwrap();
        assert!(status.success());
        let wrong_pub = fs::read_to_string(tmp.path().join("wrong_key.pub"))
            .unwrap()
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        let bad_key_cfg = Config {
            current_version: "0.1.0".into(),
            platform: Some(platform.clone()),
            base_url: Some(format!("file://{}", dist.display())),
            pubkey: wrong_pub,
            ..Config::default()
        };
        assert!(matches!(
            check(&bad_key_cfg),
            Err(UpdateError::SignatureInvalid(_))
        ));

        // 2. Tampered archive fails closed without touching destination.
        let valid_cfg = Config {
            current_version: "0.1.0".into(),
            platform: Some(platform),
            base_url: Some(format!("file://{}", dist.display())),
            pubkey,
            ..Config::default()
        };
        let CheckOutcome::UpdateAvailable(release) = check(&valid_cfg).unwrap() else {
            panic!("expected update");
        };
        let mut archive = fs::read(dist.join(&release.asset)).unwrap();
        archive[0] ^= 0xff;
        fs::write(dist.join(&release.asset), archive).unwrap();

        let dest_bin = tmp.path().join("untouched/vela");
        fs::create_dir_all(dest_bin.parent().unwrap()).unwrap();
        fs::write(&dest_bin, "original").unwrap();

        let err = download_and_install(
            &valid_cfg,
            &release,
            &InstallTarget::BinaryOnly {
                binary_path: dest_bin.clone(),
            },
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::ChecksumMismatch));
        assert_eq!(fs::read_to_string(&dest_bin).unwrap(), "original");
    }

    #[test]
    fn installs_macos_app_bundle_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let extract = tmp.path().join("extract");
        let src_app = extract.join("Vela.app");
        fs::create_dir_all(src_app.join("Contents/MacOS")).unwrap();
        let src_bin = src_app.join("Contents/MacOS/vela");
        fs::write(&src_bin, "#!/bin/sh\necho new-bundle\n").unwrap();
        fs::set_permissions(&src_bin, fs::Permissions::from_mode(0o755)).unwrap();

        let apps_dir = tmp.path().join("Applications");
        let dest_app = apps_dir.join("Vela.app");
        fs::create_dir_all(dest_app.join("Contents/MacOS")).unwrap();
        fs::write(dest_app.join("Contents/MacOS/vela"), "old-bundle").unwrap();

        let relaunch = install_macos_bundle(&extract, &dest_app).unwrap();
        assert_eq!(relaunch, RelaunchTarget::MacAppBundle(dest_app.clone()));
        let installed = fs::read_to_string(dest_app.join("Contents/MacOS/vela")).unwrap();
        assert!(installed.contains("new-bundle"));
    }
}
