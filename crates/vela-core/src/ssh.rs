use std::{
    ffi::OsString,
    fs::{self, DirBuilder},
    io,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};

use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{Error, askpass};

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    Agent {
        identity_pub: PathBuf,
        agent_socket: Option<PathBuf>,
    },
    Password,
}

#[derive(Clone, Debug)]
pub struct ConnectionSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    pub display_name: Option<String>,
    #[doc(hidden)]
    pub ssh_config: Option<PathBuf>,
    #[doc(hidden)]
    pub known_hosts: Option<PathBuf>,
}

impl ConnectionSpec {
    pub fn new(host: String, port: u16, user: String, auth: Auth) -> Self {
        Self {
            host,
            port,
            user,
            auth,
            display_name: None,
            ssh_config: None,
            known_hosts: None,
        }
    }

    pub fn destination(&self) -> String {
        self.display_name
            .clone()
            .unwrap_or_else(|| format!("{}@{}:{}", self.user, self.host, self.port))
    }

    pub(crate) fn argv(&self, control_dir: &Path, mode: &[&str], remote: &[&str]) -> Vec<OsString> {
        let mut args: Vec<_> = mode.iter().map(OsString::from).collect();
        if let Some(path) = &self.ssh_config {
            args.extend(["-F".into(), path.into()]);
        }
        if let Some(path) = &self.known_hosts {
            args.extend(["-o".into(), option_path("UserKnownHostsFile", path)]);
        }
        match &self.auth {
            Auth::Agent {
                identity_pub,
                agent_socket,
            } => {
                args.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
                args.extend(["-o".into(), option_path("IdentityFile", identity_pub)]);
                if let Some(path) = agent_socket {
                    args.extend(["-o".into(), option_path("IdentityAgent", path)]);
                }
            }
            Auth::Password => args.extend(
                [
                    "-o",
                    "PreferredAuthentications=password,keyboard-interactive",
                    "-o",
                    "PubkeyAuthentication=no",
                ]
                .map(OsString::from),
            ),
        }
        args.extend([
            "-o".into(),
            option_path("ControlPath", &control_dir.join("%C")),
        ]);
        args.extend(
            [
                "-o",
                "StrictHostKeyChecking=ask",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-p",
            ]
            .map(OsString::from),
        );
        args.extend([
            self.port.to_string().into(),
            "--".into(),
            format!("{}@{}", self.user, self.host).into(),
        ]);
        args.extend(remote.iter().map(OsString::from));
        args
    }
}

impl ConnectionSpec {
    /// Arguments for an interactive `ssh` that rides on the session's master
    /// connection through the same `ControlPath` as the SFTP channel, so it
    /// needs no new authentication. `remote_command` runs in a terminal.
    pub fn terminal_command(&self, control_dir: &Path, remote_command: &str) -> Vec<OsString> {
        self.argv(
            control_dir,
            &["-o", "ControlMaster=no", "-o", "BatchMode=yes", "-t"],
            &[remote_command],
        )
    }
}

/// Wrap `value` in single quotes for a POSIX shell, writing each `'` as `'\''`.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Start a login shell in the directory shown by the remote pane.
pub fn terminal_start_command(directory: &str) -> String {
    format!(
        "export COLORTERM=truecolor; cd {} 2>/dev/null; exec \"$SHELL\" -l",
        shell_quote(directory),
    )
}

/// Remote command that runs `name` from `directory`, then leaves a login shell
/// open. A file without an execute bit goes through `sh`.
pub fn run_script_command(directory: &str, name: &str, executable: bool) -> String {
    let runner = if executable { "" } else { "sh " };
    format!(
        "cd {} && {runner}{}; exec \"$SHELL\" -l",
        shell_quote(directory),
        shell_quote(&format!("./{name}")),
    )
}

fn option_path(key: &str, path: &Path) -> OsString {
    let mut option = OsString::from(format!("{key}="));
    // ssh parses the value as an ssh_config field, even though argv is already split.
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes
        .iter()
        .any(|byte| matches!(byte, b' ' | b'\t' | b'"' | b'\\' | b'#' | b'\''))
    {
        use std::os::unix::ffi::OsStringExt;
        let mut quoted = vec![b'"'];
        for &byte in bytes {
            if matches!(byte, b'"' | b'\\') {
                quoted.push(b'\\');
            }
            quoted.push(byte);
        }
        quoted.push(b'"');
        option.push(OsString::from_vec(quoted));
    } else {
        option.push(path);
    }
    option
}

