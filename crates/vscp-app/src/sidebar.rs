use gpui::{
    ClickEvent, Context, Div, FontWeight, IntoElement, MouseButton, Pixels, Point, Render,
    SharedString, Stateful, Window, div, prelude::*, px,
};
use vscp_core::config::Server;

use crate::{
    components::{
        button::{Variant, button, icon_button, icon_text_button},
        menu::menu_item,
        status_dot::status_dot,
    },
    icons::{IconName, icon},
    theme::Theme,
    workspace::{Connection, Workspace},
};

pub const WIDTH: f32 = 232.;

/// A server row being dragged to a new position or group.
#[derive(Clone)]
struct ServerDrag {
    id: String,
    name: SharedString,
}

struct ServerDragChip {
    name: SharedString,
    offset: Point<Pixels>,
}

impl Render for ServerDragChip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        div()
            .pl(self.offset.x + px(8.))
            .pt(self.offset.y + px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(32.))
                    .max_w(px(200.))
                    .px_2()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.control_border)
                    .bg(theme.panel)
                    .text_color(theme.text)
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().truncate().child(self.name.clone())),
            )
    }
}

pub fn render(workspace: &Workspace, theme: Theme, cx: &Context<Workspace>) -> impl IntoElement {
    let mut heading = div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(32.))
        .child(div().font_weight(FontWeight::MEDIUM).child("Servidores"));
    if workspace.dir.is_some() {
        heading = heading.child(
            icon_button(
                "add",
                "Añadir servidor",
                IconName::Plus,
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
            )
            .w(px(28.))
            .h(px(28.))
            .px_0(),
        );
    }
    let sidebar = div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(WIDTH))
        .min_h_0()
        .border_r_1()
        .border_color(theme.border)
        .bg(theme.sidebar)
        .p_3()
        .gap_2()
        .child(heading);
    let footer = || {
        div()
            .flex()
            .flex_none()
            .pt_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                icon_text_button(
                    "settings",
                    IconName::Settings,
                    "Ajustes",
                    Variant::Secondary,
                    None,
                    theme,
                    cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                )
                .h(px(26.)),
            )
    };
    if workspace.servers.is_empty() {
        return sidebar.when(workspace.dir.is_some(), |sidebar| {
            sidebar
                .child(
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
                .child(div().flex_1())
                .child(footer())
        });
    }
    let dragging = workspace.dragging_server && cx.has_active_drag();
    let (pinned, unpinned): (Vec<&Server>, Vec<&Server>) =
        workspace.servers.iter().partition(|server| server.pinned);
    let grouped = !pinned.is_empty() || dragging;
    sidebar
        .child(
            div()
                .id("server-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .when(grouped, |list| list.child(group_header("Fijados", theme)))
                .children(
                    pinned
                        .iter()
                        .map(|server| row(server, workspace, theme, cx)),
                )
                .when(!pinned.is_empty(), |list| {
                    list.child(end_zone(true, theme, cx))
                })
                .when(pinned.is_empty() && dragging, |list| {
                    list.child(empty_zone(true, "Suelta aquí para fijar", theme, cx))
                })
                .when(grouped, |list| list.child(divider(theme)))
                .children(
                    unpinned
                        .iter()
                        .map(|server| row(server, workspace, theme, cx)),
                )
                .when(!unpinned.is_empty(), |list| {
                    list.child(end_zone(false, theme, cx).flex_1())
                })
                .when(unpinned.is_empty() && dragging, |list| {
                    list.child(empty_zone(
                        false,
                        "Suelta aquí para dejar de fijar",
                        theme,
                        cx,
                    ))
                }),
        )
        .child(footer())
}

fn group_header(label: &'static str, theme: Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.))
        .px_2()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.secondary_text)
        .child(label)
}

/// Separates the pinned group from the unpinned one, which has no header.
fn divider(theme: Theme) -> Div {
    div().flex_none().h(px(1.)).my(px(6.)).bg(theme.border)
}

/// Drop target below a group's last row. The insertion line shows at its top.
fn end_zone(pinned: bool, theme: Theme, cx: &Context<Workspace>) -> Stateful<Div> {
    div()
        .id(if pinned { "end-pinned" } else { "end-unpinned" })
        .debug_selector(|| if pinned { "end-pinned" } else { "end-unpinned" }.into())
        .flex_none()
        .min_h(px(8.))
        .border_t_2()
        .border_color(gpui::transparent_black())
        .drag_over(move |style, _: &ServerDrag, _, _| style.border_color(theme.accent))
        .on_drop(cx.listener(move |this, drag: &ServerDrag, _, cx| {
            this.drop_server(&drag.id, pinned, None, cx);
        }))
}

