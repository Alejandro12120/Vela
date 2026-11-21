use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    path::PathBuf,
    rc::Rc,
    time::Instant,
};

use gpui::{
    AnyElement, AppContext, Context, Entity, FocusHandle, Focusable, FontWeight, KeyBinding,
    PathPromptOptions, Render, Window, actions, div, prelude::*, px,
};
use vscp_core::{
    Auth, Client, Error, Session, askpass,
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
    form::{Field, Form},
    pane::{PaneMemory, icon_button},
    pane_icons::Icon,
    theme::Theme,
};

actions!(workspace, [Next, Previous, DismissMenu, ToggleSidebar]);

pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys([
        KeyBinding::new("tab", Next, Some("Workspace")),
        KeyBinding::new("shift-tab", Previous, Some("Workspace")),
        KeyBinding::new("escape", DismissMenu, Some("Workspace")),
        KeyBinding::new("ctrl-cmd-s", ToggleSidebar, Some("Workspace")),
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
}

pub struct Workspace {
    pub servers: Vec<Server>,
    pub selected: Option<String>,
    pub connections: HashMap<String, Connection>,
    pub row_menu: Option<String>,
    pub dir: Option<PathBuf>,
    pub saving: bool,
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
            Ok::<_, std::io::Error>((dir, servers, agent))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok((dir, mut servers, agent)) => {
                        crate::sidebar::sort_servers(&mut servers);
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
        cx.on_app_quit(|this, _| this.shutdown()).detach();

        Self {
            servers: Vec::new(),
            selected: None,
            connections: HashMap::new(),
            row_menu: None,
            dir: None,
            saving: false,
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

    fn shutdown(&mut self) -> impl Future<Output = ()> + use<> {
        self.dialog = None;
        self.prompt_task.take();
        self.prompts.clear();
        let client = self.client.clone();
        async move {
            if let Some(client) = client {
                client.shutdown().await;
            }
        }
    }

    pub fn request_quit(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        let shutdown = self.shutdown();
        cx.notify();
        cx.spawn_in(window, async move |_, cx| {
            shutdown.await;
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
    }

    pub fn select(&mut self, id: &str) {
        if self.selected.as_deref() != Some(id) {
            self.show_stderr = false;
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
                let path = match server.initial_remote_path {
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
                    Ok((session, path)) => this.connected(&server.id, session, path, window, cx),
                    Err(error) => {
                        dev::log(format_args!("connection failed for {}", server.name));
                        Connection::Error(error)
                    }
                };
                this.connections.insert(server.id, state);
                cx.notify();
            });
        }).detach();
    }

    fn connected(
        &mut self,
        id: &str,
        session: Session,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Connection {
        let memory = self.pane_memory.entry(id.to_owned()).or_default().clone();
        let panes = Panes::new(
            session.clone(),
            path,
            memory,
            self.sidebar_visible.clone(),
            window,
            cx,
        );
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
        let form = Form::new(server, &self.agent_socket, cx);
        self.open_dialog(Dialog::Form(Box::new(form)), window, cx);
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
        let Some(input) = self
            .editing()
            .map(|form| form.input(Field::Identity).clone())
        else {
            return;
        };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Elegir clave pública".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(form) = this.editing()
                    && form.input(Field::Identity) == &input
                {
                    match result {
                        Ok(Ok(Some(paths))) => {
                            if let Some(path) = paths.first().and_then(|path| path.to_str()) {
                                input.update(cx, |input, cx| input.set_value(path, window, cx));
                                form.errors.retain(|(field, _)| *field != Field::Identity);
                                form.message = None;
                            } else {
                                form.message =
                                    Some("La ruta de la clave pública no se puede leer.");
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
            Dialog::Delete(_) => None,
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
                let server = match form.validated(cx) {
                    Ok(server) => server,
                    Err(errors) => {
                        form.input(errors[0].0)
                            .read(cx)
                            .focus_handle(cx)
                            .focus(window, cx);
                        form.errors = errors;
                        cx.notify();
                        return;
                    }
                };
                let id = form.id.clone();
                self.persist(Some(server), id, window, cx);
            }
            Some(Dialog::Delete(id)) => {
                let id = id.clone();
                self.persist(None, Some(id), window, cx);
            }
            _ => {}
        }
    }

    fn persist(
        &mut self,
        server: Option<Server>,
        existing: Option<String>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.dir.clone() else { return };
        let mut servers = self.servers.clone();
        self.saving = true;
        cx.notify();
        let save = cx.background_executor().spawn(async move {
            let mut server = server;
            if let Some(server) = &mut server {
                server.id = match &existing {
                    Some(id) => id.clone(),
                    None => Server::new_id()?,
                };
            }
            servers.retain(|server| Some(&server.id) != existing.as_ref());
            if let Some(server) = &server {
                servers.push(server.clone());
            }
            crate::sidebar::sort_servers(&mut servers);
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
        panel.child(details)
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
            .on_action(cx.listener(|_, _: &Next, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &Previous, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|this, _: &DismissMenu, _, cx| {
                this.row_menu = None;
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
                .children(self.render_dialog(theme, cx))
            })
    }
}
