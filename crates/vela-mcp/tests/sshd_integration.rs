//! Integración real contra un `sshd` de loopback: las herramientas MCP se
//! ejecutan sobre una sesión SFTP/SSH auténtica, sin simulaciones.

use std::{
    collections::HashSet,
    fs::{self, File},
    future::Future,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::time::timeout;
use vela_core::{Auth, Client, ConnectionSpec, Session};
use vela_mcp::{
    ApprovalMode, ApprovalPreview, BackupEntry, Executor, ExecutorError, FsListInput, FsReadInput,
    McpBackend, McpConfig, PolicyWarning, RollbackStore, ServerAvailability, ServerIdentity,
    ServerMcpPolicy, TextEdit, ToolOutcome, config::Decision, config::ExecMode, restore_snapshot,
};

const CONNECT_WAIT: Duration = Duration::from_secs(40);
const SHORT_WAIT: Duration = Duration::from_secs(8);

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
            .prefix("vela-mcp-sshd-")
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

    fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    fn root(&self) -> PathBuf {
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

    fn spec(&self) -> ConnectionSpec {
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

async fn connect(fixture: &Fixture) -> Session {
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

#[derive(Clone)]
struct TestBackend {
    servers: Vec<(String, ServerIdentity)>,
    session: Option<Session>,
    dirty: Arc<Mutex<HashSet<String>>>,
    ui: Option<(String, String)>,
    home: PathBuf,
    config_dir: PathBuf,
}

impl McpBackend for TestBackend {
    fn list_servers(&self) -> Vec<(String, ServerIdentity)> {
        self.servers.clone()
    }

    fn session(&self, server_id: &str) -> Option<&Session> {
        (server_id == "srv")
            .then_some(self.session.as_ref())
            .flatten()
    }

    fn edits_dirty(&self, _server_id: &str, canonical_path: &str) -> bool {
        self.dirty.lock().unwrap().contains(canonical_path)
    }

    fn ui_context(&self) -> Option<(String, String)> {
        self.ui.clone()
    }

    fn vela_config_dir(&self) -> Option<PathBuf> {
        Some(self.config_dir.clone())
    }

    fn home_dir(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }
}

struct Harness {
    _rollback_dir: TempDir,
    backend: TestBackend,
    config: McpConfig,
    rollback: Arc<RollbackStore>,
}

impl Harness {
    fn new(fixture: &Fixture, session: Session, profile: &str) -> Self {
        Self::with_config(fixture, session, config(&fixture.root(), profile))
    }

    fn with_config(fixture: &Fixture, session: Session, config: McpConfig) -> Self {
        let rollback_dir = tempfile::tempdir().unwrap();
        let rollback = Arc::new(
            RollbackStore::new(
                rollback_dir.path().join("rollback"),
                config.rollback.clone(),
            )
            .unwrap(),
        );
        let dirty = Arc::new(Mutex::new(HashSet::new()));
        let config_dir = fixture.path("config-dir");
        fs::create_dir_all(&config_dir).unwrap();
        let backend = TestBackend {
            servers: vec![(
                "srv".into(),
                ServerIdentity {
                    host: "127.0.0.1".into(),
                    port: fixture.port,
                    user: fixture.user.clone(),
                },
            )],
            session: Some(session),
            dirty,
            ui: Some(("srv".into(), fixture.root().to_string_lossy().into_owned())),
            home: fixture.root(),
            config_dir,
        };
        Self {
            _rollback_dir: rollback_dir,
            backend,
            config,
            rollback,
        }
    }

    fn executor(&self) -> Executor<TestBackend> {
        Executor::new(
            self.backend.clone(),
            self.config.clone(),
            self.rollback.clone(),
        )
    }

    fn mark_dirty(&self, path: &str) {
        self.backend.dirty.lock().unwrap().insert(path.to_owned());
    }
}

fn config(root: &Path, profile: &str) -> McpConfig {
    let mut config = McpConfig {
        enabled: true,
        share_ui_context: true,
        ..McpConfig::default()
    };
    let mut policy = ServerMcpPolicy {
        availability: ServerAvailability::Headless,
        profile_id: profile.into(),
        allowed_remote_paths: vec![format!("{}/**", root.display())],
        allowed_local_paths: vec![format!("{}/**", root.display())],
        ..ServerMcpPolicy::default()
    };
    if profile == "custom" {
        policy.exec_mode = Some(ExecMode::AllowExceptDenylist);
    }
    config.servers.insert("srv".into(), policy);
    config
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn assert_denied<T: std::fmt::Debug>(outcome: ToolOutcome<T>) {
    assert!(matches!(outcome, ToolOutcome::Denied(_)), "{outcome:?}");
}

async fn within<T>(future: impl Future<Output = Result<T, ExecutorError>>) -> T {
    timeout(SHORT_WAIT, future).await.unwrap().unwrap()
}

// fs_list, fs_stat, fs_read, paginación y symlinks ---------------------------

#[tokio::test]
async fn fs_list_stat_and_read_respect_depth_pattern_and_binary_files() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let root = fixture.root();
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), "uno\n").unwrap();
    for name in ["b.txt", "c.md"] {
        fs::write(root.join("sub").join(name), format!("{name}\n")).unwrap();
    }
    let deep = root.join("sub/mas");
    fs::create_dir(&deep).unwrap();
    fs::write(deep.join("d.txt"), "profundo\n").unwrap();
    fs::write(root.join("binario"), b"abc\0def").unwrap();

    let listed = within(executor.fs_list(
        FsListInput {
            server_id: "srv".into(),
            path: root.to_string_lossy().into_owned(),
            depth: Some(2),
            pattern: Some("**/*.txt".into()),
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    let paths: Vec<&str> = listed
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(paths.contains(&"a.txt"), "{paths:?}");
    assert!(paths.contains(&"sub/b.txt"), "{paths:?}");
    assert!(!paths.contains(&"sub/c.md"), "{paths:?}");
    assert!(!paths.contains(&"sub/mas/d.txt"), "{paths:?}");

    let full = within(executor.fs_list(
        FsListInput {
            server_id: "srv".into(),
            path: root.to_string_lossy().into_owned(),
            depth: Some(5),
            pattern: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert!(
        full.entries
            .iter()
            .any(|entry| entry.path == "sub/mas/d.txt")
    );

    let stat = within(executor.fs_stat(
        vela_mcp::FsStatInput {
            server_id: "srv".into(),
            path: root.join("a.txt").to_string_lossy().into_owned(),
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert_eq!(stat.size, Some(4));
    assert!(!stat.is_directory);

    let text = (0..10)
        .map(|index| format!("linea {index}\n"))
        .collect::<String>();
    fs::write(root.join("paginas.txt"), &text).unwrap();
    let first = within(executor.fs_read(
        FsReadInput {
            server_id: "srv".into(),
            path: root.join("paginas.txt").to_string_lossy().into_owned(),
            start_line: None,
            max_lines: Some(3),
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert_eq!(first.total_lines, 10);
    assert_eq!(first.returned_lines, 3);
    assert_eq!(first.content, "linea 0\nlinea 1\nlinea 2");
    assert!(first.truncated);
    let second = within(executor.fs_read(
        FsReadInput {
            server_id: "srv".into(),
            path: root.join("paginas.txt").to_string_lossy().into_owned(),
            start_line: Some(3),
            max_lines: Some(3),
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert_eq!(second.content, "linea 3\nlinea 4\nlinea 5");

    let binary = executor
        .fs_read(
            FsReadInput {
                server_id: "srv".into(),
                path: root.join("binario").to_string_lossy().into_owned(),
                start_line: None,
                max_lines: None,
            },
            ApprovalMode::Normal,
        )
        .await
        .unwrap_err();
    assert!(matches!(binary, ExecutorError::BinaryFile(_)), "{binary:?}");
}

#[tokio::test]
async fn fs_read_blocks_symlinks_outside_scope_and_into_ssh() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let root = fixture.root();
    // Un directorio distinto, fuera del allowlist configurado para `srv`.
    let outside_dir = tempfile::tempdir().unwrap();
    let outside = outside_dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secreto"), "secreto\n").unwrap();
    std::os::unix::fs::symlink(outside.join("secreto"), root.join("enlace")).unwrap();

    let ssh = root.join(".ssh");
    fs::create_dir(&ssh).unwrap();
    fs::write(ssh.join("id_ed25519"), "clave privada\n").unwrap();
    std::os::unix::fs::symlink(ssh.join("id_ed25519"), root.join("clave")).unwrap();

    let asked = within(executor.fs_read(
        FsReadInput {
            server_id: "srv".into(),
            path: root.join("enlace").to_string_lossy().into_owned(),
            start_line: None,
            max_lines: None,
        },
        ApprovalMode::Normal,
    ))
    .await;
    match asked {
        ToolOutcome::NeedsApproval { decision, .. } => {
            assert!(
                decision
                    .warnings
                    .iter()
                    .any(|warning| matches!(warning, PolicyWarning::OutOfScopePath { .. }))
            );
        }
        other => panic!("expected out-of-scope approval, got {other:?}"),
    }

    assert_denied(
        within(executor.fs_read(
            FsReadInput {
                server_id: "srv".into(),
                path: root.join("clave").to_string_lossy().into_owned(),
                start_line: None,
                max_lines: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
}

#[tokio::test]
async fn fs_list_does_not_follow_directory_symlinks_or_enter_denied_folders() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let root = fixture.root();

    // Un symlink a un directorio de otro ámbito dentro de `root`.
    let outside = tempfile::tempdir().unwrap();
    let outside_dir = outside.path().join("afuera");
    fs::create_dir(&outside_dir).unwrap();
    fs::write(outside_dir.join("secreto.txt"), "secreto\n").unwrap();
    std::os::unix::fs::symlink(&outside_dir, root.join("enlace-dir")).unwrap();

    // Una carpeta `.ssh` dentro de `root`, denegada por la denylist crítica.
    let ssh = root.join(".ssh");
    fs::create_dir(&ssh).unwrap();
    fs::write(ssh.join("id_ed25519"), "clave privada\n").unwrap();

    // Un subdirectorio normal, que sí debe recorrerse.
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("visible.txt"), "visible\n").unwrap();

    let listed = within(executor.fs_list(
        FsListInput {
            server_id: "srv".into(),
            path: root.to_string_lossy().into_owned(),
            depth: Some(3),
            pattern: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    let paths: Vec<&str> = listed
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert!(paths.contains(&"sub/visible.txt"), "{paths:?}");
    // El symlink puede verse como entrado, pero nunca se atraviesa.
    assert!(
        !paths.iter().any(|path| path.starts_with("enlace-dir/")),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.starts_with(".ssh")),
        "{paths:?}"
    );
    assert!(!paths.contains(&"enlace-dir/secreto.txt"), "{paths:?}");
}

// fs_write, fs_patch y rollback ---------------------------------------------

#[tokio::test]
async fn fs_write_and_patch_keep_an_exact_restorable_snapshot() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let remote = fixture.path("app.js");
    let original = b"uno\ndos\ntres\n";
    fs::write(&remote, original).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&remote, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let original_hash = sha256(original);

    let written = within(executor.fs_write(
        vela_mcp::FsWriteInput {
            server_id: "srv".into(),
            path: remote.to_string_lossy().into_owned(),
            content: "uno\ndos\ntres\ncuatro\n".into(),
            mode: Some(0o600),
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await
    .executed()
    .unwrap();
    let backup_id = written.backup_id.clone().unwrap();
    assert!(written.unified_diff.contains("+cuatro"));
    assert_eq!(fs::read(&remote).unwrap(), b"uno\ndos\ntres\ncuatro\n");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&remote).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let patched = within(executor.fs_patch(
        vela_mcp::FsPatchInput {
            server_id: "srv".into(),
            path: remote.to_string_lossy().into_owned(),
            edits: vec![TextEdit {
                old_text: "dos".into(),
                new_text: "DOS".into(),
            }],
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await
    .executed()
    .unwrap();
    assert!(patched.unified_diff.contains("-dos"));
    assert!(patched.unified_diff.contains("+DOS"));
    assert_eq!(fs::read(&remote).unwrap(), b"uno\nDOS\ntres\ncuatro\n");

    let (entry, bytes) = harness
        .rollback
        .get_snapshot(&backup_id, SystemTime::now())
        .unwrap()
        .unwrap();
    assert_eq!(entry.sha256, original_hash);
    assert_eq!(sha256(&bytes), original_hash);
    assert_eq!(entry.mode, Some(0o640));

    let restored = restore_snapshot(
        harness.backend.session("srv").unwrap(),
        &harness.rollback,
        &backup_id,
        SystemTime::now(),
    )
    .await
    .unwrap();
    assert_eq!(restored.sha256, original_hash);
    assert_eq!(sha256(&fs::read(&remote).unwrap()), original_hash);
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&remote).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[tokio::test]
async fn needs_approval_patch_shows_a_diff_and_changes_nothing_until_approved() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "production_protected");
    let executor = harness.executor();
    let remote = fixture.path("protegido.txt");
    fs::write(&remote, "antes\n").unwrap();

    let outcome = within(executor.fs_patch(
        vela_mcp::FsPatchInput {
            server_id: "srv".into(),
            path: remote.to_string_lossy().into_owned(),
            edits: vec![TextEdit {
                old_text: "antes".into(),
                new_text: "despues".into(),
            }],
            reason: Some("cambio revisado".into()),
        },
        ApprovalMode::Normal,
    ))
    .await;
    let preview = match outcome {
        ToolOutcome::NeedsApproval { preview, .. } => preview,
        other => panic!("expected approval, got {other:?}"),
    };
    assert_eq!(preview.operation, "fs_patch");
    assert_eq!(preview.agent_reason.as_deref(), Some("cambio revisado"));
    let diff = preview.unified_diff.unwrap();
    assert!(diff.contains("-antes"));
    assert!(diff.contains("+despues"));
    assert_eq!(fs::read(&remote).unwrap(), b"antes\n");

    let executed = within(executor.fs_patch(
        vela_mcp::FsPatchInput {
            server_id: "srv".into(),
            path: remote.to_string_lossy().into_owned(),
            edits: vec![TextEdit {
                old_text: "antes".into(),
                new_text: "despues".into(),
            }],
            reason: Some("aprobado".into()),
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await
    .executed()
    .unwrap();
    assert!(executed.backup_id.is_some());
    assert_eq!(fs::read(&remote).unwrap(), b"despues\n");
}

#[tokio::test]
async fn edits_conflict_denies_write_and_patch_without_touching_the_remote() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let remote = fixture.path("editado.txt");
    fs::write(&remote, "intacto\n").unwrap();
    let canonical = harness
        .backend
        .session("srv")
        .unwrap()
        .realpath(remote.clone())
        .await
        .unwrap()
        .to_string_lossy()
        .into_owned();
    harness.mark_dirty(&canonical);

    assert_denied(
        within(executor.fs_write(
            vela_mcp::FsWriteInput {
                server_id: "srv".into(),
                path: remote.to_string_lossy().into_owned(),
                content: "modificado\n".into(),
                mode: None,
                reason: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
    assert_denied(
        within(executor.fs_patch(
            vela_mcp::FsPatchInput {
                server_id: "srv".into(),
                path: remote.to_string_lossy().into_owned(),
                edits: vec![TextEdit {
                    old_text: "intacto".into(),
                    new_text: "otro".into(),
                }],
                reason: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
    assert_eq!(fs::read(&remote).unwrap(), b"intacto\n");
}

#[tokio::test]
async fn fs_patch_on_a_denied_env_file_never_reveals_its_content() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let secret = fixture.path(".env");
    let original = b"PASSWORD=super-secreto\n";
    fs::write(&secret, original).unwrap();

    // `old_text` no existe: antes de la corrección esto devolvía
    // `Err(PatchError::NotFound)`, distinto de `Denied`, y permitía sondear
    // el contenido carácter a carácter.
    let outcome = within(executor.fs_patch(
        vela_mcp::FsPatchInput {
            server_id: "srv".into(),
            path: secret.to_string_lossy().into_owned(),
            edits: vec![TextEdit {
                old_text: "no-existe-en-el-archivo".into(),
                new_text: "x".into(),
            }],
            reason: Some("sondeo".into()),
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await;
    assert_denied(outcome);
    assert_eq!(fs::read(&secret).unwrap(), original);
}

// fs_mkdir, fs_move, fs_delete y fs_chmod -----------------------------------

#[tokio::test]
async fn mkdir_move_delete_with_backup_and_chmod_apply_over_sftp() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let harness = Harness::new(&fixture, session, "development");
    let executor = harness.executor();
    let root = fixture.root();
    let dir = root.join("carpeta");
    let file = dir.join("archivo.txt");

    within(executor.fs_mkdir(
        vela_mcp::FsMkdirInput {
            server_id: "srv".into(),
            path: dir.to_string_lossy().into_owned(),
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await;
    assert!(dir.is_dir());
    fs::write(&file, "contenido\n").unwrap();

    let moved = within(executor.fs_move(
        vela_mcp::FsMoveInput {
            server_id: "srv".into(),
            from: file.to_string_lossy().into_owned(),
            to: dir.join("movido.txt").to_string_lossy().into_owned(),
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await
    .executed()
    .unwrap();
    assert_eq!(moved.from, file.to_string_lossy().as_ref());
    assert!(!file.exists());
    assert_eq!(fs::read(dir.join("movido.txt")).unwrap(), b"contenido\n");

    within(executor.fs_chmod(
        vela_mcp::FsChmodInput {
            server_id: "srv".into(),
            path: dir.join("movido.txt").to_string_lossy().into_owned(),
            mode: 0o600,
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await;
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.join("movido.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let deleted = within(executor.fs_delete(
        vela_mcp::FsDeleteInput {
            server_id: "srv".into(),
            path: dir.join("movido.txt").to_string_lossy().into_owned(),
            recursive: false,
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await
    .executed()
    .unwrap();
    let backup_id = deleted.backup_id.clone().unwrap();
    assert!(!dir.join("movido.txt").exists());
    let restored = restore_snapshot(
        harness.backend.session("srv").unwrap(),
        &harness.rollback,
        &backup_id,
        SystemTime::now(),
    )
    .await
    .unwrap();
    assert_eq!(restored.sha256, sha256(b"contenido\n"));
    assert_eq!(fs::read(dir.join("movido.txt")).unwrap(), b"contenido\n");

    within(executor.fs_delete(
        vela_mcp::FsDeleteInput {
            server_id: "srv".into(),
            path: dir.to_string_lossy().into_owned(),
            recursive: true,
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await;
    assert!(!dir.exists());
}

// transfer_upload / transfer_download ---------------------------------------

#[tokio::test]
async fn transfers_keep_exact_bytes_and_block_ssh_and_config_paths() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let root = fixture.root();
    let mut allowed = config(&root, "development");
    allowed.servers.get_mut("srv").unwrap().allowed_local_paths =
        vec![format!("{}/**", root.display())];
    let harness = Harness::with_config(&fixture, session, allowed);
    let executor = harness.executor();

    let source = root.join("origen.bin");
    let bytes: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
    fs::write(&source, &bytes).unwrap();
    let remote = root.join("remoto.bin");
    let uploaded = within(executor.transfer_upload(
        vela_mcp::TransferUploadInput {
            server_id: "srv".into(),
            local_path: source.to_string_lossy().into_owned(),
            remote_path: remote.to_string_lossy().into_owned(),
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await
    .executed()
    .unwrap();
    assert_eq!(uploaded.bytes, bytes.len() as u64);
    assert_eq!(fs::read(&remote).unwrap(), bytes);

    let downloaded_to = root.join("descargado.bin");
    within(executor.transfer_download(
        vela_mcp::TransferDownloadInput {
            server_id: "srv".into(),
            remote_path: remote.to_string_lossy().into_owned(),
            local_path: downloaded_to.to_string_lossy().into_owned(),
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await;
    assert_eq!(fs::read(&downloaded_to).unwrap(), bytes);

    // Una política que deniega transferencias bloquea tanto ~/.ssh como el
    // directorio de configuración y cualquier ruta fuera del allowlist.
    let mut denied = config(&root, "custom");
    let profile = vela_mcp::PermissionProfile {
        id: "custom".into(),
        name: "Sin transferencias".into(),
        transfer_upload: Decision::Deny,
        transfer_download: Decision::Deny,
        ..vela_mcp::PermissionProfile::default()
    };
    denied.profiles.push(profile);
    let harness = Harness::with_config(
        &fixture,
        harness.backend.session("srv").unwrap().clone(),
        denied,
    );
    let executor = harness.executor();
    let ssh_key = root.join(".ssh/id_ed25519");
    fs::create_dir_all(ssh_key.parent().unwrap()).unwrap();
    fs::write(&ssh_key, "privada").unwrap();
    assert_denied(
        within(executor.transfer_upload(
            vela_mcp::TransferUploadInput {
                server_id: "srv".into(),
                local_path: ssh_key.to_string_lossy().into_owned(),
                remote_path: root.join("copia").to_string_lossy().into_owned(),
                reason: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
    assert_denied(
        within(
            executor.transfer_download(
                vela_mcp::TransferDownloadInput {
                    server_id: "srv".into(),
                    remote_path: remote.to_string_lossy().into_owned(),
                    local_path: harness
                        .backend
                        .config_dir
                        .join("mcp.toml")
                        .to_string_lossy()
                        .into_owned(),
                    reason: None,
                },
                ApprovalMode::ApprovedOnce,
            ),
        )
        .await,
    );
    assert_denied(
        within(executor.transfer_upload(
            vela_mcp::TransferUploadInput {
                server_id: "srv".into(),
                local_path: "/etc/hosts".into(),
                remote_path: root.join("copia").to_string_lossy().into_owned(),
                reason: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
}

// exec_command --------------------------------------------------------------

#[tokio::test]
async fn exec_command_honours_allowlist_cwd_timeout_limits_and_no_agent() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let root = fixture.root();
    let sub = root.join("sub");
    fs::create_dir(&sub).unwrap();
    let mut config = config(&root, "development");
    config.servers.get_mut("srv").unwrap().exec_mode = Some(ExecMode::AllowExceptDenylist);
    config.servers.get_mut("srv").unwrap().denied_commands = vec!["rm *".into()];
    let harness = Harness::with_config(&fixture, session, config);
    let executor = harness.executor();

    let pwd = within(executor.exec_command(
        vela_mcp::ExecCommandInput {
            server_id: "srv".into(),
            command: "pwd".into(),
            cwd: Some(sub.to_string_lossy().into_owned()),
            timeout_secs: Some(10),
            reason: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert_eq!(pwd.exit_code, Some(0));
    assert_eq!(pwd.stdout.trim(), sub.to_string_lossy().as_ref());
    assert!(!pwd.timed_out);

    let agent = within(executor.exec_command(
        vela_mcp::ExecCommandInput {
            server_id: "srv".into(),
            command: "printf '%s' \"${SSH_AUTH_SOCK-unset}\"".into(),
            cwd: None,
            timeout_secs: None,
            reason: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert!(
        agent.stdout.trim().is_empty() || agent.stdout.trim() == "unset",
        "SSH_AUTH_SOCK was forwarded: {:?}",
        agent.stdout
    );

    let big = within(executor.exec_command(
        vela_mcp::ExecCommandInput {
            server_id: "srv".into(),
            command: "yes a | head -c 200000".into(),
            cwd: None,
            timeout_secs: Some(10),
            reason: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert!(big.stdout_truncated);
    assert_eq!(big.stdout.len(), 64 * 1024);
    assert!(!big.timed_out);

    let started = Instant::now();
    let slow = within(executor.exec_command(
        vela_mcp::ExecCommandInput {
            server_id: "srv".into(),
            command: "sleep 30".into(),
            cwd: None,
            timeout_secs: Some(1),
            reason: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    .executed()
    .unwrap();
    assert!(slow.timed_out);
    assert!(slow.exit_code.is_none());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout not enforced"
    );

    assert_denied(
        within(executor.exec_command(
            vela_mcp::ExecCommandInput {
                server_id: "srv".into(),
                command: "rm /tmp/archivo".into(),
                cwd: None,
                timeout_secs: None,
                reason: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
    assert_denied(
        within(executor.exec_command(
            vela_mcp::ExecCommandInput {
                server_id: "srv".into(),
                command: "rm -rf /".into(),
                cwd: None,
                timeout_secs: None,
                reason: None,
            },
            ApprovalMode::ApprovedOnce,
        ))
        .await,
    );
}

#[tokio::test]
async fn exec_command_validates_the_working_directory_with_policy() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let root = fixture.root();
    let mut config = config(&root, "development");
    config.servers.get_mut("srv").unwrap().exec_mode = Some(ExecMode::AllowExceptDenylist);
    let harness = Harness::with_config(&fixture, session, config);
    let executor = harness.executor();

    match within(executor.exec_command(
        vela_mcp::ExecCommandInput {
            server_id: "srv".into(),
            command: "pwd".into(),
            cwd: Some("/tmp".into()),
            timeout_secs: None,
            reason: None,
        },
        ApprovalMode::Normal,
    ))
    .await
    {
        ToolOutcome::NeedsApproval { decision, .. } => {
            assert_eq!(decision.matched_rule, "out_of_scope_path");
        }
        other => panic!("expected cwd approval, got {other:?}"),
    }
}

#[tokio::test]
async fn denied_command_is_not_downgraded_to_ask_by_an_out_of_scope_cwd() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let root = fixture.root();
    let mut config = config(&root, "custom");
    config.servers.get_mut("srv").unwrap().exec_mode = Some(ExecMode::AllowExceptDenylist);
    config.servers.get_mut("srv").unwrap().denied_commands = vec!["rm *".into()];
    let harness = Harness::with_config(&fixture, session, config);
    let executor = harness.executor();

    // `/tmp` está fuera del allowlist: antes de la corrección su `Ask` pisaba
    // el `Deny` del comando y, con aprobación previa, llegaba a ejecutarse.
    let outcome = within(executor.exec_command(
        vela_mcp::ExecCommandInput {
            server_id: "srv".into(),
            command: "rm -rf /".into(),
            cwd: Some("/tmp".into()),
            timeout_secs: Some(10),
            reason: None,
        },
        ApprovalMode::ApprovedOnce,
    ))
    .await;
    match outcome {
        ToolOutcome::Denied(decision) => assert_eq!(decision.outcome, Decision::Deny),
        other => panic!("se esperaba Deny: {other:?}"),
    }
}

// Introspección -------------------------------------------------------------

#[tokio::test]
async fn list_servers_hides_disabled_and_identity_when_configured() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let mut config = config(&fixture.root(), "readonly");
    config.hide_server_identity = true;
    config.servers.insert(
        "oculto".into(),
        ServerMcpPolicy {
            availability: ServerAvailability::Disabled,
            ..ServerMcpPolicy::default()
        },
    );
    let harness = Harness::with_config(&fixture, session, config);
    let mut backend = harness.backend.clone();
    backend.servers.push((
        "oculto".into(),
        ServerIdentity {
            host: "127.0.0.1".into(),
            port: fixture.port,
            user: fixture.user.clone(),
        },
    ));
    let executor = Executor::new(backend, harness.config.clone(), harness.rollback.clone());

    let servers = executor
        .list_servers(vela_mcp::ListServersInput {}, ApprovalMode::Normal)
        .unwrap()
        .executed()
        .unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].id, "srv");
    assert_eq!(servers[0].host, None);
    assert_eq!(servers[0].port, None);
    assert_eq!(servers[0].user, None);

    let mut visible = harness.config.clone();
    visible.servers.get_mut("srv").unwrap().hide_server_identity = Some(false);
    let executor = Executor::new(harness.backend.clone(), visible, harness.rollback.clone());
    let servers = executor
        .list_servers(vela_mcp::ListServersInput {}, ApprovalMode::Normal)
        .unwrap()
        .executed()
        .unwrap();
    assert_eq!(servers[0].host.as_deref(), Some("127.0.0.1"));
    assert_eq!(servers[0].port, Some(fixture.port));
}

#[tokio::test]
async fn describe_permissions_and_ui_context_work_without_a_live_transfer() {
    let fixture = Fixture::start();
    let session = connect(&fixture).await;
    let mut config = config(&fixture.root(), "diagnostics");
    config.servers.get_mut("srv").unwrap().allowed_commands = vec!["mi-comando *".into()];
    config.servers.get_mut("srv").unwrap().denied_commands = vec!["rm *".into()];
    let harness = Harness::with_config(&fixture, session, config);
    let executor = harness.executor();
    let permissions = executor
        .describe_permissions(
            vela_mcp::DescribePermissionsInput {
                server_id: "srv".into(),
            },
            ApprovalMode::Normal,
        )
        .unwrap()
        .executed()
        .unwrap();
    assert_eq!(permissions.profile_id, "diagnostics");
    assert_eq!(permissions.operations["fs_read"], Decision::Allow);
    assert_eq!(permissions.operations["fs_delete"], Decision::Deny);
    // Listas efectivas: perfil + servidor + denylist crítica.
    assert!(
        permissions
            .allowed_remote_paths
            .contains(&format!("{}/**", fixture.root().display()))
    );
    assert!(
        permissions
            .denied_remote_paths
            .iter()
            .any(|path| path == "**/.env")
    );
    assert!(permissions.allowed_commands.contains(&"ls *".to_owned()));
    assert!(
        permissions
            .allowed_commands
            .contains(&"mi-comando *".to_owned())
    );
    assert!(permissions.denied_commands.contains(&"rm *".to_owned()));

    let context = executor
        .get_ui_context(
            vela_mcp::GetUiContextInput {
                server_id: Some("srv".into()),
            },
            ApprovalMode::Normal,
        )
        .unwrap()
        .executed()
        .unwrap();
    assert_eq!(context.server_id, "srv");
    assert_eq!(
        context.current_remote_path,
        fixture.root().to_string_lossy().as_ref()
    );
}

#[test]
fn approval_preview_round_trips_as_json() {
    let preview = ApprovalPreview {
        operation: "fs_patch".into(),
        server_id: "srv".into(),
        unified_diff: Some("@@ -1 +1 @@\n-a\n+b\n".into()),
        ..ApprovalPreview::default()
    };
    let json = serde_json::to_string(&preview).unwrap();
    let decoded: ApprovalPreview = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, preview);
}

#[test]
fn backup_entry_round_trips_as_json() {
    let entry = BackupEntry {
        id: "id".into(),
        server_id: "srv".into(),
        remote_path: "/srv/a".into(),
        size_bytes: 2,
        sha256: sha256(b"ab"),
        mode: Some(0o600),
        created_at_secs: 100,
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert_eq!(serde_json::from_str::<BackupEntry>(&json).unwrap(), entry);
}
