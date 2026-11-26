use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, Div, DragMoveEvent, FocusHandle, FontWeight,
    MouseButton, MouseDownEvent, Pixels, Point, Render, Role, SharedString, SpringAnimation,
    Stateful, Window, anchored, deferred, div, point, prelude::*, px,
};
use vela_core::config::Server;

use crate::{
    components::{
        button::{Variant, button, icon_button},
        menu::menu_item,
        modal,
        motion::{self, FAST, SPRING},
        status_dot::status_dot,
    },
    context_menu::Tooltip,
    icons::{IconName, icon},
    theme::Theme,
    workspace::{Connection, UpdateState, Workspace},
};

pub const WIDTH: f32 = 232.;
/// Height of a server card.
const CARD_H: f32 = 46.;
/// A card plus the 2 px gap below it.
pub const ROW_H: f32 = 48.;
const HEADER_H: f32 = 24.;
/// A 1 px line with 6 px above and below.
const DIVIDER_H: f32 = 13.;
/// An empty group shows a drop zone the size of one row.
const ZONE_H: f32 = ROW_H;
const LIST_END: f32 = 8.;
/// Where the list starts in the window: padding, heading and gap.
pub const LIST_TOP: f32 = 52.;

/// A server row being dragged. The value only carries the id, since the card
/// itself is drawn by the list from `Workspace::reorder`.
#[derive(Clone)]
pub struct ServerDrag {
    pub id: String,
}

/// GPUI draws this under the cursor. It is empty, because the card is drawn in the list.
struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Where a dragged server would land: a group and a slot among the other
/// servers of that group (the dragged one excluded).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    pub pinned: bool,
    pub index: usize,
}

/// The state of a card being dragged by the user.
pub struct ServerReorder {
    pub id: String,
    /// Cursor y minus the card top at drag start.
    pub grab_offset_y: f32,
    /// Cursor y in list coordinates.
    pub pointer_y: f32,
    pub target: Target,
    /// Held by a development hook: no pointer drives it.
    pub held: bool,
}

/// Positions of everything in the list, in px from the top of the content.
#[derive(Debug, PartialEq)]
pub struct Layout<'a> {
    pub rows: Vec<(&'a str, f32)>,
    pub header: Option<f32>,
    pub divider: Option<f32>,
    /// Drop zones of empty groups, pinned first, with whether the target is there.
    pub zones: [Option<(f32, bool)>; 2],
    pub height: f32,
}

impl Layout<'_> {
    pub fn row_y(&self, id: &str) -> Option<f32> {
        self.rows
            .iter()
            .find(|(row, _)| *row == id)
            .map(|(_, y)| *y)
    }
}