pub(crate) fn control_dir() -> io::Result<PathBuf> {
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let path = PathBuf::from(format!("/tmp/vela-{uid}"));
    match DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    validate_control_dir(&path, uid)?;
    Ok(path)
}

fn validate_control_dir(path: &Path, uid: u32) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o7777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be a directory owned by UID {uid} with mode 0700",
                path.display()
            ),
        ));
    }
    check_control_path(&path.canonicalize()?)
}

fn check_control_path(dir: &Path) -> io::Result<()> {
    // OpenSSH expands %C to a 40-character SHA-1 digest.
    if dir.join("0".repeat(40)).as_os_str().len() > 100 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH ControlPath exceeds 100 bytes",
        ));
    }
    Ok(())
}

/// Existing Bitwarden agent sockets for the desktop package on this platform.
pub fn bitwarden_agent_sockets() -> Vec<PathBuf> {
    // https://bitwarden.com/help/ssh-agent/
    let Some(home) = std::env::home_dir() else {
        return Vec::new();
    };
    #[cfg(target_os = "macos")]
    let paths = [
        "Library/Containers/com.bitwarden.desktop/Data/.bitwarden-ssh-agent.sock",
        ".bitwarden-ssh-agent.sock",
    ];
    #[cfg(target_os = "linux")]
    let paths = [
        ".bitwarden-ssh-agent.sock",
        "snap/bitwarden/current/.bitwarden-ssh-agent.sock",
        ".var/app/com.bitwarden.desktop/data/.bitwarden-ssh-agent.sock",
    ];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let paths: [&str; 0] = [];
    paths
        .into_iter()
        .map(|path| home.join(path))
        .filter(|path| fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()))
        .collect()
}

pub(crate) fn command(
    spec: &ConnectionSpec,
    askpass: &askpass::Server,
    mode: &[&str],
    remote: &[&str],
) -> Command {
    let mut cmd = Command::new("/usr/bin/ssh");
    cmd.args(spec.argv(askpass.directory(), mode, remote))
        .stdin(Stdio::null())
        .env("LC_ALL", "C")
        .env_remove("SSH_ASKPASS_PROMPT")
        .kill_on_drop(true);
    askpass.configure(&mut cmd);
    cmd
}

pub(crate) async fn identities(
    spec: &ConnectionSpec,
    askpass: &askpass::Server,
) -> Result<Vec<PathBuf>, Error> {
    let output = timeout(
        CONNECT_TIMEOUT,
        command(spec, askpass, &["-G"], &[]).output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "ssh -G timed out"))??;
    if !output.status.success() {
        return Err(Error::ssh(
            output.status,
            String::from_utf8_lossy(&output.stderr).into(),
        ));
    }
    use std::os::unix::ffi::OsStringExt;
    Ok(output
        .stdout
        .split(|&byte| byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"identityfile "))
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
        .collect())
}

pub(crate) struct Process {
    pub(crate) child: Child,
    stderr: JoinHandle<io::Result<Vec<u8>>>,
}

impl Process {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        // The SSH children are spawned from the `vela-network` thread, which
        // lives for the whole session. A dead thread must also kill its child.
        #[cfg(target_os = "linux")]
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.stderr(Stdio::piped()).spawn()?;
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await?;
            Ok(bytes)
        });
        Ok(Self { child, stderr })
    }

    pub(crate) async fn stderr(&mut self) -> String {
        match timeout(EXIT_TIMEOUT, &mut self.stderr).await {
            Ok(Ok(Ok(bytes))) => String::from_utf8_lossy(&bytes).into(),
            _ => String::new(),
        }
    }

    pub(crate) async fn stop(&mut self) -> io::Result<ExitStatus> {
        let _ = self.child.start_kill();
        self.child.wait().await
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stderr.abort();
    }
}

