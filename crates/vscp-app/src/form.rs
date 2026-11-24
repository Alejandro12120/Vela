use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    KeyDownEvent, Role, SharedString, Stateful, Subscription, Window, div, prelude::*, px,
};
use vscp_core::{
    Auth,
    agent::{AgentError, AgentKey, read_public_key},
    config::Server,
};

use crate::{
    components::{button::Variant, radio::radio_dot, segmented::segmented, text_input::TextInput},
    icons::{IconName, icon},
    theme::Theme,
    workspace::Workspace,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Field {
    Name,
    Host,
    Port,
    User,
    Socket,
    RemotePath,
}

const FIELDS: [(Field, &str, &str); 6] = [
    (Field::Name, "Nombre (opcional)", "Igual que el host"),
    (Field::Host, "Host", "servidor.ejemplo"),
    (Field::Port, "Puerto", "22"),
    (Field::User, "Usuario", ""),
    (Field::Socket, "Socket del agente", "El de Ajustes"),
    (
        Field::RemotePath,
        "Carpeta inicial",
        "Carpeta personal del servidor",
    ),
];

type FieldError = (Field, &'static str);

const NO_KEY: &str = "Elige una clave";
const INVALID_FILE: &str = "El archivo no es una clave pública válida.";
const UNLOCK_HINT_AFTER: Duration = Duration::from_secs(2);

/// The key chosen for agent authentication.
#[derive(Clone, Debug, PartialEq)]
pub enum Identity {
    None,
    /// A key listed by the agent. It is written to `keys/` when the form is saved.
    Agent(AgentKey),
    /// The key file already stored in `keys/` for the server being edited.
    Stored(PathBuf),
    /// A public key file chosen by the user. `key` is `None` when it cannot be
    /// parsed. `chosen` is false for the path a server already had.
    File {
        path: PathBuf,
        key: Option<AgentKey>,
        chosen: bool,
    },
}

enum Keys {
    Loading(Instant),
    Ready(Vec<AgentKey>),
    Failed(AgentError),
}

/// A validated form. `key` is the agent key to store in `keys/` before saving.
#[derive(Debug)]
pub struct Submission {
    pub server: Server,
    pub key: Option<AgentKey>,
}

#[derive(Debug)]
pub struct Errors {
    pub fields: Vec<FieldError>,
    pub key: Option<&'static str>,
}

pub struct Form {
    pub id: Option<String>,
    inputs: [Entity<TextInput>; FIELDS.len()],
    agent: bool,
    pub errors: Vec<FieldError>,
    pub key_error: Option<&'static str>,
    pub message: Option<&'static str>,
    identity: Identity,
    /// The server's key file inside `keys/`, as read when the form opened.
    existing: Option<(PathBuf, AgentKey)>,
    keys: Keys,
    request: u64,
    listed_socket: String,
    effective_socket: String,
    row_focus: Vec<FocusHandle>,
    blur: Option<Subscription>,
    advanced: bool,
    preselect: Option<String>,
}

impl Form {
    pub fn new(
        server: Option<&Server>,
        default_socket: &str,
        config_dir: Option<&Path>,
        cx: &mut App,
    ) -> Self {
        Self::build(server, false, default_socket, config_dir, cx)
    }

    /// Opens the add-server form prefilled from `server`, but with no id and a
    /// suffixed name, so saving appends an independent copy.
    pub fn duplicate(
        server: &Server,
        default_socket: &str,
        config_dir: Option<&Path>,
        cx: &mut App,
    ) -> Self {
        Self::build(Some(server), true, default_socket, config_dir, cx)
    }

    fn build(
        server: Option<&Server>,
        duplicate: bool,
        default_socket: &str,
        config_dir: Option<&Path>,
        cx: &mut App,
    ) -> Self {
        let mut values = [""; FIELDS.len()];
        values[Field::Port as usize] = "22";
        let _ = default_socket;
        let mut port = String::new();
        let mut agent = true;
        let mut identity = Identity::None;
        let mut existing = None;
        let mut duplicate_name = None;
        if let Some(server) = server {
            if duplicate {
                duplicate_name = Some(format!("{} duplicate", server.display_name()));
            }
            port.push_str(&server.port.to_string());
            values[Field::Name as usize] = duplicate_name.as_deref().unwrap_or(&server.name);
            values[Field::Host as usize] = &server.host;
            values[Field::Port as usize] = &port;
            values[Field::User as usize] = &server.user;
            values[Field::RemotePath as usize] = server
                .initial_remote_path
                .as_deref()
                .and_then(|path| path.to_str())
                .unwrap_or_default();
            match &server.auth {
                Auth::Agent {
                    identity_pub,
                    agent_socket,
                } => {
                    values[Field::Socket as usize] = agent_socket
                        .as_deref()
                        .and_then(|path| path.to_str())
                        .unwrap_or_default();
                    let key = read_public_key(identity_pub).ok();
                    let stored =
                        config_dir.is_some_and(|dir| identity_pub.starts_with(dir.join("keys")));
                    identity = match key {
                        Some(key) if stored => {
                            existing = Some((identity_pub.clone(), key));
                            Identity::Stored(identity_pub.clone())
                        }
                        key => Identity::File {
                            path: identity_pub.clone(),
                            key,
                            chosen: false,
                        },
                    };
                }
                Auth::Password => agent = false,
            }
        }
        let advanced = server.is_some_and(|server| {
            server.initial_remote_path.is_some()
                || matches!(
                    &server.auth,
                    Auth::Agent {
                        agent_socket: Some(_),
                        ..
                    }
                )
        });
        let inputs = FIELDS.map(|(field, label, placeholder)| {
            cx.new(|cx| TextInput::new(values[field as usize], label, placeholder, false, cx))
        });
        Self {
            id: if duplicate {
                None
            } else {
                server.map(|server| server.id.clone())
            },
            inputs,
            agent,
            errors: Vec::new(),
            key_error: None,
            message: None,
            identity,
            existing,
            keys: Keys::Loading(Instant::now()),
            request: 0,
            listed_socket: String::new(),
            effective_socket: String::new(),
            row_focus: Vec::new(),
            blur: None,
            advanced,
            preselect: None,
        }
    }

    pub fn input(&self, field: Field) -> &Entity<TextInput> {
        &self.inputs[field as usize]
    }

    pub fn set_blur(&mut self, subscription: Subscription) {
        self.blur = Some(subscription);
    }

    pub fn is_composing(&self, window: &Window, cx: &App) -> bool {
        self.inputs.iter().any(|input| {
            let input = input.read(cx);
            input.is_composing() && input.focus_handle(cx).is_focused(window)
        })
    }

    pub fn validated(&self, cx: &App) -> Result<Submission, Errors> {
        validate(
            self.inputs.each_ref().map(|input| input.read(cx).value()),
            self.agent,
            &self.identity,
        )
    }

    fn socket(&self, cx: &App) -> String {
        self.input(Field::Socket).read(cx).value().trim().to_owned()
    }

    /// Starts a listing for `request` and returns the socket to ask. The list
    /// stays live in file mode so an agent row can switch back to it.
    pub fn begin_listing(&mut self, request: u64, cx: &App) -> Option<String> {
        if !self.agent {
            return None;
        }
        self.request = request;
        self.keys = Keys::Loading(Instant::now());
        self.listed_socket = self.socket(cx);
        self.row_focus.clear();
        Some(self.listed_socket.clone())
    }

    /// Records the socket actually used, so the header can name the agent.
    pub fn note_socket(&mut self, socket: &str) {
        self.effective_socket = socket.to_owned();
    }

    /// Whether the Socket field now differs from the one last listed.
    pub fn socket_changed(&self, cx: &App) -> bool {
        self.agent && self.socket(cx) != self.listed_socket
    }

    /// Applies a listing unless a newer one was started since. `handles` has one
    /// focus handle per possible row.
    pub fn finish_listing(
        &mut self,
        request: u64,
        result: Result<Vec<AgentKey>, AgentError>,
        handles: Vec<FocusHandle>,
    ) {
        if request != self.request {
            return;
        }
        self.keys = match result {
            Ok(keys) => {
                if let (Identity::Stored(_), Some((_, stored))) = (&self.identity, &self.existing)
                    && let Some(listed) = keys
                        .iter()
                        .find(|key| key.fingerprint() == stored.fingerprint())
                {
                    self.identity = Identity::Agent(listed.clone());
                }
                Keys::Ready(keys)
            }
            Err(error) => Keys::Failed(error),
        };
        self.row_focus = handles;
        if let Some(comment) = self.preselect.take() {
            let rows = self.rows();
            if let Some(index) = rows.iter().position(|row| row.key.comment == comment) {
                self.pick_row(index);
            }
        }
    }

    pub fn pick_file(&mut self, path: PathBuf) {
        let key = read_public_key(&path).ok();
        self.identity = Identity::File {
            path,
            key,
            chosen: true,
        };
        self.key_error = None;
        self.message = None;
    }

    pub fn preselect(&mut self, comment: String) {
        self.preselect = Some(comment);
    }

    pub fn set_agent(&mut self, agent: bool) {
        self.agent = agent;
    }

    pub fn toggle_advanced(&mut self) {
        self.advanced = !self.advanced;
    }

    /// Opens the advanced block so a field with an error can be focused.
    pub fn reveal_field(&mut self, field: Field) {
        if matches!(field, Field::Socket | Field::RemotePath) {
            self.advanced = true;
        }
    }

    fn agent_name(&self) -> &'static str {
        let socket = Path::new(&self.effective_socket);
        if vscp_core::bitwarden_agent_sockets()
            .iter()
            .any(|path| path == socket)
        {
            "Bitwarden"
        } else {
            "Agente SSH"
        }
    }

    fn status(&self, theme: Theme) -> (&'static str, gpui::Rgba) {
        match &self.keys {
            Keys::Loading(_) => ("consultando…", theme.secondary_text),
            Keys::Ready(keys) if keys.is_empty() => ("bloqueado", theme.warning),
            Keys::Ready(_) => ("disponible", theme.success),
            Keys::Failed(error) => match error {
                AgentError::Unavailable => ("no encontrado", theme.secondary_text),
                AgentError::Refused | AgentError::Timeout => ("bloqueado", theme.warning),
                AgentError::Protocol(_) => ("error", theme.danger),
            },
        }
    }

    pub fn focus_key(&self, window: &mut Window, cx: &mut App) {
        if let Some(handle) = self.row_focus.first() {
            handle.focus(window, cx);
        }
    }

    /// Key rows in display order. The server's stored key comes first when the
    /// agent does not list it.
    fn rows(&self) -> Vec<Row> {
        let listed: &[AgentKey] = match &self.keys {
            Keys::Ready(keys) => keys,
            Keys::Failed(_) => &[],
            Keys::Loading(_) => return Vec::new(),
        };
        let mut rows = Vec::new();
        if let Some((path, key)) = &self.existing
            && !listed
                .iter()
                .any(|listed| listed.fingerprint() == key.fingerprint())
        {
            rows.push(Row {
                pick: Identity::Stored(path.clone()),
                key: key.clone(),
                note: Some("No está en Bitwarden ahora mismo"),
            });
        }
        rows.extend(listed.iter().map(|key| Row {
            pick: Identity::Agent(key.clone()),
            key: key.clone(),
            note: None,
        }));
        rows
    }

    fn pick_row(&mut self, row: usize) -> bool {
        let rows = self.rows();
        if let Some(row_data) = rows.get(row) {
            self.identity = row_data.pick.clone();
            self.key_error = None;
            true
        } else {
            false
        }
    }

    fn select(&mut self, row: usize, window: &mut Window, cx: &mut App) {
        if self.pick_row(row)
            && let Some(handle) = self.row_focus.get(row)
        {
            handle.focus(window, cx);
        }
    }

    fn selected_row(&self, rows: &[Row]) -> Option<usize> {
        rows.iter()
            .position(|row| match (&self.identity, &row.pick) {
                (Identity::Agent(chosen), Identity::Agent(key)) => {
                    chosen.fingerprint() == key.fingerprint()
                }
                (Identity::Stored(chosen), Identity::Stored(path)) => chosen == path,
                _ => false,
            })
    }

    pub fn render(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let mut fields = div().flex().flex_col().gap_2();
        for (field, _, _) in FIELDS {
            if matches!(field, Field::Port | Field::Socket | Field::RemotePath) {
                continue;
            }
            fields = match field {
                Field::Host => fields.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(self.field(Field::Host, theme, cx)),
                        )
                        .child(div().w(px(76.)).child(self.field(Field::Port, theme, cx))),
                ),
                Field::User => {
                    let fields = fields
                        .child(self.field(field, theme, cx))
                        .child(self.auth(theme, cx));
                    if self.agent {
                        fields.child(self.key_section(theme, cx))
                    } else {
                        fields
                    }
                }
                _ => fields.child(self.field(field, theme, cx)),
            };
        }
        fields
            .child(self.advanced_options(theme, cx))
            .when_some(self.message, |form, message| {
                form.child(div().text_color(theme.danger).child(message))
            })
    }

    fn field(&self, field: Field, theme: Theme, _: &Context<Workspace>) -> Div {
        let (_, label, _) = FIELDS[field as usize];
        let label = if field == Field::Name {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child("Nombre"))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.secondary_text)
                        .child("(opcional)"),
                )
        } else {
            div().font_weight(FontWeight::MEDIUM).child(label)
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label)
            .child(div().child(self.input(field).clone()))
            .when_some(
                self.errors.iter().find(|(key, _)| *key == field),
                |field, (_, message)| field.child(error_text(message, theme)),
            )
    }

    fn auth(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let listener = cx.listener(|this: &mut Workspace, index: &usize, window, cx| {
            let agent = *index == 0;
            let Some(form) = this.editing() else { return };
            let changed = form.agent != agent;
            form.agent = agent;
            cx.notify();
            if changed && agent {
                this.refresh_keys(window, cx);
            }
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(icon_label(IconName::Lock, "Autenticación", theme))
            .child(segmented(
                "form-auth",
                &["Clave SSH", "Contraseña"],
                if self.agent { 0 } else { 1 },
                theme,
                move |index, window, cx| listener(&index, window, cx),
            ))
    }

    fn advanced_options(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let open = self.advanced;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .id("advanced-toggle")
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .aria_expanded(open)
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(24.))
                    .text_size(px(13.))
                    .text_color(theme.secondary_text)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(theme.text))
                    .focus(move |style| style.border_color(theme.accent))
                    .child(crate::components::motion::chevron(
                        icon(IconName::ChevronRight)
                            .size(px(14.))
                            .text_color(theme.secondary_text),
                        "advanced-chevron",
                        open,
                        true,
                    ))
                    .child("Opciones avanzadas")
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(form) = this.editing() {
                            form.toggle_advanced();
                        }
                        cx.notify();
                        let _ = window;
                    })),
            )
            .when(open, |block| {
                block.child(self.field(Field::RemotePath, theme, cx))
            })
            .when(open && self.agent, |block| {
                block.child(self.field(Field::Socket, theme, cx))
            })
    }

    fn key_section(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let (word, color) = self.status(theme);
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(10.))
            .py(px(7.))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .text_color(theme.secondary_text)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.agent_name())
                    .when(matches!(self.keys, Keys::Loading(_)), |row| {
                        row.child(crate::components::motion::spin(
                            icon(IconName::Spinner)
                                .size(px(12.))
                                .text_color(theme.secondary_text),
                            "agent-spinner",
                        ))
                    })
                    .when(!matches!(self.keys, Keys::Loading(_)), |row| {
                        row.child(crate::components::status_dot::status_dot(color, None))
                    })
                    .child(div().text_color(color).child(word)),
            )
            .child(
                crate::components::button::icon_button(
                    "refresh-keys",
                    "Volver a consultar",
                    IconName::Refresh,
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.refresh_keys(window, cx)),
                )
                .size(px(22.)),
            );

        let notice = self.key_notice(theme, cx);
        let rows = self.rows();
        let selected = self.selected_row(&rows);
        let list = div()
            .id("agent-keys")
            .role(Role::ListBox)
            .aria_label("Claves de Bitwarden")
            .max_h(px(170.))
            .overflow_y_scroll()
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let step = match event.keystroke.key.as_str() {
                    "down" => 1,
                    "up" => -1,
                    _ => return,
                };
                cx.stop_propagation();
                if let Some(form) = this.editing() {
                    let rows = form.rows();
                    if let Some(next) = step_row(rows.len(), form.selected_row(&rows), step) {
                        form.select(next, window, cx);
                        cx.notify();
                    }
                }
            }))
            .children(rows.iter().enumerate().map(|(index, row)| {
                let handle = self.row_focus.get(index).cloned();
                key_row(row, index, selected == Some(index), handle, theme, cx)
            }));

        let last = match &self.identity {
            Identity::File { path, key, .. } => file_row(path, key.as_ref(), theme, cx),
            _ => div()
                .id("from-file")
                .role(Role::Button)
                .tab_index(0)
                .flex()
                .items_center()
                .gap_2()
                .px(px(10.))
                .py(px(7.))
                .border_t_1()
                .border_color(theme.border)
                .text_size(px(13.))
                .text_color(theme.accent)
                .cursor_pointer()
                .hover(move |style| style.bg(theme.hover))
                .focus(move |style| style.border_color(theme.accent))
                .child(icon(IconName::Plus).size(px(14.)).text_color(theme.accent))
                .child("Usar un archivo de clave…")
                .on_click(cx.listener(|this, _, window, cx| this.choose_identity(window, cx))),
        };

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(icon_label(IconName::Key, "Clave", theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.control_border)
                    .bg(theme.panel)
                    .overflow_hidden()
                    .child(header)
                    .children(notice)
                    .child(list)
                    .child(last),
            )
            .when_some(self.key_error, |section, message| {
                section.child(error_text(message, theme))
            })
    }

    fn key_notice(&self, theme: Theme, cx: &Context<Workspace>) -> Option<Div> {
        let text: &'static str = match &self.keys {
            Keys::Loading(since) => {
                return Some(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .px(px(10.))
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .child(
                            icon(IconName::Error)
                                .size(px(14.))
                                .mt(px(1.))
                                .text_color(theme.warning),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_w_0()
                                .when(since.elapsed() >= UNLOCK_HINT_AFTER, |text| {
                                    text.child("Si Bitwarden pide desbloquear, hazlo y espera.")
                                }),
                        ),
                );
            }
            Keys::Ready(keys) if keys.is_empty() => {
                "Bitwarden no tiene claves SSH disponibles o está bloqueado."
            }
            Keys::Ready(_) => return None,
            Keys::Failed(error) => match error {
                AgentError::Unavailable => {
                    "No se encuentra el agente SSH de Bitwarden. Abre Bitwarden y activa el agente SSH en Ajustes."
                }
                AgentError::Refused => {
                    "Bitwarden rechazó la consulta. Desbloquéalo e inténtalo de nuevo."
                }
                AgentError::Timeout => {
                    "Bitwarden no respondió a tiempo. Desbloquéalo e inténtalo de nuevo."
                }
                AgentError::Protocol(_) => "El agente SSH respondió de forma inesperada.",
            },
        };
        let _ = cx;
        Some(
            div()
                .flex()
                .items_start()
                .gap_2()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(12.))
                .text_color(theme.secondary_text)
                .child(
                    icon(IconName::Error)
                        .size(px(14.))
                        .mt(px(1.))
                        .text_color(theme.warning),
                )
                .child(div().flex_1().min_w_0().child(text)),
        )
    }
}

