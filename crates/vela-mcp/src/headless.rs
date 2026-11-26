//! Runtime de Vela sin interfaz gráfica y conmutación a un servidor IPC.

use std::{
    collections::HashMap,
    fs::Permissions,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tempfile::{Builder, TempDir};
use tokio::{
    sync::Mutex,
    task::JoinHandle,
    time::{Instant, sleep, timeout_at},
};
use vela_core::{Auth, Client, ConnectCancel, Session, config::Server, settings::Settings};

use crate::{
    ApprovalMode, Decision, Executor, HeadlessAskBehavior, McpAction, PolicyDecision,
    ROLLBACK_DIR_NAME, RollbackStore, ServerAvailability, ServerIdentity, ToolOutcome, config,
    ipc::IpcClient,
    protocol::{CallContext, ClientInfo, Progress, RpcError, ToolHandler, ToolResponse},
    tools::ToolCall,
};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Opciones efectivas de un proceso MCP.
pub struct RuntimeOptions {
    pub force_headless: bool,
    pub socket_path: PathBuf,
    /// Una ruta explícita aísla también el fichero `known_hosts`.
    pub config_dir: Option<PathBuf>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            force_headless: false,
            socket_path: crate::ipc::socket_path(None),
            config_dir: std::env::var_os("VELA_CONFIG_DIR")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
        }
    }
}

pub trait VelaLauncher: Send + Sync + 'static {
    fn launch(&self, options: &RuntimeOptions) -> io::Result<()>;
}

/// Lanza la interfaz en una sesión de proceso aparte y sin heredar el stdio MCP.
pub struct ProcessLauncher;

impl VelaLauncher for ProcessLauncher {
    fn launch(&self, options: &RuntimeOptions) -> io::Result<()> {
        let mut command = tokio::process::Command::new(std::env::current_exe()?);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("VELA_MCP_SOCK", &options.socket_path);
        if let Some(dir) = &options.config_dir {
            command.env("VELA_CONFIG_DIR", dir);
        }
        // SAFETY: the child only calls the async-signal-safe setsid before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }
}

struct HeadlessBackend {
    servers: Vec<Server>,
    sessions: HashMap<String, Session>,
    config_dir: PathBuf,
}

impl crate::McpBackend for HeadlessBackend {
    fn list_servers(&self) -> Vec<(String, ServerIdentity)> {
        self.servers
            .iter()
            .map(|server| {
                (
                    server.id.clone(),
                    ServerIdentity {
                        host: server.host.clone(),
                        port: server.port,
                        user: server.user.clone(),
                    },
                )
            })
            .collect()
    }

    fn session(&self, server_id: &str) -> Option<&Session> {
        self.sessions
            .get(server_id)
            .filter(|session| !session.is_closed())
    }

    fn edits_dirty(&self, _: &str, _: &str) -> bool {
        false
    }
    fn ui_context(&self) -> Option<(String, String)> {
        None
    }
    fn vela_gui_open(&self) -> bool {
        false
    }
    fn vela_config_dir(&self) -> Option<PathBuf> {
        Some(self.config_dir.clone())
    }
}

fn private_tempdir(prefix: &str, parent: &Path) -> io::Result<TempDir> {
    Builder::new()
        .prefix(prefix)
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(parent)
}

struct SshRuntime {
    client: Client,
    prompts: JoinHandle<()>,
    root: TempDir,
}

impl SshRuntime {
    fn start(app: PathBuf) -> io::Result<Self> {
        let root = private_tempdir("vm-", Path::new("/tmp"))?;
        let client = Client::new(app)?;
        let prompts = tokio::spawn({
            let client = client.clone();
            async move {
                while let Some(request) = client.next_prompt().await {
                    request.respond(None);
                }
            }
        });
        Ok(Self {
            client,
            prompts,
            root,
        })
    }

    async fn connect(&self, spec: vela_core::ConnectionSpec) -> Result<Session, vela_core::Error> {
        // Each attempt gets its own path: different saved servers may share
        // the same host, port and user but have different authentication.
        let directory = private_tempdir("s", self.root.path())?.keep();
        struct CancelConnect(ConnectCancel);
        impl Drop for CancelConnect {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }

        let cancel = CancelConnect(ConnectCancel::new());
        self.client
            .connect_in_control_dir(spec, directory, cancel.0.clone())
            .await
    }
}

impl Drop for SshRuntime {
    fn drop(&mut self) {
        self.prompts.abort();
    }
}

struct HeadlessRuntime {
    executor: Executor<HeadlessBackend>,
    settings: Settings,
    known_hosts: Option<PathBuf>,
    ssh: Option<SshRuntime>,
    launch_deadline: Option<Instant>,
}

enum HeadlessResult {
    Done(ToolResponse),
    OpenVela(PolicyDecision),
}

impl HeadlessRuntime {
    fn load(config_dir: Option<&Path>) -> io::Result<Self> {
        let dir = match config_dir {
            Some(dir) => std::path::absolute(dir)?,
            None => vela_core::config::default_directory()?,
        };
        let servers = vela_core::config::load(&dir)?;
        let settings = Settings::load(&dir)?;
        let config = config::load(&dir)?;
        let rollback = Arc::new(RollbackStore::new(
            dir.join(ROLLBACK_DIR_NAME),
            config.rollback.clone(),
        )?);
        let known_hosts = config_dir.map(|_| dir.join("known_hosts"));
        let backend = HeadlessBackend {
            servers,
            sessions: HashMap::new(),
            config_dir: dir,
        };
        Ok(Self {
            executor: Executor::new(backend, config, rollback),
            settings,
            known_hosts,
            ssh: None,
            launch_deadline: None,
        })
    }