/// Lays out the groups. While `drag` is set, its server is left out and an
/// empty slot one row high is opened at its target. Groups follow the stored
/// order of `pinned` and `unpinned`.
pub fn layout<'a>(
    pinned: &[&'a str],
    unpinned: &[&'a str],
    drag: Option<(&str, Target)>,
) -> Layout<'a> {
    let dragged = drag.map(|(id, _)| id);
    let others = |ids: &[&'a str]| -> Vec<&'a str> {
        ids.iter()
            .copied()
            .filter(|id| Some(*id) != dragged)
            .collect()
    };
    // The header and divider depend on the stored data only, so starting a drag
    // never moves the other rows.
    let grouped = !pinned.is_empty();
    let mut layout = Layout {
        rows: Vec::new(),
        header: None,
        divider: None,
        zones: [None, None],
        height: 0.,
    };
    let mut y = 0.;
    for (group, ids) in [others(pinned), others(unpinned)].iter().enumerate() {
        let is_pinned = group == 0;
        if is_pinned && grouped {
            layout.header = Some(y);
            y += HEADER_H;
        }
        if !is_pinned && grouped {
            layout.divider = Some(y + 6.);
            y += DIVIDER_H;
        }
        if is_pinned && !grouped {
            continue;
        }
        let gap = drag.is_some_and(|(_, target)| target.pinned == is_pinned);
        if ids.is_empty() {
            if drag.is_some() {
                layout.zones[group] = Some((y, gap));
                y += ZONE_H;
            }
            continue;
        }
        let slot = drag.map_or(usize::MAX, |(_, target)| target.index);
        for (index, id) in ids.iter().enumerate() {
            let shift = usize::from(gap && index >= slot);
            layout.rows.push((id, y + (index + shift) as f32 * ROW_H));
        }
        y += (ids.len() + usize::from(gap)) as f32 * ROW_H;
    }
    layout.height = y + LIST_END;
    layout
}

/// The slot under a dragged card. `pinned_len` and `unpinned_len` count the
/// servers without the dragged one, `current` is the target shown now, `centre`
/// is the card centre and `pointer` the cursor, both in list coordinates.
///
/// The group boundary is read from the layout as shown, where the open slot
/// of `current` makes its group one row taller. A card therefore has to cross
/// the divider to leave a group and cross it back to return.
///
/// Without pinned servers there is no pinned group in the list, so the cursor
/// above the list means the heading, which turns into the drop zone while dragging.
pub fn target_for(
    pinned_len: usize,
    unpinned_len: usize,
    grouped: bool,
    current: Target,
    pointer: f32,
    centre: f32,
) -> Target {
    let slot = |top: f32| ((centre - top) / ROW_H).floor().max(0.) as usize;
    if !grouped {
        return if pointer < 0. {
            Target {
                pinned: true,
                index: 0,
            }
        } else {
            Target {
                pinned: false,
                index: slot(0.).min(unpinned_len),
            }
        };
    }
    let pinned_size = if pinned_len == 0 {
        ZONE_H
    } else {
        (pinned_len + usize::from(current.pinned)) as f32 * ROW_H
    };
    let divider_middle = HEADER_H + pinned_size + DIVIDER_H / 2.;
    if centre < divider_middle {
        Target {
            pinned: true,
            index: slot(HEADER_H).min(pinned_len),
        }
    } else {
        let top = HEADER_H + pinned_size + DIVIDER_H;
        Target {
            pinned: false,
            index: slot(top).min(unpinned_len),
        }
    }
}

/// The top of the dragged card, kept inside the list.
pub fn card_top(reorder: &ServerReorder, height: f32) -> f32 {
    (reorder.pointer_y - reorder.grab_offset_y).clamp(0., (height - LIST_END - ROW_H).max(0.))
}

/// The centre of a server's row in window coordinates, for development hooks.
pub fn row_centre(servers: &[Server], id: &str) -> Option<Point<Pixels>> {
    let (pinned, unpinned) = split(servers);
    let y = layout(&pinned, &unpinned, None).row_y(id)?;
    Some(point(px(WIDTH / 2.), px(LIST_TOP + y + CARD_H / 2.)))
}

/// The ids of the pinned and the unpinned servers, each in stored order.
pub fn split(servers: &[Server]) -> (Vec<&str>, Vec<&str>) {
    let (pinned, unpinned): (Vec<&Server>, Vec<&Server>) =
        servers.iter().partition(|server| server.pinned);
    (
        pinned.iter().map(|server| server.id.as_str()).collect(),
        unpinned.iter().map(|server| server.id.as_str()).collect(),
    )
}

/// The floating menu of a server row.
pub struct RowMenu {
    pub id: String,
    pub position: Point<Pixels>,
    pub scope: FocusHandle,
    pub first: FocusHandle,
    pub last: FocusHandle,
}

#[derive(Clone, Copy)]
pub enum RowAction {
    Session,
    Pin,
    Edit,
    Duplicate,
    Delete,
}

pub fn render(
    workspace: &Workspace,
    theme: Theme,
    titlebar_height: Pixels,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let target = if workspace.sidebar_visible() {
        WIDTH
    } else {
        0.
    };
    let body = body(workspace, theme, titlebar_height, cx);
    // The width springs and the content is clipped, so the panes reflow with it.
    // Nothing is rendered while closed, which keeps it out of the tab order.
    div()
        .flex()
        .flex_none()
        .min_h_0()
        .overflow_hidden()
        .with_spring(
            "sidebar-width",
            SpringAnimation::new(SPRING)
                .to(px(target))
                .with_epsilon(0.5),
            move |wrapper, width| {
                wrapper
                    .w(width)
                    .when(width > px(0.5), move |wrapper| wrapper.child(body))
            },
        )
}

fn body(
    workspace: &Workspace,
    theme: Theme,
    titlebar_height: Pixels,
    cx: &Context<Workspace>,
) -> Div {
    let (pinned, unpinned) = split(&workspace.servers);
    let reorder = workspace.reorder.as_ref();
    // Without pinned servers the heading becomes the pinned drop zone while dragging.
    let heading_zone = reorder.filter(|_| pinned.is_empty());
    let mut heading = div()
        .debug_selector(|| "sidebar-heading".into())
        .flex()
        .items_center()
        .justify_between()
        .h(px(32.));
    heading = match heading_zone {
        Some(reorder) => heading.child(
            drop_zone("Drop here to pin", reorder.target.pinned, theme)
                .id("heading-zone")
                .size_full()
                .on_drop(cx.listener(|this, _: &ServerDrag, _, cx| this.drop_reorder(cx))),
        ),
        None => heading.child(div().font_weight(FontWeight::MEDIUM).child("Servers")),
    };
    if workspace.dir.is_some() && heading_zone.is_none() {
        heading = heading.child(
            icon_button(
                "add",
                "Add Server",
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
        .bg(theme.chrome(theme.sidebar))
        .p_3()
        .when(titlebar_height > px(0.), |sidebar| {
            sidebar.pt(px(12.) + titlebar_height)
        })
        .gap_2()
        .child(heading);
    let footer = || {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .when_some(update_banner(workspace, theme), |col, banner| {
                col.child(banner)
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(settings_button(workspace, theme, cx))
                    .child(update_button(workspace, theme, cx)),
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
                        .child(div().text_color(theme.secondary_text).child("No servers"))
                        .child(button(
                            "add-first",
                            "Add Server",
                            Variant::Secondary,
                            theme,
                            cx.listener(|this, _, window, cx| this.open_form(None, window, cx)),
                        )),
                )
                .child(div().flex_1())
                .child(footer())
        });
    }

    let drag = reorder.map(|reorder| (reorder.id.as_str(), reorder.target));
    let positions = layout(&pinned, &unpinned, drag);
    let settle = workspace.settling();
    let ordered = pinned
        .iter()
        .chain(&unpinned)
        .filter(|id| drag.is_none_or(|(dragged, _)| dragged != **id));
    let rows = ordered
        .zip(&positions.rows)
        .filter_map(|(id, &(_, y))| {
            let server = workspace.servers.iter().find(|server| server.id == **id)?;
            let from = settle
                .filter(|(settling, _)| settling == id)
                .map(|(_, y)| y);
            Some(row(server, y, from, workspace, theme, cx))
        })
        .collect::<Vec<_>>();
    let dragged = reorder.and_then(|reorder| {
        let server = workspace
            .servers
            .iter()
            .find(|server| server.id == reorder.id)?;
        Some(floating_card(
            server,
            card_top(reorder, positions.height),
            workspace,
            theme,
        ))
    });
    let content = div()
        .id("server-list-content")
        .relative()
        .w_full()
        .h(px(positions.height))
        .on_drag_move(
            cx.listener(|this, event: &DragMoveEvent<ServerDrag>, _, cx| {
                let pointer = event.event.position.y - event.bounds.origin.y;
                this.reorder_move(pointer.as_f32(), cx);
            }),
        )
        .when_some(positions.header, |list, y| {
            list.child(
                group_header("Pinned", theme)
                    .absolute()
                    .top(px(y))
                    .left_0()
                    .right_0(),
            )
        })
        .when_some(positions.divider, |list, y| {
            list.child(divider(theme).absolute().top(px(y)).left_0().right_0())
        })
        .children(
            positions
                .zones
                .iter()
                .enumerate()
                .filter_map(|(group, zone)| {
                    let (y, active) = (*zone)?;
                    let label = if group == 0 {
                        "Drop here to pin"
                    } else {
                        "Drop here to unpin"
                    };
                    Some(
                        drop_zone(label, active, theme)
                            .id(if group == 0 {
                                "empty-pinned"
                            } else {
                                "empty-unpinned"
                            })
                            .absolute()
                            .top(px(y))
                            .left_0()
                            .right_0()
                            .h(px(CARD_H)),
                    )
                }),
        )
        .children(rows)
        .children(dragged);
    sidebar
        .child(
            div()
                .id("server-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .on_drop(cx.listener(|this, _: &ServerDrag, _, cx| this.drop_reorder(cx)))
                .child(content),
        )
        .child(footer())
}

fn update_banner(workspace: &Workspace, theme: Theme) -> Option<AnyElement> {
    match &workspace.update_state {
        UpdateState::Installing { progress, .. } => {
            let progress = progress.clamp(0., 1.);
            let bar = div()
                .debug_selector(|| "update-progress".into())
                .flex_none()
                .pb_2()
                .child(
                    div()
                        .w_full()
                        .h(px(6.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(theme.selection)
                        .child(
                            div().h_full().rounded_full().bg(theme.accent).with_spring(
                                "update-progress-fill",
                                SpringAnimation::new(SPRING)
                                    .to(progress)
                                    .with_epsilon(0.0005),
                                |fill, value| fill.w(gpui::relative(value.clamp(0., 1.))),
                            ),
                        ),
                );
            Some(motion::fade(bar, "update-progress-fade", FAST).into_any_element())
        }
        UpdateState::Available {
            error: Some(message),
            ..
        } => Some(
            motion::fade(
                div()
                    .debug_selector(|| "update-error".into())
                    .flex_none()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(theme.danger)
                    .truncate()
                    .child(message.clone()),
                "update-error-fade",
                FAST,
            )
            .into_any_element(),
        ),
        _ => None,
    }
}

fn settings_button(workspace: &Workspace, theme: Theme, cx: &Context<Workspace>) -> Stateful<Div> {
    let background = if workspace.settings_open() {
        theme.selection
    } else {
        theme.panel
    };

    div()
        .id("settings")
        .debug_selector(|| "settings".into())
        .role(Role::Button)
        .aria_label("Settings")
        .tab_index(0)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(6.))
        .bg(background)
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .hover(move |style| style.bg(theme.hover))
        .active(move |style| style.bg(theme.selection))
        .focus_visible(move |style| style.border_color(theme.accent))
        .child(
            icon(IconName::Settings)
                .size(px(15.))
                .text_color(theme.text),
        )
        .tooltip(|_, cx| cx.new(|_| Tooltip("Settings".into())).into())
        .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
}

fn update_button(workspace: &Workspace, theme: Theme, cx: &Context<Workspace>) -> Stateful<Div> {
    let base = div()
        .id("update-button")
        .debug_selector(|| "update-button".into())
        .role(Role::Button)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(26.))
        .h(px(26.))
        .rounded(px(6.));

    match &workspace.update_state {
        UpdateState::Idle => base
            .aria_label("Check for Updates")
            .tab_index(0)
            .bg(theme.panel)
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover))
            .active(move |style| style.bg(theme.selection))
            .child(
                icon(IconName::RefreshCw)
                    .size(px(15.))
                    .text_color(theme.text),
            )
            .on_click(cx.listener(|this, _, window, cx| this.check_for_updates(window, cx))),
        UpdateState::Checking => base
            .aria_label("Checking for Updates…")
            .bg(theme.selection)
            .opacity(0.75)
            .child(motion::spin(
                icon(IconName::RefreshCw)
                    .size(px(15.))
                    .text_color(theme.secondary_text),
                "update-check-spin",
            )),
        UpdateState::Available { release, .. } => base
            .aria_label(format!("Download Update {}", release.version))
            .tab_index(0)
            .bg(theme.panel)
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover))
            .active(move |style| style.bg(theme.selection))
            .child(
                icon(IconName::CircleArrowDown)
                    .size(px(15.))
                    .text_color(theme.accent),
            )
            .on_click(cx.listener(|this, _, window, cx| this.install_update(window, cx))),
        UpdateState::Installing { release, .. } => base
            .aria_label(format!("Installing Update {}…", release.version))
            .bg(theme.selection)
            .opacity(0.75)
            .child(
                icon(IconName::CircleArrowDown)
                    .size(px(15.))
                    .text_color(theme.secondary_text),
            ),
    }
}