struct Row {
    pick: Identity,
    key: AgentKey,
    note: Option<&'static str>,
}

/// The row an arrow key moves to. Without a selection, Down picks the first row
/// and Up the last. The selection stops at either end.
fn step_row(len: usize, selected: Option<usize>, step: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match selected {
        None if step > 0 => 0,
        None => len - 1,
        Some(index) => index.saturating_add_signed(step).min(len - 1),
    })
}

/// `SHA256:abcd…wxyz` becomes `…wxyz`: only the last four hash characters.
fn short_fingerprint(fingerprint: &str) -> String {
    let hash = fingerprint
        .split_once(':')
        .map_or(fingerprint, |(_, hash)| hash);
    let tail: String = hash
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// The type and short fingerprint shown on a key row.
fn key_detail_text(key_type: &str, fingerprint: &str) -> String {
    let family = key_type.strip_prefix("ssh-").unwrap_or(key_type);
    format!("{family} · {}", short_fingerprint(fingerprint))
}

fn key_row(
    row: &Row,
    index: usize,
    selected: bool,
    handle: Option<FocusHandle>,
    theme: Theme,
    cx: &Context<Workspace>,
) -> Stateful<Div> {
    let name = if row.key.comment.is_empty() {
        "Sin nombre".to_owned()
    } else {
        row.key.comment.clone()
    };
    let fingerprint = row.key.fingerprint();
    let detail = if let Some(note) = row.note {
        div()
            .truncate()
            .text_size(px(12.))
            .text_color(theme.warning)
            .child(note)
    } else {
        div()
            .truncate()
            .text_size(px(11.5))
            .font_family(crate::theme::monospace())
            .text_color(theme.secondary_text)
            .child(key_detail_text(&row.key.key_type, &fingerprint))
    };
    div()
        .id(SharedString::from(format!("key-{index}")))
        .role(Role::ListBoxOption)
        .aria_label(format!("{name}, {}, {fingerprint}", row.key.key_type))
        .aria_selected(selected)
        .when_some(handle, |row, handle| {
            row.track_focus(&handle.tab_stop(true))
        })
        .tab_index(0)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(9.))
        .h(px(32.))
        .px(px(10.))
        .py(px(7.))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .when(selected, |row| {
            row.bg(gpui::Hsla::from(theme.accent).opacity(0.14))
        })
        .when(!selected, |row| {
            row.hover(move |style| style.bg(theme.hover))
        })
        .focus(move |style| style.border_color(theme.accent))
        .child(radio_dot(selected, theme))
        .child(
            div()
                .truncate()
                .flex_1()
                .min_w_0()
                .text_size(px(13.))
                .child(name.clone()),
        )
        .child(detail)
        .on_click(cx.listener(move |this, _, window, cx| {
            if let Some(form) = this.editing() {
                form.select(index, window, cx);
                cx.notify();
            }
        }))
}

