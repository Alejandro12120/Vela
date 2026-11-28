use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};

use tokio::sync::{mpsc, oneshot, watch};
use vela_core::{Session, edit::Edit};
use vela_mcp::{
    EvalContext, JitGrants, McpAction, McpConfig, PolicyEngine, ServerIdentity,
    config::{Decision, ScopedRule, ServerAvailability},
    executor::{ApprovalMode, ApprovalPreview, Executor, McpBackend, ToolOutcome},
    ipc::IpcServer,
    protocol::{CallContext, ClientInfo, Progress, RpcError, ToolHandler, ToolResponse},
    rollback::{ROLLBACK_DIR_NAME, RollbackStore, restore_snapshot},
    tools::ToolCall,
};

const MAX_PENDING: usize = 32;
const MAX_HISTORY: usize = 200;
const SESSION_DURATION: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Auto,
    Approved,
    Blocked,
    Denied,
    Expired,
    Cancelled,
    Error,
    Reverted,
}

#[derive(Clone)]
pub(super) struct Activity {
    pub id: u64,
    pub time: SystemTime,
    pub agent: String,
    pub server: String,
    pub server_id: Option<String>,
    pub identity: Option<ServerIdentity>,
    pub operation: String,
    pub target: String,
    pub status: Status,
    pub detail: String,
    pub backup_id: Option<String>,
    pub restoring: bool,
}

pub(super) struct Approval {
    pub id: u64,
    pub agent: String,
    pub server: String,
    pub preview: ApprovalPreview,
    pub deadline: Instant,
    pub rollback: bool,
}

#[derive(Debug)]
pub(super) enum Answer {
    Once,
    Session,
    Save,
    Deny(String),
}

pub(super) type Reply<T> = oneshot::Sender<T>;

pub(super) enum UiRequest {
    Backend(Reply<Backend>),
    Connect(String, Reply<Option<Session>>),
    Approval(Arc<Approval>),
    Refresh(String),
    SaveRule(String, ScopedRule, Reply<io::Result<()>>),
}

#[derive(Clone, Default)]
pub(super) struct Backend {
    pub servers: Vec<(String, ServerIdentity)>,
    pub labels: HashMap<String, String>,
    pub sessions: HashMap<String, Session>,
    pub edits: Vec<Edit>,
    pub context: Option<(String, String)>,
    pub dir: PathBuf,
    pub policy: Option<(Broker, u64)>,
}

impl McpBackend for Backend {
    fn list_servers(&self) -> Vec<(String, ServerIdentity)> {
        self.servers.clone()
    }

    fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id).filter(|session| !session.is_closed())
    }

    fn edits_dirty(&self, id: &str, canonical: &str) -> bool {
        self.edits.iter().any(|row| {
            row.target.server_id == id
                && row.canonical_remote_path() == Path::new(canonical)
                && row.has_local_changes()
        })
    }

    fn ui_context(&self) -> Option<(String, String)> {
        self.context.clone()
    }

    fn live_policy(&self) -> Option<PolicyEngine> {
        self.policy
            .as_ref()
            .map(|(broker, epoch)| broker.policy(*epoch))
    }

    fn vela_config_dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
}

#[derive(Default)]
pub(super) struct State {
    pub config: McpConfig,
    pub agents: HashMap<u64, String>,
    pub history: VecDeque<Activity>,
    pub jit: JitGrants,
    pub conflict: Option<(Instant, String)>,
    pending: VecDeque<(Arc<Approval>, Reply<Answer>)>,
    serial: u64,
    epoch: u64,
}

impl State {
    pub fn pending(&self) -> Vec<Arc<Approval>> {
        self.pending.iter().map(|(item, _)| item.clone()).collect()
    }

    fn next_id(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }
}

#[derive(Clone)]
pub(super) struct Broker {
    state: Arc<Mutex<State>>,
    updates: watch::Sender<()>,
    cuts: watch::Sender<()>,
}

