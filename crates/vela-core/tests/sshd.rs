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
use vela_core::{Auth, Client, ConnectCancel, ConnectionSpec, EntryKind, Error, Session, askpass};

const SHORT_WAIT: Duration = Duration::from_secs(8);
const CONNECT_WAIT: Duration = Duration::from_secs(40);
const PASSPHRASE: &str = "throwaway Vela test passphrase";

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
            .prefix("vela-sshd-")
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
            .arg(format!("ControlPath=/tmp/vela-{uid}/%C"))
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
    Client::new(env!("CARGO_BIN_EXE_vela-askpass").into()).unwrap()
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

fn fingerprints(output: Vec<u8>) -> Vec<String> {
    // Both `ssh-add -l` and `ssh-keygen -l` print `bits SHA256:… comment (type)`.
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn lists_agent_keys_and_connects_with_the_stored_public_key() {
    let fixture = Fixture::start();
    let keys = timeout(
        SHORT_WAIT,
        vela_core::agent::list_identities(&fixture.path("agent")),
    )
    .await
    .unwrap()
    .unwrap();
    let expected = fingerprints(checked(
        Command::new("ssh-add")
            .env("SSH_AUTH_SOCK", fixture.path("agent"))
            .env_remove("SSH_AGENT_PID")
            .args(["-l", "-E", "sha256"]),
    ));
    assert_eq!(expected.len(), 2);
    assert_eq!(
        keys.iter().map(|key| key.fingerprint()).collect::<Vec<_>>(),
        expected
    );
    assert!(keys.iter().all(|key| key.key_type == "ssh-ed25519"));

    // Store the key the server accepts and compare it with ssh-keygen.
    let wanted = fingerprints(checked(
        Command::new("ssh-keygen")
            .args(["-l", "-E", "sha256", "-f"])
            .arg(fixture.path("b.pub")),
    ));
    let key = keys
        .iter()
        .find(|key| key.fingerprint() == wanted[0])
        .unwrap();
    let config = tempfile::tempdir().unwrap();
    let stored = vela_core::agent::store_public_key(config.path(), key).unwrap();
    assert_eq!(
        fingerprints(checked(
            Command::new("ssh-keygen")
                .args(["-l", "-E", "sha256", "-f"])
                .arg(&stored)
        )),
        wanted
    );
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&stored), 0o600);
    assert_eq!(mode(&config.path().join("keys")), 0o700);
    assert_eq!(
        vela_core::agent::read_public_key(&stored)
            .unwrap()
            .fingerprint(),
        wanted[0]
    );

    let client = client();
    let spec = fixture.spec(stored.to_str().unwrap());
    let identities = timeout(SHORT_WAIT, client.effective_identities(spec.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identities, vec![stored.clone()]);
    let session = trust_first(&client, spec).await;
    disconnect(&session).await;
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

    for name in ["round trip.bin", "Prueba Vela #1 ñ.txt"] {
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

fn ssh_using(marker: &str) -> Vec<u32> {
    let procs = checked(Command::new("ps").args(["-axo", "pid=,command="]));
    String::from_utf8_lossy(&procs)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            (command.trim_start().starts_with("/usr/bin/ssh ") && command.contains(marker))
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
        let socket = fs::read_dir(format!("/tmp/vela-{uid}"))
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
    let host = std::env::var("VELA_TEST_PW_HOST").unwrap();
    let port = std::env::var("VELA_TEST_PW_PORT").unwrap().parse().unwrap();
    let user = std::env::var("VELA_TEST_PW_USER").unwrap();
    let password = std::env::var("VELA_TEST_PW_PASSWORD").unwrap();
    let temp = tempfile::Builder::new()
        .prefix("vela-pw-")
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

#[tokio::test]
async fn cancel_connect_reaps_ssh_before_readiness_timeout() {
    let temp = tempfile::Builder::new()
        .prefix("vela-cancel-")
        .tempdir_in("/tmp")
        .unwrap();
    let config = temp.path().join("config");
    let known_hosts = temp.path().join("known_hosts");
    File::create(&config).unwrap();
    File::create(&known_hosts).unwrap();

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let held = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut stream = None;
        while stream.is_none() && Instant::now() < deadline {
            match listener.accept() {
                Ok((accepted, _)) => stream = Some(accepted),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
        drop(stream);
    });

    let mut spec = ConnectionSpec::new("127.0.0.1".into(), port, "test".into(), Auth::Password);
    spec.ssh_config = Some(config);
    spec.known_hosts = Some(known_hosts.clone());
    let marker = format!("UserKnownHostsFile={}", known_hosts.display());

    let client = client();
    let cancel = ConnectCancel::new();
    let connection =
        client.connect_with_timeout_and_cancel(spec, Duration::from_secs(30), cancel.clone());
    tokio::pin!(connection);

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut early = None;
    while ssh_using(&marker).is_empty() && Instant::now() < deadline {
        tokio::select! {
            result = &mut connection => {
                early = Some(result.map(|_| "session"));
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    assert!(early.is_none(), "connect finished early: {early:?}");
    assert!(
        !ssh_using(&marker).is_empty(),
        "ssh never started for the stalled listener; marker {marker:?}; processes {}",
        String::from_utf8_lossy(&checked(Command::new("ps").args(["-axo", "pid=,command="])))
    );

    cancel.cancel();
    let started = Instant::now();
    let result = timeout(Duration::from_secs(3), &mut connection)
        .await
        .expect("cancelled connect did not finish within 3 s");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        matches!(result, Err(Error::Cancelled)),
        "expected Cancelled, got another result"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    while !ssh_using(&marker).is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        ssh_using(&marker).is_empty(),
        "ssh survived cancellation: {:?}",
        ssh_using(&marker)
    );

    let _ = release_tx.send(());
    held.join().unwrap();
    client.shutdown().await;
}

use vela_core::transfer::{
    Answer, Choice, Direction, Event, ItemResult, Job, Spec, Status, Summary,
};

async fn enqueue(
    session: &Session,
    direction: Direction,
    sources: Vec<PathBuf>,
    destination: PathBuf,
) -> Job {
    within(session.enqueue_transfer(Spec {
        direction,
        sources,
        destination,
    }))
    .await
}

async fn event(job: &Job) -> Event {
    timeout(CONNECT_WAIT, job.events.recv())
        .await
        .expect("job stalled")
        .expect("job closed")
}

async fn collect(job: &Job, answers: &[Answer]) -> (Summary, Vec<ItemResult>) {
    let mut answers = answers.iter();
    let mut items = Vec::new();
    loop {
        match event(job).await {
            Event::Conflict(request) => {
                request.answer(*answers.next().expect("unexpected conflict"))
            }
            Event::Item { result, .. } => items.push(result),
            Event::Finished(summary) => {
                assert!(answers.next().is_none(), "expected another conflict");
                return (summary, items);
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn finder_staging_preserves_nested_files_mtimes_and_exact_names() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let remote = fixture.path("carpeta # ñ");
    let nested = remote.join("más # dentro");
    let top_file = fixture.path("suelto # ñ.txt");
    let staging = fixture.path("staging");
    let mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_123);
    fs::create_dir_all(&nested).unwrap();
    let nested_file = nested.join("archivo # ñ.txt");
    for path in [&top_file, &nested_file] {
        fs::write(path, path.file_name().unwrap().as_encoded_bytes()).unwrap();
    }
    for path in [&top_file, &nested_file, &nested, &remote] {
        File::open(path).unwrap().set_modified(mtime).unwrap();
    }
    let roots = vec![top_file.clone(), remote.clone(), nested_file.clone()];
    let job = within(session.enqueue_staging(roots.clone(), staging.clone(), 1_000)).await;
    let (summary, _) = collect(&job, &[]).await;
    assert_eq!(summary.status, Status::Completed);
    assert_eq!(summary.copied, 2);

    let staged_root = staging.join(remote.file_name().unwrap());
    let staged_nested = staged_root.join(nested.file_name().unwrap());
    for (original, staged) in [
        (&top_file, staging.join(top_file.file_name().unwrap())),
        (
            &nested_file,
            staged_nested.join(nested_file.file_name().unwrap()),
        ),
    ] {
        assert_eq!(sha256(&staged), sha256(original));
        assert_eq!(fs::metadata(staged).unwrap().modified().unwrap(), mtime);
    }
    for (directory, children) in [(&staging, 2), (&staged_root, 1), (&staged_nested, 1)] {
        assert_eq!(fs::read_dir(directory).unwrap().count(), children);
    }
    for directory in [&staged_root, &staged_nested] {
        let meta = fs::metadata(directory).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700);
        assert_eq!(meta.modified().unwrap(), mtime);
    }

    let refused = fixture.path("refused");
    let job = within(session.enqueue_staging(roots, refused.clone(), 1)).await;
    let mut size = None;
    loop {
        match event(&job).await {
            Event::SizeLimit(bytes) => size = Some(bytes),
            Event::Finished(summary) => {
                assert_eq!(summary.status, Status::Error);
                assert_eq!(size, Some(summary.totals.bytes));
                break;
            }
            _ => {}
        }
    }
    assert!(!refused.exists());
    disconnect(&session).await;
}

#[tokio::test]
async fn recursive_queue_round_trip_preserves_content_modes_and_mtimes() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let source = fixture.path("origen");
    let remote = fixture.path("remoto");
    let download = fixture.path("descarga");
    fs::create_dir_all(source.join("anidada/vacía")).unwrap();

    const FILES: [&str; 3] = ["anidada/espacio # ñ", "0 bytes", "mañána.txt"];
    let mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_123);
    for (index, name) in FILES.iter().enumerate() {
        let path = source.join(name);
        fs::write(&path, vec![index as u8; if index == 1 { 0 } else { 73 }]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640 + index as u32)).unwrap();
        File::open(&path).unwrap().set_modified(mtime).unwrap();
    }
    symlink(FILES[0], source.join("archivo enlace")).unwrap();
    symlink("anidada", source.join("carpeta enlace")).unwrap();
    symlink("no existe", source.join("roto")).unwrap();

    let job = enqueue(
        &session,
        Direction::Upload,
        vec![source.clone()],
        remote.clone(),
    )
    .await;
    let (summary, results) = collect(&job, &[]).await;
    assert_eq!(summary.status, Status::Completed, "{results:?}");
    assert_eq!(summary.totals.files, 4);
    assert_eq!(summary.totals.bytes, 219);
    assert_eq!(summary.copied, 4);
    assert_eq!(summary.skipped, 2);
    assert!(results.contains(&ItemResult::Skipped("Omitido: enlace a carpeta".into())));
    assert!(results.contains(&ItemResult::Skipped("Omitido: enlace roto".into())));
    let remote = remote.join("origen");
    symlink(FILES[0], remote.join("enlace remoto")).unwrap();
    symlink("anidada", remote.join("carpeta enlace")).unwrap();
    symlink("no existe", remote.join("roto")).unwrap();

    let job = enqueue(
        &session,
        Direction::Download,
        vec![remote.clone()],
        download.clone(),
    )
    .await;
    let (summary, results) = collect(&job, &[]).await;
    assert_eq!(summary.status, Status::Completed, "{results:?}");
    assert_eq!((summary.copied, summary.skipped), (5, 2));
    let download = download.join("origen");
    for path in [&remote, &download] {
        assert!(path.join("anidada/vacía").is_dir());
        for (index, name) in FILES.iter().enumerate() {
            let file = path.join(name);
            assert_eq!(sha256(&file), sha256(&source.join(name)));
            let meta = fs::metadata(&file).unwrap();
            assert_eq!(meta.modified().unwrap(), mtime);
            assert_eq!(meta.mode() & 0o777, 0o640 + index as u32);
        }
        let copy = path.join("archivo enlace");
        assert!(fs::symlink_metadata(&copy).unwrap().is_file());
        assert_eq!(sha256(&copy), sha256(&source.join(FILES[0])));
        assert_eq!(fs::metadata(copy).unwrap().modified().unwrap(), mtime);
    }
    assert!(!download.join("carpeta enlace").exists());
    assert!(!download.join("roto").is_symlink());
    assert!(
        fs::symlink_metadata(download.join("enlace remoto"))
            .unwrap()
            .is_file()
    );
    assert_eq!(
        sha256(&download.join("enlace remoto")),
        sha256(&source.join(FILES[0]))
    );
    disconnect(&session).await;
}

#[tokio::test]
async fn queued_conflicts_overwrite_skip_rename_apply_to_all_and_cancel() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let source = fixture.path("local");
    let remote = fixture.path("remote");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&remote).unwrap();
    let sources: Vec<_> = ["a.txt", "b.txt"]
        .into_iter()
        .map(|name| {
            fs::write(source.join(name), b"new").unwrap();
            fs::write(remote.join(name), b"old contents").unwrap();
            source.join(name)
        })
        .collect();
    fs::write(remote.join("a (1).txt"), b"occupied").unwrap();
    let a = remote.join("a.txt");
    fs::set_permissions(&a, fs::Permissions::from_mode(0o704)).unwrap();
    let before = fs::metadata(&a).unwrap();

    for (choice, status, copied) in [
        (Choice::Skip, Status::Completed, 0),
        (Choice::Rename, Status::Completed, 2),
        (Choice::Cancel, Status::Cancelled, 0),
        (Choice::Overwrite, Status::Completed, 2),
    ] {
        let job = enqueue(&session, Direction::Upload, sources.clone(), remote.clone()).await;
        let (summary, items) = collect(
            &job,
            &[Answer {
                choice,
                apply_to_all: true,
            }],
        )
        .await;
        assert_eq!(
            (summary.status, summary.copied),
            (status, copied),
            "{items:?}"
        );
        let expected: &[u8] = if choice == Choice::Overwrite {
            b"new"
        } else {
            b"old contents"
        };
        assert_eq!(fs::read(&a).unwrap(), expected);
    }
    assert_eq!(fs::read(remote.join("a (2).txt")).unwrap(), b"new");
    assert_eq!(fs::read(remote.join("b (1).txt")).unwrap(), b"new");
    let after = fs::metadata(&a).unwrap();
    assert_eq!(
        (before.ino(), before.mode(), before.uid()),
        (after.ino(), after.mode(), after.uid())
    );

    fs::write(&sources[0], b"old local file").unwrap();
    fs::set_permissions(&sources[0], fs::Permissions::from_mode(0o604)).unwrap();
    let job = enqueue(&session, Direction::Download, vec![a], source).await;
    loop {
        if let Event::Conflict(request) = event(&job).await {
            assert_eq!(
                (request.source.size, request.existing.size),
                (Some(3), Some(14))
            );
            assert!(request.source.mtime.is_some() && request.existing.mtime.is_some());
            request.answer(Answer {
                choice: Choice::Overwrite,
                apply_to_all: false,
            });
            break;
        }
    }
    let (summary, results) = collect(&job, &[]).await;
    assert_eq!(summary.status, Status::Completed, "{results:?}");
    assert_eq!(fs::read(&sources[0]).unwrap(), b"new");
    assert_eq!(fs::metadata(&sources[0]).unwrap().mode() & 0o777, 0o604);
    disconnect(&session).await;
}

#[tokio::test]
async fn queued_cancellation_cleans_parts_and_keeps_completed_files() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let source = fixture.path("source");
    let dest = fixture.path("dest");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&dest).unwrap();
    let small = source.join("a-small");
    let large = source.join("b-large");
    fs::write(&small, b"complete").unwrap();
    File::create(&large)
        .unwrap()
        .set_len(100 * 1024 * 1024)
        .unwrap();

    for (direction, overwrite) in [
        (Direction::Upload, false),
        (Direction::Download, false),
        (Direction::Upload, true),
        (Direction::Download, true),
    ] {
        if overwrite {
            fs::write(dest.join("b-large"), b"original").unwrap();
        }
        let job = enqueue(
            &session,
            direction,
            vec![small.clone(), large.clone()],
            dest.clone(),
        )
        .await;
        loop {
            match event(&job).await {
                Event::Conflict(request) => request.answer(Answer {
                    choice: Choice::Overwrite,
                    apply_to_all: false,
                }),
                Event::Progress { bytes } if bytes > 8 => {
                    assert!(bytes < 100 * 1024 * 1024, "finished before cancellation");
                    job.cancel.cancel();
                    break;
                }
                Event::Finished(summary) => panic!("job ended early: {summary:?}"),
                _ => {}
            }
        }
        let (summary, results) = collect(&job, &[]).await;
        assert_eq!(
            (summary.status, summary.copied),
            (Status::Cancelled, 1),
            "{results:?}"
        );
        if overwrite {
            let overwritten = dest.join("b-large");
            if direction == Direction::Upload {
                assert!(
                    results
                        .iter()
                        .any(|result| matches!(result, ItemResult::Incomplete(_))),
                    "{results:?}"
                );
                assert!(overwritten.exists());
            } else {
                assert_eq!(fs::read(&overwritten).unwrap(), b"original");
            }
            fs::remove_file(overwritten).unwrap();
        }
        assert!(!dest.join(".b-large.vela-part").exists());
        assert!(!dest.join("b-large.vela-part").exists());
        assert!(!dest.join("b-large").exists());
        assert_eq!(fs::read(dest.join("a-small")).unwrap(), b"complete");
        fs::remove_file(dest.join("a-small")).unwrap();
        assert!(within(session.list_dir(dest.clone())).await.is_empty());
    }
    disconnect(&session).await;
}

