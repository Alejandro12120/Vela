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
    components::{
        button::{Variant, button},
        text_input::TextInput,
    },
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
    (Field::Name, "Nombre", "Mi servidor"),
    (Field::Host, "Host", "servidor.ejemplo"),
    (Field::Port, "Puerto", "22"),
    (Field::User, "Usuario", ""),
    (Field::Socket, "Socket del agente", "Opcional"),
    (
        Field::RemotePath,
        "Ruta remota inicial",
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
    row_focus: Vec<FocusHandle>,
    blur: Option<Subscription>,
}

impl Form {
    pub fn new(
        server: Option<&Server>,
        default_socket: &str,
        config_dir: Option<&Path>,
        cx: &mut App,
    ) -> Self {
        let mut values = [""; FIELDS.len()];
        values[Field::Port as usize] = "22";
        values[Field::Socket as usize] = default_socket;
        let mut port = String::new();
        let mut agent = true;
        let mut identity = Identity::None;
        let mut existing = None;
        if let Some(server) = server {
            port.push_str(&server.port.to_string());
            values[Field::Name as usize] = &server.name;
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
        let inputs = FIELDS.map(|(field, label, placeholder)| {
            cx.new(|cx| TextInput::new(values[field as usize], label, placeholder, false, cx))
        });
        Self {
            id: server.map(|server| server.id.clone()),
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
            row_focus: Vec::new(),
            blur: None,
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

    /// Whether the key comes from a chosen file instead of the agent list.
    fn file_mode(&self) -> bool {
        matches!(self.identity, Identity::File { .. })
    }

    fn socket(&self, cx: &App) -> String {
        self.input(Field::Socket).read(cx).value().trim().to_owned()
    }

    /// Starts a listing for `request` and returns the socket to ask. Nothing is
    /// listed outside agent mode or while a file is chosen.
    pub fn begin_listing(&mut self, request: u64, cx: &App) -> Option<String> {
        if !self.agent || self.file_mode() {
            return None;
        }
        self.request = request;
        self.keys = Keys::Loading(Instant::now());
        self.listed_socket = self.socket(cx);
        self.row_focus.clear();
        Some(self.listed_socket.clone())
    }

    /// Whether the Socket field now differs from the one last listed.
    pub fn socket_changed(&self, cx: &App) -> bool {
        self.agent && !self.file_mode() && self.socket(cx) != self.listed_socket
    }

    /// Applies a listing unless a newer one was started since. `handles` has one
    /// focus handle per possible row.
    pub fn finish_listing(
        &mut self,
        request: u64,
        result: Result<Vec<AgentKey>, AgentError>,
        handles: Vec<FocusHandle>,
    ) {
        if request != self.request || self.file_mode() {
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

    pub fn use_agent(&mut self) {
        self.identity = Identity::None;
        self.key_error = None;
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

    fn select(&mut self, row: usize, window: &mut Window, cx: &mut App) {
        let rows = self.rows();
        if let Some(row_data) = rows.get(row) {
            self.identity = row_data.pick.clone();
            self.key_error = None;
            if let Some(handle) = self.row_focus.get(row) {
                handle.focus(window, cx);
            }
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
            if field == Field::Port {
                continue;
            }
            if field == Field::Socket && !self.agent {
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
                    let fields = fields.child(self.field(field, theme, cx)).child(
                        labeled("Autenticación").child(
                            div()
                                .flex()
                                .gap_1()
                                .flex_wrap()
                                .child(auth_choice(
                                    true,
                                    self.agent,
                                    "Agente (Bitwarden)",
                                    theme,
                                    cx,
                                ))
                                .child(auth_choice(false, self.agent, "Contraseña", theme, cx)),
                        ),
                    );
                    if self.agent {
                        fields.child(self.key_section(theme, cx))
                    } else {
                        fields
                    }
                }
                _ => fields.child(self.field(field, theme, cx)),
            };
        }
        fields.when_some(self.message, |form, message| {
            form.child(div().text_color(theme.danger).child(message))
        })
    }

    fn field(&self, field: Field, theme: Theme, _: &Context<Workspace>) -> Div {
        let (_, label, _) = FIELDS[field as usize];
        labeled(label)
            .child(div().child(self.input(field).clone()))
            .when_some(
                self.errors.iter().find(|(key, _)| *key == field),
                |label, (_, message)| label.child(error_text(message, theme)),
            )
    }

    fn key_section(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let section = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .font_weight(FontWeight::MEDIUM)
                .child(icon(IconName::Key).text_color(theme.secondary_text))
                .child("Clave"),
        );
        let section = match &self.identity {
            Identity::File { path, key, .. } => section
                .child(file_card(path, key.as_ref(), theme))
                .child(div().flex().child(link(
                    "use-agent",
                    "Usar una clave de Bitwarden",
                    theme,
                    cx.listener(|this, _, window, cx| {
                        if let Some(form) = this.editing() {
                            form.use_agent();
                        }
                        this.refresh_keys(window, cx);
                    }),
                ))),
            _ => section
                .child(self.agent_keys(theme, cx))
                .child(div().flex().child(link(
                    "from-file",
                    "Desde archivo…",
                    theme,
                    cx.listener(|this, _, window, cx| this.choose_identity(window, cx)),
                ))),
        };
        section.when_some(self.key_error, |section, message| {
            section.child(error_text(message, theme))
        })
    }

    fn agent_keys(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let message = |text: &'static str| {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .w_full()
                        .items_start()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .child(
                            icon(IconName::Error)
                                .size(px(14.))
                                .mt(px(1.))
                                .text_color(theme.danger),
                        )
                        .child(div().flex_1().min_w_0().child(text)),
                )
                .child(button(
                    "retry",
                    "Reintentar",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.refresh_keys(window, cx)),
                ))
        };
        let rows = self.rows();
        let notice = match &self.keys {
            Keys::Loading(since) => {
                return boxed(theme).child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .child(crate::components::motion::spin(
                            icon(IconName::Spinner)
                                .size(px(14.))
                                .mt(px(1.))
                                .text_color(theme.secondary_text),
                            "keys-spinner",
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_w_0()
                                .child("Consultando Bitwarden…")
                                .when(since.elapsed() >= UNLOCK_HINT_AFTER, |text| {
                                    text.child("Si Bitwarden pide desbloquear, hazlo y espera.")
                                }),
                        ),
                );
            }
            Keys::Ready(keys) if keys.is_empty() => Some(message(
                "Bitwarden no tiene claves SSH disponibles o está bloqueado.",
            )),
            Keys::Ready(_) => None,
            Keys::Failed(error) => Some(message(match error {
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
            })),
        };
        let selected = self.selected_row(&rows);
        let list = (!rows.is_empty()).then(|| {
            let items = rows.iter().enumerate().map(|(index, row)| {
                let handle = self.row_focus.get(index).cloned();
                key_row(row, index, selected == Some(index), handle, theme, cx)
            });
            div()
                .id("agent-keys")
                .role(Role::ListBox)
                .aria_label("Claves de Bitwarden")
                .max_h(px(168.))
                .overflow_y_scroll()
                .rounded(px(6.))
                .border_1()
                .border_color(theme.control_border)
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
                .children(items)
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(notice.map(|notice| boxed(theme).child(notice)))
            .children(list)
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

/// `SHA256:abcd…wxyz`: the first and last four characters after the prefix.
fn short_fingerprint(fingerprint: &str) -> String {
    let (prefix, hash) = fingerprint.split_once(':').unwrap_or(("", fingerprint));
    let chars: Vec<char> = hash.chars().collect();
    if chars.len() <= 9 {
        return fingerprint.to_owned();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{prefix}:{head}…{tail}")
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
    let fingerprint = short_fingerprint(&row.key.fingerprint());
    let detail = key_detail(
        &row.key.key_type,
        match row.note {
            Some(note) => div().truncate().child(note),
            None => div()
                .truncate()
                .font_family(crate::theme::monospace())
                .child(fingerprint.clone()),
        },
        theme,
    );
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
        .gap_2()
        .h(px(40.))
        .px_2()
        .border_1()
        .border_color(gpui::transparent_black())
        .rounded(px(6.))
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.selection))
        .when(!selected, |row| {
            row.hover(move |style| style.bg(theme.hover))
        })
        .focus(move |style| style.border_color(theme.accent))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(name.clone()),
                )
                .child(detail),
        )
        .when(selected, |row| {
            row.child(icon(IconName::Check).text_color(theme.text))
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            if let Some(form) = this.editing() {
                form.select(index, window, cx);
                cx.notify();
            }
        }))
}

/// The second line of a key: its type, a dot and `tail`.
fn key_detail(key_type: &str, tail: Div, theme: Theme) -> Div {
    div()
        .flex()
        .gap_1()
        .min_w_0()
        .text_size(px(12.))
        .text_color(theme.secondary_text)
        .child(div().flex_none().child(key_type.to_owned()))
        .child(div().flex_none().child("·"))
        .child(tail)
}

fn file_card(path: &Path, key: Option<&AgentKey>, theme: Theme) -> Div {
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned();
    let card = boxed(theme)
        .flex()
        .flex_col()
        .child(div().truncate().font_weight(FontWeight::MEDIUM).child(name));
    let secondary = || {
        div()
            .truncate()
            .text_size(px(12.))
            .text_color(theme.secondary_text)
    };
    match key {
        Some(key) => card
            .child(key_detail(
                &key.key_type,
                div()
                    .truncate()
                    .font_family(crate::theme::monospace())
                    .child(short_fingerprint(&key.fingerprint())),
                theme,
            ))
            .child(secondary().child(path.to_string_lossy().into_owned())),
        None => card.child(
            div()
                .text_size(px(12.))
                .text_color(theme.danger)
                .child(INVALID_FILE),
        ),
    }
}

fn boxed(theme: Theme) -> Div {
    div()
        .px_2()
        .py_2()
        .rounded(px(6.))
        .border_1()
        .border_color(theme.control_border)
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
        // Aligns the text, not the focus border, with the field edge.
        .ml(px(-4.))
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

fn labeled(label: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().font_weight(FontWeight::MEDIUM).child(label))
}

fn auth_choice(
    agent: bool,
    selected: bool,
    label: &'static str,
    theme: Theme,
    cx: &Context<Workspace>,
) -> impl gpui::IntoElement {
    button(
        label,
        label,
        if selected == agent {
            Variant::Selected
        } else {
            Variant::Secondary
        },
        theme,
        cx.listener(move |this, _, window, cx| {
            let Some(form) = this.editing() else { return };
            let changed = form.agent != agent;
            form.agent = agent;
            cx.notify();
            if changed && agent {
                this.refresh_keys(window, cx);
            }
        }),
    )
}

fn validate(
    values: [&str; FIELDS.len()],
    agent: bool,
    identity: &Identity,
) -> Result<Submission, Errors> {
    let [name, host, port, user, socket, path] = values.map(str::trim);
    let mut fields = Vec::new();
    if name.is_empty() {
        fields.push((Field::Name, "Escribe un nombre."));
    }
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
            [Field::Name, Field::Host, Field::Port, Field::User]
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
    fn shortens_fingerprints_to_four_characters_at_each_end() {
        assert_eq!(
            short_fingerprint("SHA256:Nv9k5CyzST5gm9sN9kt+A2d/j0Nw8kvUzIVrJ8cyTC0"),
            "SHA256:Nv9k…yTC0"
        );
        assert_eq!(short_fingerprint("SHA256:abc"), "SHA256:abc");
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
