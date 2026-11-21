use std::path::PathBuf;

use gpui::{
    App, AppContext, Context, Div, Entity, Focusable, FontWeight, ParentElement, Styled, Window,
    div, prelude::FluentBuilder, px,
};
use vscp_core::{Auth, config::Server};

use crate::{
    components::{
        button::{Variant, button},
        text_input::TextInput,
    },
    theme::Theme,
    workspace::Workspace,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Field {
    Name,
    Host,
    Port,
    User,
    Identity,
    Socket,
    RemotePath,
}

const FIELDS: [(Field, &str, &str); 7] = [
    (Field::Name, "Nombre", "Mi servidor"),
    (Field::Host, "Host", "servidor.ejemplo"),
    (Field::Port, "Puerto", "22"),
    (Field::User, "Usuario", ""),
    (Field::Identity, "Clave pública", "/ruta/clave.pub"),
    (Field::Socket, "Socket del agente", "Opcional"),
    (
        Field::RemotePath,
        "Ruta remota inicial",
        "Carpeta personal del servidor",
    ),
];

type FieldError = (Field, &'static str);

pub struct Form {
    pub id: Option<String>,
    inputs: [Entity<TextInput>; FIELDS.len()],
    agent: bool,
    favorite: bool,
    pub errors: Vec<FieldError>,
    pub message: Option<&'static str>,
}

impl Form {
    pub fn new(server: Option<&Server>, default_socket: &str, cx: &mut App) -> Self {
        let mut values = [""; FIELDS.len()];
        values[Field::Port as usize] = "22";
        values[Field::Socket as usize] = default_socket;
        let mut port = String::new();
        let mut agent = true;
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
                    values[Field::Identity as usize] = identity_pub.to_str().unwrap_or_default();
                    values[Field::Socket as usize] = agent_socket
                        .as_deref()
                        .and_then(|path| path.to_str())
                        .unwrap_or_default();
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
            favorite: server.is_some_and(|server| server.favorite),
            errors: Vec::new(),
            message: None,
        }
    }

    pub fn input(&self, field: Field) -> &Entity<TextInput> {
        &self.inputs[field as usize]
    }

    pub fn is_composing(&self, window: &Window, cx: &App) -> bool {
        self.inputs.iter().any(|input| {
            let input = input.read(cx);
            input.is_composing() && input.focus_handle(cx).is_focused(window)
        })
    }

    pub fn validated(&self, cx: &App) -> Result<Server, Vec<FieldError>> {
        validate(
            self.inputs.each_ref().map(|input| input.read(cx).value()),
            self.agent,
            self.favorite,
        )
    }

    pub fn render(&self, theme: Theme, cx: &Context<Workspace>) -> Div {
        let mut fields = div().flex().flex_col().gap_2();
        for (field, _, _) in FIELDS {
            if field == Field::Port {
                continue;
            }
            if matches!(field, Field::Identity | Field::Socket) && !self.agent {
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
                Field::Identity => fields.child(self.field(field, theme, cx)),
                Field::User => fields.child(self.field(field, theme, cx)).child(
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
                ),
                _ => fields.child(self.field(field, theme, cx)),
            };
        }
        fields
            .child(div().flex().child(button(
                "favorite",
                if self.favorite {
                    "✓ Favorito"
                } else {
                    "＋ Favorito"
                },
                if self.favorite {
                    Variant::Selected
                } else {
                    Variant::Secondary
                },
                theme,
                cx.listener(|this, _, _, cx| {
                    if let Some(form) = this.editing() {
                        form.favorite = !form.favorite;
                        cx.notify();
                    }
                }),
            )))
            .when_some(self.message, |form, message| {
                form.child(div().text_color(theme.danger).child(message))
            })
    }

    fn field(&self, field: Field, theme: Theme, cx: &Context<Workspace>) -> Div {
        let (_, label, _) = FIELDS[field as usize];
        let input = self.input(field).clone();
        let control = if field == Field::Identity {
            div()
                .flex()
                .gap_2()
                .child(div().flex_1().min_w_0().child(input))
                .child(button(
                    "choose",
                    "Elegir…",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.choose_identity(window, cx)),
                ))
        } else {
            div().child(input)
        };
        labeled(label).child(control).when_some(
            self.errors.iter().find(|(key, _)| *key == field),
            |label, (_, message)| {
                label.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.danger)
                        .child(*message),
                )
            },
        )
    }
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
        cx.listener(move |this, _, _, cx| {
            if let Some(form) = this.editing() {
                form.agent = agent;
                cx.notify();
            }
        }),
    )
}

fn validate(
    values: [&str; FIELDS.len()],
    agent: bool,
    favorite: bool,
) -> Result<Server, Vec<FieldError>> {
    let [name, host, port, user, identity, socket, path] = values.map(str::trim);
    let mut errors = Vec::new();
    if name.is_empty() {
        errors.push((Field::Name, "Escribe un nombre."));
    }
    if host.is_empty()
        || host.starts_with('-')
        || host.contains(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '@' | '/'))
    {
        errors.push((Field::Host, "Escribe un nombre de host o una dirección IP."));
    }
    let valid_port = port.bytes().all(|byte| byte.is_ascii_digit());
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|port| valid_port && *port > 0);
    if port.is_none() {
        errors.push((Field::Port, "Usa un número del 1 al 65535."));
    }
    if user.is_empty() || user.contains(|c: char| c.is_whitespace() || c.is_control() || c == '@') {
        errors.push((Field::User, "Escribe un usuario válido."));
    }
    if agent && identity.is_empty() {
        errors.push((Field::Identity, "Elige el archivo de clave pública."));
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let optional_path = |value: &str| (!value.is_empty()).then(|| PathBuf::from(value));
    Ok(Server {
        id: String::new(),
        name: name.into(),
        host: host.into(),
        port: port.expect("port was validated"),
        user: user.into(),
        auth: if agent {
            Auth::Agent {
                identity_pub: identity.into(),
                agent_socket: optional_path(socket),
            }
        } else {
            Auth::Password
        },
        initial_remote_path: optional_path(path),
        favorite,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_required_fields_and_port() {
        let errors = validate(["", "bad host", "65536", "", "", "", ""], true, false).unwrap_err();
        assert_eq!(
            errors
                .into_iter()
                .map(|(field, _)| field)
                .collect::<Vec<_>>(),
            [
                Field::Name,
                Field::Host,
                Field::Port,
                Field::User,
                Field::Identity
            ]
        );
        for port in ["0", "+22", "22.5", "dos"] {
            assert!(validate(["S", "host", port, "user", "", "", ""], false, false).is_err());
        }
    }

    #[test]
    fn validates_agent_and_password_forms() {
        let server = validate(
            [
                " Casa ",
                "::1",
                "22",
                "ana",
                "/tmp/clave ñ.pub",
                "",
                "/mi ruta/ñ",
            ],
            true,
            true,
        )
        .unwrap();
        assert_eq!(server.name, "Casa");
        assert_eq!(server.initial_remote_path, Some("/mi ruta/ñ".into()));
        assert!(server.favorite);
        assert_eq!(
            server.auth,
            Auth::Agent {
                identity_pub: "/tmp/clave ñ.pub".into(),
                agent_socket: None
            }
        );
        let server = validate(["Casa", "host", "65535", "ana", "", "", ""], false, false).unwrap();
        assert_eq!(server.auth, Auth::Password);
        assert_eq!(server.port, 65535);
    }
}
