//! Fixture OpenSSH compartido entre las pruebas de herramientas y de stdio.

use std::{
    fs::{self, File},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;
use tokio::time::timeout;
use vela_core::{Auth, Client, ConnectionSpec, Session};

const CONNECT_WAIT: Duration = Duration::from_secs(40);
pub const SHORT_WAIT: Duration = Duration::from_secs(8);

struct Process(Child);

impl Process {
    fn spawn(command: &mut Command) -> Self {
        Self(command.stdin(Stdio::null()).spawn().unwrap())
    }

    fn ready(&mut self, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + SHORT_WAIT;
        while !condition() {
            assert!(Instant::now() < deadline, "test process never became ready");
            std::thread::sleep(Duration::from_millis(20));
        }
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

fn checked(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn generate(key: &Path) {
    checked(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(key),
    );
}

pub struct Fixture {
    server: Option<Process>,
    _agent: Process,
    temp: TempDir,
    pub port: u16,
    pub user: String,
}

impl Fixture {
    pub fn start() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("vela-mcp-sshd-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        let base = temp.path();
        generate(&base.join("host"));
        generate(&base.join("client"));
        File::create(base.join("config")).unwrap();
        fs::copy(base.join("client.pub"), base.join("authorized")).unwrap();

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
                .arg(base.join("client")),
        );

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let host_pub = fs::read_to_string(base.join("host.pub")).unwrap();
        fs::write(
            base.join("known_hosts"),
            format!("[127.0.0.1]:{port} {}\n", host_pub.trim()),
        )
        .unwrap();

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

    pub fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    pub fn root(&self) -> PathBuf {
        self.temp.path().to_path_buf()
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
                .stderr(File::create(log).unwrap()),
        );
        let address = ([127, 0, 0, 1], self.port).into();
        let deadline = Instant::now() + SHORT_WAIT;
        loop {
            assert!(
                Instant::now() < deadline && server.0.try_wait().unwrap().is_none(),
                "sshd did not start"
            );
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.server = Some(server);
    }

    pub fn spec(&self) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(
            "127.0.0.1".into(),
            self.port,
            self.user.clone(),
            Auth::Agent {
                identity_pub: self.path("client.pub"),
                agent_socket: Some(self.path("agent")),
            },
        );
        spec.ssh_config = Some(self.path("config"));
        spec.known_hosts = Some(self.path("known_hosts"));
        spec
    }
}

pub async fn connect(fixture: &Fixture) -> Session {
    // `/bin/true` nunca se invoca: la clave del host ya está en known_hosts.
    let client = Client::new(PathBuf::from("/bin/true")).unwrap();
    let connection = client.connect(fixture.spec());
    tokio::pin!(connection);
    timeout(CONNECT_WAIT, async {
        tokio::select! {
            result = &mut connection => result,
            request = client.next_prompt() => {
                panic!("unexpected askpass prompt: {:?}", request.expect("prompt closed").kind)
            }
        }
    })
    .await
    .expect("connection timed out")
    .unwrap()
}
