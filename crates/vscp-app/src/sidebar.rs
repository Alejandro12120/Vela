use gpui::{
    ClickEvent, Context, FontWeight, IntoElement, MouseButton, SharedString, div, prelude::*, px,
};
use vscp_core::config::Server;

use crate::{
    components::{
        button::{Variant, button},
        status_dot::status_dot,
    },
    theme::Theme,
    workspace::{Connection, Workspace},
};

pub fn sort_servers(servers: &mut [Server]) {
    servers.sort_by_cached_key(|server| {
        (
            !server.favorite,
            server.name.to_lowercase(),
            server.id.clone(),
        )
    });
}

pub fn render(workspace: &Workspace, theme: Theme, cx: &Context<Workspace>) -> impl IntoElement {
    let mut heading = div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(32.))
        .child(div().font_weight(FontWeight::SEMIBOLD).child("Servidores"));
    if workspace.dir.is_some() {
        heading = heading.child(
            button(
                "add",
                "+",
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
            )
            .w(px(28.))
            .h(px(28.))
            .text_size(px(16.))
            .aria_label("Añadir servidor"),
        );
    }
    let sidebar = div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(232.))
        .min_h_0()
        .border_r_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .p_3()
        .gap_2()
        .child(heading);
    if workspace.servers.is_empty() {
        return sidebar.when(workspace.dir.is_some(), |sidebar| {
            sidebar.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .py_2()
                    .child(
                        div()
                            .text_color(theme.secondary_text)
                            .child("Sin servidores"),
                    )
                    .child(button(
                        "add-first",
                        "Añadir servidor",
                        Variant::Secondary,
                        theme,
                        cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                    )),
            )
        });
    }
    sidebar.child(
        div()
            .id("server-list")
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                workspace
                    .servers
                    .iter()
                    .map(|server| row(server, workspace, theme, cx)),
            ),
    )
}

fn row(
    server: &Server,
    workspace: &Workspace,
    theme: Theme,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let id = &server.id;
    let selected = workspace.selected.as_ref() == Some(id);
    let menu_open = workspace.row_menu.as_ref() == Some(id);
    let connection = workspace.connections.get(id);
    let (status, color) = match connection {
        Some(Connection::Connecting) => ("Conectando", theme.warning),
        Some(Connection::Disconnecting) => ("Desconectando", theme.warning),
        Some(Connection::Connected { .. }) => ("Conectado", theme.success),
        Some(Connection::Error(_)) => ("Error", theme.danger),
        None => ("Desconectado", theme.secondary_text),
    };
    let select_id = id.clone();
    let menu_id = id.clone();
    let more_id = id.clone();
    let endpoint = format!("{}@{}", server.user, server.host);
    let toggle_menu = cx.listener(move |this, _, _, cx| {
        this.select(&more_id);
        this.row_menu = (!menu_open).then(|| more_id.clone());
        cx.notify();
    });
    div()
        .id(SharedString::from(id.clone()))
        .flex()
        .flex_col()
        .flex_none()
        .child(
            div()
                .group("server-row")
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(6.))
                .when(selected, |row| row.bg(theme.selection))
                .child(
                    div()
                        .id("select-server")
                        .tab_index(0)
                        .flex()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .h(px(46.))
                        .px_2()
                        .gap_2()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(gpui::transparent_black())
                        .when(!selected, |row| {
                            row.hover(move |style| style.bg(theme.hover))
                        })
                        .focus(move |style| style.border_color(theme.accent))
                        .cursor_pointer()
                        .aria_label(format!("{}, {endpoint}, {status}", server.name))
                        .child(status_dot(color))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(server.name.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.))
                                        .text_color(theme.secondary_text)
                                        .child(endpoint),
                                ),
                        )
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.select(&select_id);
                            if event.click_count() == 2 || matches!(event, ClickEvent::Keyboard(_))
                            {
                                this.connect(&select_id, window, cx);
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                this.select(&menu_id);
                                this.row_menu = Some(menu_id.clone());
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    button(
                        "server-actions",
                        "⋯",
                        Variant::Secondary,
                        theme,
                        toggle_menu,
                    )
                    .aria_label(format!("Acciones de {}", server.name))
                    .w(px(28.))
                    .h(px(28.))
                    .mr_1()
                    .when(!selected, |button| {
                        button
                            .invisible()
                            .tab_stop(false)
                            .group_hover("server-row", |style| style.visible())
                    }),
                ),
        )
        .when(menu_open, |row| row.child(menu(id, connection, theme, cx)))
}

fn menu(
    id: &str,
    connection: Option<&Connection>,
    theme: Theme,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let connected = matches!(connection, Some(Connection::Connected { .. }));
    let working = matches!(
        connection,
        Some(Connection::Connecting | Connection::Disconnecting)
    );
    let connection_id = id.to_owned();
    let edit_id = id.to_owned();
    let delete_id = id.to_owned();
    let menu = div()
        .p_2()
        .ml_2()
        .mr_1()
        .mt_1()
        .flex()
        .flex_col()
        .gap_1()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised);
    if working {
        return menu.child(
            div()
                .px_2()
                .text_color(theme.secondary_text)
                .child("Conexión en curso…"),
        );
    }
    menu.child(button(
        "session",
        if connected { "Desconectar" } else { "Conectar" },
        Variant::Secondary,
        theme,
        cx.listener(move |this, _, window, cx| {
            this.row_menu = None;
            if connected {
                this.disconnect(&connection_id, window, cx);
            } else {
                this.connect(&connection_id, window, cx);
            }
            cx.notify();
        }),
    ))
    .child(button(
        "edit",
        "Editar…",
        Variant::Secondary,
        theme,
        cx.listener(move |this, _, window, cx| this.open_form(Some(&edit_id), window, cx)),
    ))
    .child(
        button(
            "remove",
            "Eliminar…",
            Variant::Secondary,
            theme,
            cx.listener(move |this, _, window, cx| this.open_delete(delete_id.clone(), window, cx)),
        )
        .text_color(theme.danger),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vscp_core::Auth;

    #[test]
    fn favorites_first_then_case_insensitive_name() {
        let server = |id: &str, name: &str, favorite| Server {
            id: id.into(),
            name: name.into(),
            host: "example.test".into(),
            port: 22,
            user: "ana".into(),
            auth: Auth::Password,
            initial_remote_path: None,
            favorite,
        };
        let mut servers = [
            server("3", "alfa", false),
            server("4", "Beta", false),
            server("2", "Zeta", true),
            server("1", "bravo", true),
        ];
        sort_servers(&mut servers);
        assert_eq!(servers.map(|server| server.id), ["1", "2", "3", "4"]);
    }
}
