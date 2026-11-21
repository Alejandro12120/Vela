#[path = "edits_view.rs"]
mod edits_view;
#[path = "settings_view.rs"]
mod settings_view;

use edits_view::{EditDialog, Edits};
use vscp_core::{
    edit::{Index, Target},
    settings::Settings,
};

use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    PathPromptOptions, Render, Window, actions, div, prelude::*, px,
};
use vscp_core::{
    Auth, Client, Error, Session,
    agent::{self, AgentError, AgentKey},
    askpass,
    config::{self, Server},
};

use crate::{
    components::{
        button::{Variant, button},
        modal,
        text_input::TextInput,
    },
    dev,
    file_workspace::Panes,
    form::{Errors, Field, Form},
    pane::{PaneMemory, icon_button},
    pane_icons::Icon,
    theme::Theme,
};

actions!(
    workspace,
    [Next, Previous, DismissMenu, ToggleSidebar, OpenSettings]
);

pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, Some("Workspace")),
        KeyBinding::new("tab", Next, Some("Workspace")),
        KeyBinding::new("shift-tab", Previous, Some("Workspace")),
        KeyBinding::new("escape", DismissMenu, Some("Workspace")),
        KeyBinding::new("cmd-s", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("tab", modal::Next, Some("Dialog")),
        KeyBinding::new("shift-tab", modal::Previous, Some("Dialog")),
        KeyBinding::new("enter", modal::Confirm, Some("Dialog")),
        KeyBinding::new("escape", modal::Cancel, Some("Dialog")),
    ]);
}

pub enum Connection {
    Connecting,
    Connected { session: Session, panes: Panes },
    Disconnecting,
    Error(Error),
}

struct Prompt {
    request: askpass::Request,
    input: Option<Entity<TextInput>>,
}

enum Dialog {
    Form(Box<Form>),
    Delete(String),
    Prompt(Prompt),
    Edit(EditDialog),
    Settings,
}

pub struct Workspace {
    pub servers: Vec<Server>,
    pub selected: Option<String>,
    pub connections: HashMap<String, Connection>,
    pub row_menu: Option<String>,
    pub dir: Option<PathBuf>,
    pub saving: bool,
    /// A server row is being dragged. Reset once the drag ends.
    pub dragging_server: bool,
    key_requests: u64,
    edits: Edits,
    settings: Settings,
    settings_error: Option<String>,
    loading: bool,
    sidebar_visible: Rc<Cell<bool>>,
    pane_memory: HashMap<String, PaneMemory>,
    quitting: bool,
    config_error: Option<&'static str>,
    client: Option<Client>,
    agent_socket: String,
    warnings: HashMap<String, String>,
    show_stderr: bool,
    dialog: Option<Dialog>,
    prompts: VecDeque<askpass::Request>,
    focus: FocusHandle,
    dialog_focus: FocusHandle,
    confirm_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    prompt_task: Option<gpui::Task<()>>,
    trace_startup: Option<Instant>,
}