/// Reads the expanded ControlPath from `ssh -G` output. `-G` reports the
/// effective configuration as stable `key value` lines, unlike the localized
/// prose of a failed `-O check`, so it is used to find the `%C` socket.
fn control_path_from_g(output: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    output
        .split(|&byte| byte == b'\n')
        .find_map(|line| line.strip_prefix(b"controlpath "))
        .map(|path| PathBuf::from(OsString::from_vec(path.trim_ascii().to_vec())))
        .filter(|path| !path.as_os_str().is_empty())
}

fn master_pid(output: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(output).lines().find_map(|line| {
        line.trim()
            .strip_prefix("Master running (pid=")?
            .strip_suffix(')')?
            .parse()
            .ok()
    })
}

/// A master is an orphan unless its parent is a live process named `vela`;
/// in that case a second application instance owns the socket.
fn is_orphan(parent_comm: Option<&str>) -> bool {
    parent_comm.and_then(|comm| comm.rsplit('/').next()) != Some("vela")
}

/// Removes a socket left by a killed master. Returns the effective path even
/// when nothing needed removing, so the session can wait for orphan cleanup.
async fn clear_stale_socket(spec: &ConnectionSpec, askpass: &askpass::Server) -> Option<PathBuf> {
    let path = timeout(
        CONNECT_TIMEOUT,
        command(spec, askpass, &["-G"], &[]).output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .filter(|output| output.status.success())
    .and_then(|output| control_path_from_g(&output.stdout));
    let answers = timeout(
        EXIT_TIMEOUT,
        command(spec, askpass, &["-O", "check"], &[]).output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .is_some_and(|output| output.status.success());
    if !answers
        && let Some(path) = &path
        && path.exists()
    {
        let _ = fs::remove_file(path);
    }
    path
}

async fn parent_comm(pid: u32) -> Option<String> {
    let output = timeout(
        EXIT_TIMEOUT,
        tokio::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok)?;
    let ppid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if ppid.is_empty() || !ppid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let output = timeout(
        EXIT_TIMEOUT,
        tokio::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &ppid])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok)?;
    if !output.status.success() {
        return None;
    }
    let comm = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!comm.is_empty()).then_some(comm)
}

pub(crate) struct Master {
    pub(crate) spec: ConnectionSpec,
    pub(crate) askpass: askpass::Server,
    process: Process,
    control_path: Option<PathBuf>,
}

const MASTER_ARGS: [&str; 6] = [
    "-M",
    "-N",
    "-o",
    "ControlMaster=yes",
    "-o",
    "ControlPersist=no",
];

impl Master {
    pub(crate) async fn connect(
        spec: ConnectionSpec,
        askpass: askpass::Server,
        readiness_timeout: Duration,
        stopping: &async_channel::Receiver<()>,
        cancel: &async_channel::Receiver<()>,
    ) -> Result<Self, Error> {
        let control_path = tokio::select! {
            biased;
            _ = stopping.recv() => return Err(Error::Cancelled),
            _ = cancel.recv() => return Err(Error::Cancelled),
            path = clear_stale_socket(&spec, &askpass) => path,
        };
        if cancel.is_closed() {
            return Err(Error::Cancelled);
        }
        let process =
            Process::spawn(command(&spec, &askpass, &MASTER_ARGS, &[]).stdout(Stdio::null()))?;
        let mut master = Self {
            spec,
            askpass,
            process,
            control_path,
        };
        let timer = master.askpass.readiness_timer(readiness_timeout);
        let result = tokio::select! {
            biased;
            _ = stopping.recv() => Some(Err(Error::Cancelled)),
            _ = cancel.recv() => Some(Err(Error::Cancelled)),
            result = master.wait_until_ready() => Some(result),
            _ = timer => None,
        };
        if matches!(result, Some(Ok(()))) {
            return Ok(master);
        }
        let _ = master.disconnect().await;
        match result {
            Some(result) => result.map(|()| master),
            None => Err(Error::ConnectFailed {
                status: None,
                stderr: format!(
                    "Timed out waiting for SSH master\n{}",
                    master.process.stderr().await
                ),
            }),
        }
    }