#[tokio::test]
async fn queue_continues_after_a_source_cannot_be_read() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let blocked = fixture.path("a-blocked");
    let readable = fixture.path("b-readable");
    let folder = fixture.path("target");
    fs::write(&blocked, b"blocked").unwrap();
    fs::write(&readable, b"readable").unwrap();
    fs::create_dir(&folder).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let job = enqueue(
        &session,
        Direction::Upload,
        vec![blocked.clone(), readable],
        folder.clone(),
    )
    .await;
    let (summary, results) = collect(&job, &[]).await;
    fs::set_permissions(blocked, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        (summary.status, summary.copied, summary.errors),
        (Status::CompletedWithErrors, 1, 1),
        "{results:?}"
    );
    assert!(!folder.join("a-blocked").exists());
    assert_eq!(fs::read(folder.join("b-readable")).unwrap(), b"readable");
    disconnect(&session).await;
}

#[tokio::test]
async fn remote_file_operations_refuse_rename_collision_and_retain_link_target() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let outside = fixture.path("outside");
    let folder = fixture.path("folder");
    let nested = folder.join("nested");
    let renamed = folder.join("renamed");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"safe").unwrap();
    within(session.mkdir(folder.clone())).await;
    within(session.mkdir(nested.clone())).await;
    fs::write(nested.join("file"), b"tree").unwrap();
    symlink(&outside, nested.join("link")).unwrap();
    within(session.rename(nested, renamed.clone())).await;
    let collision = folder.join("collision");
    within(session.mkdir(collision.clone())).await;
    let error = session
        .rename(renamed.clone(), collision)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Io(error) if error.kind() == io::ErrorKind::AlreadyExists));
    assert!(renamed.join("file").exists());
    let sibling = fixture.path("sibling");
    within(session.mkdir(sibling.clone())).await;
    let moved = sibling.join("renamed");
    within(session.rename(renamed.clone(), moved.clone())).await;
    assert!(!renamed.exists());
    assert_eq!(fs::read(moved.join("file")).unwrap(), b"tree");
    assert_eq!(fs::read_link(moved.join("link")).unwrap(), outside);
    symlink(&outside, folder.join("standalone")).unwrap();
    let trailing = PathBuf::from(format!("{}/", folder.join("standalone").display()));
    within(session.remove_tree(trailing)).await;
    within(session.remove_tree(folder.clone())).await;
    within(session.remove_tree(sibling)).await;
    assert!(!folder.exists());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"safe");
    disconnect(&session).await;
}