fn file_row(
    path: &Path,
    key: Option<&AgentKey>,
    theme: Theme,
    cx: &Context<Workspace>,
) -> Stateful<Div> {
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned();
    let detail = match key {
        Some(key) => div()
            .truncate()
            .text_size(px(11.5))
            .font_family(crate::theme::monospace())
            .text_color(theme.secondary_text)
            .child(key_detail_text(&key.key_type, &key.fingerprint())),
        None => div()
            .truncate()
            .text_size(px(12.))
            .text_color(theme.danger)
            .child(INVALID_FILE),
    };
    div()
        .id("file-key-row")
        .role(Role::ListBoxOption)
        .aria_selected(true)
        .flex()
        .items_center()
        .gap(px(9.))
        .min_h(px(32.))
        .px(px(10.))
        .py(px(7.))
        .border_t_1()
        .border_color(theme.border)
        .bg(gpui::Hsla::from(theme.accent).opacity(0.14))
        .child(radio_dot(true, theme))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().text_size(px(13.)).child(name))
                .child(detail),
        )
        .child(link(
            "change-file",
            "Cambiar…",
            theme,
            cx.listener(|this, _, window, cx| this.choose_identity(window, cx)),
        ))
}

fn icon_label(name: IconName, label: &'static str, theme: Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .font_weight(FontWeight::MEDIUM)
        .child(icon(name).size(px(14.)).text_color(theme.text))
        .child(label)
}

