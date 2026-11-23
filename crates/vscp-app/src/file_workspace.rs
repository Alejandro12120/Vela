use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, ClickEvent, Div, Entity, FocusHandle,
    FontWeight, Stateful, Window, div, prelude::*, px,
};
use vscp_core::{Session, config::Server};

use crate::{
    components::{
        button::{Variant, icon_text_button},
        motion,
    },
    dev,
    icons::IconName,
    pane::{Pane, PaneMemory, Source},
    terminal::view::TerminalView,
    theme::Theme,
    transfers_view::{TransferOverlay, Transfers},
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionView {
    #[default]
    Files,
    Terminal,
}

impl SessionView {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "Archivos",
            Self::Terminal => "Terminal",
        }
    }
}

pub struct Panes {
    pub view: SessionView,
    pub terminal: Option<Entity<TerminalView>>,
    pub switched: Option<Instant>,
    local: Entity<Pane>,
    remote: Entity<Pane>,
    local_focus: FocusHandle,
    transfers: Entity<Transfers>,
    overlay: Entity<TransferOverlay>,
}

impl Panes {
    pub fn new(
        session: Session,
        server: &Server,
        remote_path: PathBuf,
        memory: PaneMemory,
        sidebar: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let transfers = cx.new(|cx| Transfers::new(session.clone(), server.name.clone(), cx));
        let overlay = cx.new(|cx| TransferOverlay::new(transfers.clone(), cx));
        let local_focus = cx.focus_handle().tab_stop(true);
        let remote_focus = cx.focus_handle().tab_stop(true);
        let local_path = dev::local_root()
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| "/".into());
        let local = cx.new(|cx| {
            Pane::new(
                Source::Local,
                local_path,
                memory.local,
                sidebar.clone(),
                local_focus.clone(),
                remote_focus.clone(),
                cx,
            )
        });
        let remote = cx.new(|cx| {
            Pane::new(
                Source::Remote(session),
                remote_path,
                memory.remote,
                sidebar,
                remote_focus,
                local_focus.clone(),
                cx,
            )
        });
        let server = Arc::<str>::from(server.id.as_str());
        local.update(cx, |pane, cx| {
            pane.wire(server.clone(), remote.downgrade(), transfers.clone());
            pane.start(window, cx);
        });
        remote.update(cx, |pane, cx| {
            pane.wire(server, local.downgrade(), transfers.clone());
            pane.start(window, cx);
        });
        if dev::drop_highlight() {
            crate::pane::finder::dev_drop_highlight(window, cx);
        }
        let copy_paths = dev::copy_paths();
        let paste = dev::paste_remote();
        if !copy_paths.is_empty() || paste {
            remote.update(cx, |_, cx| {
                let timer = cx.background_executor().timer(Duration::from_millis(500));
                cx.spawn_in(window, async move |pane, cx| {
                    timer.await;
                    let _ = pane.update_in(cx, |pane, window, cx| {
                        if !copy_paths.is_empty() {
                            pane.dev_copy(copy_paths, window, cx);
                        }
                        if paste {
                            pane.finder_paste(window, cx);
                        }
                    });
                })
                .detach();
            });
        }
        if let Some(spec) = dev::transfer() {
            let pane = match spec.direction {
                vscp_core::transfer::Direction::Upload => remote.downgrade(),
                vscp_core::transfer::Direction::Download => local.downgrade(),
            };
            transfers.update(cx, |queue, cx| {
                let delay = dev::transfer_delay();
                if delay.is_zero() {
                    queue.enqueue(spec, pane, window, cx);
                } else {
                    let timer = cx.background_executor().timer(delay);
                    cx.spawn_in(window, async move |queue, cx| {
                        timer.await;
                        let _ = queue.update_in(cx, |queue, window, cx| {
                            queue.enqueue(spec, pane, window, cx);
                        });
                    })
                    .detach();
                }
            });
        }
        if let Some(state) = dev::file_state()
            && let Some((side, action)) = state.split_once(':')
        {
            let pane = if side == "remote" { &remote } else { &local };
            let action = action.to_owned();
            pane.update(cx, |_, cx| {
                let timer = cx.background_executor().timer(Duration::from_millis(500));
                cx.spawn_in(window, async move |pane, cx| {
                    timer.await;
                    let _ =
                        pane.update_in(cx, |pane, window, cx| pane.dev_state(&action, window, cx));
                })
                .detach();
            });
        }
        Self {
            view: SessionView::Files,
            terminal: None,
            switched: None,
            local,
            remote,
            local_focus,
            transfers,
            overlay,
        }
    }

    pub fn local(&self) -> &Entity<Pane> {
        &self.local
    }

    pub fn remote(&self) -> &Entity<Pane> {
        &self.remote
    }

    pub fn overlay(&self) -> Entity<TransferOverlay> {
        self.overlay.clone()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.view == SessionView::Terminal
            && let Some(terminal) = &self.terminal
        {
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        } else {
            self.local_focus.focus(window, cx);
        }
    }

    fn body(&self, theme: Theme) -> AnyElement {
        let files = || {
            div()
                .size_full()
                .flex()
                .bg(theme.panel)
                .child(self.local.clone())
                .child(div().w(px(1.)).flex_none().bg(theme.border))
                .child(self.remote.clone())
        };
        let terminal = || {
            div()
                .size_full()
                .flex()
                .bg(theme.panel)
                .children(self.terminal.clone())
        };
        let (current, previous) = match self.view {
            SessionView::Files => (files(), terminal()),
            SessionView::Terminal => (terminal(), files()),
        };
        let mut body = div()
            .id("session-body")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden();
        if let Some(switched) = self.switched
            && switched.elapsed() < motion::FAST
        {
            let id = ("session-view", self.view as u32);
            body = body.child(previous.absolute().inset_0()).child(
                current.relative().occlude().with_animation(
                    id,
                    Animation::new(motion::FAST),
                    |view, progress| view.opacity(progress),
                ),
            );
        } else {
            body = body.child(current);
        }
        body.into_any_element()
    }

    pub fn render(
        &self,
        server: &Server,
        controls: (impl IntoElement, impl IntoElement, Option<AnyElement>, bool),
        theme: Theme,
        edit_header: impl IntoElement,
        edit_status: impl IntoElement,
        disconnect: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        let (sidebar_button, view_switch, kitty_button, compact) = controls;
        let identity = div()
            .flex_1()
            .min_w_0()
            .h(px(40.))
            .flex()
            .items_center()
            .gap_1()
            .child(sidebar_button)
            .child(view_switch)
            // The name keeps its natural width. `user@host` starts from zero
            // width, takes what is left and shrinks to 80 px before the name.
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(server.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(80.))
                    .truncate()
                    .text_color(theme.secondary_text)
                    .child(format!("{}@{}", server.user, server.host)),
            );
        let actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(
                icon_text_button(
                    "disconnect",
                    IconName::Unplug,
                    "Desconectar",
                    Variant::Secondary,
                    None,
                    theme,
                    disconnect,
                )
                .h(px(26.))
                .px_1(),
            )
            .child(edit_header)
            .children(kitty_button)
            .when(compact, |actions| actions.justify_end().pb_2());
        div()
            .id("main")
            .relative()
            .min_w_0()
            .min_h_0()
            .flex_1()
            .flex()
            .flex_col()
            .bg(theme.panel)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(compact, |header| header.flex_col().gap_0())
                    .child(identity)
                    .child(actions),
            )
            .child(self.body(theme))
            .child(edit_status)
            .child(self.transfers.clone())
    }
}

pub fn session_switch(
    current: SessionView,
    theme: Theme,
    switch: impl Fn(SessionView, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    div()
        .flex_none()
        .flex()
        .gap_1()
        .children([SessionView::Files, SessionView::Terminal].map(|view| {
            let switch = switch.clone();
            let name = match view {
                SessionView::Files => IconName::Folder,
                SessionView::Terminal => IconName::SquareTerminal,
            };
            icon_text_button(
                view.label(),
                name,
                view.label(),
                if current == view {
                    Variant::Selected
                } else {
                    Variant::Secondary
                },
                None,
                theme,
                move |_, window, cx| switch(view, window, cx),
            )
            .h(px(26.))
            .px_1()
            .gap_1()
        }))
}