/// Shown while dragging when a group has no servers.
fn empty_zone(
    pinned: bool,
    label: &'static str,
    theme: Theme,
    cx: &Context<Workspace>,
) -> Stateful<Div> {
    div()
        .id(if pinned {
            "empty-pinned"
        } else {
            "empty-unpinned"
        })
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(32.))
        .rounded(px(6.))
        .border_1()
        .border_dashed()
        .border_color(theme.control_border)
        .text_size(px(12.))
        .text_color(theme.secondary_text)
        .drag_over(move |style, _: &ServerDrag, _, _| {
            style.border_color(theme.accent).bg(theme.selection)
        })
        .on_drop(cx.listener(move |this, drag: &ServerDrag, _, cx| {
            this.drop_server(&drag.id, pinned, None, cx);
        }))
        .child(label)
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
    let pin_id = id.clone();
    let drop_id = id.clone();
    let over_id = id.clone();
    let pinned = server.pinned;
    let shown = selected || menu_open;
    let group = SharedString::from(format!("server-row-{id}"));
    let drag = ServerDrag {
        id: id.clone(),
        name: server.name.clone().into(),
    };
    let workspace_handle = cx.entity().downgrade();
    let endpoint = format!("{}@{}", server.user, server.host);
    let toggle_menu = cx.listener(move |this, _, _, cx| {
        this.select(&more_id);
        this.row_menu = (!menu_open).then(|| more_id.clone());
        cx.notify();
    });
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(|| format!("server-row-{id}"))
        .flex()
        .flex_col()
        .flex_none()
        .pb(px(2.))
        .border_t_2()
        .border_color(gpui::transparent_black())
        .drag_over(move |style, drag: &ServerDrag, _, _| {
            if drag.id == over_id {
                style
            } else {
                style.border_color(theme.accent)
            }
        })
        .on_drop(cx.listener(move |this, drag: &ServerDrag, _, cx| {
            this.drop_server(&drag.id, pinned, Some(&drop_id), cx);
        }))
        .child(
            div()
                .group(group.clone())
                .relative()
                .flex()
                .items_center()
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
                                .when(shown, |text| text.pr(px(56.)))
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
                        .on_drag(drag, move |drag, offset, _, cx| {
                            let _ = workspace_handle.update(cx, |workspace, cx| {
                                workspace.dragging_server = true;
                                cx.notify();
                            });
                            cx.new(|_| ServerDragChip {
                                name: drag.name.clone(),
                                offset,
                            })
                        })
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
                    // Floats over the end of the row, so names keep their width.
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl_1()
                        .rounded(px(6.))
                        .bg(if selected {
                            theme.selection
                        } else {
                            theme.hover
                        })
                        .when(!shown, |actions| {
                            actions
                                .invisible()
                                .group_hover(group, |style| style.visible())
                        })
                        .child(pin_button(
                            pinned,
                            shown,
                            theme,
                            cx.listener(move |this, _, _, cx| this.toggle_pin(&pin_id, cx)),
                        ))
                        .child(
                            icon_button(
                                "server-actions",
                                format!("Acciones de {}", server.name),
                                IconName::Ellipsis,
                                Variant::Secondary,
                                theme,
                                toggle_menu,
                            )
                            .w(px(28.))
                            .h(px(28.))
                            .px_0()
                            .when(!shown, |button| button.tab_stop(false)),
                        ),
                ),
        )
        .when(menu_open, |row| {
            row.child(menu(server, connection, theme, cx))
        })
}

/// Pin toggle shown on hover, and while the row is selected or its menu is open.
fn pin_button(
    pinned: bool,
    shown: bool,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    div()
        .id("pin-server")
        .role(gpui::Role::Button)
        .aria_label(if pinned { "Dejar de fijar" } else { "Fijar" })
        .tab_index(0)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.))
        .h(px(28.))
        .rounded(px(6.))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .focus(move |style| style.border_color(theme.accent))
        .child(
            icon(if pinned {
                IconName::PinOff
            } else {
                IconName::Pin
            })
            .text_color(if pinned {
                theme.text
            } else {
                theme.secondary_text
            }),
        )
        .when(!shown, |button| button.tab_stop(false))
        .on_click(on_click)
}

fn menu(
    server: &Server,
    connection: Option<&Connection>,
    theme: Theme,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let connected = matches!(connection, Some(Connection::Connected { .. }));
    let working = matches!(
        connection,
        Some(Connection::Connecting | Connection::Disconnecting)
    );
    let connection_id = server.id.clone();
    let edit_id = server.id.clone();
    let delete_id = server.id.clone();
    let pin_id = server.id.clone();
    let menu = div()
        .p_1()
        .ml_2()
        .mr_1()
        .mt_1()
        .flex()
        .flex_col()
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
    menu.child(
        menu_item(
            if server.pinned {
                IconName::PinOff
            } else {
                IconName::Pin
            },
            if server.pinned {
                "Dejar de fijar"
            } else {
                "Fijar"
            },
            "",
            true,
            false,
            theme,
        )
        .id("pin")
        .role(gpui::Role::MenuItem)
        .tab_index(0)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.row_menu = None;
            this.toggle_pin(&pin_id, cx);
        })),
    )
    .child(
        menu_item(
            if connected {
                IconName::Unplug
            } else {
                IconName::Plug
            },
            if connected { "Desconectar" } else { "Conectar" },
            "",
            true,
            false,
            theme,
        )
        .id("session")
        .role(gpui::Role::MenuItem)
        .tab_index(0)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.row_menu = None;
            if connected {
                this.disconnect(&connection_id, window, cx);
            } else {
                this.connect(&connection_id, window, cx);
            }
            cx.notify();
        })),
    )
    .child(
        menu_item(IconName::Pencil, "Editar…", "", true, false, theme)
            .id("edit")
            .role(gpui::Role::MenuItem)
            .tab_index(0)
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_form(Some(&edit_id), window, cx)),
            ),
    )
    .child(
        menu_item(IconName::Trash, "Eliminar…", "", true, true, theme)
            .id("remove")
            .role(gpui::Role::MenuItem)
            .tab_index(0)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_delete(delete_id.clone(), window, cx)
            })),
    )
}
