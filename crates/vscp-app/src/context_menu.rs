use std::path::PathBuf;

use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, Pixels, Point, Role, Window, anchored, deferred,
    div, point, prelude::*, px,
};

use crate::{components::modal, pane::Pane, theme::Theme};

#[derive(Clone, Copy)]
pub enum Action {
    NewFolder,
    Rename,
    Delete,
    Copy,
    Refresh,
}

pub struct Menu {
    pub position: Point<Pixels>,
    pub folder: PathBuf,
    pub on_row: bool,
    pub scope: FocusHandle,
    pub first: FocusHandle,
    pub last: FocusHandle,
}

impl Menu {
    fn actions(&self, selected: usize) -> [(Action, &'static str, &'static str, bool); 5] {
        [
            (Action::NewFolder, "Nueva carpeta", "⌘⇧N", true),
            (
                Action::Rename,
                "Renombrar",
                "↵",
                self.on_row && selected == 1,
            ),
            (
                Action::Delete,
                "Eliminar…",
                "⌘⌫",
                self.on_row && selected > 0,
            ),
            (
                Action::Copy,
                "Copiar al otro panel",
                "F5",
                self.on_row && selected > 0,
            ),
            (Action::Refresh, "Refrescar", "⌘R", true),
        ]
    }

    pub fn render(&self, selected: usize, theme: Theme, cx: &Context<Pane>) -> AnyElement {
        let panel = div()
            .id("file-menu")
            .role(Role::Menu)
            .track_focus(&self.scope)
            .key_context("FileMenu")
            .tab_group()
            .tab_stop(false)
            .on_action({
                let scope = self.scope.clone();
                let first = self.first.clone();
                cx.listener(move |_, _: &modal::Next, window, cx| {
                    window.focus_next(cx);
                    if !scope.contains_focused(window, cx) {
                        first.focus(window, cx);
                    }
                })
            })
            .on_action({
                let scope = self.scope.clone();
                let last = self.last.clone();
                cx.listener(move |_, _: &modal::Previous, window, cx| {
                    window.focus_prev(cx);
                    if !scope.contains_focused(window, cx) {
                        last.focus(window, cx);
                    }
                })
            })
            .occlude()
            .w(px(224.))
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
            .on_mouse_down_out(cx.listener(|pane, _, _, cx| pane.dismiss_menu(cx)))
            .children(self.actions(selected).into_iter().enumerate().map(
                |(index, (action, label, key, enabled))| {
                    div()
                        .id(("file-menu-action", index))
                        .role(Role::MenuItem)
                        .tab_index(0)
                        .tab_stop(enabled)
                        .when(index == 0, |row| row.track_focus(&self.first))
                        .when(index == 4, |row| row.track_focus(&self.last))
                        .h(px(28.))
                        .px_2()
                        .rounded(px(6.))
                        .flex()
                        .justify_between()
                        .items_center()
                        .gap_2()
                        .text_color(if enabled {
                            theme.text
                        } else {
                            theme.control_border
                        })
                        .child(div().font_weight(FontWeight::MEDIUM).child(label))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme.secondary_text)
                                .child(key),
                        )
                        .when(enabled, |row| {
                            row.cursor_pointer()
                                .hover(move |s| s.bg(theme.hover))
                                .focus(move |s| s.bg(theme.selection))
                                .on_click(cx.listener(move |pane, _, window, cx| {
                                    pane.menu_action(action, window, cx)
                                }))
                        })
                },
            ));
        deferred(
            anchored()
                .position(self.position)
                .snap_to_window_with_margin(px(8.))
                .child(panel),
        )
        .with_priority(1)
        .into_any_element()
    }
}

pub struct Tooltip(pub &'static str);

impl gpui::Render for Tooltip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = Theme::for_window(window);
        div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .text_color(theme.text)
            .text_size(px(12.))
            .child(self.0)
    }
}