#[tokio::test]
async fn transfer_queue_is_fifo_while_the_session_remains_responsive() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let first = fixture.path("first");
    let second = fixture.path("second");
    let dest = fixture.path("dest");
    fs::create_dir(&dest).unwrap();
    File::create(&first)
        .unwrap()
        .set_len(100 * 1024 * 1024)
        .unwrap();
    fs::write(&second, b"second").unwrap();
    fs::write(dest.join("first"), b"conflict").unwrap();
    let one = enqueue(&session, Direction::Upload, vec![first], dest.clone()).await;
    let two = enqueue(&session, Direction::Upload, vec![second], dest.clone()).await;
    assert!(one.id < two.id);
    assert!(matches!(event(&one).await, Event::Status(Status::Queued)));
    assert!(matches!(
        event(&one).await,
        Event::Status(Status::Preparing)
    ));
    assert!(matches!(event(&one).await, Event::Planned(_)));
    assert!(matches!(event(&one).await, Event::Status(Status::Running)));
    assert!(matches!(event(&one).await, Event::File { .. }));
    let Event::Conflict(conflict) = event(&one).await else {
        panic!("expected conflict");
    };
    assert!(matches!(event(&two).await, Event::Status(Status::Queued)));
    assert!(two.events.try_recv().is_err());
    assert_eq!(within(session.list_dir(dest.clone())).await.len(), 1);
    conflict.answer(Answer {
        choice: Choice::Overwrite,
        apply_to_all: false,
    });
    assert!(matches!(event(&one).await, Event::Progress { .. }));
    assert_eq!(within(session.list_dir(dest.clone())).await.len(), 1);
    assert!(two.events.try_recv().is_err());
    assert_eq!(collect(&one, &[]).await.0.status, Status::Completed);
    assert!(matches!(
        event(&two).await,
        Event::Status(Status::Preparing)
    ));
    assert_eq!(collect(&two, &[]).await.0.status, Status::Completed);
    assert_eq!(fs::read(dest.join("second")).unwrap(), b"second");
    disconnect(&session).await;
}