    async fn wait_until_ready(&mut self) -> Result<(), Error> {
        // Check an existing socket before trusting it. An owned master must be the
        // process listening, even if another client connected to the same host.
        let mut retried = false;
        loop {
            if let Some(status) = self.process.child.try_wait()? {
                return Err(Error::ssh(status, self.process.stderr().await));
            }
            let result = self.control("check").output().await?;
            if result.status.success() {
                let expected = format!("Master running (pid={})", self.process.child.id().unwrap());
                if String::from_utf8_lossy(&result.stderr)
                    .lines()
                    .any(|line| line == expected)
                {
                    return Ok(());
                }
                let other_master = master_pid(&result.stderr);
                let orphan = !retried
                    && match other_master {
                        Some(pid) => is_orphan(parent_comm(pid).await.as_deref()),
                        None => false,
                    };
                if orphan {
                    // A previous instance was killed without unlinking its
                    // socket. Ask that master to exit, then reclaim the path.
                    let _ = timeout(EXIT_TIMEOUT, self.control("exit").output()).await;
                    if let Some(path) = &self.control_path {
                        let deadline = Instant::now() + EXIT_TIMEOUT;
                        while fs::metadata(path).is_ok() && Instant::now() < deadline {
                            sleep(POLL_INTERVAL).await;
                        }
                    }
                    let _ = self.process.stop().await;
                    self.process = Process::spawn(
                        command(&self.spec, &self.askpass, &MASTER_ARGS, &[]).stdout(Stdio::null()),
                    )?;
                    retried = true;
                    continue;
                }
                return Err(Error::ConnectFailed {
                    status: None,
                    stderr: "The SSH control socket belongs to another process".into(),
                });
            }
            sleep(POLL_INTERVAL).await;
        }
    }

    fn control(&self, operation: &str) -> Command {
        command(&self.spec, &self.askpass, &["-O", operation], &[])
    }

    pub(crate) async fn disconnect(&mut self) -> io::Result<()> {
        if let Some(pid) = self.process.child.id() {
            // A failed startup may have found another master's socket.
            if let Ok(Ok(output)) = timeout(EXIT_TIMEOUT, self.control("check").output()).await {
                let expected = format!("Master running (pid={pid})");
                if String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .any(|line| line == expected)
                {
                    let _ = timeout(EXIT_TIMEOUT, self.control("exit").output()).await;
                    // Give ssh time to unlink its socket before forcing termination.
                    if let Ok(result) = timeout(EXIT_TIMEOUT, self.process.child.wait()).await {
                        result?;
                        return Ok(());
                    }
                }
            }
            // Let OpenSSH unlink its socket even if the mux exit request failed.
            // SAFETY: the child PID remains ours until we reap it below.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if let Ok(result) = timeout(EXIT_TIMEOUT, self.process.child.wait()).await {
                result?;
                return Ok(());
            }
            self.process.stop().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn agent_argv() {
        let spec = ConnectionSpec {
            host: "test.example".into(),
            port: 2222,
            user: "tester".into(),
            auth: Auth::Agent {
                identity_pub: "/tmp/identity.pub".into(),
                agent_socket: Some("/tmp/agent.sock".into()),
            },
            display_name: None,
            ssh_config: Some("/tmp/config".into()),
            known_hosts: Some("/tmp/test hosts".into()),
        };
        assert_eq!(
            spec.argv(
                Path::new("/tmp/vela-1"),
                &[
                    "-M",
                    "-N",
                    "-o",
                    "ControlMaster=yes",
                    "-o",
                    "ControlPersist=no"
                ],
                &[]
            ),
            [
                "-M",
                "-N",
                "-o",
                "ControlMaster=yes",
                "-o",
                "ControlPersist=no",
                "-F",
                "/tmp/config",
                "-o",
                "UserKnownHostsFile=\"/tmp/test hosts\"",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityFile=/tmp/identity.pub",
                "-o",
                "IdentityAgent=/tmp/agent.sock",
                "-o",
                "ControlPath=/tmp/vela-1/%C",
                "-o",
                "StrictHostKeyChecking=ask",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-p",
                "2222",
                "--",
                "tester@test.example",
            ]
        );
    }

    #[test]
    fn terminal_argv_reuses_the_master_connection() {
        let spec = ConnectionSpec::new("host".into(), 2200, "user".into(), Auth::Password);
        assert_eq!(
            spec.terminal_command(Path::new("/tmp/vela-1"), "cd '/srv' && './a.sh'"),
            [
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes",
                "-t",
                "-o",
                "PreferredAuthentications=password,keyboard-interactive",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "ControlPath=/tmp/vela-1/%C",
                "-o",
                "StrictHostKeyChecking=ask",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-p",
                "2200",
                "--",
                "user@host",
                "cd '/srv' && './a.sh'",
            ]
        );
    }

    #[test]
    fn terminal_start_quotes_the_remote_directory() {
        assert_eq!(
            terminal_start_command("/srv/it's ñ"),
            "export COLORTERM=truecolor; cd '/srv/it'\\''s ñ' 2>/dev/null; exec \"$SHELL\" -l",
        );
    }

    #[test]
    fn quotes_names_with_spaces_quotes_and_accents() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's ñ"), "'it'\\''s ñ'");
        assert_eq!(
            run_script_command("/srv/mi carpeta", "it's ñ.sh", false),
            "cd '/srv/mi carpeta' && sh './it'\\''s ñ.sh'; exec \"$SHELL\" -l"
        );
        assert_eq!(
            run_script_command("/srv", "run", true),
            "cd '/srv' && './run'; exec \"$SHELL\" -l"
        );
    }

