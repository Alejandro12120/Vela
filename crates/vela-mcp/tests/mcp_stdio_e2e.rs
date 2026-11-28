//! Sesiones SSH reales y tráfico NDJSON a través de pipes anónimos del SO.

mod common;

use std::{
    fs::{self, File},
    io::{self, pipe},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use common::{Fixture, SHORT_WAIT, connect};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, timeout},
};
use vela_core::{Auth, Session, config::Server, settings::Settings};
use vela_mcp::{
    ApprovalMode, Executor, HeadlessAskBehavior, McpBackend, McpConfig, ROLLBACK_DIR_NAME,
    RollbackStore, ServerAvailability, ServerIdentity, ServerMcpPolicy, ToolOutcome, cli,
    config::{self, ExecMode},
    executor::ApprovalPreview,
    headless::{RuntimeOptions, VelaLauncher},
    ipc::IpcServer,
    protocol::{CallContext, Progress, ToolHandler, ToolResponse},
    tools::ToolCall,
};

struct Stdio {
    stdin: tokio::fs::File,
    stdout: BufReader<tokio::fs::File>,
    server: JoinHandle<io::Result<()>>,
}

impl Stdio {
    fn start(options: RuntimeOptions, launcher: impl VelaLauncher) -> Self {
        let (input, stdin) = pipe().unwrap();
        let (stdout, output) = pipe().unwrap();
        let input = pipe_file(input.into());
        let output = pipe_file(output.into());
        let server = tokio::spawn(cli::run(
            BufReader::new(input),
            output,
            options,
            "/bin/false".into(),
            launcher,
        ));
        Self {
            stdin: pipe_file(stdin.into()),
            stdout: BufReader::new(pipe_file(stdout.into())),
            server,
        }
    }

    async fn send(&mut self, message: Value) {
        self.stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        let mut message = String::new();
        assert_ne!(
            timeout(SHORT_WAIT, self.stdout.read_line(&mut message))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        serde_json::from_str(&message).unwrap()
    }

    async fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                       "clientInfo": {"name": "loopback-agent", "version": "1.2.3"}},
        }))
        .await;
        assert_eq!(
            self.receive().await["result"]["protocolVersion"],
            "2025-03-26"
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    async fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.send(tool_request(id, name, arguments)).await;
        let response = self.receive().await;
        assert_eq!(response["id"], id);
        response["result"].clone()
    }

    async fn close(self) {
        let Self {
            stdin,
            mut stdout,
            server,
        } = self;
        drop(stdin);
        timeout(SHORT_WAIT, server).await.unwrap().unwrap().unwrap();
        assert_eq!(stdout.read_line(&mut String::new()).await.unwrap(), 0);
    }
}

fn pipe_file(fd: OwnedFd) -> tokio::fs::File {
    tokio::fs::File::from_std(File::from(fd))
}

fn tool_request(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
        "name": name, "arguments": arguments,
    }})
}