#[tokio::test]
#[ignore = "loopback benchmark with 100 MiB of traffic in each direction"]
async fn benchmark_transfer_and_directory_listing() {
    let fixture = Fixture::start();
    let client = client();
    let session = trust_first(&client, fixture.spec("b.pub")).await;
    let source = fixture.path("100MiB");
    let remote = fixture.path("remote");
    let local = fixture.path("local");
    let listing = fixture.path("listing");
    for path in [&remote, &local, &listing] {
        fs::create_dir(path).unwrap();
    }
    let mut block = vec![0; 1024 * 1024];
    getrandom::fill(&mut block).unwrap();
    let mut file = File::create(&source).unwrap();
    for _ in 0..100 {
        std::io::Write::write_all(&mut file, &block).unwrap();
    }
    drop((file, block));
    for (direction, source, dest) in [
        (Direction::Upload, source.clone(), remote.clone()),
        (Direction::Download, remote.join("100MiB"), local.clone()),
    ] {
        let start = Instant::now();
        let job = enqueue(&session, direction, vec![source], dest).await;
        let (summary, results) = collect(&job, &[]).await;
        assert_eq!(summary.status, Status::Completed, "{results:?}");
        eprintln!(
            "{direction:?}: {:.2} MB/s",
            summary.totals.bytes as f64 / start.elapsed().as_secs_f64() / 1e6
        );
    }
    assert_eq!(sha256(&source), sha256(&local.join("100MiB")));
    for index in 0..10_000 {
        File::create(listing.join(index.to_string())).unwrap();
    }
    for _ in 0..3 {
        let start = Instant::now();
        assert_eq!(
            within(session.list_dir(listing.clone())).await.len(),
            10_000
        );
        eprintln!(
            "10,000 entries without symlinks: {:.2} ms",
            start.elapsed().as_secs_f64() * 1000.
        );
    }
    disconnect(&session).await;
}