    #[test]
    fn password_argv() {
        let spec = ConnectionSpec::new("host".into(), 22, "user".into(), Auth::Password);
        assert_eq!(
            spec.argv(
                Path::new("/tmp/vela-1"),
                &["-o", "ControlMaster=no", "-s"],
                &["sftp"]
            ),
            [
                "-o",
                "ControlMaster=no",
                "-s",
                "-o",
                "PreferredAuthentications=password,keyboard-interactive",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "ControlPath=/tmp/vela-1/%C",
                "-o",
                "StrictHostKeyChecking=ask",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-p",
                "22",
                "--",
                "user@host",
                "sftp",
            ]
        );
    }

    #[test]
    fn control_dir_validation() {
        let temp = tempfile::Builder::new()
            .prefix("vela-unit-")
            .tempdir_in("/tmp")
            .unwrap();
        let directory = temp.path().join("directory");
        DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let uid = fs::metadata(&directory).unwrap().uid();
        assert!(validate_control_dir(&directory, uid).is_ok());
        assert!(validate_control_dir(&directory, uid + 1).is_err());

        let alias = temp.path().join("symlink");
        symlink(&directory, &alias).unwrap();
        assert!(validate_control_dir(&alias, uid).is_err());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_control_dir(&directory, uid).is_err());
    }

    #[test]
    fn control_path_is_read_from_ssh_g_output() {
        let output =
            b"host test\nuser tester\ncontrolpath /tmp/vela-1000/0123456789abcdef\nport 22\n";
        assert_eq!(
            control_path_from_g(output),
            Some(PathBuf::from("/tmp/vela-1000/0123456789abcdef"))
        );
        assert_eq!(control_path_from_g(b"host test\n"), None);
        assert_eq!(control_path_from_g(b"controlpath \n"), None);
    }

    #[test]
    fn master_pid_is_parsed_from_check_output() {
        assert_eq!(master_pid(b"Master running (pid=1234)\n"), Some(1234));
        assert_eq!(
            master_pid(b"Control socket connect(/tmp/x): No such file\n"),
            None
        );
        assert_eq!(master_pid(b""), None);
    }

    #[test]
    fn orphan_decision_requires_a_live_vela_parent() {
        assert!(is_orphan(None));
        assert!(!is_orphan(Some("vela")));
        assert!(!is_orphan(Some(
            "/Applications/vela.app/Contents/MacOS/vela"
        )));
        assert!(is_orphan(Some("zsh")));
        assert!(is_orphan(Some("vela ")));
    }

    #[test]
    fn control_path_maximum_counts_bytes() {
        assert!(check_control_path(Path::new(&format!("/{}", "ñ".repeat(29)))).is_ok());
        assert!(check_control_path(Path::new(&format!("/{}a", "ñ".repeat(29)))).is_err());
    }
}
