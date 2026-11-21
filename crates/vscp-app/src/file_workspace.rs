use std::{cell::Cell, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use gpui::{
    App, AppContext, ClickEvent, Div, Entity, FocusHandle, FontWeight, Stateful, Window, div,
    prelude::*, px,
};
use vscp_core::{Session, config::Server};

use crate::{
    components::button::{Variant, button},
    dev,
    pane::{Pane, PaneMemory, Source},
    theme::Theme,
    transfers_view::{TransferOverlay, Transfers},
};

pub struct Panes {
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
            local,
            remote,
            local_focus,
            transfers,
            overlay,
        }
    }

    pub fn overlay(&self) -> Entity<TransferOverlay> {
        self.overlay.clone()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.local_focus.focus(window, cx);
    }

    pub fn render(
        &self,
        server: &Server,
        sidebar_button: impl IntoElement,
        theme: Theme,
        disconnect: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
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
                    .h(px(40.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(sidebar_button)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(server.name.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.secondary_text)
                            .child(format!("{}@{}", server.user, server.host)),
                    )
                    .child(
                        button(
                            "disconnect",
                            "Desconectar",
                            Variant::Secondary,
                            theme,
                            disconnect,
                        )
                        .h(px(26.)),
                    )
                    .child(div().flex_1()),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.local.clone())
                    .child(div().w(px(1.)).flex_none().bg(theme.border))
                    .child(self.remote.clone()),
            )
            .child(self.transfers.clone())
    }
}
