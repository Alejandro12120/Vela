use std::path::PathBuf;

use gpui::{
    AnyElement, Context, FocusHandle, MouseButton, Pixels, Point, Role, Window, anchored, deferred,
    div, point, prelude::*, px,
};

use crate::{
    components::{menu::menu_item, modal, motion},
    icons::IconName,
    pane::Pane,
    theme::Theme,
};

#[derive(Clone, Copy)]
pub enum Action {
    NewFolder,
    NewFile,
    Rename,
    Run,
    RunInKitty,
    Delete,
    Copy,
    Refresh,
}

/// Whether "Run" is offered for the selection.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum RunState {
    Hidden,
    Disabled,
    Enabled,
    Remote,
}

struct Item {
    action: Action,
    icon: IconName,
    label: &'static str,
    shortcut: &'static str,
    enabled: bool,
    danger: bool,
}

impl Item {
    fn new(
        action: Action,
        icon: IconName,
        label: &'static str,
        shortcut: &'static str,
        enabled: bool,
    ) -> Self {
        Self {
            action,
            icon,
            label,
            shortcut,
            enabled,
            danger: false,
        }
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
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
    fn actions(&self, selected: usize, run: RunState, copy_icon: IconName) -> Vec<Item> {
        let mut actions = vec![
            Item::new(
                Action::NewFolder,
                IconName::FolderPlus,
                "New Folder",
                crate::keybindings::shortcut("⌘⇧N", "Ctrl+⇧N"),
                true,
            ),
            Item::new(
                Action::NewFile,
                IconName::FilePlus,
                "New Text File",
                "",
                true,
            ),
            Item::new(
                Action::Rename,
                IconName::Pencil,
                "Rename",
                "↵",
                self.on_row && selected == 1,
            ),
        ];
        if run != RunState::Hidden {
            actions.push(Item::new(
                Action::Run,
                IconName::Play,
                "Run",
                "",
                matches!(run, RunState::Enabled | RunState::Remote),
            ));
        }
        if run == RunState::Remote {
            actions.push(Item::new(
                Action::RunInKitty,
                IconName::ExternalLink,
                "Run in Kitty",
                "",
                true,
            ));
        }
        actions.extend([
            Item::new(
                Action::Delete,
                IconName::Trash,
                "Delete…",
                crate::keybindings::shortcut("⌘⌫", "Delete"),
                self.on_row && selected > 0,
            )
            .danger(),
            Item::new(
                Action::Copy,
                copy_icon,
                "Copy to Other Pane",
                "F5",
                self.on_row && selected > 0,
            ),
            Item::new(
                Action::Refresh,
                IconName::Refresh,
                "Refresh",
                crate::keybindings::shortcut("⌘R", "Ctrl+R"),
                true,
            ),
        ]);
        actions
    }

    pub fn render(
        &self,
        selected: usize,
        run: RunState,
        copy_icon: IconName,
        theme: Theme,
        cx: &Context<Pane>,
    ) -> AnyElement {
        let actions = self.actions(selected, run, copy_icon);
        let last = actions.len() - 1;
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
            .children(actions.into_iter().enumerate().map(|(index, item)| {
                let Item {
                    action,
                    icon,
                    label,
                    shortcut,
                    enabled,
                    danger,
                } = item;
                menu_item(icon, label, shortcut, enabled, danger, theme)
                    .id(("file-menu-action", index))
                    .role(Role::MenuItem)
                    .tab_index(0)
                    .tab_stop(enabled)
                    .when(index == 0, |row| row.track_focus(&self.first))
                    .when(index == last, |row| row.track_focus(&self.last))
                    .when(enabled, |row| {
                        row.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |pane, _, window, cx| {
                                cx.stop_propagation();
                                pane.menu_action(action, window, cx)
                            }))
                    })
            }));
        deferred(
            anchored()
                .position(self.position)
                .snap_to_window_with_margin(px(8.))
                .child(motion::appear(panel, "file-menu-appear", 3., motion::FAST)),
        )
        .with_priority(1)
        .into_any_element()
    }
}

pub struct Tooltip(pub gpui::SharedString);

impl gpui::Render for Tooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = Theme::current(window, cx);
        let (title, details) = self.0.split_once('\n').unwrap_or((&self.0, ""));
        div()
            .debug_selector(|| "tooltip".into())
            .px_2()
            .py_1()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .text_color(theme.text)
            .text_size(px(12.))
            .child(title.to_owned())
            .when(!details.is_empty(), |tooltip| {
                tooltip.child(
                    div()
                        .debug_selector(|| "tooltip-details".into())
                        .mt_1()
                        .max_w(px(280.))
                        .text_color(theme.secondary_text)
                        .children(details.lines().map(|line| div().child(line.to_owned()))),
                )
            })
    }
}