impl Default for Broker {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            updates: watch::channel(()).0,
            cuts: watch::channel(()).0,
        }
    }
}

impl Broker {
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn policy(&self, epoch: u64) -> PolicyEngine {
        let state = self.state();
        let mut config = state.config.clone();
        config.enabled &= epoch == state.epoch;
        PolicyEngine::with_jit(config, state.jit.clone())
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.updates.subscribe()
    }

    pub fn changed(&self) {
        self.updates.send_replace(());
    }

    pub fn configure(&self, config: McpConfig) {
        let mut state = self.state();
        let disable = state.config.enabled && !config.enabled;
        state.config = config;
        if disable {
            self.cut(&mut state);
        }
        drop(state);
        self.changed();
    }

    #[cfg(any(test, feature = "dev"))]
    pub fn preview_approval(&self, mut item: Approval) -> (u64, oneshot::Receiver<Answer>) {
        let (reply, receiver) = oneshot::channel();
        let mut state = self.state();
        item.id = state.next_id();
        let id = item.id;
        state.pending.push_back((Arc::new(item), reply));
        drop(state);
        self.changed();
        (id, receiver)
    }

    pub fn answer(&self, id: u64, answer: Answer) {
        let mut state = self.state();
        let Some(index) = state.pending.iter().position(|(item, _)| item.id == id) else {
            return;
        };
        if state.pending[index].0.deadline > Instant::now() {
            let (_, reply) = state.pending.remove(index).unwrap();
            let _ = reply.send(answer);
        }
        drop(state);
        self.changed();
    }

    pub fn grant_profile(&self, id: &str, duration: Duration) {
        let mut state = self.state();
        if state.config.enabled
            && state
                .config
                .servers
                .get(id)
                .is_some_and(|server| server.availability != ServerAvailability::Disabled)
        {
            state
                .jit
                .grant_profile(id, "development", SystemTime::now() + duration);
        }
        drop(state);
        self.changed();
    }

    pub fn revoke(&self, id: &str) {
        self.state().jit.revoke_server(id);
        self.changed();
    }

    fn cut(&self, state: &mut State) {
        state.epoch += 1;
        state.jit.revoke_all();
        state.pending.clear();
        self.cuts.send_replace(());
    }

    pub fn kill(&self) {
        self.cut(&mut self.state());
        self.system_event(
            "Disconnect Agents",
            Status::Cancelled,
            "Disconnected agents and revoked temporary access.",
        );
    }

    fn system_event(&self, operation: &str, status: Status, detail: &str) {
        let id = self.state().next_id();
        self.record(Activity {
            id,
            time: SystemTime::now(),
            agent: "Vela".into(),
            server: String::new(),
            server_id: None,
            identity: None,
            operation: operation.into(),
            target: String::new(),
            status,
            detail: detail.into(),
            backup_id: None,
            restoring: false,
        });
    }

    pub fn record(&self, mut activity: Activity) {
        activity.target = activity.target.chars().take(4096).collect();
        activity.detail = activity.detail.chars().take(500).collect();
        let mut state = self.state();
        if activity.status == Status::Blocked
            && activity.detail == "Some edits haven’t been uploaded."
        {
            state.conflict = Some((
                Instant::now() + Duration::from_secs(8),
                format!(
                    "{} tried to modify {}. Upload your changes from Edits first.",
                    activity.agent, activity.target
                ),
            ));
        }
        state.history.push_front(activity);
        state.history.truncate(MAX_HISTORY);
        drop(state);
        self.changed();
    }

