#[path = "edits_view.rs"]
mod edits_view;
#[path = "settings_view.rs"]
mod settings_view;
#[path = "terminal_workspace.rs"]
mod terminal_workspace;

use edits_view::{EditDialog, Edits};
use vscp_core::{
    edit::{Index, Target},
    settings::Settings,
};

use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    PathPromptOptions, Render, Window, WindowBackgroundAppearance, actions, div, prelude::*, px,
};
use vscp_core::{
    Auth, Client, ConnectionSpec, Error, Session,
    agent::{self, AgentError, AgentKey},
    askpass,
    config::{self, Server},
};

use crate::{
    appearance::{Appearance, Loaded},
    components::{
        button::{Variant, button, icon_text_button},
        modal, motion,
        text_input::TextInput,
    },
    dev,
    file_workspace::{Panes, SessionView, session_switch},
    form::{Errors, Field, Form},
    icons::IconName,
    pane::{PaneMemory, icon_button},
    sidebar::{self, RowAction, RowMenu, ServerReorder},
    terminal::{font_size, kitty::Kitty},
    theme::Theme,
};

actions!(
    workspace,
    [
        Next,
        Previous,
        DismissMenu,
        ToggleSidebar,
        OpenSettings,
        ShowFiles,
        ShowTerminal
    ]
);

pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys([
        crate::keybindings::binding(cx, "secondary-1", ShowFiles, Some("Workspace")),
        crate::keybindings::binding(cx, "secondary-2", ShowTerminal, Some("Workspace")),
        crate::keybindings::binding(cx, "secondary-,", OpenSettings, Some("Workspace")),
        KeyBinding::new("tab", Next, Some("Workspace")),
        KeyBinding::new("shift-tab", Previous, Some("Workspace")),
        KeyBinding::new("escape", DismissMenu, Some("Workspace")),
        KeyBinding::new(
            "escape",
            crate::components::dropdown::Dismiss,
            Some("EditorMenu"),
        ),
        crate::keybindings::binding(cx, "secondary-s", ToggleSidebar, Some("Workspace")),
        KeyBinding::new("tab", modal::Next, Some("RowMenu")),
        KeyBinding::new("down", modal::Next, Some("RowMenu")),
        KeyBinding::new("shift-tab", modal::Previous, Some("RowMenu")),
        KeyBinding::new("up", modal::Previous, Some("RowMenu")),
        KeyBinding::new("tab", modal::Next, Some("ThemeEditor")),
        KeyBinding::new("shift-tab", modal::Previous, Some("ThemeEditor")),
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
    rejected: bool,
}

enum Dialog {
    Form(Box<Form>),
    Delete(String),
    Prompt(Prompt),
    Edit(EditDialog),
    DeleteTheme(String),
    EmptyEdits,
}

pub struct Workspace {
    pub servers: Vec<Server>,
    pub selected: Option<String>,
    pub connections: HashMap<String, Connection>,
    pub row_menu: Option<RowMenu>,
    pub dir: Option<PathBuf>,
    pub saving: bool,
    /// The card being dragged in the server list, until the drag ends.
    pub reorder: Option<ServerReorder>,
    /// A card that just left a drag, with the y where it was let go and when.
    /// Its row starts there and glides into its slot.
    settle: Option<(String, f32, Instant)>,
    key_requests: u64,
    edits: Edits,
    settings: Settings,
    kitty_settings: Option<Kitty>,
    settings_error: Option<String>,
    settings_page: settings_view::SettingsPage,
    window_background: Option<WindowBackgroundAppearance>,
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
    password_attempts: HashMap<String, u32>,
    focus: FocusHandle,
    dialog_focus: FocusHandle,
    confirm_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    prompt_task: Option<gpui::Task<()>>,
    trace_startup: Option<Instant>,
    font_size_generation: u64,
}

/// The socket used when a server does not set its own: the global setting
/// first, then the Bitwarden socket detected at startup.
fn spec_with_socket(
    server: &Server,
    global: Option<&Path>,
    detected: Option<&Path>,
) -> ConnectionSpec {
    let mut spec = server.connection_spec();
    if matches!(
        server.auth,
        Auth::Agent {
            agent_socket: None,
            ..
        }
    ) && let Some(socket) = global.or(detected)
        && let Auth::Agent { agent_socket, .. } = &mut spec.auth
    {
        *agent_socket = Some(socket.to_owned());
    }
    spec
}

/// Fills a missing agent socket from a freshly detected one, leaving an
/// explicit socket untouched. Returns the socket that was filled in.
fn fill_detected_socket(spec: &mut ConnectionSpec, detected: Option<&Path>) -> Option<PathBuf> {
    if !matches!(
        spec.auth,
        Auth::Agent {
            agent_socket: None,
            ..
        }
    ) {
        return None;
    }
    let socket = detected.map(Path::to_owned);
    if let Auth::Agent { agent_socket, .. } = &mut spec.auth {
        *agent_socket = socket.clone();
    }
    socket
}

/// Detects the Bitwarden socket off the main thread and fills the spec when
/// its agent socket is still unset. Returns the socket that was filled in.
async fn redetect_socket(
    spec: &mut ConnectionSpec,
    executor: &gpui::BackgroundExecutor,
) -> Option<PathBuf> {
    if !matches!(
        spec.auth,
        Auth::Agent {
            agent_socket: None,
            ..
        }
    ) {
        return None;
    }
    let detected = executor
        .spawn(async { vscp_core::bitwarden_agent_sockets().into_iter().next() })
        .await;
    fill_detected_socket(spec, detected.as_deref())
}

impl Workspace {
    pub fn new(
        client: Option<Client>,
        trace_startup: Option<Instant>,
        loaded: Option<Loaded>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if cx.try_global::<Appearance>().is_none() {
            cx.set_global(Appearance::default());
        }
        let preloaded = loaded.is_some();
        let (settings, settings_error) =
            loaded.map_or_else(|| (Settings::default(), None), |loaded| loaded.install(cx));
        font_size::apply_saved(settings.terminal_font_size, cx);
        cx.observe_global::<font_size::FontSize>(|this, cx| {
            let size = cx.global::<font_size::FontSize>().override_size;
            if font_size::dev_override(cx) || this.settings.terminal_font_size == size {
                cx.notify();
                return;
            }
            this.settings.terminal_font_size = size;
            this.font_size_generation = this.font_size_generation.wrapping_add(1);
            let generation = this.font_size_generation;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.font_size_generation != generation {
                        return;
                    }
                    this.save_settings(this.settings.clone(), window, cx);
                });
            })
            .detach();
            cx.notify();
        })
        .detach();
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
        let load = cx.background_executor().spawn(async move {
            let dir = dev::config_directory()?;
            let servers = config::load(&dir)?;
            let agent = vscp_core::bitwarden_agent_sockets().into_iter().next();
            let index = dev::edit_root().and_then(|root| Index::load(dir.clone(), root));
            let loaded = (!preloaded).then(|| Loaded::read(&dir));
            Ok::<_, std::io::Error>((dir, servers, agent, index, loaded))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((dir, servers, agent, index, loaded)) => {
                        this.load_edits(index, window, cx);
                        if let Some(loaded) = loaded {
                            (this.settings, this.settings_error) = loaded.install(cx);
                            font_size::apply_saved(this.settings.terminal_font_size, cx);
                        }
                        this.selected = servers.first().map(|server| server.id.clone());
                        this.dir = Some(dir);
                        this.servers = servers;
                        this.agent_socket = agent.map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                        this.migrate_default_socket(window, cx);
                        if let Some(name) = dev::autoconnect()
                            && let Some(server) = this.servers.iter().find(|server| server.display_name() == name)
                        {
                            let id = server.id.clone();
                            this.select(&id);
                            this.connect(&id, window, cx);
                        }
                        if let Some(name) = dev::row_menu()
                            && let Some(server) = this.servers.iter().find(|server| server.display_name() == name)
                        {
                            let id = server.id.clone();
                            this.select(&id);
                            if let Some(mut position) = sidebar::row_centre(&this.servers, &id) {
                                position.y += titlebar_height(window);
                                this.open_row_menu(&id, position, window, cx);
                            }
                        }
                        if let Some((name, dy)) = dev::drag()
                            && let Some(server) = this.servers.iter().find(|server| server.display_name() == name)
                        {
                            let id = server.id.clone();
                            this.hold_reorder(&id, dy, cx);
                        }
                        match dev::form() {
                            Some(dev::FormRequest::New) => this.open_form(None, window, cx),
                            Some(dev::FormRequest::Edit(name)) => {
                                let id = this.servers.iter().find(|server| server.display_name() == name).map(|server| server.id.clone());
                                if let Some(id) = id {
                                    this.open_form(Some(&id), window, cx);
                                }
                            }
                            Some(dev::FormRequest::Duplicate(name)) => {
                                let id = this.servers.iter().find(|server| server.display_name() == name).map(|server| server.id.clone());
                                if let Some(id) = id {
                                    this.open_duplicate_form(&id, window, cx);
                                }
                            }
                            None => {}
                        }
                        this.dev_open_settings(window, cx);
                        if dev::prompt().as_deref() == Some("password-rejected") {
                            #[cfg(feature = "dev")]
                            {
                                this.password_attempts.insert("prueba".into(), 1);
                                this.prompts.push_back(askpass::Request::fake(
                                    "prueba",
                                    "Contraseña para 'prueba':",
                                    askpass::Kind::Password,
                                ));
                                this.show_next_prompt(window, cx);
                            }
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
        // The setting can change while the app runs, so read it when the window is activated.
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                cx.set_reduce_motion(crate::accessibility::reduce_motion());
                let reduced = crate::accessibility::reduce_transparency();
                if reduced != cx.global::<Appearance>().reduce_transparency {
                    cx.global_mut::<Appearance>().reduce_transparency = reduced;
                    cx.refresh_windows();
                }
            }
        })
        .detach();
        cx.on_app_quit(|this, cx| {
            let cleanup = (!this.quitting).then(|| crate::pane::finder::quit_cleanup(cx));
            let shutdown = this.shutdown(cx);
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
            reorder: None,
            settle: None,
            key_requests: 0,
            edits: Edits::default(),
            settings,
            kitty_settings: None,
            settings_error,
            settings_page: settings_view::SettingsPage::new(cx),
            window_background: None,
            loading: true,
            sidebar_visible: Rc::new(Cell::new(!dev::hide_sidebar())),
            pane_memory: HashMap::new(),
            quitting: false,
            config_error: None,
            client,
            agent_socket: String::new(),
            warnings: HashMap::new(),
            show_stderr: false,
            dialog: None,
            prompts: VecDeque::new(),
            password_attempts: HashMap::new(),
            focus,
            dialog_focus: cx.focus_handle(),
            confirm_focus: cx.focus_handle(),
            previous_focus: None,
            prompt_task,
            trace_startup,
            font_size_generation: 0,
        }
    }

    fn shutdown(&mut self, cx: &mut Context<Self>) -> impl Future<Output = ()> + use<> {
        let executor = cx.background_executor().clone();
        self.dialog = None;
        self.prompt_task.take();
        self.prompts.clear();
        for connection in self.connections.values() {
            if let Connection::Connected { panes, .. } = connection
                && let Some(terminal) = &panes.terminal
            {
                terminal.update(cx, |terminal, _| terminal.stop());
            }
        }
        self.connections.clear();
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
        let shutdown = self.shutdown(cx);
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

    /// The root focus, which a drag takes so that Escape reaches the workspace
    /// even though the dragged row leaves the tree.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn sidebar_visible(&self) -> bool {
        self.sidebar_visible.get()
    }

    /// The card that is gliding into its slot after a drag, with its starting y.
    pub fn settling(&self) -> Option<(&String, f32)> {
        self.settle
            .as_ref()
            .filter(|(_, _, since)| since.elapsed() < Duration::from_secs(1))
            .map(|(id, y, _)| (id, *y))
    }

    /// Opens the floating menu of a server row at `position`.
    pub fn open_row_menu(
        &mut self,
        id: &str,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() || self.reorder.is_some() {
            return;
        }
        let first = cx.focus_handle().tab_stop(true);
        first.focus(window, cx);
        self.row_menu = Some(RowMenu {
            id: id.to_owned(),
            position,
            scope: cx.focus_handle(),
            first,
            last: cx.focus_handle().tab_stop(true),
        });
        cx.notify();
    }

    pub fn dismiss_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.row_menu.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    pub fn row_menu_action(
        &mut self,
        action: RowAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.row_menu.take() else {
            return;
        };
        let id = menu.id;
        self.focus.focus(window, cx);
        match action {
            RowAction::Session => {
                if matches!(
                    self.connections.get(&id),
                    Some(Connection::Connected { .. })
                ) {
                    self.disconnect(&id, window, cx);
                } else {
                    self.connect(&id, window, cx);
                }
            }
            RowAction::Pin => self.toggle_pin(&id, cx),
            RowAction::Edit => self.open_form(Some(&id), window, cx),
            RowAction::Duplicate => self.open_duplicate_form(&id, window, cx),
            RowAction::Delete => self.open_delete(id, window, cx),
        }
        cx.notify();
    }

    /// Starts moving a card. `grab_offset_y` is the cursor y within the card.
    pub fn begin_reorder(&mut self, id: &str, grab_offset_y: f32, cx: &mut Context<Self>) {
        let (pinned, unpinned) = sidebar::split(&self.servers);
        let Some(y) = sidebar::layout(&pinned, &unpinned, None).row_y(id) else {
            return;
        };
        let is_pinned = pinned.contains(&id);
        let group = if is_pinned { &pinned } else { &unpinned };
        let index = group.iter().position(|other| *other == id).unwrap_or(0);
        self.row_menu = None;
        self.settle = None;
        self.reorder = Some(ServerReorder {
            id: id.to_owned(),
            grab_offset_y,
            pointer_y: y + grab_offset_y,
            target: sidebar::Target {
                pinned: is_pinned,
                index,
            },
            held: false,
        });
        cx.notify();
    }

    /// Follows the cursor, `pointer_y` being its y within the list content.
    pub fn reorder_move(&mut self, pointer_y: f32, cx: &mut Context<Self>) {
        let Some(reorder) = &mut self.reorder else {
            return;
        };
        reorder.pointer_y = pointer_y;
        let (pinned, unpinned) = sidebar::split(&self.servers);
        let id = reorder.id.as_str();
        let others = |group: &[&str]| group.iter().filter(|other| **other != id).count();
        let centre = pointer_y - reorder.grab_offset_y + sidebar::ROW_H / 2.;
        // Moving to another group changes the layout the card is measured against,
        // so repeat until the slot agrees with the layout it produces.
        for _ in 0..3 {
            let target = sidebar::target_for(
                others(&pinned),
                others(&unpinned),
                !pinned.is_empty(),
                reorder.target,
                pointer_y,
                centre,
            );
            if target == reorder.target {
                break;
            }
            reorder.target = target;
        }
        cx.notify();
    }

    /// Releases the card over the list: it moves to its target and is saved.
    pub fn drop_reorder(&mut self, cx: &mut Context<Self>) {
        self.finish_reorder(true, cx);
    }

    /// Ends the drag. Either way the card glides from where it was let go to its slot.
    fn finish_reorder(&mut self, drop: bool, cx: &mut Context<Self>) {
        let Some(reorder) = self.reorder.take() else {
            return;
        };
        let (pinned, unpinned) = sidebar::split(&self.servers);
        let height = sidebar::layout(
            &pinned,
            &unpinned,
            Some((reorder.id.as_str(), reorder.target)),
        )
        .height;
        let from = sidebar::card_top(&reorder, height);
        self.settle = Some((reorder.id.clone(), from, Instant::now()));
        if drop {
            let group = if reorder.target.pinned {
                pinned
            } else {
                unpinned
            };
            let before = group
                .into_iter()
                .filter(|id| *id != reorder.id)
                .nth(reorder.target.index)
                .map(str::to_owned);
            self.drop_server(&reorder.id, reorder.target.pinned, before.as_deref(), cx);
        }
        cx.notify();
    }

    /// Development hook: shows a card dragged `dy` px from its slot and holds it.
    fn hold_reorder(&mut self, id: &str, dy: f32, cx: &mut Context<Self>) {
        self.begin_reorder(id, sidebar::ROW_H / 2., cx);
        if let Some(reorder) = &mut self.reorder {
            reorder.held = true;
            let pointer = reorder.pointer_y + dy;
            self.reorder_move(pointer, cx);
        }
    }

    pub(crate) fn spec_for(&self, server: &Server) -> ConnectionSpec {
        let detected = (!self.agent_socket.is_empty()).then(|| Path::new(&self.agent_socket));
        spec_with_socket(server, self.settings.agent_socket.as_deref(), detected)
    }

    pub(crate) fn effective_socket(&self) -> Option<&Path> {
        self.settings
            .agent_socket
            .as_deref()
            .or_else(|| (!self.agent_socket.is_empty()).then(|| Path::new(&self.agent_socket)))
    }

    fn migrate_default_socket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(default) = self.effective_socket().map(Path::to_owned) else {
            return;
        };
        let Some(dir) = self.dir.clone() else { return };
        let mut servers = std::mem::take(&mut self.servers);
        if !config::clear_default_sockets(&mut servers, &default) {
            self.servers = servers;
            return;
        }
        let to_save = servers.clone();
        self.servers = servers;
        let save = cx
            .background_executor()
            .spawn(async move { config::save(&dir, to_save) });
        cx.spawn_in(window, async move |_, _| {
            if let Err(error) = save.await {
                dev::log(format_args!("no se pudo migrar servers.toml: {error}"));
            }
        })
        .detach();
    }

    pub fn connect(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(window, cx);
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
        let mut spec = self.spec_for(&server);
        dev::isolate_known_hosts(&mut spec);
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Some(socket) = redetect_socket(&mut spec, &executor).await {
                let _ = this.update(cx, |this, _| {
                    this.agent_socket = socket.to_string_lossy().into_owned();
                });
            }
            let result = async {
                let session = client.connect(spec).await?;
                dev::log(format_args!("connected {}", server.display_name()));
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
                this.clear_password_attempts(server.display_name());
                if matches!(&this.dialog, Some(Dialog::Prompt(prompt)) if !prompt.request.is_pending()) {
                    this.finish_dialog(window, cx);
                }
                let state = match result {
                    Ok((session, path)) => this.connected(&server, session, path, window, cx),
                    Err(error) => {
                        dev::log(format_args!("connection failed for {}", server.display_name()));
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
        let mut panes = Panes::new(
            session.clone(),
            server,
            path,
            memory,
            self.sidebar_visible.clone(),
            window,
            cx,
        );
        let run_server = id.clone();
        cx.subscribe_in(
            panes.remote(),
            window,
            move |this, _, run: &crate::pane::RunRemote, window, cx| {
                this.run_remote(&run_server, run, window, cx);
            },
        )
        .detach();
        if dev::terminal_view() {
            let terminal = Self::terminal_for(&mut panes, id, window, cx);
            panes.view = SessionView::Terminal;
            let command = vscp_core::terminal_start_command(
                &panes.remote().read(cx).directory().to_string_lossy(),
            );
            let args = session.terminal_command(&command);
            terminal.update(cx, |terminal, cx| terminal.start(args, window, cx));
        }
        let edit_server = server.clone();
        cx.subscribe_in(
            panes.remote(),
            window,
            move |this, _, event: &crate::pane::EditFile, window, cx| {
                this.open_edit(Target::new(&edit_server, event.0.clone()), window, cx);
            },
        )
        .detach();
        let move_server = id.clone();
        cx.subscribe_in(
            panes.remote(),
            window,
            move |this, pane, event: &crate::pane::MoveFiles, window, cx| {
                let blocked = event.sources.iter().find(|source| {
                    this.edits.rows().iter().any(|row| {
                        row.target.server_id == move_server
                            && row.target.remote_path.starts_with(source)
                    })
                });
                pane.update(cx, |pane, cx| {
                    if let Some(path) = blocked {
                        let name = path
                            .file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy();
                        pane.report_error(
                            format!("Cierra las ediciones de «{name}» antes de moverlo."),
                            cx,
                        );
                    } else {
                        pane.move_files(
                            crate::pane::MoveFiles {
                                sources: event.sources.clone(),
                                folder: event.folder.clone(),
                            },
                            window,
                            cx,
                        );
                    }
                });
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
        let Some(Connection::Connected { session, panes }) = self
            .connections
            .insert(id.into(), Connection::Disconnecting)
        else {
            return;
        };
        if let Some(terminal) = panes.terminal {
            terminal.update(cx, |terminal, _| terminal.stop());
        }
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
        let form = Form::new(server, self.effective_socket(), self.dir.as_deref(), cx);
        self.show_form(form, window, cx);
    }

    /// Prefills the add-server form from a saved server without reusing its id.
    pub fn open_duplicate_form(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir.is_none() || self.dialog.is_some() || self.saving {
            return;
        }
        let Some(server) = self.servers.iter().find(|server| server.id == id) else {
            return;
        };
        let form = Form::duplicate(server, self.effective_socket(), self.dir.as_deref(), cx);
        self.show_form(form, window, cx);
    }

    fn show_form(&mut self, mut form: Form, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = dev::form_file() {
            form.pick_file(path);
        }
        if let Some(comment) = dev::form_key() {
            form.preselect(comment);
        }
        if dev::form_password() {
            form.set_agent(false);
        }
        if dev::form_advanced() {
            form.toggle_advanced();
        }
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
        let global = self.settings.agent_socket.clone();
        cx.notify();
        let hint = executor.clone();
        cx.spawn_in(window, async move |this, cx| {
            hint.timer(Duration::from_millis(2100)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            let path = if socket.is_empty() {
                if global.is_some() {
                    global
                } else {
                    executor
                        .spawn(async { vscp_core::bitwarden_agent_sockets().into_iter().next() })
                        .await
                }
            } else {
                Some(PathBuf::from(socket))
            };
            let noted = path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            let result = match (client, path) {
                (Some(client), Some(path)) => client.list_agent_keys(path).await,
                _ => Err(AgentError::Unavailable),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                let handles = (0..=result.as_ref().map_or(0, Vec::len))
                    .map(|_| cx.focus_handle())
                    .collect();
                if let Some(Dialog::Form(form)) = &mut this.dialog {
                    if let Some(noted) = &noted {
                        form.note_socket(noted);
                    }
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
            Dialog::Delete(_) | Dialog::Edit(_) | Dialog::DeleteTheme(_) | Dialog::EmptyEdits => {
                None
            }
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

    fn clear_password_attempts(&mut self, display_name: &str) {
        self.password_attempts.remove(display_name);
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
                let rejected = matches!(request.kind, askpass::Kind::Password)
                    && self
                        .password_attempts
                        .get(&request.destination)
                        .is_some_and(|count| *count > 0);
                self.open_dialog(
                    Dialog::Prompt(Prompt {
                        request,
                        input,
                        rejected,
                    }),
                    window,
                    cx,
                );
                return;
            }
        }
    }

    fn cancel_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.saving {
            let password_destination = match &self.dialog {
                Some(Dialog::Prompt(prompt))
                    if matches!(prompt.request.kind, askpass::Kind::Password) =>
                {
                    Some(prompt.request.destination.clone())
                }
                _ => None,
            };
            if let Some(destination) = password_destination {
                self.clear_password_attempts(&destination);
            }
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
                let password_destination = match &self.dialog {
                    Some(Dialog::Prompt(prompt))
                        if matches!(prompt.request.kind, askpass::Kind::Password) =>
                    {
                        Some(prompt.request.destination.clone())
                    }
                    _ => None,
                };
                if let Some(Dialog::Prompt(prompt)) = self.dialog.take() {
                    let answer = prompt.input.map_or_else(
                        || "yes".into(),
                        |input| input.update(cx, |input, _| input.take_value()),
                    );
                    if let Some(destination) = password_destination {
                        *self.password_attempts.entry(destination).or_insert(0) += 1;
                    }
                    prompt.request.respond(Some(answer));
                }
                self.finish_dialog(window, cx);
            }
            Some(Dialog::Form(form)) if !form.is_composing(window, cx) => {
                let submission = match form.validated(cx) {
                    Ok(submission) => submission,
                    Err(Errors { fields, key }) => {
                        if let Some((field, _)) = fields.first() {
                            form.reveal_field(*field);
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
            Some(Dialog::DeleteTheme(id)) => {
                let id = id.clone();
                self.remove_theme(id, window, cx);
            }
            Some(Dialog::Delete(id)) => {
                let id = id.clone();
                self.persist(None, Some(id), window, cx);
            }
            Some(Dialog::EmptyEdits) => self.empty_edit_copies(window, cx),
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
                let Ok(mut spec) = this.update(cx, |this, _| this.spec_for(&server)) else {
                    return;
                };
                if let Some(socket) = redetect_socket(&mut spec, cx.background_executor()).await {
                    let _ = this.update(cx, |this, _| {
                        this.agent_socket = socket.to_string_lossy().into_owned();
                    });
                }
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
        let (title, lead, body, confirm): (gpui::SharedString, modal::Lead, _, _) = match dialog {
            Dialog::Edit(dialog) => return Some(self.edit_dialog(dialog, theme, cx)),
            Dialog::DeleteTheme(id) => {
                let theme_def = cx
                    .global::<Appearance>()
                    .themes
                    .iter()
                    .find(|theme| &theme.id == id)?;
                (
                    "Eliminar tema".into(),
                    Some((IconName::Trash, theme.danger)),
                    div().child(format!(
                        "Se eliminará «{}». Esta acción no se puede deshacer.",
                        theme_def.name
                    )),
                    "Eliminar",
                )
            }
            Dialog::Form(form) => (
                if form.id.is_some() {
                    "Editar servidor".into()
                } else {
                    "Añadir servidor".into()
                },
                None,
                form.render(theme, cx),
                "Guardar",
            ),
            Dialog::Delete(id) => {
                let server = self.servers.iter().find(|server| &server.id == id)?;
                (
                    "Eliminar servidor".into(),
                    Some((IconName::Trash, theme.danger)),
                    div().child(format!(
                        "¿Eliminar «{}» de la lista de servidores?",
                        server.display_name()
                    )),
                    "Eliminar",
                )
            }
            Dialog::EmptyEdits => (
                "¿Vaciar las copias de edición?".into(),
                Some((IconName::Trash, theme.danger)),
                div().child(
                    "Se borran las copias ya subidas. Las que tienen cambios sin subir o están abiertas en el editor se conservan.",
                ),
                "Vaciar",
            ),
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
                        Some((IconName::Key, theme.accent)),
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
                            Some((IconName::Lock, theme.secondary_text)),
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(label.to_owned())
                                .children(prompt.input.clone())
                                .when(
                                    matches!(kind, askpass::Kind::Password) && prompt.rejected,
                                    |body| {
                                        body.child(crate::form::error_text(
                                            "El servidor rechazó la contraseña.",
                                            theme,
                                        ))
                                    },
                                ),
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
            .child({
                let label = if self.saving { "Guardando…" } else { confirm };
                let on_click = cx.listener(|this, _, window, cx| this.confirm_dialog(window, cx));
                if label == "Conectar" {
                    icon_text_button(
                        "confirm",
                        IconName::Plug,
                        label,
                        Variant::Primary,
                        None,
                        theme,
                        on_click,
                    )
                } else if matches!(dialog, Dialog::DeleteTheme(_) | Dialog::EmptyEdits) {
                    let mut danger = theme;
                    danger.primary = theme.danger;
                    danger.primary_hover = theme.danger;
                    button("confirm", label, Variant::Primary, danger, on_click)
                } else {
                    button("confirm", label, Variant::Primary, theme, on_click)
                }
                .track_focus(&self.confirm_focus)
            });
        Some(
            modal::overlay(
                title,
                lead,
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
            IconName::Sidebar,
            true,
            theme,
        )
        .aria_keyshortcuts(crate::keybindings::shortcut("Meta+S", "Control+S"))
        .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)))
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible.set(!self.sidebar_visible.get());
        self.row_menu = None;
        window.refresh();
        cx.notify();
    }

    fn content(&self, theme: Theme, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        if let Some(server) = self
            .servers
            .iter()
            .find(|server| Some(&server.id) == self.selected.as_ref())
            && let Some(Connection::Connected { panes, .. }) = self.connections.get(&server.id)
        {
            let id = server.id.clone();
            return panes.render(
                server,
                (
                    self.sidebar_button(theme, cx),
                    session_switch(panes.view, theme, {
                        let workspace = cx.weak_entity();
                        move |view, window, cx| {
                            let _ = workspace
                                .update(cx, |this, cx| this.switch_session_view(view, window, cx));
                        }
                    }),
                    (panes.view == SessionView::Terminal).then(|| {
                        icon_text_button(
                            "open-kitty",
                            IconName::ExternalLink,
                            "Abrir en Kitty",
                            Variant::Secondary,
                            None,
                            theme,
                            cx.listener(|this, _, window, cx| {
                                this.open_terminal_in_kitty(window, cx)
                            }),
                        )
                        .h(px(26.))
                        .px_1()
                        .into_any_element()
                    }),
                    self.sidebar_visible.get() && window.viewport_size().width < px(862.),
                ),
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
            .bg(if theme.glassy {
                theme.chrome(theme.background)
            } else {
                theme.panel
            })
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
                    .font_weight(FontWeight::MEDIUM)
                    .child(server.display_name().to_owned()),
            )
            .child(
                div()
                    .text_color(theme.secondary_text)
                    .child(format!("{}@{}", server.user, server.host)),
            );
        if let Some(warning) = self.warnings.get(id) {
            details = details.child(motion::appear(
                div()
                    .w_full()
                    .p_3()
                    .rounded(px(8.))
                    .bg(theme.warning_bg)
                    .text_color(theme.warning)
                    .child(warning.clone()),
                "ssh-warning",
                -4.,
                motion::FAST,
            ));
        }

        let action = |label: &'static str, cx: &Context<Self>| {
            let id = id.clone();
            let on_click = cx.listener(move |this, _, window, cx| {
                this.connect(&id, window, cx);
            });
            if label == "Conectar" {
                icon_text_button(
                    "connect",
                    IconName::Plug,
                    label,
                    Variant::Primary,
                    None,
                    theme,
                    on_click,
                )
            } else {
                button("connect", label, Variant::Primary, theme, on_click)
            }
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
        .font_family(crate::theme::monospace())
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

#[cfg(target_os = "macos")]
fn titlebar_height(window: &Window) -> gpui::Pixels {
    if window.is_fullscreen() {
        px(0.)
    } else {
        px(crate::MACOS_TITLEBAR_HEIGHT)
    }
}

#[cfg(not(target_os = "macos"))]
fn titlebar_height(_: &Window) -> gpui::Pixels {
    px(0.)
}

#[cfg(target_os = "macos")]
fn macos_titlebar(height: gpui::Pixels, theme: Theme) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(height)
        .px(px(80.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.secondary_text)
        .font_weight(FontWeight::MEDIUM)
        .child(div().min_w_0().truncate().child("vSCP"))
        .on_mouse_down(gpui::MouseButton::Left, |event, window, cx| {
            // The macOS backend handles native controls above this view and
            // leaves drag handling to apps with a transparent title bar.
            if event.click_count == 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
            cx.stop_propagation();
        })
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
        let background = cx.global::<Appearance>().window_background();
        if self.window_background != Some(background) {
            window.set_background_appearance(background);
            self.window_background = Some(background);
        }
        let theme = Theme::current(window, cx);
        if self.reorder.as_ref().is_some_and(|reorder| !reorder.held) && !cx.has_active_drag() {
            // The drag ended without a drop over the list.
            self.finish_reorder(false, cx);
        }
        for (id, connection) in &self.connections {
            if let Connection::Connected { panes, .. } = connection
                && let Some(terminal) = &panes.terminal
            {
                let visible = !self.quitting
                    && !self.settings_page.is_open()
                    && self.selected.as_ref() == Some(id)
                    && panes.view == SessionView::Terminal;
                terminal.update(cx, |terminal, _| {
                    terminal.set_palette(theme.terminal);
                    terminal.set_selected(visible, window);
                });
            }
        }
        let settings_open = self.settings_page.is_open();
        let transfer_overlay = if !settings_open && self.dialog.is_none() && !self.edits.popover {
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
        let content = if settings_open {
            self.render_settings(theme, window, cx).into_any_element()
        } else {
            self.content(theme, window, cx).into_any_element()
        };
        let titlebar_height = titlebar_height(window);
        // Each column paints its own portion of the strip. The sidebar's
        // background and divider follow its animated width.
        let content = if titlebar_height > px(0.) {
            let connected = self.selected.as_ref().is_some_and(|id| {
                matches!(self.connections.get(id), Some(Connection::Connected { .. }))
            });
            let color = if settings_open || (theme.glassy && !connected) {
                theme.background
            } else {
                theme.panel
            };
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(div().h(titlebar_height).flex_none().bg(theme.chrome(color)))
                .child(content)
                .into_any_element()
        } else {
            content
        };
        let root = div()
            .relative()
            .flex()
            .size_full()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &ShowFiles, window, cx| {
                this.switch_session_view(SessionView::Files, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowTerminal, window, cx| {
                this.switch_session_view(SessionView::Terminal, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                if this.dialog.is_none() {
                    this.toggle_sidebar(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Next, window, cx| {
                if !this.theme_editor_tab(false, window, cx) {
                    window.focus_next(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Previous, window, cx| {
                if !this.theme_editor_tab(true, window, cx) {
                    window.focus_prev(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DismissMenu, window, cx| {
                if this.reorder.is_some() {
                    cx.stop_active_drag(window);
                    this.finish_reorder(false, cx);
                    return;
                }
                if this.settings_page.is_open() && this.dialog.is_none() && this.row_menu.is_none()
                {
                    if !this.dismiss_theme_editor(window, cx) {
                        this.close_settings(window, cx);
                    }
                    return;
                }
                this.dismiss_row_menu(window, cx);
                this.edits.popover = false;
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &modal::Confirm, window, cx| this.confirm_dialog(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &modal::Cancel, window, cx| this.cancel_dialog(window, cx)),
            )
            // Each zone paints its own backdrop in glass mode. A full-window
            // base here would increase the effective opacity under every zone.
            .when(!theme.glassy, |root| root.bg(theme.background))
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(theme.text)
            .when(self.quitting, |root| {
                root.child(
                    div()
                        .flex_1()
                        .p_4()
                        .when(titlebar_height > px(0.), |panel| {
                            panel.pt(px(16.) + titlebar_height)
                        })
                        .bg(theme.chrome(theme.background))
                        .child("Cerrando conexiones…"),
                )
            })
            .when(!self.quitting, |root| {
                root.child(sidebar::render(self, theme, titlebar_height, cx))
                    .child(content)
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .top(titlebar_height)
                            .children(transfer_overlay)
                            .children(self.edit_popover(theme, titlebar_height, window, cx))
                            .children(self.render_dialog(theme, cx))
                            .children(self.row_menu.as_ref().and_then(|menu| {
                                let server =
                                    self.servers.iter().find(|server| server.id == menu.id)?;
                                Some(sidebar::row_menu(
                                    menu,
                                    server,
                                    self.connections.get(&menu.id),
                                    theme,
                                    cx,
                                ))
                            })),
                    )
            });
        #[cfg(target_os = "macos")]
        let root = root.when(titlebar_height > px(0.), |root| {
            root.child(macos_titlebar(titlebar_height, theme))
        });
        root
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};

    use super::*;

    #[gpui::test]
    fn a_second_password_prompt_for_the_same_destination_is_rejected(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        let fake = || askpass::Request::fake("prueba", "Contraseña:", askpass::Kind::Password);
        workspace.update_in(cx, |this, window, cx| {
            this.prompts.push_back(fake());
            this.show_next_prompt(window, cx);
        });
        workspace.update(cx, |this, _| {
            let Some(Dialog::Prompt(prompt)) = &this.dialog else {
                panic!("no prompt");
            };
            assert!(!prompt.rejected);
        });
        workspace.update_in(cx, |this, window, cx| this.confirm_dialog(window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, cx| {
            this.prompts.push_back(fake());
            this.show_next_prompt(window, cx);
        });
        workspace.update(cx, |this, _| {
            let Some(Dialog::Prompt(prompt)) = &this.dialog else {
                panic!("no prompt");
            };
            assert!(prompt.rejected);
            assert_eq!(this.password_attempts.get("prueba"), Some(&1));
        });
    }

    #[gpui::test]
    fn a_connection_result_forgets_the_password_attempts(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update(cx, |this, _| {
            this.password_attempts.insert("prueba".into(), 3);
            this.clear_password_attempts("prueba");
            assert!(this.password_attempts.is_empty());
        });
    }

    #[gpui::test]
    fn cancelling_a_password_prompt_forgets_the_attempts(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        let fake = || askpass::Request::fake("prueba", "Contraseña:", askpass::Kind::Password);
        workspace.update_in(cx, |this, window, cx| {
            this.prompts.push_back(fake());
            this.show_next_prompt(window, cx);
        });
        workspace.update_in(cx, |this, window, cx| this.confirm_dialog(window, cx));
        cx.run_until_parked();
        workspace.update(cx, |this, _| {
            assert_eq!(this.password_attempts.get("prueba"), Some(&1))
        });
        workspace.update_in(cx, |this, window, cx| {
            this.prompts.push_back(fake());
            this.show_next_prompt(window, cx);
        });
        workspace.update_in(cx, |this, window, cx| this.cancel_dialog(window, cx));
        cx.run_until_parked();
        workspace.update(cx, |this, _| assert!(this.password_attempts.is_empty()));
    }

    #[test]
    fn spec_fills_a_missing_socket_from_settings_or_detection() {
        let mut server = Server {
            id: "id".into(),
            name: "Nombre".into(),
            host: "host".into(),
            port: 22,
            user: "u".into(),
            auth: Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: None,
            },
            initial_remote_path: None,
            pinned: false,
        };
        let spec = spec_with_socket(&server, Some(Path::new("/tmp/global.sock")), None);
        assert_eq!(
            spec.auth,
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: Some("/tmp/global.sock".into()),
            }
        );
        let spec = spec_with_socket(&server, None, Some(Path::new("/tmp/detected.sock")));
        assert_eq!(
            spec.auth,
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: Some("/tmp/detected.sock".into()),
            }
        );
        server.auth = Auth::Agent {
            identity_pub: "/tmp/key.pub".into(),
            agent_socket: Some("/tmp/own.sock".into()),
        };
        let spec = spec_with_socket(&server, Some(Path::new("/tmp/global.sock")), None);
        assert_eq!(
            spec.auth,
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: Some("/tmp/own.sock".into()),
            }
        );
    }

    #[test]
    fn detection_fills_only_a_missing_agent_socket() {
        let mut spec = ConnectionSpec::new(
            "host".into(),
            22,
            "u".into(),
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: None,
            },
        );
        assert_eq!(
            fill_detected_socket(&mut spec, Some(Path::new("/tmp/detected.sock"))),
            Some(PathBuf::from("/tmp/detected.sock"))
        );
        assert_eq!(
            spec.auth,
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: Some("/tmp/detected.sock".into()),
            }
        );

        let mut spec = ConnectionSpec::new(
            "host".into(),
            22,
            "u".into(),
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: Some("/tmp/own.sock".into()),
            },
        );
        assert_eq!(
            fill_detected_socket(&mut spec, Some(Path::new("/tmp/detected.sock"))),
            None
        );
        assert_eq!(
            spec.auth,
            Auth::Agent {
                identity_pub: "/tmp/key.pub".into(),
                agent_socket: Some("/tmp/own.sock".into()),
            }
        );
    }

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
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
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

    fn row_bounds(cx: &mut VisualTestContext, id: &str) -> gpui::Bounds<gpui::Pixels> {
        let selector: &'static str = Box::leak(format!("server-row-{id}").into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("no row for {id}"))
    }

    /// Presses a card 20 px below its row top and moves just far enough to start
    /// the drag. Returns the cursor position at the start of the drag.
    fn grab(cx: &mut VisualTestContext, id: &str) -> gpui::Point<gpui::Pixels> {
        let row = row_bounds(cx, id);
        let none = Modifiers::none();
        let down = point(row.center().x, row.top() + px(20.));
        cx.simulate_mouse_move(down, None, none);
        cx.simulate_mouse_down(down, MouseButton::Left, none);
        let start = point(down.x, down.y + px(4.));
        cx.simulate_mouse_move(start, MouseButton::Left, none);
        assert!(
            cx.update(|_, cx| cx.has_active_drag()),
            "the drag did not start"
        );
        start
    }

    fn carry(cx: &mut VisualTestContext, to: gpui::Point<gpui::Pixels>) {
        cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    }

    fn release(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        // Let the rows finish gliding, so the next grab finds them in their slots.
        for _ in 0..30 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(50));
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        }
    }

    fn target(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<crate::sidebar::Target> {
        workspace.read_with(cx, |workspace, _| {
            workspace.reorder.as_ref().map(|reorder| reorder.target)
        })
    }

    fn slot(pinned: bool, index: usize) -> Option<crate::sidebar::Target> {
        Some(crate::sidebar::Target { pinned, index })
    }

    /// Five pinned servers a1..a5 and two unpinned ones b1, b2, in this order.
    fn open_many<'a>(
        dir: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        let mut text = String::new();
        for (id, favorite) in [
            ("a1", true),
            ("a2", true),
            ("a3", true),
            ("a4", true),
            ("a5", true),
            ("b1", false),
            ("b2", false),
        ] {
            text.push_str(&format!(
                "[[servers]]\nid = \"{id}\"\nname = \"{id}\"\nhost = \"h\"\nport = 22\nuser = \"u\"\nfavorite = {favorite}\n[servers.auth]\nkind = \"password\"\n\n"
            ));
        }
        fs::write(dir.join("servers.toml"), text).unwrap();
        dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.to_path_buf()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        (workspace, cx)
    }

    fn ids(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
        order(workspace, cx)
            .into_iter()
            .map(|(id, pinned)| format!("{}{id}", if pinned { "*" } else { "" }))
            .collect()
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
    async fn a_card_dragged_by_one_and_six_tenths_rows_targets_two_slots_further(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open_many(dir.path(), cx);
        let start = grab(cx, "a2");
        assert_eq!(target(&workspace, cx), slot(true, 1));
        let row = crate::sidebar::ROW_H;
        let at = |rows: f32| point(start.x, start.y + px(rows * row));
        carry(cx, at(0.4));
        assert_eq!(target(&workspace, cx), slot(true, 1));
        carry(cx, at(0.6));
        assert_eq!(target(&workspace, cx), slot(true, 2));
        carry(cx, at(1.4));
        assert_eq!(target(&workspace, cx), slot(true, 2));
        carry(cx, at(1.6));
        assert_eq!(target(&workspace, cx), slot(true, 3));
        release(cx, at(1.6));
        let expected = ["*a1", "*a3", "*a4", "*a2", "*a5", "b1", "b2"];
        assert_eq!(ids(&workspace, cx), expected);
        let saved = config::load(dir.path()).unwrap();
        assert_eq!(
            saved
                .iter()
                .map(|server| server.id.as_str())
                .collect::<Vec<_>>(),
            ["a1", "a3", "a4", "a2", "a5", "b1", "b2"]
        );
        assert!(workspace.read_with(cx, |workspace, _| workspace.reorder.is_none()));
        assert!(!cx.update(|_, cx| cx.has_active_drag()));
    }

    #[gpui::test]
    async fn crossing_the_divider_changes_the_group_and_saves_the_pin(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open_many(dir.path(), cx);
        let start = grab(cx, "a5");
        assert_eq!(target(&workspace, cx), slot(true, 4));
        // Below the last pinned slot and the divider: the unpinned group. The open
        // slot moves with the card, so the card ends up over the group's second slot.
        let row = crate::sidebar::ROW_H;
        let at = |rows: f32| point(start.x, start.y + px(rows * row));
        carry(cx, at(0.3));
        assert_eq!(target(&workspace, cx), slot(true, 4));
        carry(cx, at(1.2));
        assert_eq!(target(&workspace, cx), slot(false, 1));
        carry(cx, at(1.5));
        assert_eq!(target(&workspace, cx), slot(false, 1));
        // The divider is crossed back once the card is above its shifted position.
        carry(cx, at(-0.5));
        assert_eq!(target(&workspace, cx), slot(true, 4));
        carry(cx, at(2.6));
        assert_eq!(target(&workspace, cx), slot(false, 2));
        release(cx, at(2.6));
        assert_eq!(
            ids(&workspace, cx),
            ["*a1", "*a2", "*a3", "*a4", "b1", "b2", "a5"]
        );
        let saved = config::load(dir.path()).unwrap();
        assert_eq!(
            saved
                .iter()
                .map(|server| (server.id.as_str(), server.pinned))
                .collect::<Vec<_>>(),
            [
                ("a1", true),
                ("a2", true),
                ("a3", true),
                ("a4", true),
                ("b1", false),
                ("b2", false),
                ("a5", false)
            ]
        );
        // An unpinned card dragged over the top of the pinned group pins it.
        grab(cx, "b2");
        let top = point(px(100.), row_bounds(cx, "a1").top() + px(4.));
        carry(cx, top);
        assert_eq!(target(&workspace, cx), slot(true, 0));
        release(cx, top);
        assert_eq!(
            ids(&workspace, cx),
            ["*b2", "*a1", "*a2", "*a3", "*a4", "b1", "a5"]
        );
    }

    #[gpui::test]
    async fn releasing_outside_the_list_or_pressing_escape_cancels_without_saving(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open_many(dir.path(), cx);
        cx.update(|_, cx| bind_keys(cx));
        let before = fs::read_to_string(dir.path().join("servers.toml")).unwrap();
        let initial = ids(&workspace, cx);

        // Released over the main area.
        let start = grab(cx, "a2");
        carry(cx, point(start.x, start.y + px(100.)));
        assert_eq!(target(&workspace, cx), slot(true, 3));
        let outside = point(px(600.), px(300.));
        carry(cx, outside);
        release(cx, outside);
        assert_eq!(ids(&workspace, cx), initial);
        assert_eq!(
            fs::read_to_string(dir.path().join("servers.toml")).unwrap(),
            before
        );
        assert!(workspace.read_with(cx, |workspace, _| workspace.reorder.is_none()));
        assert!(!cx.update(|_, cx| cx.has_active_drag()));

        // Escape during the drag.
        let start = grab(cx, "a3");
        carry(cx, point(start.x, start.y + px(100.)));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |workspace, _| workspace.reorder.is_none()));
        assert!(!cx.update(|_, cx| cx.has_active_drag()));
        release(cx, point(start.x, start.y + px(100.)));
        assert_eq!(ids(&workspace, cx), initial);
        assert_eq!(
            fs::read_to_string(dir.path().join("servers.toml")).unwrap(),
            before
        );
    }

    #[gpui::test]
    async fn dropping_a_card_on_its_own_slot_changes_nothing(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open_many(dir.path(), cx);
        let initial = ids(&workspace, cx);
        let start = grab(cx, "a3");
        carry(cx, point(start.x, start.y + px(10.)));
        assert_eq!(target(&workspace, cx), slot(true, 2));
        release(cx, point(start.x, start.y + px(10.)));
        assert_eq!(ids(&workspace, cx), initial);
    }

    #[gpui::test]
    async fn without_pinned_servers_the_heading_takes_a_dropped_card(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open(dir.path(), cx);
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_pin("id-bravo", cx);
        });
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_pin("id-zeta", cx);
        });
        cx.run_until_parked();
        // Everything is unpinned now. Dragging above the list targets the pinned group.
        let first = order(&workspace, cx)[0].0.clone();
        assert!(!order(&workspace, cx)[0].1);
        let heading = cx.debug_bounds("sidebar-heading").unwrap().center();
        grab(cx, &first);
        carry(cx, heading);
        assert_eq!(target(&workspace, cx), slot(true, 0));
        release(cx, heading);
        assert!(order(&workspace, cx)[0].1);
        assert_eq!(order(&workspace, cx)[0].0, first);
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

    #[gpui::test]
    async fn duplicating_a_server_saves_an_independent_copy(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let key = AgentKey {
            key_type: "ssh-ed25519".into(),
            blob: [&[0, 0, 0, 11][..], b"ssh-ed25519"].concat(),
            comment: "Servidor".into(),
        };
        let key_path = agent::store_public_key(dir.path(), &key).unwrap();
        let original = Server {
            id: "original".into(),
            name: "Servidor principal".into(),
            host: "h.example".into(),
            port: 2201,
            user: "ana".into(),
            auth: Auth::Agent {
                identity_pub: key_path.clone(),
                agent_socket: Some("/tmp/agente".into()),
            },
            initial_remote_path: Some("/srv/ñ".into()),
            pinned: true,
        };
        config::save(dir.path(), vec![original.clone()]).unwrap();
        dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_path_buf()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_duplicate_form("original", window, cx)
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            let Some(Dialog::Form(form)) = &workspace.dialog else {
                panic!("the duplicate form is not open");
            };
            assert_eq!(form.id, None);
            let value = |field| form.input(field).read(cx).value();
            assert_eq!(value(Field::Name), "Servidor principal duplicate");
            assert_eq!(value(Field::Host), "h.example");
            assert_eq!(value(Field::Port), "2201");
            assert_eq!(value(Field::User), "ana");
            assert_eq!(value(Field::Socket), "/tmp/agente");
            assert_eq!(value(Field::RemotePath), "/srv/ñ");
            let submission = form.validated(cx).unwrap();
            assert_eq!(submission.server.name, "Servidor principal duplicate");
            assert_eq!(submission.server.auth, original.auth);
            assert!(submission.key.is_none());
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.confirm_dialog(window, cx)
        });
        cx.run_until_parked();
        let servers = config::load(dir.path()).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0], original);
        let copy = &servers[1];
        assert_ne!(copy.id, original.id);
        assert!(!copy.pinned);
        assert_eq!(copy.name, "Servidor principal duplicate");
        assert_eq!(copy.auth, original.auth);
        assert_eq!(copy.initial_remote_path, original.initial_remote_path);

        // Deleting either server keeps the shared key file for the other one.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_delete(original.id.clone(), window, cx)
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.confirm_dialog(window, cx)
        });
        cx.run_until_parked();
        let remaining = config::load(dir.path()).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, copy.id);
        assert_eq!(remaining[0].auth, original.auth);
        assert!(key_path.exists());
    }

    #[gpui::test]
    async fn the_five_item_row_menu_keeps_its_tab_wrap(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = open(dir.path(), cx);
        cx.update(|_, cx| bind_keys(cx));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_row_menu("id-bravo", point(px(100.), px(100.)), window, cx)
        });
        cx.run_until_parked();
        let (first, last) = workspace.read_with(cx, |workspace, _| {
            let menu = workspace.row_menu.as_ref().unwrap();
            (menu.first.clone(), menu.last.clone())
        });
        cx.update(|window, cx| assert_eq!(window.focused(cx), Some(first.clone())));
        for _ in 0..4 {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
        }
        cx.update(|window, cx| assert_eq!(window.focused(cx), Some(last.clone())));
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        cx.update(|window, cx| assert_eq!(window.focused(cx), Some(first)));
    }
}