    async fn call(&mut self, tool: ToolCall, app: &Path, can_launch: bool) -> HeadlessResult {
        match self.prepare(&tool, app, can_launch).await {
            Ok(()) => {}
            Err(result) => return result,
        }
        let result = tool.execute(&self.executor, ApprovalMode::Normal).await;
        match result {
            Ok(ToolOutcome::NeedsApproval { decision, .. })
                if decision.requires_launch_vela && can_launch =>
            {
                HeadlessResult::OpenVela(decision)
            }
            Ok(ToolOutcome::NeedsApproval { .. }) => closed_for_approval(),
            result => HeadlessResult::Done(ToolResponse::from_outcome(result)),
        }
    }

    async fn prepare(
        &mut self,
        tool: &ToolCall,
        app: &Path,
        can_launch: bool,
    ) -> Result<(), HeadlessResult> {
        let Some(id) = tool.session_server() else {
            return Ok(());
        };
        let action = McpAction::ConnectOnDemand {
            server_id: id.to_owned(),
        };
        let context = self.executor.context(id, false);
        let decision = self.executor.policy.evaluate(&action, &context);
        let config = self.executor.policy.config();
        let backend = &mut self.executor.backend;
        let server = backend.servers.iter().find(|server| server.id == id);
        let availability = config
            .servers
            .get(id)
            .map_or(ServerAvailability::Disabled, |policy| policy.availability);
        if can_launch
            && config.enabled
            && server.is_some()
            && config.headless_ask == HeadlessAskBehavior::OpenVela
            && (decision.requires_launch_vela || availability == ServerAvailability::ConnectedOnly)
        {
            return Err(HeadlessResult::OpenVela(decision));
        }
        if decision.outcome == Decision::Deny {
            return Err(HeadlessResult::Done(ToolResponse::denied(&decision)));
        }
        if availability != ServerAvailability::Headless {
            return Err(closed_for_approval());
        }
        let server = server
            .ok_or_else(|| HeadlessResult::Done(ToolResponse::error("Servidor desconocido.")))?;
        let mut spec = server.connection_spec();
        let Auth::Agent { agent_socket, .. } = &mut spec.auth else {
            return Err(HeadlessResult::Done(ToolResponse::error(
                "El modo headless requiere un agente SSH y no admite autenticación por contraseña.",
            )));
        };
        if backend
            .sessions
            .get(id)
            .is_some_and(|session| !session.is_closed())
        {
            return Ok(());
        }
        if agent_socket.is_none() {
            *agent_socket = self
                .settings
                .agent_socket
                .clone()
                .or_else(|| vela_core::bitwarden_agent_sockets().into_iter().next());
        }
        spec.known_hosts = self.known_hosts.clone();
        if self.ssh.is_none() {
            self.ssh = Some(SshRuntime::start(app.to_path_buf()).map_err(|_| {
                HeadlessResult::Done(ToolResponse::error(
                    "No se pudo iniciar el cliente SSH aislado.",
                ))
            })?);
        }
        let ssh = self.ssh.as_ref().expect("initialized SSH runtime");
        let session = ssh.connect(spec).await.map_err(|error| {
            let message = match error {
                vela_core::Error::HostKeyRejected { .. }
                | vela_core::Error::HostKeyChanged { .. } => {
                    "La identidad SSH del servidor no es de confianza."
                }
                vela_core::Error::AuthFailed { .. } => "Falló la autenticación con el agente SSH.",
                _ => "No se pudo establecer la sesión SSH.",
            };
            HeadlessResult::Done(ToolResponse::error(message))
        })?;
        backend.sessions.insert(id.to_owned(), session);
        Ok(())
    }

    async fn shutdown(&mut self) {
        if let Some(ssh) = &self.ssh {
            ssh.client.shutdown().await;
        }
        self.executor.backend.sessions.clear();
        self.ssh = None;
    }
}

fn closed_for_approval() -> HeadlessResult {
    HeadlessResult::Done(ToolResponse::denied(&PolicyDecision {
        outcome: Decision::Deny,
        reason: "Se requiere aprobación pero no hay interfaz disponible.".into(),
        matched_rule: "headless_fail_closed".into(),
        warnings: Vec::new(),
        requires_launch_vela: false,
    }))
}

enum Mode {
    Unselected,
    Headless(Box<HeadlessRuntime>),
    Handoff(Box<HeadlessRuntime>, IpcClient),
    Bridge(IpcClient),
    Closed,
}

struct Runtime {
    options: RuntimeOptions,
    app: PathBuf,
    launcher: Box<dyn VelaLauncher>,
    mode: Mutex<Mode>,
}

/// Selecciona el transporte en la primera llamada y conserva el modo puente
/// tras un handoff. Sus clones comparten conexiones y configuración.
#[derive(Clone)]
pub struct McpRuntime(Arc<Runtime>);

impl McpRuntime {
    pub fn new(options: RuntimeOptions, app: PathBuf, launcher: impl VelaLauncher) -> Self {
        Self(Arc::new(Runtime {
            options,
            app,
            launcher: Box::new(launcher),
            mode: Mutex::new(Mode::Unselected),
        }))
    }

