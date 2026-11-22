use std::{
    ffi::OsString,
    fs::{self, DirBuilder},
    io,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
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

    pub(crate) fn destination(&self) -> String {
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
    let path = PathBuf::from(format!("/tmp/vscp-{uid}"));
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

pub(crate) struct Master {
    pub(crate) spec: ConnectionSpec,
    pub(crate) askpass: askpass::Server,
    process: Process,
}

impl Master {
    pub(crate) async fn connect(
        spec: ConnectionSpec,
        askpass: askpass::Server,
        readiness_timeout: Duration,
        stopping: &async_channel::Receiver<()>,
    ) -> Result<Self, Error> {
        let process = Process::spawn(
            command(
                &spec,
                &askpass,
                &[
                    "-M",
                    "-N",
                    "-o",
                    "ControlMaster=yes",
                    "-o",
                    "ControlPersist=no",
                ],
                &[],
            )
            .stdout(Stdio::null()),
        )?;
        let mut master = Self {
            spec,
            askpass,
            process,
        };
        let timer = master.askpass.readiness_timer(readiness_timeout);
        let result = tokio::select! {
            biased;
            _ = stopping.recv() => Some(Err(Error::Cancelled)),
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
                Path::new("/tmp/vscp-1"),
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
                "ControlPath=/tmp/vscp-1/%C",
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
            spec.terminal_command(Path::new("/tmp/vscp-1"), "cd '/srv' && './a.sh'"),
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
                "ControlPath=/tmp/vscp-1/%C",
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
                Path::new("/tmp/vscp-1"),
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
                "ControlPath=/tmp/vscp-1/%C",
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
            .prefix("vscp-unit-")
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
    fn control_path_maximum_counts_bytes() {
        assert!(check_control_path(Path::new(&format!("/{}", "ñ".repeat(29)))).is_ok());
        assert!(check_control_path(Path::new(&format!("/{}a", "ñ".repeat(29)))).is_err());
    }
}