    pub async fn restore(&self, activity: &Activity, backend: Backend) -> Result<(), String> {
        let Some((id, backup)) = activity.server_id.as_ref().zip(activity.backup_id.as_ref())
        else {
            return Err("There’s no backup for this operation.".into());
        };
        let session = backend
            .session(id)
            .ok_or("Connect to the server to restore the backup.")?;
        if !backend.servers.iter().any(|(server_id, identity)| {
            server_id == id && Some(identity) == activity.identity.as_ref()
        }) {
            return Err("The server’s identity has changed.".into());
        }
        let limits = self.state().config.rollback.clone();
        let store = RollbackStore::new(backend.dir.join(ROLLBACK_DIR_NAME), limits)
            .map_err(|e| e.to_string())?;
        let now = SystemTime::now();
        let (entry, _) = store
            .get_snapshot(backup, now)
            .map_err(|e| e.to_string())?
            .ok_or("The backup expired or was deleted.")?;
        if entry.server_id != *id || backend.edits_dirty(id, &entry.remote_path) {
            return Err(
                "Some edits haven’t been uploaded, or the backup belongs to another server.".into(),
            );
        }
        let path = PathBuf::from(&entry.remote_path);
        // The original canonical destination may have been replaced with a symlink.
        let parent = path
            .parent()
            .ok_or("The backup destination has no parent folder.")?;
        let canonical_parent = session
            .realpath(parent.to_owned())
            .await
            .map_err(|e| e.to_string())?;
        if canonical_parent != parent
            || session
                .stat(path)
                .await
                .is_ok_and(|entry| entry.kind == vela_core::EntryKind::Symlink)
        {
            return Err("The backup destination now points to a different path.".into());
        }
        restore_snapshot(session, &store, backup, now)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn restored(&self, activity: Activity, result: Result<(), String>) {
        let status = if result.is_ok() {
            Status::Reverted
        } else {
            Status::Error
        };
        let mut state = self.state();
        if let Some(row) = state.history.iter_mut().find(|row| row.id == activity.id) {
            row.restoring = false;
            if result.is_ok() {
                row.status = Status::Reverted;
            }
        }
        let id = state.next_id();
        drop(state);
        self.record(Activity {
            id,
            time: SystemTime::now(),
            agent: "Vela".into(),
            operation: "Rollback".into(),
            status,
            detail: result.err().unwrap_or_else(|| "Backup restored.".into()),
            backup_id: None,
            ..activity
        });
    }
}

pub(super) struct Worker {
    enabled: watch::Sender<bool>,
    done: oneshot::Receiver<()>,
    pub runtime: tokio::runtime::Handle,
}

impl Worker {
    pub fn start(
        broker: Broker,
        path: PathBuf,
        ui: mpsc::UnboundedSender<UiRequest>,
    ) -> io::Result<Self> {
        let (enabled, control) = watch::channel(true);
        let (finished, done) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("vela-mcp".into())
            .spawn(move || {
                runtime.block_on(Handler { broker, ui }.listen(path, control));
                drop(runtime);
                let _ = finished.send(());
            })?;
        Ok(Self {
            enabled,
            done,
            runtime: handle,
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.send_if_modified(|previous| {
            let changed = *previous != enabled;
            *previous = enabled;
            changed
        });
    }

    pub async fn shutdown(self) {
        drop(self.enabled);
        let _ = self.done.await;
    }
}

#[derive(Clone)]
struct Handler {
    broker: Broker,
    ui: mpsc::UnboundedSender<UiRequest>,
}

impl Handler {
    async fn listen(self, path: PathBuf, mut enabled: watch::Receiver<bool>) {
        loop {
            // A closed watch can still satisfy wait_for with its last value.
            if enabled.has_changed().is_err() || enabled.wait_for(|value| *value).await.is_err() {
                return;
            }
            let cuts = self.broker.cuts.subscribe();
            let result = match IpcServer::bind(path.clone()).await {
                Ok(server) => {
                    server
                        .run_with_client_reset(
                            self.clone(),
                            async {
                                let _ = enabled.wait_for(|value| !*value).await;
                            },
                            cuts,
                        )
                        .await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                self.broker
                    .system_event("MCP Server", Status::Error, &error.to_string());
                if enabled.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    async fn ui<T>(&self, request: impl FnOnce(Reply<T>) -> UiRequest) -> Result<T, ToolResponse> {
        let (reply, receive) = oneshot::channel();
        let _ = self.ui.send(request(reply));
        receive
            .await
            .map_err(|_| ToolResponse::error("The workspace closed."))
    }

    async fn ask(
        &self,
        call: &Call,
        preview: ApprovalPreview,
        progress: &mut CallProgress<'_>,
    ) -> Result<Answer, (Status, ToolResponse)> {
        let (reply, mut receive) = oneshot::channel();
        let item = {
            let mut state = self.broker.state();
            if !state.config.enabled || state.epoch != call.epoch {
                return Err((Status::Cancelled, ToolResponse::error("Request canceled.")));
            }
            if state.pending.len() >= MAX_PENDING {
                return Err((
                    Status::Error,
                    ToolResponse::error("The approval queue is full."),
                ));
            }
            let id = state.next_id();
            let item = Arc::new(Approval {
                id,
                agent: call.row().agent.clone(),
                server: call.row().server.clone(),
                deadline: Instant::now() + Duration::from_secs(state.config.approval_timeout_secs),
                rollback: state.config.rollback.enabled,
                preview,
            });
            state.pending.push_back((item.clone(), reply));
            item
        };
        let _pending = Pending(self.broker.clone(), item.id);
        self.broker.changed();
        let _ = self.ui.send(UiRequest::Approval(item.clone()));
        let answer = tokio::select! {
            biased;
            answer = progress.wait("Waiting for approval in Vela.", &mut receive) => {
                answer.map_err(|_| (Status::Cancelled, ToolResponse::error("Request canceled.")))?
            }
            _ = tokio::time::sleep_until(item.deadline.into()) => {
                return Err((Status::Expired, ToolResponse::error("The approval request expired.")));
            }
        };
        if let Answer::Deny(reason) = answer {
            let message = if reason.trim().is_empty() {
                "The user denied the operation.".into()
            } else {
                format!("The user denied the operation. {}", reason.trim())
            };
            Err((Status::Denied, ToolResponse::error(message)))
        } else {
            Ok(answer)
        }
    }

    async fn execute(
        &self,
        context: &CallContext,
        tool: ToolCall,
        call: &mut Call,
    ) -> Result<ToolResponse, (Status, ToolResponse)> {
        let mut progress = CallProgress(context, 0);
        let mut approved = None;
        loop {
            let mut backend = self.ui(UiRequest::Backend).await.map_err(error)?;
            backend.policy = Some((self.broker.clone(), call.epoch));
            let policy = self.broker.policy(call.epoch);
            if !policy.config().enabled {
                return Err((
                    Status::Cancelled,
                    ToolResponse::error("The MCP server is off or the agents were disconnected."),
                ));
            }
            if let Some(id) = tool.session_server() {
                let identity = backend
                    .servers
                    .iter()
                    .find(|(key, _)| key == id)
                    .map(|(_, identity)| identity.clone());
                if let Some(previous) = &call.row().identity
                    && Some(previous) != identity.as_ref()
                {
                    return Err((
                        Status::Blocked,
                        ToolResponse::error("The server’s identity has changed."),
                    ));
                }
                call.row_mut().identity = identity;
                call.row_mut().server = backend
                    .labels
                    .get(id)
                    .cloned()
                    .ok_or_else(|| error(ToolResponse::error("Unknown server.")))?;
                if backend.session(id).is_none() {
                    let decision = policy.evaluate(
                        &McpAction::ConnectOnDemand {
                            server_id: id.into(),
                        },
                        &EvalContext::default(),
                    );
                    if decision.outcome == Decision::Deny {
                        return Err((Status::Blocked, ToolResponse::denied(&decision)));
                    }
                    let availability = policy
                        .config()
                        .servers
                        .get(id)
                        .map(|item| item.availability);
                    if availability != Some(ServerAvailability::OnDemandAsk) || approved.is_some() {
                        return Err(error(ToolResponse::error(
                            "The server has no active connection.",
                        )));
                    }
                    self.ask(
                        call,
                        ApprovalPreview {
                            operation: format!("Connect via SSH · {}", tool.name()),
                            server_id: id.into(),
                            paths: vec![display_target(&tool)],
                            agent_reason: tool.arguments()["reason"].as_str().map(str::to_owned),
                            reason: "This operation requires opening an SSH connection.".into(),
                            ..ApprovalPreview::default()
                        },
                        &mut progress,
                    )
                    .await?;
                    let current = self.broker.policy(call.epoch);
                    if !current.config().enabled
                        || !current.config().servers.get(id).is_some_and(|item| {
                            item.availability == ServerAvailability::OnDemandAsk
                        })
                    {
                        return Err((
                            Status::Blocked,
                            ToolResponse::error("The connection policy has changed."),
                        ));
                    }
                    let session = progress
                        .wait(
                            "Opening an SSH connection.",
                            self.ui(|reply| UiRequest::Connect(id.into(), reply)),
                        )
                        .await
                        .map_err(error)?;
                    if session.is_none() {
                        return Err(error(ToolResponse::error("Couldn’t connect via SSH.")));
                    }
                    call.row_mut().status = Status::Approved;
                    continue;
                }
            }
            let store = RollbackStore::new(
                backend.dir.join(ROLLBACK_DIR_NAME),
                policy.config().rollback.clone(),
            )
            .map_err(|_| {
                error(ToolResponse::error(
                    "Couldn’t open the rollback backup store.",
                ))
            })?;
            let mut executor = Executor::with_policy(backend, policy, Arc::new(store));
            let mode = if let Some((preview, _)) = &approved {
                executor = executor.with_approved_preview(ApprovalPreview::clone(preview));
                ApprovalMode::ApprovedOnce
            } else {
                ApprovalMode::Normal
            };
            match tool.clone().execute(&executor, mode).await {
                Ok(ToolOutcome::NeedsApproval { preview, .. }) => {
                    call.row_mut().target = preview
                        .command
                        .clone()
                        .unwrap_or_else(|| preview.paths.join(" → "));
                    let answer = self.ask(call, (*preview).clone(), &mut progress).await?;
                    approved = Some((*preview, answer));
                }
                Ok(ToolOutcome::Denied(decision)) => {
                    call.row_mut().detail = decision.reason.clone();
                    return Err((Status::Blocked, ToolResponse::denied(&decision)));
                }
                Ok(ToolOutcome::Executed(value)) => {
                    call.row_mut().backup_id = value["backup_id"].as_str().map(str::to_owned);
                    if let Some(path) = value["path"].as_str() {
                        call.row_mut().target = path.into();
                    }
                    if let Some(exit) = value["exit_code"].as_i64() {
                        call.row_mut().detail = format!("Exit code {exit}");
                    }
                    if let Some((preview, answer)) = approved {
                        call.row_mut().status = Status::Approved;
                        if let Some(rule) = preview.scoped_rule() {
                            match answer {
                                Answer::Once | Answer::Deny(_) => {}
                                Answer::Session => {
                                    let mut state = self.broker.state();
                                    if state.epoch == call.epoch {
                                        let now = SystemTime::now();
                                        state.jit.prune_expired(now);
                                        state.jit.grant_scoped_allow(
                                            preview.server_id,
                                            rule.operation,
                                            rule.scope,
                                            now + SESSION_DURATION,
                                        );
                                    }
                                }
                                Answer::Save => {
                                    let saved = self
                                        .ui(|reply| {
                                            UiRequest::SaveRule(preview.server_id, rule, reply)
                                        })
                                        .await
                                        .is_ok_and(|result| result.is_ok());
                                    if !saved {
                                        call.row_mut().detail =
                                            "The operation completed. Vela couldn’t save the rule."
                                                .into();
                                    }
                                }
                            }
                        }
                    }
                    if let Some(id) = tool.session_server()
                        && matches!(
                            tool.name(),
                            "fs_write"
                                | "fs_patch"
                                | "fs_mkdir"
                                | "fs_delete"
                                | "fs_move"
                                | "fs_chmod"
                                | "transfer_upload"
                        )
                    {
                        let _ = self.ui.send(UiRequest::Refresh(id.into()));
                    }
                    return Ok(ToolResponse::executed(value));
                }
                Err(error) => return Err((Status::Error, ToolResponse::from_outcome(Err(error)))),
            }
        }
    }
}

fn error(response: ToolResponse) -> (Status, ToolResponse) {
    (Status::Error, response)
}

fn display_target(tool: &ToolCall) -> String {
    let args = tool.arguments();
    ["command", "path", "from", "to", "remote_path", "local_path"]
        .into_iter()
        .filter_map(|key| args[key].as_str())
        .collect::<Vec<_>>()
        .join(" → ")
}

struct CallProgress<'a>(&'a CallContext, u64);

impl CallProgress<'_> {
    async fn wait<T>(&mut self, message: &str, future: impl Future<Output = T>) -> T {
        let mut ticks = tokio::time::interval(Duration::from_secs(5));
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                _ = ticks.tick() => {
                    self.1 += 1;
                    self.0.progress.report(Progress {
                        progress: self.1, total: None, message: message.into(),
                    });
                }
                result = &mut future => return result,
            }
        }
    }
}

struct Agent(Broker, u64);

impl Drop for Agent {
    fn drop(&mut self) {
        self.0.state().agents.remove(&self.1);
        self.0.changed();
    }
}

struct Pending(Broker, u64);

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.state().pending.retain(|(item, _)| item.id != self.1);
        self.0.changed();
    }
}

struct Call {
    broker: Broker,
    epoch: u64,
    activity: Option<Activity>,
}

impl Call {
    fn row(&self) -> &Activity {
        self.activity.as_ref().unwrap()
    }