    pub async fn shutdown(&self) {
        let mut mode = self.0.mode.lock().await;
        if let Mode::Headless(headless) | Mode::Handoff(headless, _) = &mut *mode {
            headless.shutdown().await;
        }
        *mode = Mode::Closed;
    }
}

impl ToolHandler for McpRuntime {
    async fn call(&self, context: CallContext, tool: ToolCall) -> Result<ToolResponse, RpcError> {
        self.0.call(context, tool).await
    }
}

impl Runtime {
    async fn call(&self, context: CallContext, tool: ToolCall) -> Result<ToolResponse, RpcError> {
        let mut mode = self.mode.lock().await;
        loop {
            match &mut *mode {
                Mode::Unselected => match self.select_mode(&context.client).await {
                    Ok(selected) => *mode = selected,
                    Err(response) => return Ok(response),
                },
                Mode::Headless(headless) => {
                    match headless
                        .call(tool.clone(), &self.app, !self.options.force_headless)
                        .await
                    {
                        HeadlessResult::Done(response) => return Ok(response),
                        HeadlessResult::OpenVela(decision) => {
                            context.progress.report(Progress {
                                progress: 1,
                                total: None,
                                message: "Abriendo Vela para solicitar aprobación.".into(),
                            });
                            let client = match self.open_vela(&context.client, &mut headless.launch_deadline).await {
                                Ok(client) => client,
                                Err(()) => return Ok(ToolResponse::error(serde_json::json!({
                                    "reason": "No se pudo conectar con Vela para solicitar aprobación.",
                                    "matched_rule": decision.matched_rule,
                                }).to_string())),
                            };
                            let Mode::Headless(headless) =
                                std::mem::replace(&mut *mode, Mode::Closed)
                            else {
                                unreachable!()
                            };
                            *mode = Mode::Handoff(headless, client);
                        }
                    }
                }
                Mode::Handoff(headless, client) => {
                    // Keep both sides in Mode across the await so cancellation
                    // cannot expose IPC before the local SSH worker is reaped.
                    headless.shutdown().await;
                    *mode = Mode::Bridge(client.clone());
                }
                Mode::Bridge(client) => {
                    let client = client.clone();
                    drop(mode);
                    return client.call(tool, context.progress).await;
                }
                Mode::Closed => return Ok(ToolResponse::error("El runtime MCP se ha cerrado.")),
            }
        }
    }

    async fn select_mode(&self, client: &ClientInfo) -> Result<Mode, ToolResponse> {
        if !self.options.force_headless {
            match IpcClient::connect(&self.options.socket_path, client).await {
                Ok(client) => return Ok(Mode::Bridge(client)),
                Err(error) if absent(&error) => {}
                Err(_) => {
                    return Err(ToolResponse::error(
                        "No se pudo verificar la conexión IPC con Vela.",
                    ));
                }
            }
        }
        let headless = HeadlessRuntime::load(self.options.config_dir.as_deref()).map_err(|_| {
            ToolResponse::error("No se pudo cargar la configuración local de Vela.")
        })?;
        Ok(Mode::Headless(Box::new(headless)))
    }

    async fn open_vela(
        &self,
        client: &ClientInfo,
        launch_deadline: &mut Option<Instant>,
    ) -> Result<IpcClient, ()> {
        let path = &self.options.socket_path;
        if let Some(deadline) = *launch_deadline
            && deadline <= Instant::now()
        {
            *launch_deadline = None;
        }
        let deadline = match *launch_deadline {
            Some(deadline) => deadline,
            None => {
                match IpcClient::connect(path, client).await {
                    Ok(client) => return Ok(client),
                    Err(error) if absent(&error) => {}
                    Err(_) => return Err(()),
                }
                self.launcher.launch(&self.options).map_err(|_| ())?;
                let deadline = Instant::now() + LAUNCH_TIMEOUT;
                *launch_deadline = Some(deadline);
                deadline
            }
        };
        let mut delay = Duration::from_millis(25);
        let result = timeout_at(deadline, async {
            loop {
                match IpcClient::connect(path, client).await {
                    Ok(client) => break Ok(client),
                    Err(error)
                        if absent(&error) || error.kind() == io::ErrorKind::PermissionDenied => {}
                    Err(_) => break Err(()),
                }
                sleep(delay).await;
                delay = (delay * 2).min(Duration::from_millis(200));
            }
        })
        .await;
        *launch_deadline = None;
        result.map_err(|_| ())?
    }
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}