fn content(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

struct Environment {
    fixture: Fixture,
    config_dir: PathBuf,
    config: McpConfig,
    server: Server,
}

impl Environment {
    fn start(behavior: HeadlessAskBehavior) -> Self {
        let fixture = Fixture::start();
        let config_dir = fixture.path("vela");
        fs::create_dir(&config_dir).unwrap();
        fs::copy(fixture.path("known_hosts"), config_dir.join("known_hosts")).unwrap();
        fs::write(fixture.path("notes.txt"), "original\n").unwrap();
        fs::write(fixture.path(".env"), "private\n").unwrap();
        let spec = fixture.spec();
        let server = Server {
            id: "srv".into(),
            name: "Loopback".into(),
            host: spec.host,
            port: spec.port,
            user: spec.user,
            auth: Auth::Agent {
                identity_pub: fixture.path("client.pub"),
                agent_socket: None,
            },
            initial_remote_path: None,
            pinned: false,
        };
        let password = Server {
            id: "pw".into(),
            auth: Auth::Password,
            ..server.clone()
        };
        let mut config = McpConfig {
            enabled: true,
            headless_ask: behavior,
            ..Default::default()
        };
        let policy = ServerMcpPolicy {
            availability: ServerAvailability::Headless,
            profile_id: "production_protected".into(),
            allowed_remote_paths: vec![format!(
                "{}/**",
                fixture.root().canonicalize().unwrap().display()
            )],
            exec_mode: Some(ExecMode::Allowlist),
            allowed_commands: vec!["pwd".into()],
            ..Default::default()
        };
        config.servers.insert(server.id.clone(), policy.clone());
        config.servers.insert(password.id.clone(), policy);
        config::save(&config_dir, &config).unwrap();
        vela_core::config::save(&config_dir, vec![server.clone(), password]).unwrap();
        Settings {
            agent_socket: Some(fixture.path("agent")),
            ..Default::default()
        }
        .save(&config_dir)
        .unwrap();
        Self {
            fixture,
            config_dir,
            config,
            server,
        }
    }

    fn options(&self, force_headless: bool) -> RuntimeOptions {
        RuntimeOptions {
            force_headless,
            socket_path: self.fixture.path("mcp.sock"),
            config_dir: Some(self.config_dir.clone()),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.fixture.root().canonicalize().unwrap().join(name)
    }

    fn ssh_control_socket(&self) -> Option<PathBuf> {
        let spec = self.fixture.spec();
        let output = Command::new("/usr/bin/ssh")
            .arg("-G")
            .args(spec.terminal_command(Path::new("/tmp"), ""))
            .output()
            .unwrap();
        assert!(output.status.success());
        let config = String::from_utf8(output.stdout).unwrap();
        let socket = config
            .lines()
            .find_map(|line| line.strip_prefix("controlpath "))
            .unwrap();
        let basename = Path::new(socket).file_name().unwrap();
        fs::read_dir("/tmp")
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().as_encoded_bytes().starts_with(b"vm-"))
            .filter_map(|entry| fs::read_dir(entry.path()).ok())
            .flat_map(|entries| entries.filter_map(Result::ok))
            .map(|entry| entry.path().join(basename))
            .find(|path| path.exists())
    }
}

struct NeverLaunch;

impl VelaLauncher for NeverLaunch {
    fn launch(&self, _: &RuntimeOptions) -> io::Result<()> {
        panic!("esta prueba no debe abrir Vela")
    }
}

#[derive(Clone)]
struct GuiBackend {
    id: String,
    server: ServerIdentity,
    session: Session,
    config_dir: PathBuf,
}

impl McpBackend for GuiBackend {
    fn list_servers(&self) -> Vec<(String, ServerIdentity)> {
        vec![(self.id.clone(), self.server.clone())]
    }

    fn session(&self, id: &str) -> Option<&Session> {
        (self.id == id).then_some(&self.session)
    }

    fn edits_dirty(&self, _: &str, _: &str) -> bool {
        false
    }
    fn ui_context(&self) -> Option<(String, String)> {
        None
    }
    fn vela_config_dir(&self) -> Option<PathBuf> {
        Some(self.config_dir.clone())
    }
}

async fn gui(env: &Environment) -> Arc<Executor<GuiBackend>> {
    let session = connect(&env.fixture).await;
    let server = ServerIdentity {
        host: env.server.host.clone(),
        port: env.server.port,
        user: env.server.user.clone(),
    };
    let backend = GuiBackend {
        id: env.server.id.clone(),
        server,
        session,
        config_dir: env.config_dir.clone(),
    };
    let rollback = Arc::new(
        RollbackStore::new(
            env.config_dir.join(ROLLBACK_DIR_NAME),
            env.config.rollback.clone(),
        )
        .unwrap(),
    );
    Arc::new(Executor::new(backend, env.config.clone(), rollback))
}

fn approved_gui(
    executor: Arc<Executor<GuiBackend>>,
    calls: Arc<AtomicUsize>,
    old_control_dir: Arc<Mutex<Option<PathBuf>>>,
) -> impl ToolHandler {
    move |context: CallContext, tool: ToolCall| {
        let (executor, calls, old_control_dir) =
            (executor.clone(), calls.clone(), old_control_dir.clone());
        async move {
            assert_eq!(context.client.name, "loopback-agent");
            assert_eq!(context.client.version, "1.2.3");
            if let Some(path) = &*old_control_dir.lock().unwrap() {
                assert!(
                    !path.exists(),
                    "headless aún conserva su ControlPath antes de reenviar"
                );
            }
            calls.fetch_add(1, Ordering::Relaxed);
            let pending = tool.clone().execute(&executor, ApprovalMode::Normal).await;
            if let Ok(ToolOutcome::NeedsApproval { preview, .. }) = &pending {
                assert_eq!(preview.operation, "fs_patch");
                assert_eq!(preview.agent_reason.as_deref(), Some("corregir notas"));
                context.progress.report(Progress {
                    progress: 4,
                    total: None,
                    message: "Esperando aprobación humana".into(),
                });
                sleep(Duration::from_millis(40)).await;
                return Ok(ToolResponse::from_outcome(
                    tool.execute(&executor, ApprovalMode::ApprovedOnce).await,
                ));
            }
            Ok(ToolResponse::from_outcome(pending))
        }
    }
}

async fn patch(stdio: &mut Stdio, path: &Path) -> Value {
    let mut request = tool_request(
        4,
        "fs_patch",
        json!({
            "server_id": "srv", "path": path, "reason": "corregir notas",
            "edits": [{"old_text": "original", "new_text": "corregido"}],
        }),
    );
    request["params"]["_meta"] = json!({"progressToken": "review"});
    stdio.send(request).await;
    let mut awaiting_human = false;
    loop {
        let reply = stdio.receive().await;
        if reply["method"] == "notifications/progress" {
            assert_eq!(reply["params"]["progressToken"], "review");
            awaiting_human |= reply["params"]["message"] == "Esperando aprobación humana";
        } else {
            assert_eq!(reply["id"], 4);
            assert!(awaiting_human, "{reply}");
            assert_eq!(reply["result"]["isError"], false, "{reply}");
            return content(&reply["result"]);
        }
    }
}

#[tokio::test]
async fn bridge_forwards_real_ssh_patch_with_approval_rollback_and_exec() {
    let env = Environment::start(HeadlessAskBehavior::Deny);
    let executor = gui(&env).await;
    let options = env.options(false);
    let socket = options.socket_path.clone();
    let server = IpcServer::bind(socket.clone()).await.unwrap();
    let (stop, stopped) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = approved_gui(executor.clone(), calls.clone(), Default::default());
    let task = tokio::spawn(server.run(handler, async {
        let _ = stopped.await;
    }));

    let mut stdio = Stdio::start(options, NeverLaunch);
    stdio.initialize().await;
    stdio
        .send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .await;
    assert_eq!(
        stdio.receive().await["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        15
    );

    let notes = env.file("notes.txt");
    let result = stdio
        .call(3, "fs_read", json!({"server_id": "srv", "path": notes}))
        .await;
    assert_eq!(content(&result)["content"], "original");
    assert!(env.ssh_control_socket().is_none());
    let patched = patch(&mut stdio, &notes).await;
    assert_eq!(fs::read_to_string(&notes).unwrap(), "corregido\n");
    let (_, saved) = executor
        .rollback
        .get_snapshot(patched["backup_id"].as_str().unwrap(), SystemTime::now())
        .unwrap()
        .unwrap();
    assert_eq!(saved, b"original\n");

    let cwd = notes.parent().unwrap();
    let result = stdio
        .call(
            5,
            "exec_command",
            json!({"server_id": "srv", "command": "pwd", "cwd": cwd}),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        content(&result)["stdout"].as_str().unwrap().trim(),
        cwd.to_str().unwrap()
    );
    let result = stdio
        .call(
            6,
            "fs_read",
            json!({"server_id": "srv", "path": env.file(".env")}),
        )
        .await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(content(&result)["matched_rule"].is_string());
    assert_eq!(calls.load(Ordering::Relaxed), 4);

    let disabled = McpConfig {
        enabled: false,
        ..env.config.clone()
    };
    config::save(&env.config_dir, &disabled).unwrap();
    let next = stdio.call(7, "list_servers", json!({})).await;
    assert_eq!(content(&next)["matched_rule"], "mcp_disabled");
    assert_eq!(calls.load(Ordering::Relaxed), 4);

    stdio.close().await;
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!socket.exists());
    executor.backend.session.disconnect().await.unwrap();
}

#[tokio::test]
async fn headless_uses_isolated_ssh_fails_closed_and_cleans_up_at_eof() {
    let env = Environment::start(HeadlessAskBehavior::Deny);
    let mut stdio = Stdio::start(env.options(false), NeverLaunch);
    stdio.initialize().await;
    let notes = env.file("notes.txt");
    let result = stdio
        .call(2, "fs_read", json!({"server_id": "srv", "path": notes}))
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(content(&result)["content"], "original");
    let control = env
        .ssh_control_socket()
        .expect("headless SSH must own an isolated socket");
    let root = control.parent().unwrap().parent().unwrap();
    assert!(root.is_dir());

    // Connecting the GUI concurrently must leave the headless master untouched.
    let gui_session = connect(&env.fixture).await;
    assert!(control.exists());
    let result = stdio
        .call(
            3,
            "fs_write",
            json!({"server_id": "srv", "path": notes, "content": "new"}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(content(&result)["matched_rule"], "headless_fail_closed");
    assert_eq!(fs::read_to_string(&notes).unwrap(), "original\n");

    let mut config = env.config.clone();
    let policy = config.servers.get_mut("srv").unwrap();
    policy.exec_mode = Some(ExecMode::AskAll);
    let cwd = notes.parent().unwrap().to_str().unwrap();
    for preview in [
        ApprovalPreview {
            operation: "fs_write".into(),
            paths: vec![notes.to_str().unwrap().into()],
            ..Default::default()
        },
        ApprovalPreview {
            operation: "exec_command".into(),
            paths: vec![cwd.into()],
            command: Some(vela_core::exec_remote_command("pwd", Some(cwd))),
            ..Default::default()
        },
    ] {
        policy.scoped_rules.push(preview.scoped_rule().unwrap());
    }
    config::save(&env.config_dir, &config).unwrap();
    let result = stdio
        .call(
            4,
            "fs_write",
            json!({"server_id": "srv", "path": notes, "content": "new"}),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(fs::read_to_string(&notes).unwrap(), "new");
    let result = stdio
        .call(
            5,
            "exec_command",
            json!({"server_id": "srv", "command": "pwd", "cwd": cwd}),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(content(&result)["stdout"].as_str().unwrap().trim(), cwd);
    let result = stdio
        .call(
            6,
            "exec_command",
            json!({"server_id": "srv", "command": "pwd"}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(content(&result)["matched_rule"], "headless_fail_closed");

    let result = stdio
        .call(7, "fs_read", json!({"server_id": "pw", "path": notes}))
        .await;
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("contraseña")
    );

    stdio.close().await;
    assert!(!root.exists());
    assert!(gui_session.stat(notes).await.is_ok());
    gui_session.disconnect().await.unwrap();
}

struct LaunchFixture {
    executor: Arc<Executor<GuiBackend>>,
    calls: Arc<AtomicUsize>,
    old_control_dir: Arc<Mutex<Option<PathBuf>>>,
    launched: Mutex<Option<oneshot::Sender<JoinHandle<io::Result<()>>>>>,
    stop: Mutex<Option<oneshot::Receiver<()>>>,
}

impl VelaLauncher for LaunchFixture {
    fn launch(&self, options: &RuntimeOptions) -> io::Result<()> {
        let handler = approved_gui(
            self.executor.clone(),
            self.calls.clone(),
            self.old_control_dir.clone(),
        );
        let path = options.socket_path.clone();
        let stop = self
            .stop
            .lock()
            .unwrap()
            .take()
            .expect("Vela se abre una sola vez");
        let task = tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            IpcServer::bind(path)
                .await?
                .run(handler, async {
                    let _ = stop.await;
                })
                .await
        });
        self.launched
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(task)
            .unwrap();
        Ok(())
    }
}

#[tokio::test]
async fn open_vela_handoff_releases_headless_ssh_and_forwards_later_calls() {
    let env = Environment::start(HeadlessAskBehavior::OpenVela);
    let options = env.options(false);
    let executor = gui(&env).await;
    let (launched, launch) = oneshot::channel();
    let (stop, stopping) = oneshot::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let old_control_dir = Arc::new(Mutex::new(None));
    let launcher = LaunchFixture {
        executor: executor.clone(),
        calls: calls.clone(),
        old_control_dir: old_control_dir.clone(),
        launched: Mutex::new(Some(launched)),
        stop: Mutex::new(Some(stopping)),
    };
    let mut stdio = Stdio::start(options, launcher);
    stdio.initialize().await;
    let notes = env.file("notes.txt");
    let result = stdio
        .call(2, "fs_read", json!({"server_id": "srv", "path": notes}))
        .await;
    assert_eq!(result["isError"], false, "{result}");
    let control = env.ssh_control_socket().unwrap();
    let root = control.parent().unwrap().parent().unwrap();
    *old_control_dir.lock().unwrap() = Some(root.to_path_buf());

    patch(&mut stdio, &notes).await;
    assert!(!root.exists());
    let result = stdio
        .call(5, "fs_read", json!({"server_id": "srv", "path": notes}))
        .await;
    assert_eq!(content(&result)["content"], "corregido");
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    stdio.close().await;
    stop.send(()).unwrap();
    launch.await.unwrap().await.unwrap().unwrap();
    executor.backend.session.disconnect().await.unwrap();
}

#[tokio::test]
async fn access_to_gui_only_servers_hands_off_unless_forced_headless() {
    for availability in [
        ServerAvailability::OnDemandAsk,
        ServerAvailability::ConnectedOnly,
    ] {
        let mut env = Environment::start(HeadlessAskBehavior::OpenVela);
        env.config.servers.get_mut("srv").unwrap().availability = availability;
        config::save(&env.config_dir, &env.config).unwrap();

        let notes = env.file("notes.txt");
        let arguments = json!({"server_id": "srv", "path": notes});
        let mut forced = Stdio::start(env.options(true), NeverLaunch);
        forced.initialize().await;
        let result = forced.call(2, "fs_read", arguments.clone()).await;
        assert_eq!(result["isError"], true);
        assert!(env.ssh_control_socket().is_none());
        forced.close().await;

        let options = env.options(false);
        let executor = gui(&env).await;
        let (launched, launch) = oneshot::channel();
        let (stop, stopping) = oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let launcher = LaunchFixture {
            executor: executor.clone(),
            calls: calls.clone(),
            old_control_dir: Default::default(),
            launched: Mutex::new(Some(launched)),
            stop: Mutex::new(Some(stopping)),
        };
        let mut stdio = Stdio::start(options, launcher);
        stdio.initialize().await;
        let result = stdio
            .call(2, "describe_permissions", json!({"server_id": "srv"}))
            .await;
        assert_eq!(result["isError"], false);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let result = stdio.call(3, "fs_read", arguments.clone()).await;
        assert_eq!(result["isError"], false, "{availability:?}: {result}");
        assert_eq!(content(&result)["content"], "original");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(env.ssh_control_socket().is_none());

        // --headless also ignores an already listening bridge.
        let mut forced = Stdio::start(env.options(true), NeverLaunch);
        forced.initialize().await;
        let result = forced.call(2, "fs_read", arguments).await;
        assert_eq!(result["isError"], true);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        forced.close().await;

        stdio.close().await;
        stop.send(()).unwrap();
        launch.await.unwrap().await.unwrap().unwrap();
        executor.backend.session.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn next_stdio_call_uses_updated_policy_and_disabled_switch() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = McpConfig {
        enabled: true,
        ..Default::default()
    };
    config.servers.insert(
        "srv".into(),
        ServerMcpPolicy {
            availability: ServerAvailability::Headless,
            ..Default::default()
        },
    );
    config::save(dir.path(), &config).unwrap();
    vela_core::config::save(
        dir.path(),
        vec![Server {
            id: "srv".into(),
            name: "Fixture".into(),
            host: "example.test".into(),
            port: 22,
            user: "local".into(),
            auth: Auth::Password,
            initial_remote_path: None,
            pinned: false,
        }],
    )
    .unwrap();
    let mut stdio = Stdio::start(
        RuntimeOptions {
            force_headless: true,
            socket_path: dir.path().join("mcp.sock"),
            config_dir: Some(dir.path().to_owned()),
        },
        NeverLaunch,
    );
    stdio.initialize().await;
    let list = stdio.call(2, "list_servers", json!({})).await;
    assert!(content(&list).to_string().contains("srv"), "{list}");
    let inspect = stdio
        .call(3, "describe_permissions", json!({"server_id": "srv"}))
        .await;
    assert_eq!(inspect["isError"], false, "{inspect}");

    config.servers.get_mut("srv").unwrap().availability = ServerAvailability::Disabled;
    config::save(dir.path(), &config).unwrap();
    let inspect = stdio
        .call(4, "describe_permissions", json!({"server_id": "srv"}))
        .await;
    assert_eq!(content(&inspect)["matched_rule"], "server_disabled");

    config.enabled = false;
    config::save(dir.path(), &config).unwrap();
    let list = stdio.call(5, "list_servers", json!({})).await;
    assert_eq!(content(&list)["matched_rule"], "mcp_disabled");
    stdio.close().await;
}