    fn row_mut(&mut self) -> &mut Activity {
        self.activity.as_mut().unwrap()
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if let Some(mut row) = self.activity.take() {
            row.status = Status::Cancelled;
            row.detail = "The agent closed or canceled the request.".into();
            self.broker.record(row);
        }
    }
}

impl ToolHandler for Handler {
    fn client_connected(&self, client: &ClientInfo) -> Box<dyn Send> {
        let mut state = self.broker.state();
        let id = state.next_id();
        state
            .agents
            .insert(id, client.name.chars().take(80).collect());
        drop(state);
        self.broker.changed();
        Box::new(Agent(self.broker.clone(), id))
    }

    async fn call(&self, context: CallContext, tool: ToolCall) -> Result<ToolResponse, RpcError> {
        let (id, epoch) = {
            let mut state = self.broker.state();
            (state.next_id(), state.epoch)
        };
        let mut call = Call {
            broker: self.broker.clone(),
            epoch,
            activity: Some(Activity {
                id,
                time: SystemTime::now(),
                agent: context.client.name.chars().take(80).collect(),
                server: String::new(),
                server_id: tool.session_server().map(str::to_owned),
                identity: None,
                operation: tool.name().into(),
                target: display_target(&tool),
                status: Status::Auto,
                detail: String::new(),
                backup_id: None,
                restoring: false,
            }),
        };
        let result = self.execute(&context, tool, &mut call).await;
        let mut row = call.activity.take().unwrap();
        let response = match result {
            Ok(response) => response,
            Err((status, response)) => {
                row.status = status;
                if row.detail.is_empty()
                    && let Some(content) = response.content.first()
                {
                    row.detail = content.text.clone();
                }
                response
            }
        };
        row.time = SystemTime::now();
        self.broker.record(row);
        Ok(response)
    }
}

#[cfg(test)]
mod tests;