#[tokio::test]
async fn remote_edit_watches_saves_and_preserves_the_remote_inode() {
    use vela_core::edit::{DirectoryWatch, Index, OpenRemote, Schedule, State, Target, Upload};

    let fixture = Fixture::start();
    let client = client();
    let spec = fixture.spec("b.pub");
    let session = trust_first(&client, spec.clone()).await;
    let remote = fixture.path("archivo # ñ.txt");
    fs::write(&remote, b"original\n").unwrap();
    fs::set_permissions(&remote, fs::Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(&remote).unwrap();
    let target = Target::new(
        &vela_core::config::Server {
            id: "server".into(),
            name: "Fixture".into(),
            host: spec.host.clone(),
            port: fixture.port,
            user: fixture.user.clone(),
            auth: spec.auth.clone(),
            initial_remote_path: None,
            pinned: false,
        },
        remote.clone(),
    );
    let config = fixture.path("edit-config");
    let cache = fixture.path("edit-cache");
    let mut index = Index::load(config.clone(), cache.clone()).unwrap();
    let dir = index.directory(&target).unwrap();
    let inspected = within(OpenRemote::inspect(&session, &target)).await;
    let mut edit = within(inspected.open(&session, target.clone(), dir.path())).await;
    index.put(edit.clone()).unwrap();
    let _ = dir.keep();
    assert_eq!(fs::read(&edit.local_path).unwrap(), b"original\n");
    assert_eq!(
        fs::metadata(&edit.local_path).unwrap().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(edit.local_path.parent().unwrap())
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
    let (mut watcher, events) = DirectoryWatch::new().unwrap();
    watcher.add(&edit.local_path).unwrap();

    // Same scheduler as the desktop path. Wait only until the watched file
    // has been quiet for one debounce interval.
    macro_rules! watched_save {
        () => {{
            timeout(SHORT_WAIT, async {
                let mut schedule = Schedule::default();
                loop {
                    match schedule.delay(Instant::now()) {
                        Some(delay) => {
                            if let Ok(event) = timeout(delay, events.recv()).await {
                                if DirectoryWatch::affects(
                                    &event.unwrap().unwrap(),
                                    &edit.local_path,
                                ) {
                                    schedule.changed(Instant::now());
                                }
                                continue;
                            }
                            assert!(schedule.begin(Instant::now(), false));
                            if let Some(snapshot) = edit.snapshot().unwrap() {
                                break snapshot;
                            }
                            schedule.finish();
                        }
                        None => {
                            if DirectoryWatch::affects(
                                &events.recv().await.unwrap().unwrap(),
                                &edit.local_path,
                            ) {
                                schedule.changed(Instant::now());
                            }
                        }
                    }
                }
            })
            .await
            .unwrap()
        }};
    }
    macro_rules! upload_clean {
        ($index:expr, $connection:expr, $snapshot:expr) => {{
            let Upload::Synced(baseline) =
                within(edit.upload($connection, $snapshot, edit.baseline.remote)).await
            else {
                panic!("unexpected conflict");
            };
            assert_eq!(baseline.sha256.as_bytes(), sha256(&remote));
            assert_eq!(sha256(&remote), sha256(&edit.local_path));
            edit.baseline = baseline;
            edit.state = State::Synced;
            $index.put(edit.clone()).unwrap();
        }};
    }

    fs::write(&edit.local_path, b"in place\n").unwrap();
    upload_clean!(index, &session, watched_save!());
    assert_eq!(fs::metadata(&remote).unwrap().ino(), before.ino());
    assert_eq!(fs::metadata(&remote).unwrap().mode(), before.mode());
    assert_eq!(fs::metadata(&remote).unwrap().uid(), before.uid());

    let replace = edit.local_path.with_file_name("editor-temporary");
    fs::write(&replace, b"atomic rename\n").unwrap();
    fs::rename(&replace, &edit.local_path).unwrap();
    upload_clean!(index, &session, watched_save!());

    let local = edit.local_path.clone();
    let (snapshot, ()) = tokio::join!(async { watched_save!() }, async {
        fs::write(&local, b"quick first\n").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        fs::write(&local, b"quick final\n").unwrap();
    });
    upload_clean!(index, &session, snapshot);
    assert_eq!(fs::read(&remote).unwrap(), b"quick final\n");
    for bytes in [b"slow first\n".as_slice(), b"slow final\n".as_slice()] {
        tokio::time::sleep(Duration::from_secs(1)).await;
        fs::write(&edit.local_path, bytes).unwrap();
        upload_clean!(index, &session, watched_save!());
        assert_eq!(fs::read(&remote).unwrap(), bytes);
    }

    fs::write(&remote, b"someone else's remote version\n").unwrap();
    fs::write(&edit.local_path, b"my local version\n").unwrap();
    let Upload::Conflict(Some(remote_stamp)) =
        within(edit.upload(&session, watched_save!(), edit.baseline.remote)).await
    else {
        panic!("remote edit went undetected");
    };
    edit.state = State::Conflict;
    index.put(edit.clone()).unwrap();
    assert_eq!(
        fs::read(&remote).unwrap(),
        b"someone else's remote version\n"
    );
    let copy = within(edit.save_remote_copy(&session)).await;
    assert_eq!(copy.file_name().unwrap(), "archivo # ñ (remoto).txt");
    assert_eq!(fs::read(&copy).unwrap(), fs::read(&remote).unwrap());
    assert_eq!(fs::read(&edit.local_path).unwrap(), b"my local version\n");
    assert_eq!(index.get(&target).unwrap().state, State::Conflict);
    let Upload::Synced(baseline) =
        within(edit.upload(&session, edit.snapshot().unwrap().unwrap(), remote_stamp)).await
    else {
        panic!("overwrite of the confirmed remote version failed");
    };
    assert_eq!(fs::read(&remote).unwrap(), b"my local version\n");
    edit.baseline = baseline;

    disconnect(&session).await;
    fs::write(&edit.local_path, b"saved offline\n").unwrap();
    let snapshot = watched_save!();
    let result = timeout(
        SHORT_WAIT,
        edit.upload(&session, snapshot, edit.baseline.remote),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    edit.state = State::Pending;
    index.put(edit.clone()).unwrap();
    drop(index);
    drop(watcher);

    let mut index = Index::load(config, cache).unwrap();
    assert_eq!(index.edits(), [edit.clone()]);
    assert_eq!(fs::read(&edit.local_path).unwrap(), b"saved offline\n");
    assert_eq!(fs::read(&remote).unwrap(), b"my local version\n");
    let session = connect(&client, spec, unexpected).await.unwrap();
    upload_clean!(index, &session, edit.snapshot().unwrap().unwrap());
    assert_eq!(fs::read(&remote).unwrap(), b"saved offline\n");
    assert_eq!(index.get(&target).unwrap().state, State::Synced);
    disconnect(&session).await;
}