fn group_header(label: &'static str, theme: Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(HEADER_H))
        .px_2()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.secondary_text)
        .child(label)
}

/// Separates the pinned group from the unpinned one, which has no header.
fn divider(theme: Theme) -> Div {
    div().flex_none().h(px(1.)).bg(theme.border)
}

/// Shown while dragging where a group has no servers.
fn drop_zone(label: &'static str, active: bool, theme: Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .border_1()
        .border_dashed()
        .text_size(px(12.))
        .when(active, |zone| {
            zone.border_color(theme.accent)
                .bg(theme.selection)
                .text_color(theme.text)
        })
        .when(!active, |zone| {
            zone.border_color(theme.control_border)
                .text_color(theme.secondary_text)
        })
        .child(label)
}

fn status(connection: Option<&Connection>, theme: Theme) -> (&'static str, gpui::Rgba) {
    match connection {
        Some(Connection::Connecting { .. }) => ("Connecting", theme.warning),
        Some(Connection::Disconnecting) => ("Disconnecting", theme.warning),
        Some(Connection::Connected { .. }) => ("Connected", theme.success),
        Some(Connection::Error(_)) => ("Error", theme.danger),
        None => ("Disconnected", theme.secondary_text),
    }
}

fn labels(server: &Server, theme: Theme, end_padding: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .when(end_padding, |text| text.pr(px(28.)))
        .child(
            div()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(server.display_name().to_owned()),
        )
        .child(
            div()
                .truncate()
                .text_size(px(12.))
                .text_color(theme.secondary_text)
                .child(format!("{}@{}", server.user, server.host)),
        )
}

