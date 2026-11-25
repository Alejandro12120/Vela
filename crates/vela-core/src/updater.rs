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
const MAX_SIG_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("Plataforma no soportada para actualizaciones automáticas.")]
    UnsupportedPlatform,
    #[error("No se pudo comprobar actualizaciones: {0}")]
    DownloadFailed(String),
    #[error("La firma Ed25519 de {0} no es válida.")]
    SignatureInvalid(String),
    #[error("El manifiesto de actualización no es válido: {0}")]
    InvalidManifest(String),
    #[error("No hay paquete disponible para esta plataforma ({0}).")]
    PlatformMissing(String),
    #[error("Tamaño del archivo inesperado (esperado {expected}, obtenido {actual}).")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("La suma SHA-256 del paquete descargado no coincide.")]
    ChecksumMismatch,
    #[error("El paquete descargado no es válido: {0}")]
    InvalidArchive(String),
    #[error("No se pudo instalar la actualización: {0}")]
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
    /// OpenSSH Ed25519 public key (`ssh-ed25519 AAAA...`).
    pub pubkey: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            current_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: current_platform().map(str::to_owned),
            base_url: None,
            pubkey: DEFAULT_PUBKEY.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    pub asset: String,
    pub url: String,
    pub sig_url: String,
    pub sha256: String,
    pub size: u64,
    pub notes_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckOutcome {
    UpToDate,
    UpdateAvailable(ReleaseInfo),
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
pub fn spawn_install(
    config: Config,
    release: ReleaseInfo,
    target: InstallTarget,
) -> InstallHandle {
    let (tx, rx) = async_channel::unbounded();
    thread::spawn(move || {
        let result = download_and_install(&config, &release, &target, |progress| {
            let _ = tx.send_blocking(InstallEvent::Progress(progress));
        });
        let _ = tx.send_blocking(InstallEvent::Finished(result));
    });
    InstallHandle { events: rx }
}

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    version: String,
    tag: String,
    #[serde(default)]
    notes_url: Option<String>,
    platforms: HashMap<String, PlatformEntry>,
}

#[derive(Deserialize)]
struct PlatformEntry {
    asset: String,
    url: String,
    sha256: String,
    size: u64,
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

/// Checks whether a newer signed release is available for the current platform.
pub fn check(config: &Config) -> Result<CheckOutcome, UpdateError> {
    let platform = config
        .platform
        .as_deref()
        .ok_or(UpdateError::UnsupportedPlatform)?;
    let current = semver::Version::parse(&config.current_version)
        .map_err(|err| UpdateError::InvalidManifest(format!("versión actual: {err}")))?;

    let custom_base = config.base_url.as_deref().map(|u| u.trim_end_matches('/'));
    let base = custom_base.unwrap_or(DEFAULT_BASE_URL);
    let strict_https = custom_base.is_none();

    let tmp = private_tempdir("vela-update-check-")?;
    let allowed_signers = write_allowed_signers(tmp.path(), &config.pubkey)?;

    let manifest_path = tmp.path().join("manifest.json");
    let manifest_sig = tmp.path().join("manifest.json.sshsig");

    download_small_file(
        &format!("{base}/manifest.json"),
        &manifest_path,
        MAX_MANIFEST_BYTES,
        strict_https,
    )?;
    download_small_file(
        &format!("{base}/manifest.json.sshsig"),
        &manifest_sig,
        MAX_SIG_BYTES,
        strict_https,
    )?;

    verify_sshsig(&manifest_path, &manifest_sig, &allowed_signers, "manifest.json")?;

    let raw = fs::read_to_string(&manifest_path)?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .map_err(|err| UpdateError::InvalidManifest(err.to_string()))?;

    if manifest.schema != 1 {
        return Err(UpdateError::InvalidManifest(format!(
            "esquema no soportado ({})",
            manifest.schema
        )));
    }

    let remote = semver::Version::parse(&manifest.version)
        .map_err(|err| UpdateError::InvalidManifest(format!("versión remota: {err}")))?;

    if remote <= current {
        return Ok(CheckOutcome::UpToDate);
    }

    let entry = manifest
        .platforms
        .get(platform)
        .ok_or_else(|| UpdateError::PlatformMissing(platform.to_owned()))?;

    validate_asset_name(&entry.asset)?;
    validate_sha256(&entry.sha256)?;

    let (url, sig_url) = if let Some(base) = custom_base {
        (
            format!("{base}/{}", entry.asset),
            format!("{base}/{}.sshsig", entry.asset),
        )
    } else {
        validate_download_url(&entry.url)?;
        (entry.url.clone(), format!("{}.sshsig", entry.url))
    };

    Ok(CheckOutcome::UpdateAvailable(ReleaseInfo {
        version: manifest.version,
        tag: manifest.tag,
        asset: entry.asset.clone(),
        url,
        sig_url,
        sha256: entry.sha256.clone(),
        size: entry.size,
        notes_url: manifest.notes_url,
    }))
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
    verify_sshsig(&archive_path, &archive_sig, &allowed_signers, &release.asset)?;

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

fn curl_command(url: &str, strict_https: bool, max_time: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
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
        cmd.args(["--proto", "=https", "--tlsv1.2"]);
    }
    cmd.arg("--").arg(url);
    cmd
}

fn download_small_file(
    url: &str,
    dest: &Path,
    max_bytes: u64,
    strict_https: bool,
) -> Result<(), UpdateError> {
    let mut child = curl_command(url, strict_https, "60")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| UpdateError::DownloadFailed(format!("curl: {err}")))?;

