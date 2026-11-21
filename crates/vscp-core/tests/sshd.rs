use std::{
    fs::{self, File},
    future::Future,
    io,
    net::{TcpListener, TcpStream},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;
use tokio::time::timeout;
use vscp_core::{Auth, Client, ConnectionSpec, EntryKind, Error, Session, askpass};

const SHORT_WAIT: Duration = Duration::from_secs(8);
const CONNECT_WAIT: Duration = Duration::from_secs(40);
const PASSPHRASE: &str = "throwaway vSCP test passphrase";

struct Process(Child);

impl Process {
    fn spawn(command: &mut Command) -> Self {
        let child = command.stdin(Stdio::null()).spawn().unwrap();
        Self(child)
    }

    fn ready(&mut self, condition: impl FnMut() -> bool) {
        eventually(condition);
        assert_eq!(self.0.try_wait().unwrap(), None);
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn eventually(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + SHORT_WAIT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for test process"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn run(command: &mut Command) -> std::process::Output {
    // Files let a timed-out child be killed without pipe backpressure.
    use std::io::{Read, Seek};
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut process = Process::spawn(
        command
            .stdout(stdout.try_clone().unwrap())
            .stderr(stderr.try_clone().unwrap()),
    );
    let mut status = None;
    eventually(|| {
        status = process.0.try_wait().unwrap();
        status.is_some()
    });
    let mut output = std::process::Output {
        status: status.unwrap(),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.rewind().unwrap();
    stderr.rewind().unwrap();
    stdout.read_to_end(&mut output.stdout).unwrap();
    stderr.read_to_end(&mut output.stderr).unwrap();
    output
}

fn checked(command: &mut Command) -> Vec<u8> {
    let output = run(command);
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn generate(key: &Path, passphrase: &str) {
    checked(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", passphrase, "-f"])
            .arg(key),
    );
}

struct Fixture {
    server: Option<Process>,
    _agent: Process,
    temp: TempDir,
    port: u16,
    user: String,
}

impl Fixture {
    fn start() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("vscp-sshd-")
            .tempdir_in("/tmp")
            .unwrap();
        let base = temp.path();
        for (name, passphrase) in [("host", ""), ("a", ""), ("b", ""), ("c", PASSPHRASE)] {
            generate(&base.join(name), passphrase);
        }
        File::create(base.join("config")).unwrap();
        File::create(base.join("known_hosts")).unwrap();
        fs::copy(base.join("b.pub"), base.join("authorized")).unwrap();

        let socket = base.join("agent");
        let mut agent = Process::spawn(
            Command::new("ssh-agent")
                .args(["-D", "-a"])
                .arg(&socket)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        agent.ready(|| socket.exists());
        checked(
            Command::new("ssh-add")
                .env("SSH_AUTH_SOCK", &socket)
                .env_remove("SSH_AGENT_PID")
                .arg(base.join("a"))
                .arg(base.join("b")),
        );

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let user = String::from_utf8(checked(Command::new("id").arg("-un")))
            .unwrap()
            .trim()
            .to_owned();
        assert_ne!(user, "root", "run the fixture as a regular user");
        let mut fixture = Self {
            server: None,
            _agent: agent,
            temp,
            port,
            user,
        };
        fixture.start_server();
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    fn start_server(&mut self) {
        let cfg = self.path("sshd_config");
        let base = self.temp.path().display();
        fs::write(
            &cfg,
            format!(
                "ListenAddress 127.0.0.1\nPort {}\nHostKey {base}/host\nPidFile {base}/pid\n\
             AuthorizedKeysFile {base}/authorized\nUsePAM no\nStrictModes no\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nMaxAuthTries 1\n\
             Subsystem sftp internal-sftp\n",
                self.port,
            ),
        )
        .unwrap();
        let log = self.path("sshd.log");
        let mut server = Process::spawn(
            Command::new("/usr/sbin/sshd")
                .args(["-D", "-e", "-f"])
                .arg(cfg)
                .stdout(Stdio::null())
                .stderr(File::create(&log).unwrap()),
        );
        let address = ([127, 0, 0, 1], self.port).into();
        let deadline = Instant::now() + SHORT_WAIT;
        loop {
            assert!(
                Instant::now() < deadline && server.0.try_wait().unwrap().is_none(),
                "sshd did not start: {}",
                fs::read_to_string(&log).unwrap(),
            );
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.server = Some(server);
    }

    fn replace_host_key(&mut self) {
        drop(self.server.take());
        fs::remove_file(self.path("host")).unwrap();
        fs::remove_file(self.path("host.pub")).unwrap();
        generate(&self.path("host"), "");
        self.start_server();
    }

    fn spec(&self, identity: &str) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(
            "127.0.0.1".into(),
            self.port,
            self.user.clone(),
            Auth::Agent {
                identity_pub: self.path(identity),
                agent_socket: Some(self.path("agent")),
            },
        );
        spec.ssh_config = Some(self.path("config"));
        spec.known_hosts = Some(self.path("known_hosts"));
        spec
    }

    fn control_check(&self) -> std::process::Output {
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        run(Command::new("/usr/bin/ssh")
            .env_remove("SSH_AUTH_SOCK")
            .arg("-F")
            .arg(self.path("config"))
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                self.path("known_hosts").display()
            ))
            .arg("-o")
            .arg(format!("ControlPath=/tmp/vscp-{uid}/%C"))
            .args([
                "-O",
                "check",
                "-p",
                &self.port.to_string(),
                "--",
                &format!("{}@127.0.0.1", self.user),
            ]))
    }
}

fn client() -> Client {
    Client::new(env!("CARGO_BIN_EXE_vscp-askpass").into()).unwrap()
}

async fn connect(
    client: &Client,
    spec: ConnectionSpec,
    mut handle: impl FnMut(askpass::Request),
) -> Result<Session, Error> {
    timeout(CONNECT_WAIT, async {
        let connection = client.connect(spec);
        tokio::pin!(connection);
        loop {
            tokio::select! {
                result = &mut connection => break result,
                request = client.next_prompt() => handle(request.expect("askpass channel closed")),
            }
        }
    })
    .await
    .expect("connection timed out")
}

fn unexpected(request: askpass::Request) {
    panic!("unexpected askpass prompt: {:?}", request.kind);
}

async fn trust_first(client: &Client, spec: ConnectionSpec) -> Session {
    let mut prompts = 0;
    let session = connect(client, spec, |request| {
        let askpass::Kind::HostKeyConfirm { fingerprint } = &request.kind else {
            unexpected(request);
            return;
        };
        assert!(fingerprint.starts_with("SHA256:"));
        assert!(
            request
                .prompt
                .contains("Are you sure you want to continue connecting")
        );
        eprintln!(
            "Host-key prompt (hint {:?}): {}",
            request.hint, request.prompt
        );
        prompts += 1;
        request.respond(Some("yes".into()));
    })
    .await
    .unwrap();
    assert_eq!(prompts, 1);
    session
}

async fn disconnect(session: &Session) {
    timeout(SHORT_WAIT, session.disconnect())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn selects_one_agent_identity() {
    let fixture = Fixture::start();
    let client = client();
    let spec = fixture.spec("b.pub");
    let identities = timeout(SHORT_WAIT, client.effective_identities(spec.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identities, vec![fixture.path("b.pub")]);
    let session = trust_first(&client, spec).await;
    disconnect(&session).await;
    assert!(matches!(
        connect(&client, fixture.spec("a.pub"), unexpected).await,
        Err(Error::AuthFailed { .. })
    ));
}

#[tokio::test]
async fn accepts_new_host_and_remembers_it() {
    let fixture = Fixture::start();
    let client = client();
    let spec = fixture.spec("b.pub");
    disconnect(&trust_first(&client, spec.clone()).await).await;
    disconnect(&connect(&client, spec, unexpected).await.unwrap()).await;
}

#[tokio::test]
async fn declines_new_host() {
    let fixture = Fixture::start();
    let client = client();
    let mut prompted = false;
    let result = connect(&client, fixture.spec("b.pub"), |request| {
        assert!(matches!(request.kind, askpass::Kind::HostKeyConfirm { .. }));
        prompted = true;
        request.respond(Some("no".into()));
    })
    .await;
    assert!(prompted);
    assert!(
        matches!(result, Err(Error::HostKeyRejected { .. })),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn rejects_changed_host_without_asking() {
    let mut fixture = Fixture::start();
    let client = client();
    let spec = fixture.spec("b.pub");
    disconnect(&trust_first(&client, spec.clone()).await).await;
    fixture.replace_host_key();
    let result = connect(&client, spec, unexpected).await;
    assert!(
        matches!(result, Err(Error::HostKeyChanged { .. })),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn prompts_for_private_key_passphrase() {
    let fixture = Fixture::start();
    let client = client();
    disconnect(&trust_first(&client, fixture.spec("b.pub")).await).await;
    fs::copy(fixture.path("c.pub"), fixture.path("authorized")).unwrap();
    let mut spec = fixture.spec("c");
    spec.auth = Auth::Agent {
        identity_pub: fixture.path("c"),
        agent_socket: Some("none".into()),
    };
    for (reply, succeeds) in [(PASSPHRASE, true), ("wrong", false)] {
        let mut prompts = 0;
        let result = connect(&client, spec.clone(), |request| {
            assert_eq!(request.kind, askpass::Kind::Passphrase);
            eprintln!(
                "Passphrase prompt (hint {:?}): {}",
                request.hint, request.prompt
            );
            prompts += 1;
            request.respond(Some(reply.into()));
        })
        .await;
        assert!(prompts > 0);
        if succeeds {
            disconnect(&result.unwrap()).await;
        } else {
            assert!(
                matches!(result, Err(Error::AuthFailed { .. })),
                "{:?}",
                result.err()
            );
        }
    }
}

fn sha256(path: &Path) -> Vec<u8> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("shasum");
        command.args(["-a", "256"]);
        command
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = Command::new("sha256sum");
    checked(command.arg(path))
        .split(|byte| byte.is_ascii_whitespace())
        .next()
        .unwrap()
        .to_vec()
}

fn random_file(path: &Path) {
    let mut bytes = vec![0; 5 * 1024 * 1024];
    getrandom::fill(&mut bytes).unwrap();
    fs::write(path, bytes).unwrap();
}

async fn within<T>(future: impl Future<Output = Result<T, Error>>) -> T {
    timeout(SHORT_WAIT, future).await.unwrap().unwrap()
}

#[tokio::test]
async fn transfers_and_edits_files_in_place() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let local = fixture.path("local");
    let downloaded = fixture.path("downloaded");
    let folder = fixture.path("remote");
    let cancel = Arc::new(AtomicBool::new(false));
    random_file(&local);
    within(session.mkdir(folder.clone())).await;

    for name in ["round trip.bin", "Prueba vSCP #1 ñ.txt"] {
        let remote = folder.join(name);
        fs::write(&remote, b"old data").unwrap();
        fs::set_permissions(&remote, fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::metadata(&remote).unwrap();
        let progress = Arc::new(AtomicU64::new(0));
        let uploaded = progress.clone();
        within(session.upload(
            local.clone(),
            remote.clone(),
            move |count| {
                uploaded.store(count, Relaxed);
            },
            cancel.clone(),
        ))
        .await;
        assert_eq!(progress.load(Relaxed), 5 * 1024 * 1024);
        let after = fs::metadata(&remote).unwrap();
        assert_eq!(
            (before.ino(), before.mode(), before.uid()),
            (after.ino(), after.mode(), after.uid())
        );
        within(session.download(remote.clone(), downloaded.clone(), |_| {}, cancel.clone())).await;
        assert_eq!(sha256(&local), sha256(&downloaded));
        let entries = within(session.list_dir(folder.clone())).await;
        let entry = entries.iter().find(|entry| entry.name == name).unwrap();
        assert_eq!(
            (entry.kind, entry.size, entry.mode),
            (EntryKind::File, Some(after.len()), Some(0o640))
        );
        assert!(entry.mtime.is_some());

        let renamed = folder.join("renamed #ñ");
        within(session.rename(remote.clone(), renamed.clone())).await;
        assert!(!remote.exists());
        let link = folder.join("link");
        symlink(&renamed, &link).unwrap();
        let entry = within(session.stat(link.clone())).await;
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert_eq!(entry.symlink_target_kind, Some(EntryKind::File));
        within(session.remove_file(link)).await;
        within(session.remove_file(renamed)).await;
    }
    assert!(within(session.list_dir(folder)).await.is_empty());
    disconnect(&session).await;
    let log = fs::read_to_string(fixture.path("sshd.log")).unwrap();
    assert_eq!(log.matches("Accepted publickey").count(), 1);
}

#[tokio::test]
async fn lists_directory_and_broken_symlink_targets() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let folder = fixture.path("remote");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(folder.join("destino # ñ")).unwrap();
    symlink("destino # ñ", folder.join("enlace")).unwrap();
    symlink("no existe", folder.join("roto")).unwrap();

    let entries = within(session.list_dir(folder)).await;
    let link = entries.iter().find(|entry| entry.name == "enlace").unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(link.symlink_target_kind, Some(EntryKind::Directory));
    let broken = entries.iter().find(|entry| entry.name == "roto").unwrap();
    assert_eq!(broken.kind, EntryKind::Symlink);
    assert_eq!(broken.symlink_target_kind, None);
    disconnect(&session).await;
}

#[tokio::test]
async fn cancelled_download_removes_partial_and_keeps_session_usable() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let source = fixture.path("source");
    let partial = fixture.path("partial");
    random_file(&source);
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let download = session.download(
        source.clone(),
        partial.clone(),
        move |count| {
            if count >= 1024 * 1024 {
                signal.store(true, Relaxed);
            }
        },
        cancel,
    );
    assert!(matches!(
        timeout(SHORT_WAIT, download).await.unwrap(),
        Err(Error::Cancelled)
    ));
    assert_eq!(
        fs::metadata(partial).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        within(session.stat(source)).await.size,
        Some(5 * 1024 * 1024)
    );
    disconnect(&session).await;
}

fn ssh_children(fixture: &Fixture) -> Vec<u32> {
    let procs = checked(Command::new("ps").args(["-axo", "pid=,command="]));
    let config = format!(" -F {} ", fixture.path("config").display());
    String::from_utf8_lossy(&procs)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            (command.trim_start().starts_with("/usr/bin/ssh ") && command.contains(&config))
                .then(|| pid.parse().unwrap())
        })
        .collect()
}

#[tokio::test]
async fn disconnect_and_drop_reap_ssh_processes_and_unlink_socket() {
    use std::os::unix::fs::FileTypeExt;
    let fixture = Fixture::start();
    let client = client();

    for explicit in [true, false] {
        let session = if explicit {
            trust_first(&client, fixture.spec("b.pub")).await
        } else {
            connect(&client, fixture.spec("b.pub"), unexpected)
                .await
                .unwrap()
        };
        assert!(fixture.control_check().status.success());
        let children = ssh_children(&fixture);
        assert_eq!(children.len(), 2, "master and SFTP ssh processes");
        // Identify the master socket with a control check against each socket.
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let socket = fs::read_dir(format!("/tmp/vscp-{uid}"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_type().unwrap().is_socket() && entry.file_name().len() == 40)
            .find(|entry| {
                let result = run(Command::new("/usr/bin/ssh")
                    .arg("-F")
                    .arg(fixture.path("config"))
                    .arg("-S")
                    .arg(entry.path())
                    .args(["-O", "check", "--", "127.0.0.1"]));
                let stderr = String::from_utf8_lossy(&result.stderr);
                result.status.success()
                    && children
                        .iter()
                        .any(|pid| stderr.contains(&format!("(pid={pid})")))
            })
            .expect("master's control socket")
            .path();
        if explicit {
            disconnect(&session).await;
        }
        drop(session);
        eventually(|| !socket.exists() && ssh_children(&fixture).is_empty());
        assert!(!fixture.control_check().status.success());
    }
}

#[tokio::test]
#[ignore = "requires a password-enabled sshd in the Linux test container"]
async fn password_authentication() {
    let host = std::env::var("VSCP_TEST_PW_HOST").unwrap();
    let port = std::env::var("VSCP_TEST_PW_PORT").unwrap().parse().unwrap();
    let user = std::env::var("VSCP_TEST_PW_USER").unwrap();
    let password = std::env::var("VSCP_TEST_PW_PASSWORD").unwrap();
    let temp = tempfile::Builder::new()
        .prefix("vscp-pw-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut spec = ConnectionSpec::new(host, port, user, Auth::Password);
    for name in ["config", "known_hosts"] {
        File::create(temp.path().join(name)).unwrap();
    }
    spec.ssh_config = Some(temp.path().join("config"));
    spec.known_hosts = Some(temp.path().join("known_hosts"));
    let client = client();
    let mut passwords = 0;
    let session = connect(&client, spec, |request| {
        let reply = match request.kind {
            askpass::Kind::HostKeyConfirm { .. } => "yes",
            askpass::Kind::Password => {
                passwords += 1;
                &password
            }
            _ => {
                unexpected(request);
                return;
            }
        };
        request.respond(Some(reply.to_owned()));
    })
    .await
    .unwrap();
    assert_eq!(passwords, 1);
    disconnect(&session).await;
}

#[tokio::test]
async fn pending_host_prompt_pauses_readiness_deadline() {
    let fixture = Fixture::start();
    let client = client();
    let spec = fixture.spec("b.pub");
    let destination = format!("{}@{}:{}", spec.user, spec.host, spec.port);
    let budget = Duration::from_millis(700);
    let session = within(async {
        tokio::try_join!(client.connect_with_timeout(spec, budget), async {
            let request = client.next_prompt().await.expect("host key prompt");
            assert_eq!(request.destination, destination);
            assert!(matches!(request.kind, askpass::Kind::HostKeyConfirm { .. }));
            tokio::time::sleep(budget * 2).await;
            request.respond(Some("yes".into()));
            Ok(())
        })
    })
    .await
    .0;
    disconnect(&session).await;
}

async fn in_progress_download(
    fixture: &Fixture,
    session: &Session,
) -> (
    PathBuf,
    Arc<AtomicBool>,
    tokio::task::JoinHandle<Result<(), Error>>,
) {
    let source = fixture.path("large");
    File::create(&source)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let partial = fixture.path("partial");
    let cancel = Arc::new(AtomicBool::new(false));
    let (started, ready) = async_channel::bounded(1);
    let download = tokio::spawn({
        let session = session.clone();
        let partial = partial.clone();
        let cancel = cancel.clone();
        async move {
            session
                .download(
                    source,
                    partial,
                    move |_| {
                        let _ = started.try_send(());
                        // Keep the transfer in flight without relying on link throughput.
                        thread::sleep(Duration::from_millis(2));
                    },
                    cancel,
                )
                .await
        }
    });
    timeout(SHORT_WAIT, ready.recv()).await.unwrap().unwrap();
    (partial, cancel, download)
}

#[tokio::test]
async fn lists_directory_while_download_is_in_progress() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let (_, cancel, download) = in_progress_download(&fixture, &session).await;
    let entries = within(session.list_dir(fixture.temp.path().to_owned())).await;
    assert!(entries.iter().any(|entry| entry.name == "large"));
    assert!(!download.is_finished());
    cancel.store(true, Relaxed);
    assert!(matches!(download.await.unwrap(), Err(Error::Cancelled)));
    disconnect(&session).await;
}

#[tokio::test]
async fn disconnect_cancels_download_and_reaps_ssh_children() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let (partial, cancel, download) = in_progress_download(&fixture, &session).await;
    disconnect(&session).await;
    assert!(cancel.load(Relaxed));
    assert!(matches!(download.await.unwrap(), Err(Error::Cancelled)));
    assert!(!partial.exists());
    assert!(ssh_children(&fixture).is_empty());
}

#[tokio::test]
async fn client_shutdown_reaps_ssh_while_waiting_for_askpass() {
    let fixture = Fixture::start();
    let client = client();
    let spec = fixture.spec("b.pub");
    within(async {
        tokio::try_join!(
            async {
                assert!(matches!(client.connect(spec).await, Err(Error::Cancelled)));
                Ok(())
            },
            async {
                let request = client.next_prompt().await.expect("host key prompt");
                client.shutdown().await;
                assert!(!request.is_pending());
                assert!(ssh_children(&fixture).is_empty());
                Ok(())
            }
        )
    })
    .await;
}