/// The card that follows the cursor while a drag is in progress.
fn floating_card(server: &Server, top: f32, workspace: &Workspace, theme: Theme) -> Div {
    let connection = workspace.connections.get(&server.id);
    let (_, color) = status(connection, theme);
    let pulse =
        matches!(connection, Some(Connection::Connecting { .. })).then(|| server.id.clone());
    div()
        .absolute()
        .top(px(top))
        .left_0()
        .right_0()
        .h(px(CARD_H))
        .flex()
        .items_center()
        .px_2()
        .gap_2()
        .rounded(px(6.))
        .border_1()
        .border_color(theme.accent)
        .bg(theme.raised)
        .shadow_md()
        .child(status_dot(color, pulse))
        .child(labels(server, theme, false))
}

fn row(
    server: &Server,
    y: f32,
    settle_from: Option<f32>,
    workspace: &Workspace,
    theme: Theme,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let id = &server.id;
    let selected = workspace.selected.as_ref() == Some(id);
    let menu_open = workspace
        .row_menu
        .as_ref()
        .is_some_and(|menu| &menu.id == id);
    let connection = workspace.connections.get(id);
    let (status, color) = status(connection, theme);
    let pulse = matches!(connection, Some(Connection::Connecting { .. })).then(|| id.clone());
    let select_id = id.clone();
    let menu_id = id.clone();
    let pin_id = id.clone();
    let pinned = server.pinned;
    let shown = selected || menu_open;
    let group = SharedString::from(format!("server-row-{id}"));
    let drag = ServerDrag { id: id.clone() };
    let workspace_handle = cx.entity().downgrade();
    let root_focus = workspace.focus_handle();
    let endpoint = format!("{}@{}", server.user, server.host);
    let mut spring = SpringAnimation::new(SPRING).to(px(y)).with_epsilon(0.3);
    if let Some(from) = settle_from {
        spring = spring.from(px(from));
    }
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(|| format!("server-row-{id}"))
        .absolute()
        .left_0()
        .right_0()
        .h(px(ROW_H))
        .pb(px(2.))
        .child(
            div()
                .group(group.clone())
                .relative()
                .flex()
                .items_center()
                .h(px(CARD_H))
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
                        .h_full()
                        .px_2()
                        .gap_2()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(if menu_open {
                            gpui::Hsla::from(theme.accent)
                        } else {
                            gpui::transparent_black()
                        })
                        .when(!selected, |row| {
                            row.hover(move |style| style.bg(theme.hover))
                        })
                        .focus_visible(move |style| style.border_color(theme.accent))
                        .cursor_pointer()
                        .aria_label(format!("{}, {endpoint}, {status}", server.display_name()))
                        .child(status_dot(color, pulse))
                        .child(labels(server, theme, shown))
                        .on_drag(drag, move |drag, offset, window, cx| {
                            // The row leaves the tree while dragged, so Escape needs the root focus.
                            root_focus.focus(window, cx);
                            let _ = workspace_handle.update(cx, |workspace, cx| {
                                workspace.begin_reorder(&drag.id, offset.y.as_f32(), cx);
                            });
                            cx.new(|_| NoPreview)
                        })
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.select_session(&select_id, window, cx);
                            if event.click_count() == 2 || matches!(event, ClickEvent::Keyboard(_))
                            {
                                this.connect(&select_id, window, cx);
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.select(&menu_id);
                                this.open_row_menu(&menu_id, event.position, window, cx);
                            }),
                        ),
                )
                .child(
                    // Inside the row, so the border stays whole.
                    pin_button(
                        pinned,
                        theme,
                        cx.listener(move |this, _, _, cx| this.toggle_pin(&pin_id, cx)),
                    )
                    .absolute()
                    .right(px(6.))
                    .top(px((CARD_H - 28.) / 2.))
                    .when(!shown, |button| {
                        button
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .tab_stop(false)
                    }),
                ),
        )
        .with_spring(format!("server-y-{id}"), spring, move |row, y| row.top(y))
}