impl Workspace {
    pub fn new(
        client: Option<Client>,
        trace_startup: Option<Instant>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let prompt_task = client.clone().map(|client| {
            cx.spawn_in(window, async move |this, cx| {
                while let Some(request) = client.next_prompt().await {
                    if this
                        .update_in(cx, |this, window, cx| {
                            this.prompts.push_back(request);
                            if matches!(&this.dialog, Some(Dialog::Prompt(prompt)) if !prompt.request.is_pending()) {
                                this.finish_dialog(window, cx);
                            } else {
                                this.show_next_prompt(window, cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        let load = cx.background_executor().spawn(async {
            let dir = dev::config_directory()?;
            let servers = config::load(&dir)?;
            let agent = vscp_core::bitwarden_agent_sockets().into_iter().next();
            let index = dev::edit_root().and_then(|root| Index::load(dir.clone(), root));
            let settings = Settings::load(&dir);
            Ok::<_, std::io::Error>((dir, servers, agent, index, settings))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((dir, servers, agent, index, settings)) => {
                        this.load_edits(index, window, cx);
                        match settings {
                            Ok(settings) => this.settings = settings,
                            Err(error) => this.settings_error = Some(format!("No se pudo leer settings.toml. {error}")),
                        }
                        if dev::edit_view().as_deref() == Some("settings") {
                            this.open_dialog(Dialog::Settings, window, cx);
                        }
                        this.selected = servers.first().map(|server| server.id.clone());
                        this.dir = Some(dir);
                        this.servers = servers;
                        this.agent_socket = agent.map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                        if let Some(name) = dev::autoconnect()
                            && let Some(server) = this.servers.iter().find(|server| server.name == name)
                        {
                            let id = server.id.clone();
                            this.select(&id);
                            this.connect(&id, window, cx);
                        }
                        if let Some(name) = dev::row_menu()
                            && let Some(server) = this.servers.iter().find(|server| server.name == name)
                        {
                            let id = server.id.clone();
                            this.select(&id);
                            this.row_menu = Some(id);
                        }
                        match dev::form() {
                            Some(dev::FormRequest::New) => this.open_form(None, window, cx),
                            Some(dev::FormRequest::Edit(name)) => {
                                let id = this.servers.iter().find(|server| server.name == name).map(|server| server.id.clone());
                                if let Some(id) = id {
                                    this.open_form(Some(&id), window, cx);
                                }
                            }
                            None => {}
                        }
                    }
                    Err(err) => {
                        let message = if err.kind() == std::io::ErrorKind::InvalidData {
                            "El archivo servers.toml contiene errores. No se puede modificar desde la aplicación."
                        } else {
                            "No se pudo leer la configuración de servidores."
                        };
                        this.config_error = Some(message);
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        let executor = cx.background_executor().clone();
        cx.on_app_quit(move |this, cx| {
            let cleanup = (!this.quitting).then(|| crate::pane::finder::quit_cleanup(cx));
            let shutdown = this.shutdown(executor.clone());
            async move {
                shutdown.await;
                if let Some(cleanup) = cleanup {
                    cleanup();
                }
            }
        })
        .detach();

        Self {
            servers: Vec::new(),
            selected: None,
            connections: HashMap::new(),
            row_menu: None,
            dir: None,
            saving: false,
            dragging_server: false,
            key_requests: 0,
            edits: Edits::default(),
            settings: Settings::default(),
            settings_error: None,
            loading: true,
            sidebar_visible: Rc::new(Cell::new(true)),
            pane_memory: HashMap::new(),
            quitting: false,
            config_error: None,
            client,
            agent_socket: String::new(),
            warnings: HashMap::new(),
            show_stderr: false,
            dialog: None,
            prompts: VecDeque::new(),
            focus,
            dialog_focus: cx.focus_handle(),
            confirm_focus: cx.focus_handle(),
            previous_focus: None,
            prompt_task,
            trace_startup,
        }
    }

    fn shutdown(&mut self, executor: gpui::BackgroundExecutor) -> impl Future<Output = ()> + use<> {
        self.dialog = None;
        self.prompt_task.take();
        self.prompts.clear();
        let client = self.client.clone();
        let index = self.edits.shutdown();
        async move {
            if let Some(client) = client {
                client.shutdown().await;
            }
            if let Some(mut index) = index {
                executor
                    .spawn(async move {
                        if let Err(error) = index.cleanup_synced() {
                            eprintln!("vSCP: No se pudo limpiar la caché de ediciones. {error}");
                        }
                    })
                    .await;
            }
        }
    }

    pub fn request_quit(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        crate::pane::finder::begin_quit(cx);
        let shutdown = self.shutdown(cx.background_executor().clone());
        cx.notify();
        cx.spawn_in(window, async move |_, cx| {
            shutdown.await;
            let _ = cx.update(|_, cx| {
                crate::pane::finder::quit_cleanup(cx)();
                cx.quit();
            });
        })
        .detach();
    }

    pub fn select(&mut self, id: &str) {
        if self.selected.as_deref() != Some(id) {
            self.show_stderr = false;
            self.edits.popover = false;
        }
        self.selected = Some(id.to_owned());
        self.row_menu = None;
    }

    pub fn connect(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving
            || self.quitting
            || matches!(
                self.connections.get(id),
                Some(
                    Connection::Connecting
                        | Connection::Connected { .. }
                        | Connection::Disconnecting
                )
            )
        {
            return;
        }
        let Some(server) = self.servers.iter().find(|server| server.id == id).cloned() else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        self.connections.insert(id.into(), Connection::Connecting);
        self.show_stderr = false;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut spec = server.connection_spec();
            dev::isolate_known_hosts(&mut spec);
            let result = async {
                let session = client.connect(spec).await?;
                dev::log(format_args!("connected {}", server.name));
                let path = match server.initial_remote_path.clone() {
                    Some(path) if path.is_absolute() => Ok(path),
                    Some(path) => session.home_dir().await.map(|home| home.join(path)),
                    None => session.home_dir().await,
                };
                match path {
                    Ok(path) => Ok((session, path)),
                    Err(error) => {
                        let _ = session.disconnect().await;
                        Err(error)
                    }
                }
            }.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if matches!(&this.dialog, Some(Dialog::Prompt(prompt)) if !prompt.request.is_pending()) {
                    this.finish_dialog(window, cx);
                }
                let state = match result {
                    Ok((session, path)) => this.connected(&server, session, path, window, cx),
                    Err(error) => {
                        dev::log(format_args!("connection failed for {}", server.name));
                        Connection::Error(error)
                    }
                };
                let connected = if let Connection::Connected { session, .. } = &state {
                    Some(session.clone())
                } else {
                    None
                };
                this.connections.insert(server.id.clone(), state);
                if let Some(session) = connected {
                    this.edits_connected(&server, window, cx);
                    let id = server.id.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        session.closed().await;
                        let _ = this.update_in(cx, |this, _, cx| {
                            if matches!(this.connections.get(&id), Some(Connection::Connected { session: current, .. }) if current.same_connection(&session)) {
                                this.connections.insert(id, Connection::Error(std::io::Error::new(
                                    std::io::ErrorKind::BrokenPipe, "SSH connection closed").into()));
                                cx.notify();
                            }
                        });
                    }).detach();
                }
                cx.notify();
            });
        }).detach();
    }

    fn connected(
        &mut self,
        server: &Server,
        session: Session,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Connection {
        let id = &server.id;
        let memory = self.pane_memory.entry(id.to_owned()).or_default().clone();
        let panes = Panes::new(
            session.clone(),
            server,
            path,
            memory,
            self.sidebar_visible.clone(),
            window,
            cx,
        );
        let edit_server = server.clone();
        cx.subscribe_in(
            panes.remote(),
            window,
            move |this, _, event: &crate::pane::EditFile, window, cx| {
                this.open_edit(Target::new(&edit_server, event.0.clone()), window, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            panes.local(),
            window,
            move |this, pane, event: &crate::pane::OpenLocalFile, window, cx| {
                let path = event.0.clone();
                let editor = this.settings.editor.clone();
                let pane = pane.downgrade();
                let open = cx
                    .background_executor()
                    .spawn(async move { dev::open_file(&path, editor.as_deref()) });
                cx.spawn_in(window, async move |_, cx| {
                    if let Err(error) = open.await {
                        let _ =
                            pane.update(cx, |pane, cx| pane.report_error(error.to_string(), cx));
                    }
                })
                .detach();
            },
        )
        .detach();
        if self.selected.as_deref() == Some(id) && self.dialog.is_none() {
            panes.focus(window, cx);
        }
        Connection::Connected { session, panes }
    }

    pub fn disconnect(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        if !matches!(self.connections.get(id), Some(Connection::Connected { .. })) {
            return;
        }
        let Some(Connection::Connected { session, .. }) = self
            .connections
            .insert(id.into(), Connection::Disconnecting)
        else {
            return;
        };
        let id = id.to_owned();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.disconnect().await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(()) => {
                        this.connections.remove(&id);
                    }
                    Err(error) if this.servers.iter().any(|server| server.id == id) => {
                        this.connections.insert(id, Connection::Error(error));
                    }
                    Err(_) => {
                        this.connections.remove(&id);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn open_form(&mut self, id: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir.is_none() || self.dialog.is_some() || self.saving {
            return;
        }
        let server = id.and_then(|id| self.servers.iter().find(|server| server.id == id));
        let form = Form::new(server, &self.agent_socket, self.dir.as_deref(), cx);
        let socket = form.input(Field::Socket).read(cx).focus_handle(cx);
        self.open_dialog(Dialog::Form(Box::new(form)), window, cx);
        let blur = cx.on_blur(&socket, window, |this, window, cx| {
            if this.editing().is_some_and(|form| form.socket_changed(cx)) {
                this.refresh_keys(window, cx);
            }
        });
        if let Some(form) = self.editing() {
            form.set_blur(blur);
        }
        self.refresh_keys(window, cx);
    }

    /// Asks the agent for its keys without blocking the interface. A newer
    /// request or a closed form makes the answer stale.
    pub fn refresh_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.key_requests += 1;
        let request = self.key_requests;
        let client = self.client.clone();
        let executor = cx.background_executor().clone();
        let Some(form) = self.editing() else { return };
        let Some(socket) = form.begin_listing(request, cx) else {
            return;
        };
        cx.notify();
        let hint = executor.clone();
        cx.spawn_in(window, async move |this, cx| {
            hint.timer(Duration::from_millis(2100)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            let path = if socket.is_empty() {
                executor
                    .spawn(async { vscp_core::bitwarden_agent_sockets().into_iter().next() })
                    .await
            } else {
                Some(PathBuf::from(socket))
            };
            let result = match (client, path) {
                (Some(client), Some(path)) => client.list_agent_keys(path).await,
                _ => Err(AgentError::Unavailable),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                let handles = (0..=result.as_ref().map_or(0, Vec::len))
                    .map(|_| cx.focus_handle())
                    .collect();
                if let Some(Dialog::Form(form)) = &mut this.dialog {
                    form.finish_listing(request, result, handles);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Pins or unpins a server, moving it to the edge of the other group.
    pub fn toggle_pin(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(pinned) = self
            .servers
            .iter()
            .find(|server| server.id == id)
            .map(|server| server.pinned)
        else {
            return;
        };
        self.reorder(cx, |servers| config::set_pinned(servers, id, !pinned));
    }

    /// Drops a dragged server into a group, before `before` or at the group's end.
    pub fn drop_server(
        &mut self,
        id: &str,
        pinned: bool,
        before: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.reorder(cx, |servers| {
            config::move_server(servers, id, pinned, before);
        });
    }

    fn reorder(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Vec<Server>)) {
        let Some(dir) = self.dir.clone() else { return };
        if self.saving || self.quitting {
            return;
        }
        let previous = self.servers.clone();
        let mut servers = previous.clone();
        change(&mut servers);
        if servers == previous {
            return;
        }
        self.servers = servers.clone();
        self.row_menu = None;
        self.saving = true;
        cx.notify();
        let save = {
            let servers = servers.clone();
            cx.background_executor()
                .spawn(async move { config::save(&dir, servers) })
        };
        cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                if result.is_ok() {
                    this.config_error = None;
                } else {
                    if this.servers == servers {
                        this.servers = previous;
                    }
                    this.config_error = Some("No se pudo guardar servers.toml.");
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn open_delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_none() && !self.saving {
            self.open_dialog(Dialog::Delete(id), window, cx);
        }
    }

    pub fn editing(&mut self) -> Option<&mut Form> {
        match &mut self.dialog {
            Some(Dialog::Form(form)) if !self.saving => Some(form),
            _ => None,
        }
    }

    pub fn choose_identity(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.editing().is_none() {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Elegir clave pública".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, _, cx| {
                if let Some(form) = this.editing() {
                    match result {
                        Ok(Ok(Some(paths))) => {
                            if let Some(path) = paths.into_iter().next() {
                                form.pick_file(path);
                            }
                        }
                        Ok(Ok(None)) => {}
                        _ => form.message = Some("No se pudo abrir el selector de archivos."),
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus = if self.row_menu.is_some() {
            None
        } else {
            window.focused(cx)
        };
        self.row_menu = None;
        let focus = match &dialog {
            Dialog::Form(form) => Some(form.input(Field::Name)),
            Dialog::Prompt(prompt) => prompt.input.as_ref(),
            Dialog::Delete(_) | Dialog::Edit(_) | Dialog::Settings => None,
        };
        let focus = focus
            .map(|input| input.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.dialog_focus.clone());
        self.dialog = Some(dialog);
        focus.focus(window, cx);
        cx.notify();
    }

    fn finish_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        self.previous_focus
            .take()
            .unwrap_or_else(|| self.focus.clone())
            .focus(window, cx);
        self.show_next_prompt(window, cx);
        cx.notify();
    }

    fn show_next_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        while let Some(request) = self.prompts.pop_front() {
            if request.is_pending() {
                let input = match &request.kind {
                    askpass::Kind::HostKeyConfirm { .. } => None,
                    kind => {
                        let (label, secure) = match kind {
                            askpass::Kind::Password => ("Contraseña", true),
                            askpass::Kind::Passphrase => ("Frase de paso", true),
                            _ => ("Respuesta", false),
                        };
                        Some(cx.new(|cx| TextInput::new("", label, "", secure, cx)))
                    }
                };
                self.open_dialog(Dialog::Prompt(Prompt { request, input }), window, cx);
                return;
            }
        }
    }

    fn cancel_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.saving {
            self.finish_dialog(window, cx);
        }
    }

    fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        match self.dialog.as_mut() {
            Some(Dialog::Prompt(prompt)) => {
                if prompt
                    .input
                    .as_ref()
                    .is_some_and(|input| input.read(cx).is_composing())
                {
                    return;
                }
                if let Some(Dialog::Prompt(prompt)) = self.dialog.take() {
                    let answer = prompt.input.map_or_else(
                        || "yes".into(),
                        |input| input.update(cx, |input, _| input.take_value()),
                    );
                    prompt.request.respond(Some(answer));
                }
                self.finish_dialog(window, cx);
            }
            Some(Dialog::Form(form)) if !form.is_composing(window, cx) => {
                let submission = match form.validated(cx) {
                    Ok(submission) => submission,
                    Err(Errors { fields, key }) => {
                        if let Some((field, _)) = fields.first() {
                            form.input(*field)
                                .read(cx)
                                .focus_handle(cx)
                                .focus(window, cx);
                        } else {
                            form.focus_key(window, cx);
                        }
                        form.errors = fields;
                        form.key_error = key;
                        cx.notify();
                        return;
                    }
                };
                let id = form.id.clone();
                self.persist(Some((submission.server, submission.key)), id, window, cx);
            }
            Some(Dialog::Edit(EditDialog::Conflict(..)))
                if !self.confirm_focus.is_focused(window) =>
            {
                self.finish_dialog(window, cx);
            }
            Some(Dialog::Edit(_)) => self.confirm_edit(window, cx),
            Some(Dialog::Settings) => self.cancel_dialog(window, cx),
            Some(Dialog::Delete(id)) => {
                let id = id.clone();
                self.persist(None, Some(id), window, cx);
            }
            _ => {}
        }
    }

    fn persist(
        &mut self,
        server: Option<(Server, Option<AgentKey>)>,
        existing: Option<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.dir.clone() else { return };
        if let Some(id) = &existing
            && self.edits.has_server(id)
            && !self
                .servers
                .iter()
                .find(|old| &old.id == id)
                .zip(server.as_ref())
                .is_some_and(|(old, (new, _))| {
                    old.host == new.host && old.port == new.port && old.user == new.user
                })
        {
            let message =
                "Cierra las ediciones del servidor antes de cambiar su destino o eliminarlo.";
            if let Some(form) = self.editing() {
                form.message = Some(message);
            } else {
                self.config_error = Some(message);
                self.dialog = None;
            }
            cx.notify();
            return;
        }
        let mut servers = self.servers.clone();
        self.saving = true;
        cx.notify();
        let server_input = server;
        let save = cx.background_executor().spawn(async move {
            let mut server = None;
            if let Some((mut new, key)) = server_input {
                new.id = match &existing {
                    Some(id) => id.clone(),
                    None => Server::new_id()?,
                };
                if let (Some(key), Auth::Agent { identity_pub, .. }) = (&key, &mut new.auth) {
                    *identity_pub = agent::store_public_key(&dir, key)?;
                }
                server = Some(new);
            }
            match (&mut server, &existing) {
                // An edited server keeps its place and its pin.
                (Some(new), Some(id)) => match servers.iter_mut().find(|old| &old.id == id) {
                    Some(old) => {
                        new.pinned = old.pinned;
                        *old = new.clone();
                    }
                    None => servers.push(new.clone()),
                },
                (Some(new), None) => servers.push(new.clone()),
                (None, Some(id)) => servers.retain(|old| &old.id != id),
                (None, None) => {}
            }
            config::save(&dir, servers.clone())?;
            Ok::<_, std::io::Error>((servers, server, existing))
        });
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            let check = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok((servers, server, existing)) => {
                        if let Some(id) = &existing {
                            let original = this.servers.iter().find(|entry| &entry.id == id);
                            let keep_session =
                                original.zip(server.as_ref()).is_some_and(|(old, new)| {
                                    old.host == new.host
                                        && old.port == new.port
                                        && old.user == new.user
                                        && old.auth == new.auth
                                        && old.initial_remote_path == new.initial_remote_path
                                });
                            if !keep_session {
                                this.disconnect(id, window, cx);
                            }
                            if server.is_none()
                                || matches!(this.connections.get(id), Some(Connection::Error(_)))
                            {
                                this.connections.remove(id);
                            }
                            this.warnings.remove(id);
                        }
                        this.config_error = None;
                        this.servers = servers;
                        this.selected = server
                            .as_ref()
                            .or_else(|| this.servers.first())
                            .map(|server| server.id.clone());
                        this.show_stderr = false;
                        this.finish_dialog(window, cx);
                        server.filter(|server| matches!(server.auth, Auth::Agent { .. }))
                    }
                    Err(_) => {
                        let message = "No se pudo guardar servers.toml.";
                        if let Some(form) = this.editing() {
                            form.message = Some(message);
                        } else {
                            this.config_error = Some(message);
                            this.finish_dialog(window, cx);
                        }
                        cx.notify();
                        None
                    }
                }
            });
            if let (Ok(Some(server)), Some(client)) = (check, client) {
                let mut spec = server.connection_spec();
                dev::isolate_known_hosts(&mut spec);
                let warning = identity_warning(&client, spec).await;
                let _ = this.update_in(cx, |this, _, cx| {
                    if let Some(warning) = warning
                        && this.servers.contains(&server)
                    {
                        this.warnings.insert(server.id, warning);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn render_dialog(&self, theme: Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let (title, body, confirm): (gpui::SharedString, _, _) = match dialog {
            Dialog::Edit(dialog) => return Some(self.edit_dialog(dialog, theme, cx)),
            Dialog::Settings => return Some(self.settings_dialog(theme, cx)),
            Dialog::Form(form) => (
                if form.id.is_some() {
                    "Editar servidor".into()
                } else {
                    "Añadir servidor".into()
                },
                form.render(theme, cx),
                "Guardar",
            ),
            Dialog::Delete(id) => {
                let server = self.servers.iter().find(|server| &server.id == id)?;
                (
                    "Eliminar servidor".into(),
                    div().child(format!(
                        "¿Eliminar «{}» de la lista de servidores?",
                        server.name
                    )),
                    "Eliminar",
                )
            }
            Dialog::Prompt(prompt) => {
                let destination = &prompt.request.destination;
                let body = div().flex().flex_col().gap_2().child(
                    div()
                        .text_color(theme.secondary_text)
                        .child(destination.clone()),
                );
                match &prompt.request.kind {
                    askpass::Kind::HostKeyConfirm { fingerprint } => (
                        "Clave del servidor desconocida".into(),
                        body.child("Es la primera conexión con este servidor. Comprueba que la huella coincide antes de confiar en su clave.")
                            .child(technical_text(fingerprint.clone(), theme)),
                        "Confiar y conectar",
                    ),
                    kind => {
                        let (kind_name, label) = match kind {
                            askpass::Kind::Password => ("Contraseña del servidor", "Contraseña"),
                            askpass::Kind::Passphrase => ("Frase de paso de la clave", "Frase de paso"),
                            _ => ("Solicitud de autenticación", prompt.request.prompt.as_str()),
                        };
                        (
                            format!("{kind_name} · {destination}").into(),
                            div().flex().flex_col().gap_2().child(label.to_owned()).children(prompt.input.clone()),
                            "Conectar",
                        )
                    }
                }
            }
        };
        let footer = div()
            .flex()
            .flex_none()
            .justify_end()
            .gap_2()
            .child(button(
                "cancel",
                "Cancelar",
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| this.cancel_dialog(window, cx)),
            ))
            .child(
                button(
                    "confirm",
                    if self.saving { "Guardando…" } else { confirm },
                    Variant::Primary,
                    theme,
                    cx.listener(|this, _, window, cx| this.confirm_dialog(window, cx)),
                )
                .track_focus(&self.confirm_focus),
            );
        Some(
            modal::overlay(
                title,
                body.into_any_element(),
                footer.into_any_element(),
                &self.dialog_focus,
                &self.confirm_focus,
                theme,
                cx,
            )
            .into_any_element(),
        )
    }

    fn sidebar_button(&self, theme: Theme, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        icon_button(
            "toggle-sidebar",
            if self.sidebar_visible.get() {
                "Ocultar barra lateral"
            } else {
                "Mostrar barra lateral"
            },
            Icon::Sidebar,
            true,
            theme,
        )
        .aria_keyshortcuts("Control+Meta+S")
        .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)))
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible.set(!self.sidebar_visible.get());
        self.row_menu = None;
        window.refresh();
        cx.notify();
    }

    fn content(&self, theme: Theme, cx: &Context<Self>) -> impl IntoElement {
        if let Some(server) = self
            .servers
            .iter()
            .find(|server| Some(&server.id) == self.selected.as_ref())
            && let Some(Connection::Connected { panes, .. }) = self.connections.get(&server.id)
        {
            let id = server.id.clone();
            return panes.render(
                server,
                self.sidebar_button(theme, cx),
                theme,
                self.edit_header(&id, theme, cx),
                self.edit_status(&id, theme, cx),
                cx.listener(move |this, _, window, cx| this.disconnect(&id, window, cx)),
            );
        }
        let mut panel = div()
            .id("main")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .bg(theme.panel)
            .p_6()
            .when(!self.sidebar_visible.get(), |panel| {
                panel.child(self.sidebar_button(theme, cx))
            });
        if let Some(error) = self.config_error {
            panel = panel.child(
                div()
                    .p_3()
                    .rounded(px(8.))
                    .bg(theme.danger_bg)
                    .text_color(theme.danger)
                    .child(error),
            );
        }
        let Some(server) = self
            .servers
            .iter()
            .find(|server| Some(&server.id) == self.selected.as_ref())
        else {
            let hint = if self.loading {
                "Cargando servidores…"
            } else {
                "Conecta un servidor para empezar"
            };
            return panel.when(self.config_error.is_none(), |panel| {
                panel.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.secondary_text)
                        .child(hint),
                )
            });
        };
        let id = &server.id;
        let mut details = div()
            .mt_2()
            .flex()
            .flex_col()
            .gap_3()
            .items_start()
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(server.name.clone()),
            )
            .child(
                div()
                    .text_color(theme.secondary_text)
                    .child(format!("{}@{}", server.user, server.host)),
            );
        if let Some(warning) = self.warnings.get(id) {
            details = details.child(
                div()
                    .w_full()
                    .p_3()
                    .rounded(px(8.))
                    .bg(theme.warning_bg)
                    .text_color(theme.warning)
                    .child(warning.clone()),
            );
        }

        let action = |label, cx: &Context<Self>| {
            let id = id.clone();
            button(
                "connect",
                label,
                Variant::Primary,
                theme,
                cx.listener(move |this, _, window, cx| {
                    this.connect(&id, window, cx);
                }),
            )
        };
        let details = match self.connections.get(id) {
            Some(Connection::Connecting) => details.child("Conectando…"),
            Some(Connection::Disconnecting) => details.child("Desconectando…"),
            Some(Connection::Connected { .. }) => unreachable!("rendered above"),
            Some(Connection::Error(error)) => {
                let (message, extra, changed) = match error {
                    Error::AuthFailed { .. } => ("Autenticación rechazada", None, false),
                    Error::HostKeyChanged { .. } => (
                        "La clave del servidor ha cambiado. La conexión se ha bloqueado.",
                        None,
                        true,
                    ),
                    Error::HostKeyRejected { .. } => {
                        ("No se aceptó la clave del servidor.", None, false)
                    }
                    Error::Io(_) => (
                        "No se pudo iniciar o mantener la conexión SSH.",
                        None,
                        false,
                    ),
                    Error::Sftp(_) => (
                        "No se pudo abrir la sesión SFTP o leer la carpeta remota.",
                        None,
                        false,
                    ),
                    Error::ConnectFailed { stderr, .. } => {
                        ("No se pudo conectar con el servidor.", Some(stderr), false)
                    }
                    Error::SizeLimit(_) => ("Se superó el límite de transferencia.", None, false),
                    Error::Cancelled => ("Se canceló la conexión.", None, false),
                };
                details
                    .child(
                        div()
                            .w_full()
                            .p_3()
                            .rounded(px(8.))
                            .bg(theme.danger_bg)
                            .text_color(theme.danger)
                            .when(changed, |alert| alert.font_weight(FontWeight::SEMIBOLD))
                            .child(message),
                    )
                    .when_some(extra, |details, stderr| {
                        details
                            .child(button(
                                "stderr",
                                if self.show_stderr {
                                    "Ocultar detalles de SSH"
                                } else {
                                    "Ver detalles de SSH"
                                },
                                Variant::Secondary,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.show_stderr = !this.show_stderr;
                                    cx.notify();
                                }),
                            ))
                            .when(self.show_stderr, |details| {
                                details.child(technical_text(stderr.clone(), theme))
                            })
                    })
                    .child(action("Reintentar", cx))
            }
            None if self.client.is_some() => details.child(action("Conectar", cx)),
            None => details.child(
                div()
                    .text_color(theme.danger)
                    .child("No se pudo iniciar el cliente SSH."),
            ),
        };
        panel.child(
            details
                .child(self.edit_header(id, theme, cx))
                .child(self.edit_status(id, theme, cx)),
        )
    }
}

fn technical_text(text: String, theme: Theme) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .p_2()
        .rounded(px(6.))
        .bg(theme.selection)
        .font_family("Menlo")
        .text_size(px(12.))
        .child(text)
}

async fn identity_warning(client: &Client, spec: vscp_core::ConnectionSpec) -> Option<String> {
    match client.effective_identities(spec.clone()).await {
        Ok(identities) if identities.len() > 1 => {
            let Auth::Agent { identity_pub, .. } = spec.auth else {
                return None;
            };
            let mut skipped_selected = false;
            let extra = identities
                .into_iter()
                .filter(|path| {
                    if !skipped_selected && *path == identity_pub {
                        skipped_selected = true;
                        false
                    } else {
                        true
                    }
                })
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            Some(format!(
                "Tu ~/.ssh/config añade otras claves para este host; podrían agotarse los intentos de autenticación.\n{}",
                extra.join("\n")
            ))
        }
        Ok(_) => None,
        Err(_) => Some("No se pudieron comprobar las claves de ~/.ssh/config.".into()),
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(started_at) = self.trace_startup.take() {
            window.on_next_frame(move |_, _| {
                eprintln!(
                    "vSCP startup: {:.2} ms",
                    started_at.elapsed().as_secs_f64() * 1000.0
                );
            });
        }
        let theme = Theme::for_window(window);
        if !cx.has_active_drag() {
            self.dragging_server = false;
        }
        let transfer_overlay = if self.dialog.is_none() && !self.edits.popover {
            match self
                .selected
                .as_ref()
                .and_then(|id| self.connections.get(id))
            {
                Some(Connection::Connected { panes, .. }) => Some(panes.overlay()),
                _ => None,
            }
        } else {
            None
        };
        div()
            .relative()
            .flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                if this.dialog.is_none() {
                    this.toggle_sidebar(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|_, _: &Next, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &Previous, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|this, _: &DismissMenu, _, cx| {
                this.row_menu = None;
                this.edits.popover = false;
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &modal::Confirm, window, cx| this.confirm_dialog(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &modal::Cancel, window, cx| this.cancel_dialog(window, cx)),
            )
            .bg(theme.background)
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(theme.text)
            .when(self.quitting, |root| {
                root.child(div().p_4().child("Cerrando conexiones…"))
            })
            .when(!self.quitting, |root| {
                root.when(self.sidebar_visible.get(), |root| {
                    root.child(crate::sidebar::render(self, theme, cx))
                })
                .child(self.content(theme, cx))
                .children(transfer_overlay)
                .children(self.edit_popover(theme, window, cx))
                .children(self.render_dialog(theme, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

    use super::*;

    fn write_legacy(dir: &std::path::Path) {
        let mut text = String::new();
        for (id, name, favorite) in [
            ("id-zeta", "Zeta", true),
            ("id-bravo", "bravo", true),
            ("id-fixture", "Fixture", false),
            ("id-beta", "Beta", false),
        ] {
            text.push_str(&format!(
                "[[servers]]\nid = \"{id}\"\nname = \"{name}\"\nhost = \"h\"\nport = 22\nuser = \"u\"\nfavorite = {favorite}\n[servers.auth]\nkind = \"password\"\n\n"
            ));
        }
        fs::write(dir.join("servers.toml"), text).unwrap();
    }

    fn open<'a>(
        dir: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        write_legacy(dir);
        dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.to_path_buf()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, window, cx));
        cx.run_until_parked();
        (workspace, cx)
    }

    fn order(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<(String, bool)> {
        workspace.read_with(cx, |workspace, _| {
            workspace
                .servers
                .iter()
                .map(|server| (server.id.clone(), server.pinned))
                .collect()
        })
    }

    fn saved(dir: &std::path::Path) -> Vec<(String, bool)> {
        config::load(dir)
            .unwrap()
            .into_iter()
            .map(|server| (server.id, server.pinned))
            .collect()
    }

    fn pair(id: &str, pinned: bool) -> (String, bool) {
        (id.to_owned(), pinned)
    }

    fn drag(
        cx: &mut VisualTestContext,
        from: gpui::Point<gpui::Pixels>,
        to: gpui::Point<gpui::Pixels>,
    ) {
        let none = Modifiers::none();
        cx.simulate_mouse_move(from, None, none);
        cx.simulate_mouse_down(from, MouseButton::Left, none);
        for step in 1..=10 {
            let t = step as f32 / 10.;
            let at = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            cx.simulate_mouse_move(at, MouseButton::Left, none);
        }
        cx.simulate_mouse_up(to, MouseButton::Left, none);
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn legacy_favorites_load_in_the_old_order_and_pinning_saves_version_two(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open(dir.path(), cx);
        assert_eq!(
            order(&workspace, cx),
            [
                pair("id-bravo", true),
                pair("id-zeta", true),
                pair("id-beta", false),
                pair("id-fixture", false)
            ]
        );

        workspace.update(cx, |workspace, cx| workspace.toggle_pin("id-fixture", cx));
        cx.run_until_parked();
        let expected = [
            pair("id-bravo", true),
            pair("id-zeta", true),
            pair("id-fixture", true),
            pair("id-beta", false),
        ];
        assert_eq!(order(&workspace, cx), expected);
        assert_eq!(saved(dir.path()), expected);
        let text = fs::read_to_string(dir.path().join("servers.toml")).unwrap();
        assert!(
            text.contains("version = 2") && text.contains("pinned") && !text.contains("favorite")
        );
    }

    #[gpui::test]
    async fn a_click_selects_without_starting_a_drag(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open(dir.path(), cx);
        let row = cx.debug_bounds("server-row-id-beta").unwrap();
        let center = row.center();
        cx.simulate_mouse_move(center, None, Modifiers::none());
        cx.simulate_click(center, Modifiers::none());
        cx.run_until_parked();
        assert!(!cx.update(|_, cx| cx.has_active_drag()));
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.selected.clone()),
            Some("id-beta".into())
        );
        // A jitter below GPUI's drag threshold is still a click.
        let none = Modifiers::none();
        let nudged = point(center.x + px(1.), center.y);
        cx.simulate_mouse_down(center, MouseButton::Left, none);
        cx.simulate_mouse_move(nudged, MouseButton::Left, none);
        assert!(!cx.update(|_, cx| cx.has_active_drag()));
        cx.simulate_mouse_up(nudged, MouseButton::Left, none);
        assert_eq!(order(&workspace, cx).len(), 4);
    }

    #[gpui::test]
    async fn dragging_a_row_moves_and_pins_it_and_saves(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open(dir.path(), cx);
        let selected = workspace.read_with(cx, |workspace, _| workspace.selected.clone());

        // Fixture (unpinned) onto the top of the first pinned row.
        let from = cx.debug_bounds("server-row-id-fixture").unwrap().center();
        let target = cx.debug_bounds("server-row-id-bravo").unwrap();
        drag(cx, from, point(target.center().x, target.top() + px(8.)));
        let expected = [
            pair("id-fixture", true),
            pair("id-bravo", true),
            pair("id-zeta", true),
            pair("id-beta", false),
        ];
        assert_eq!(order(&workspace, cx), expected);
        assert_eq!(saved(dir.path()), expected);
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.selected.clone()),
            selected
        );
        assert!(!cx.update(|_, cx| cx.has_active_drag()));

        // Bravo (pinned) to the end of the unpinned group.
        let from = cx.debug_bounds("server-row-id-bravo").unwrap().center();
        let end = cx.debug_bounds("end-unpinned").unwrap();
        drag(cx, from, point(end.center().x, end.top() + px(4.)));
        let expected = [
            pair("id-fixture", true),
            pair("id-zeta", true),
            pair("id-beta", false),
            pair("id-bravo", false),
        ];
        assert_eq!(order(&workspace, cx), expected);
        assert_eq!(saved(dir.path()), expected);

        // Dropping a row onto itself changes nothing.
        let own = cx.debug_bounds("server-row-id-beta").unwrap();
        drag(cx, own.center(), point(own.center().x, own.top() + px(8.)));
        assert_eq!(order(&workspace, cx), expected);
    }

    #[gpui::test]
    async fn saving_stores_the_agent_key_and_edits_keep_their_place_and_pin(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open(dir.path(), cx);
        let key = AgentKey {
            key_type: "ssh-ed25519".into(),
            blob: [&[0, 0, 0, 11][..], b"ssh-ed25519"].concat(),
            comment: "Servidor".into(),
        };
        let new = Server {
            id: String::new(),
            name: "Nuevo".into(),
            host: "n.example".into(),
            port: 22,
            user: "ana".into(),
            auth: Auth::Agent {
                identity_pub: PathBuf::new(),
                agent_socket: None,
            },
            initial_remote_path: None,
            pinned: false,
        };
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.persist(Some((new, Some(key.clone()))), None, window, cx);
        });
        cx.run_until_parked();

        let servers = config::load(dir.path()).unwrap();
        let added = servers.last().unwrap();
        assert!(!added.pinned);
        assert_eq!(added.name, "Nuevo");
        let Auth::Agent { identity_pub, .. } = &added.auth else {
            panic!("expected agent authentication");
        };
        assert_eq!(identity_pub.parent().unwrap(), dir.path().join("keys"));
        assert_eq!(
            agent::read_public_key(identity_pub).unwrap().fingerprint(),
            key.fingerprint()
        );
        assert_eq!(servers.len(), 5);

        // Editing the first pinned server keeps its position and its pin.
        let mut edited = servers[0].clone();
        edited.name = "bravo renombrado".into();
        edited.pinned = false;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.persist(Some((edited, None)), Some("id-bravo".into()), window, cx);
        });
        cx.run_until_parked();
        let servers = config::load(dir.path()).unwrap();
        assert_eq!(servers[0].id, "id-bravo");
        assert_eq!(servers[0].name, "bravo renombrado");
        assert!(servers[0].pinned);
        assert_eq!(order(&workspace, cx)[0], pair("id-bravo", true));
    }
}