    let stderr_handle = spawn_stderr_reader(child.stderr.take());
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| UpdateError::DownloadFailed("sin salida de curl".into()))?;

    let mut file = File::create(dest)?;
    let mut total: u64 = 0;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let read = stdout.read(&mut buf)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max_bytes {
            let _ = child.kill();
            let _ = child.wait();
            return Err(UpdateError::DownloadFailed(
                "la respuesta superó el tamaño máximo permitido".into(),
            ));
        }
        file.write_all(&buf[..read])?;
    }
    file.flush()?;

    let status = child.wait()?;
    let stderr = join_stderr_reader(stderr_handle);
    if !status.success() {
        let detail = stderr.trim();
        return Err(UpdateError::DownloadFailed(if detail.is_empty() {
            format!("curl terminó con código {status}")
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
    let mut child = curl_command(url, strict_https, "600")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| UpdateError::DownloadFailed(format!("curl: {err}")))?;

    let stderr_handle = spawn_stderr_reader(child.stderr.take());
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| UpdateError::DownloadFailed("sin salida de curl".into()))?;

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
                "el archivo superó el límite de 512 MiB".into(),
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
            format!("curl terminó con código {status}")
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
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(UpdateError::InvalidManifest(format!(
            "nombre de archivo no válido: '{name}'"
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
            "SHA-256 no válido: '{digest}'"
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
            "URL de descarga no permitida: '{url}'"
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
                "ruta peligrosa en el archivo: '{entry}'"
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
        "no se encontró el binario 'vela' en el paquete".into(),
    ))
}

fn atomic_replace_binary(src_bin: &Path, dest_bin: &Path) -> Result<(), UpdateError> {
    let bin_dir = dest_bin.parent().ok_or_else(|| {
        UpdateError::InstallFailed("no se pudo determinar la carpeta del binario".into())
    })?;
    let tmp_bin = bin_dir.join(format!(".vela.update.{}", std::process::id()));
    let _ = fs::remove_file(&tmp_bin);
    fs::copy(src_bin, &tmp_bin).map_err(|err| {
        UpdateError::InstallFailed(format!(
            "sin permisos de escritura en {}: {err}",
            bin_dir.display()
        ))
    })?;
    fs::set_permissions(&tmp_bin, fs::Permissions::from_mode(0o755))?;
    if let Err(err) = fs::rename(&tmp_bin, dest_bin) {
        let _ = fs::remove_file(&tmp_bin);
        return Err(UpdateError::InstallFailed(format!(
            "no se pudo reemplazar {}: {err}",
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
            "el paquete no contiene un Vela.app válido".into(),
        ));
    }
    let apps_dir = dest_app.parent().ok_or_else(|| {
        UpdateError::InstallFailed("no se pudo determinar la carpeta de Vela.app".into())
    })?;
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
                    "sin permisos de escritura en {}",
                    apps_dir.display()
                )));
            }
        }
    }

    let had_existing = dest_app.exists();
    if had_existing && let Err(err) = fs::rename(dest_app, &backup_app) {
        let _ = fs::remove_dir_all(&staged_app);
        return Err(UpdateError::InstallFailed(format!(
            "no se pudo mover el paquete actual: {err}"
        )));
    }

    if let Err(err) = fs::rename(&staged_app, dest_app) {
        if had_existing {
            let _ = fs::rename(&backup_app, dest_app);
        }
        let _ = fs::remove_dir_all(&staged_app);
        return Err(UpdateError::InstallFailed(format!(
            "no se pudo activar el nuevo paquete: {err}"
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

    fn create_signed_fixture(version: &str) -> (tempfile::TempDir, String, PathBuf, String) {
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
        let manifest = serde_json::json!({
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
        let manifest_path = dist.join("manifest.json");
        fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        sign_sshsig(&key_path, &manifest_path);

        (tmp, pubkey, dist, platform.to_owned())
    }

    #[test]
    fn check_and_install_signed_update_end_to_end() {
        let (tmp, pubkey, dist, platform) = create_signed_fixture("0.2.0");
        let config = Config {
            current_version: "0.1.0".into(),
            platform: Some(platform.clone()),
            base_url: Some(format!("file://{}", dist.display())),
            pubkey: pubkey.clone(),
        };

        let outcome = check(&config).unwrap();
        let release = match outcome {
            CheckOutcome::UpdateAvailable(info) => info,
            CheckOutcome::UpToDate => panic!("expected update to be available"),
        };
        assert_eq!(release.version, "0.2.0");

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
    fn rejects_wrong_key_and_tampered_artifacts() {
        let (tmp, pubkey, dist, platform) = create_signed_fixture("0.2.0");

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