/// Pin toggle shown on hover, and while the row is selected or its menu is open.
fn pin_button(
    pinned: bool,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    div()
        .id("pin-server")
        .role(Role::Button)
        .aria_label(if pinned { "Unpin" } else { "Pin" })
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
        .focus_visible(move |style| style.border_color(theme.accent))
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
        .on_click(on_click)
}

/// The floating menu opened by right-clicking a server row.
pub fn row_menu(
    menu: &RowMenu,
    server: &Server,
    connection: Option<&Connection>,
    theme: Theme,
    cx: &Context<Workspace>,
) -> AnyElement {
    let connected = matches!(connection, Some(Connection::Connected { .. }));
    let working = matches!(
        connection,
        Some(Connection::Connecting { .. } | Connection::Disconnecting)
    );
    let items: [(RowAction, IconName, &'static str, bool, bool); 5] = [
        (
            RowAction::Session,
            if connected {
                IconName::Unplug
            } else {
                IconName::Plug
            },
            if connected { "Disconnect" } else { "Connect" },
            !working,
            false,
        ),
        (
            RowAction::Pin,
            if server.pinned {
                IconName::PinOff
            } else {
                IconName::Pin
            },
            if server.pinned { "Unpin" } else { "Pin" },
            true,
            false,
        ),
        (RowAction::Edit, IconName::Pencil, "Edit…", !working, false),
        (
            RowAction::Duplicate,
            IconName::Copy,
            "Duplicate…",
            true,
            false,
        ),
        (
            RowAction::Delete,
            IconName::Trash,
            "Delete…",
            !working,
            true,
        ),
    ];
    let item = |index: usize| {
        let (action, name, label, enabled, danger) = items[index];
        menu_item(name, label, "", enabled, danger, theme)
            .id(("row-menu-action", index))
            .role(Role::MenuItem)
            .tab_index(0)
            .tab_stop(enabled)
            .when(index == 0, |row| row.track_focus(&menu.first))
            .when(index == items.len() - 1, |row| row.track_focus(&menu.last))
            .when(enabled, |row| {
                row.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.row_menu_action(action, window, cx)
                    }))
            })
    };
    let panel = div()
        .id("server-menu")
        .role(Role::Menu)
        .track_focus(&menu.scope)
        .key_context("RowMenu")
        .tab_group()
        .tab_stop(false)
        .on_action({
            let scope = menu.scope.clone();
            let first = menu.first.clone();
            cx.listener(move |_, _: &modal::Next, window, cx| {
                window.focus_next(cx);
                if !scope.contains_focused(window, cx) {
                    first.focus(window, cx);
                }
            })
        })
        .on_action({
            let scope = menu.scope.clone();
            let last = menu.last.clone();
            cx.listener(move |_, _: &modal::Previous, window, cx| {
                window.focus_prev(cx);
                if !scope.contains_focused(window, cx) {
                    last.focus(window, cx);
                }
            })
        })
        .occlude()
        .w(px(200.))
        .p_1()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised)
        .shadow(vec![gpui::BoxShadow {
            color: theme.overlay,
            offset: point(px(0.), px(4.)),
            blur_radius: px(16.),
            inset: false,
            spread_radius: px(0.),
        }])
        .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss_row_menu(window, cx)))
        .child(item(0))
        .child(item(1))
        .child(item(2))
        .child(item(3))
        .child(div().h(px(1.)).mx_2().my_1().bg(theme.border))
        .child(item(4));
    deferred(
        anchored()
            .position(menu.position)
            .snap_to_window_with_margin(px(8.))
            .child(motion::appear(panel, "server-menu-appear", 3., FAST)),
    )
    .with_priority(1)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(pinned: bool, index: usize) -> Target {
        Target { pinned, index }
    }

    #[test]
    fn layout_without_pinned_servers_has_no_header() {
        let layout = layout(&[], &["a", "b"], None);
        assert_eq!(layout.rows, [("a", 0.), ("b", ROW_H)]);
        assert_eq!((layout.header, layout.divider), (None, None));
        assert_eq!(layout.height, 2. * ROW_H + LIST_END);
    }

    #[test]
    fn layout_with_pinned_servers_adds_header_and_divider() {
        let layout = layout(&["p", "q"], &["a"], None);
        assert_eq!(layout.header, Some(0.));
        assert_eq!(layout.rows[0], ("p", HEADER_H));
        assert_eq!(layout.rows[1], ("q", HEADER_H + ROW_H));
        let divider = HEADER_H + 2. * ROW_H;
        assert_eq!(layout.divider, Some(divider + 6.));
        assert_eq!(layout.rows[2], ("a", divider + DIVIDER_H));
    }

    #[test]
    fn dragging_opens_a_gap_and_closes_the_origin() {
        // Dragging "b" to the front of its group.
        let layout = layout(&["a", "b", "c"], &["u"], Some(("b", target(true, 0))));
        assert_eq!(layout.row_y("b"), None);
        assert_eq!(layout.row_y("a"), Some(HEADER_H + ROW_H));
        assert_eq!(layout.row_y("c"), Some(HEADER_H + 2. * ROW_H));
        // The group still has three slots, so the unpinned group stays put.
        assert_eq!(layout.row_y("u"), Some(HEADER_H + 3. * ROW_H + DIVIDER_H));
        // Dragging it into the other group moves rows of both.
        let layout = super::layout(&["a", "b", "c"], &["u"], Some(("b", target(false, 0))));
        assert_eq!(layout.row_y("c"), Some(HEADER_H + ROW_H));
        assert_eq!(
            layout.row_y("u"),
            Some(HEADER_H + 2. * ROW_H + DIVIDER_H + ROW_H)
        );
    }

    #[test]
    fn empty_groups_show_a_zone_as_one_slot() {
        // The only unpinned server leaves its group.
        let layout = layout(&["a"], &["u"], Some(("u", target(true, 1))));
        let zone = HEADER_H + 2. * ROW_H + DIVIDER_H;
        assert_eq!(layout.zones, [None, Some((zone, false))]);
        // Dropping it back highlights the zone.
        let layout = super::layout(&["a"], &["u"], Some(("u", target(false, 0))));
        assert_eq!(layout.zones[1].map(|(_, active)| active), Some(true));
        // The only pinned server leaves: its slot becomes a zone of the same height.
        let layout = super::layout(&["p"], &["u"], Some(("p", target(false, 1))));
        assert_eq!(layout.zones[0], Some((HEADER_H, false)));
        assert_eq!(layout.row_y("u"), Some(HEADER_H + ROW_H + DIVIDER_H));
    }

    #[test]
    fn target_follows_the_card_centre_within_a_group() {
        // Pinned [a, b, c, d] without the dragged one, which was in this group.
        let top = HEADER_H;
        let at = |rows: f32| target_for(4, 2, true, target(true, 0), 0., top + rows * ROW_H);
        assert_eq!(at(0.), target(true, 0));
        assert_eq!(at(0.99), target(true, 0));
        assert_eq!(at(1.), target(true, 1));
        assert_eq!(at(2.5), target(true, 2));
        assert_eq!(at(3.9), target(true, 3));
        // Past the last row it is the end of the group.
        assert_eq!(at(4.2), target(true, 4));
        assert_eq!(at(4.9), target(true, 4));
        // Above the header it is the first slot.
        assert_eq!(
            target_for(4, 2, true, target(true, 0), 0., -30.),
            target(true, 0)
        );
    }

    #[test]
    fn a_card_moving_down_by_one_and_six_tenths_rows_skips_two_slots() {
        // Pinned [a, b, c, d, e]; "b" (slot 1) is dragged, so four others remain.
        let start = HEADER_H + 1.5 * ROW_H;
        let at = |rows: f32| target_for(4, 2, true, target(true, 1), 0., start + rows * ROW_H);
        assert_eq!(at(0.), target(true, 1));
        assert_eq!(at(0.4), target(true, 1));
        assert_eq!(at(0.6), target(true, 2));
        assert_eq!(at(1.4), target(true, 2));
        assert_eq!(at(1.6), target(true, 3));
    }

    #[test]
    fn crossing_the_divider_switches_the_group() {
        // Pinned [a, b, c] and unpinned [x, y, z] besides the dragged one.
        let leaving = |centre| target_for(3, 3, true, target(true, 3), 0., centre);
        let divider_middle = HEADER_H + 4. * ROW_H + DIVIDER_H / 2.;
        assert_eq!(leaving(divider_middle - 0.5), target(true, 3));
        assert_eq!(leaving(divider_middle + 0.5), target(false, 0));
        // Once in the other group its slot is open there, so the divider has moved up.
        let returning = |centre| target_for(3, 3, true, target(false, 0), 0., centre);
        let divider_middle = HEADER_H + 3. * ROW_H + DIVIDER_H / 2.;
        assert_eq!(returning(divider_middle + 0.5), target(false, 0));
        assert_eq!(returning(divider_middle - 0.5), target(true, 3));
        let unpinned_top = HEADER_H + 3. * ROW_H + DIVIDER_H;
        assert_eq!(returning(unpinned_top + 1.5 * ROW_H), target(false, 1));
        assert_eq!(returning(unpinned_top + 40. * ROW_H), target(false, 3));
    }

    #[test]
    fn an_empty_group_is_a_single_slot() {
        // No other pinned server: the zone is one row, the divider follows it.
        let divider_middle = HEADER_H + ZONE_H + DIVIDER_H / 2.;
        let from_zone = |centre| target_for(0, 2, true, target(true, 0), 0., centre);
        assert_eq!(from_zone(divider_middle - 1.), target(true, 0));
        assert_eq!(from_zone(divider_middle + 1.), target(false, 0));
        // No other unpinned server.
        let unpinned_top = HEADER_H + 2. * ROW_H + DIVIDER_H;
        assert_eq!(
            target_for(2, 0, true, target(false, 0), 0., unpinned_top + 30.),
            target(false, 0)
        );
    }

    #[test]
    fn without_pinned_servers_the_heading_is_the_pinned_zone() {
        let at = |pointer, centre| target_for(0, 3, false, target(false, 0), pointer, centre);
        assert_eq!(at(-5., 20.), target(true, 0));
        assert_eq!(at(5., 20.), target(false, 0));
        assert_eq!(at(60., 1.5 * ROW_H), target(false, 1));
        assert_eq!(at(400., 400.), target(false, 3));
    }

    #[test]
    fn the_card_stays_inside_the_list() {
        let reorder = |pointer_y| ServerReorder {
            id: "a".into(),
            grab_offset_y: 10.,
            pointer_y,
            target: target(false, 0),
            held: false,
        };
        let height = 4. * ROW_H + LIST_END;
        assert_eq!(card_top(&reorder(-50.), height), 0.);
        assert_eq!(card_top(&reorder(60.), height), 50.);
        assert_eq!(card_top(&reorder(900.), height), 3. * ROW_H);
    }
}