fn error_text(message: &'static str, theme: Theme) -> Div {
    div()
        .text_size(px(12.))
        .text_color(theme.danger)
        .child(message)
}

/// A text action with the accent colour, for secondary choices.
fn link(
    id: &'static str,
    label: &'static str,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .tab_index(0)
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .px_1()
        .rounded(px(4.))
        .border_1()
        .border_color(gpui::transparent_black())
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.accent)
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .focus(move |style| style.border_color(theme.accent))
        .child(label)
        .on_click(on_click)
}

fn validate(
    values: [&str; FIELDS.len()],
    agent: bool,
    identity: &Identity,
) -> Result<Submission, Errors> {
    let [name, host, port, user, socket, path] = values.map(str::trim);
    let mut fields = Vec::new();
    if host.is_empty()
        || host.starts_with('-')
        || host.contains(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '@' | '/'))
    {
        fields.push((Field::Host, "Escribe un nombre de host o una dirección IP."));
    }
    let valid_port = port.bytes().all(|byte| byte.is_ascii_digit());
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|port| valid_port && *port > 0);
    if port.is_none() {
        fields.push((Field::Port, "Usa un número del 1 al 65535."));
    }
    if user.is_empty() || user.contains(|c: char| c.is_whitespace() || c.is_control() || c == '@') {
        fields.push((Field::User, "Escribe un usuario válido."));
    }
    // The agent key's file is only known once it is stored, so it stays empty.
    let (identity_pub, key) = match (agent, identity) {
        (false, _) => (PathBuf::new(), None),
        (true, Identity::None) => (PathBuf::new(), Some(Err(NO_KEY))),
        (true, Identity::Agent(key)) => (PathBuf::new(), Some(Ok(key.clone()))),
        (true, Identity::Stored(path)) => (path.clone(), None),
        (
            true,
            Identity::File {
                key: None,
                chosen: true,
                ..
            },
        ) => (PathBuf::new(), Some(Err(INVALID_FILE))),
        (true, Identity::File { path, .. }) => (path.clone(), None),
    };
    let key_error = key.as_ref().and_then(|key| key.as_ref().err().copied());
    if !fields.is_empty() || key_error.is_some() {
        return Err(Errors {
            fields,
            key: key_error,
        });
    }

    let optional_path = |value: &str| (!value.is_empty()).then(|| PathBuf::from(value));
    Ok(Submission {
        server: Server {
            id: String::new(),
            name: name.into(),
            host: host.into(),
            port: port.expect("port was validated"),
            user: user.into(),
            auth: if agent {
                Auth::Agent {
                    identity_pub,
                    agent_socket: optional_path(socket),
                }
            } else {
                Auth::Password
            },
            initial_remote_path: optional_path(path),
            pinned: false,
        },
        key: key.and_then(Result::ok),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(comment: &str) -> AgentKey {
        AgentKey {
            key_type: "ssh-ed25519".into(),
            blob: vec![
                0, 0, 0, 11, b's', b's', b'h', b'-', b'e', b'd', b'2', b'5', b'5', b'1', b'9',
            ],
            comment: comment.into(),
        }
    }

    #[test]
    fn validates_required_fields_and_port() {
        let errors =
            validate(["", "bad host", "65536", "", "", ""], true, &Identity::None).unwrap_err();
        assert_eq!(
            errors
                .fields
                .into_iter()
                .map(|(field, _)| field)
                .collect::<Vec<_>>(),
            [Field::Host, Field::Port, Field::User]
        );
        assert_eq!(errors.key, Some("Elige una clave"));
        for port in ["0", "+22", "22.5", "dos"] {
            assert!(validate(["S", "host", port, "user", "", ""], false, &Identity::None).is_err());
        }
    }

    #[test]
    fn agent_mode_needs_a_key_or_a_file() {
        let values = ["S", "host", "22", "user", "", ""];
        let error = |identity| validate(values, true, &identity).unwrap_err().key;
        assert_eq!(error(Identity::None), Some("Elige una clave"));
        assert_eq!(
            error(Identity::File {
                path: "/tmp/mala.pub".into(),
                key: None,
                chosen: true
            }),
            Some("El archivo no es una clave pública válida.")
        );
        // The path a server already had is kept even if the file is gone.
        assert!(
            validate(
                values,
                true,
                &Identity::File {
                    path: "/tmp/vieja.pub".into(),
                    key: None,
                    chosen: false
                }
            )
            .is_ok()
        );
        // Password mode ignores the key.
        assert!(validate(values, false, &Identity::None).is_ok());
    }

    #[test]
    fn validates_agent_and_password_forms() {
        let file = |path: &str| Identity::File {
            path: path.into(),
            key: Some(key("c")),
            chosen: true,
        };
        let submission = validate(
            [" Casa ", "::1", "22", "ana", "", "/mi ruta/ñ"],
            true,
            &file("/tmp/clave ñ.pub"),
        )
        .unwrap();
        let server = submission.server;
        assert_eq!(server.name, "Casa");
        assert_eq!(server.initial_remote_path, Some("/mi ruta/ñ".into()));
        assert!(!server.pinned);
        assert!(submission.key.is_none());
        assert_eq!(
            server.auth,
            Auth::Agent {
                identity_pub: "/tmp/clave ñ.pub".into(),
                agent_socket: None
            }
        );

        let agent_key = key("Servidor");
        let submission = validate(
            ["Casa", "host", "22", "ana", "/tmp/agente", ""],
            true,
            &Identity::Agent(agent_key.clone()),
        )
        .unwrap();
        assert_eq!(submission.key, Some(agent_key));
        assert_eq!(
            submission.server.auth,
            Auth::Agent {
                identity_pub: PathBuf::new(),
                agent_socket: Some("/tmp/agente".into())
            }
        );

        let stored = validate(
            ["Casa", "host", "22", "ana", "", ""],
            true,
            &Identity::Stored("/cfg/keys/ab.pub".into()),
        )
        .unwrap();
        assert!(stored.key.is_none());

        let server = validate(
            ["Casa", "host", "65535", "ana", "", ""],
            false,
            &Identity::None,
        )
        .unwrap()
        .server;
        assert_eq!(server.auth, Auth::Password);
        assert_eq!(server.port, 65535);
    }

    #[test]
    fn shortens_fingerprints_to_the_last_four_characters() {
        assert_eq!(
            short_fingerprint("SHA256:Nv9k5CyzST5gm9sN9kt+A2d/j0Nw8kvUzIVrJ8cyTC0"),
            "…yTC0"
        );
        assert_eq!(short_fingerprint("SHA256:abc"), "…abc");
        assert_eq!(
            key_detail_text(
                "ssh-ed25519",
                "SHA256:Nv9k5CyzST5gm9sN9kt+A2d/j0Nw8kvUzIVrJ8cyTC0"
            ),
            "ed25519 · …yTC0"
        );
    }

    #[test]
    fn an_empty_name_is_saved_as_an_empty_string() {
        let submission =
            validate(["  ", "host", "22", "ana", "", ""], false, &Identity::None).unwrap();
        assert_eq!(submission.server.name, "");
        assert_eq!(submission.server.display_name(), "host");
    }

    #[test]
    fn a_blank_agent_socket_is_saved_as_none() {
        let submission = validate(
            ["", "host", "22", "ana", "   ", ""],
            true,
            &Identity::Agent(key("Servidor")),
        )
        .unwrap();
        assert_eq!(
            submission.server.auth,
            Auth::Agent {
                identity_pub: PathBuf::new(),
                agent_socket: None,
            }
        );
    }

    #[gpui::test]
    fn choosing_an_agent_row_switches_back_from_a_file(cx: &mut gpui::TestAppContext) {
        let mut form = cx.update(|cx| Form::new(None, "", None, cx));
        form.keys = Keys::Ready(vec![key("uno"), key("dos")]);
        form.pick_file(PathBuf::from("/tmp/clave.pub"));
        assert!(matches!(form.identity, Identity::File { .. }));
        // The agent rows stay listed while a file is chosen.
        assert_eq!(form.rows().len(), 2);
        assert!(form.pick_row(1));
        assert!(matches!(form.identity, Identity::Agent(_)));
    }

    #[test]
    fn arrow_keys_move_within_the_list() {
        assert_eq!(step_row(0, None, 1), None);
        assert_eq!(step_row(3, None, 1), Some(0));
        assert_eq!(step_row(3, None, -1), Some(2));
        assert_eq!(step_row(3, Some(0), 1), Some(1));
        assert_eq!(step_row(3, Some(2), 1), Some(2));
        assert_eq!(step_row(3, Some(0), -1), Some(0));
        assert_eq!(step_row(3, Some(2), -1), Some(1));
    }
}
