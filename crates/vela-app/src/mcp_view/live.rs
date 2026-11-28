use std::{
    future::{Future, pending},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{Focusable, Task, point};
use tokio::sync::{mpsc, oneshot};
use vela_core::Session;
#[cfg(any(test, feature = "dev"))]
use vela_mcp::executor::ApprovalPreview;
use vela_mcp::{ServerIdentity, config::ServerAvailability, ipc::socket_path};

use super::*;
use crate::{
    components::button::icon_text_button,
    mcp_broker::{Activity, Answer, Approval, Backend, Broker, Reply, Status, UiRequest, Worker},
    mcp_model::{DiffLine, classify_diff_line},
    workspace::Connection,
};

#[derive(Default)]
pub(in crate::workspace) struct Live {
    broker: Broker,
    worker: Option<Worker>,
    bridge: Option<Task<()>>,
    popover: bool,
    filter: Filter,
    selected: usize,
    denial: Option<(u64, Entity<TextInput>, Option<FocusHandle>)>,
    #[cfg(feature = "dev")]
    preview: Option<Vec<oneshot::Receiver<Answer>>>,
}

impl Live {
    pub(in crate::workspace) fn stop(&mut self) -> impl Future<Output = ()> + use<> {
        self.bridge.take();
        let worker = self.worker.take();
        async {
            if let Some(worker) = worker {
                worker.shutdown().await;
            }
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Approved,
    Blocked,
    Rollback,
}

impl Filter {
    fn matches(self, row: &Activity) -> bool {
        match self {
            Self::All => true,
            Self::Approved => matches!(
                row.status,
                Status::Auto | Status::Approved | Status::Reverted
            ),
            Self::Blocked => matches!(row.status, Status::Blocked | Status::Denied),
            Self::Rollback => row.backup_id.is_some(),
        }
    }
}

impl Workspace {
    pub(in crate::workspace) fn sync_mcp_broker(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.quitting {
            return;
        }
        #[cfg(feature = "dev")]
        if self.dev_mcp_live(window, cx) {
            return;
        }
        let config = self.mcp.config().cloned().unwrap_or_default();
        let enabled = config.enabled;
        self.mcp_live.broker.configure(config);
        if let Some(worker) = &self.mcp_live.worker {
            worker.set_enabled(enabled);
            return;
        }
        if enabled {
            let path = if cfg!(test) {
                self.dir
                    .as_ref()
                    .expect("isolated test configuration")
                    .join("mcp.sock")
            } else {
                socket_path(None)
            };
            self.start_mcp_broker(path, window, cx);
        }
    }

    fn start_mcp_broker(&mut self, path: PathBuf, window: &Window, cx: &mut Context<Self>) {
        let (ui, mut requests) = mpsc::unbounded_channel();
        let broker = self.mcp_live.broker.clone();
        let mut updates = broker.subscribe();
        let worker = match Worker::start(broker.clone(), path, ui) {
            Ok(worker) => worker,
            Err(error) => {
                self.mcp.config_error = Some(format!("No se pudo iniciar el broker MCP. {error}"));
                return;
            }
        };
        let executor = cx.background_executor().clone();
        self.mcp_live.worker = Some(worker);
        self.mcp_live.bridge = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let ticking = {
                    let state = broker.state();
                    !state.pending().is_empty()
                        || state.jit.active_count(SystemTime::now()) > 0
                        || state
                            .conflict
                            .as_ref()
                            .is_some_and(|(until, _)| *until > Instant::now())
                };
                let request = tokio::select! {
                    request = requests.recv() => match request {
                        Some(request) => Some(request),
                        None => break,
                    },
                    change = updates.changed() => {
                        if change.is_err() { break; }
                        None
                    }
                    _ = async {
                        if ticking {
                            executor.timer(Duration::from_secs(1)).await;
                        } else {
                            pending::<()>().await;
                        }
                    } => None
                };
                if this
                    .update_in(cx, |this, window, cx| {
                        if let Some(request) = request {
                            this.mcp_request(request, window, cx);
                        }
                        let queue = this.mcp_live.broker.state().pending();
                        if this
                            .mcp_live
                            .denial
                            .as_ref()
                            .is_some_and(|(id, _, _)| !queue.iter().any(|item| item.id == *id))
                        {
                            this.close_mcp_denial(window, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn mcp_backend(&self, cx: &Context<Self>) -> Backend {
        let mut backend = Backend {
            dir: self.dir.clone().unwrap_or_default(),
            edits: self.edits.rows().to_vec(),
            ..Backend::default()
        };
        for server in &self.servers {
            backend
                .labels
                .insert(server.id.clone(), server.display_name().to_owned());
            backend.servers.push((
                server.id.clone(),
                ServerIdentity {
                    host: server.host.clone(),
                    port: server.port,
                    user: server.user.clone(),
                },
            ));
        }
        for (id, connection) in &self.connections {
            if let Connection::Connected { session, panes } = connection {
                backend.sessions.insert(id.clone(), session.clone());
                if self.selected.as_ref() == Some(id)
                    && self.mcp.config().is_some_and(|config| {
                        config.share_ui_context
                            && config.servers.get(id).is_some_and(|server| {
                                server.availability != ServerAvailability::Disabled
                            })
                    })
                {
                    backend.context = Some((
                        id.clone(),
                        panes
                            .remote()
                            .read(cx)
                            .directory()
                            .to_string_lossy()
                            .into_owned(),
                    ));
                }
            }
        }
        backend
    }

    fn mcp_request(&mut self, request: UiRequest, window: &mut Window, cx: &mut Context<Self>) {
        match request {
            UiRequest::Backend(reply) => {
                let _ = reply.send(self.mcp_backend(cx));
            }
            UiRequest::Connect(id, reply) => self.connect_for_agent(id, reply, window, cx),
            UiRequest::Approval(item) => {
                if !window.is_window_active()
                    && self
                        .mcp_live
                        .broker
                        .state()
                        .pending()
                        .iter()
                        .any(|pending| pending.id == item.id)
                {
                    cx.background_executor()
                        .spawn(async move { notify_approval(&item) })
                        .detach();
                }
            }
            UiRequest::Refresh(id) => self.refresh_for_agent(&id, window, cx),
            UiRequest::SaveRule(id, rule, reply) => {
                if !reply.is_closed()
                    && self.mcp.config().is_some_and(|config| {
                        config.enabled
                            && config.servers.get(&id).is_some_and(|server| {
                                server.availability != ServerAvailability::Disabled
                            })
                    })
                {
                    self.save_mcp_with_reply(window, cx, false, Some(reply), |config| {
                        if let Some(policy) = config.servers.get_mut(&id)
                            && !policy.scoped_rules.contains(&rule)
                        {
                            policy.scoped_rules.push(rule);
                        }
                    });
                }
            }
        }
    }

    fn connect_for_agent(
        &mut self,
        id: String,
        mut reply: Reply<Option<Session>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if reply.is_closed() {
            return;
        }
        let joining = matches!(
            self.connections.get(&id),
            Some(Connection::Connecting { .. })
        );
        if !joining
            && !matches!(
                self.connections.get(&id),
                Some(Connection::Connected { .. })
            )
        {
            if !self.mcp.config().is_some_and(|config| {
                config.enabled
                    && config.servers.get(&id).is_some_and(|server| {
                        server.availability == ServerAvailability::OnDemandAsk
                    })
            }) {
                return;
            }
            self.connect(&id, window, cx);
        }
        let attempt = match self.connections.get(&id) {
            Some(Connection::Connecting { attempt, .. }) => *attempt,
            Some(Connection::Connected { session, .. }) => {
                let _ = reply.send(Some(session.clone()));
                return;
            }
            _ => return,
        };
        let (done, connected) = oneshot::channel();
        let id_for_observer = id.clone();
        let mut done = Some(done);
        let observer = cx.observe_self(move |this, _| {
            if matches!(this.connections.get(&id_for_observer), Some(Connection::Connecting { attempt: current, .. }) if *current == attempt) {
                return;
            }
            if let Some(done) = done.take() {
                let session = match this.connections.get(&id_for_observer) {
                    Some(Connection::Connected { session, .. }) => Some(session.clone()),
                    _ => None,
                };
                let _ = done.send(session);
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let _observer = observer;
            tokio::select! {
                biased;
                _ = reply.closed() => {
                    if !joining {
                        let _ = this.update_in(cx, |this, window, cx| {
                            if matches!(this.connections.get(&id), Some(Connection::Connecting { attempt: current, .. }) if *current == attempt) {
                                this.cancel_connect(&id, window, cx);
                            }
                        });
                    }
                }
                result = connected => { let _ = reply.send(result.ok().flatten()); }
            }
        }).detach();
    }

    fn refresh_for_agent(&self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Connection::Connected { panes, .. }) = self.connections.get(id) {
            panes
                .remote()
                .update(cx, |pane, cx| pane.refresh(window, cx));
        }
    }

    fn rollback_agent_change(&mut self, id: u64, window: &Window, cx: &mut Context<Self>) {
        let Some(worker) = &self.mcp_live.worker else {
            return;
        };
        let broker = self.mcp_live.broker.clone();
        let activity = {
            let mut state = broker.state();
            let Some(row) = state.history.iter_mut().find(|row| {
                row.id == id
                    && row.backup_id.is_some()
                    && row.status != Status::Reverted
                    && !row.restoring
            }) else {
                return;
            };
            row.restoring = true;
            row.clone()
        };
        let backend = self.mcp_backend(cx);
        let task = worker.runtime.spawn({
            let broker = broker.clone();
            let activity = activity.clone();
            async move { broker.restore(&activity, backend).await }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Se cerró el broker MCP.".into()));
            let refresh = result.is_ok();
            broker.restored(activity.clone(), result);
            let _ = this.update_in(cx, |this, window, cx| {
                if refresh && let Some(id) = &activity.server_id {
                    this.refresh_for_agent(id, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::workspace) fn mcp_agent_header(
        &self,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.mcp_live.broker.state();
        if !state.config.enabled {
            return None;
        }
        let count = state.pending().len();
        let jit = state.jit.active_count(SystemTime::now()) > 0;
        if count == 0 && !jit && state.agents.is_empty() && state.history.is_empty() {
            return None;
        }
        Some(
            icon_text_button(
                "mcp-agents",
                IconName::Shield,
                "Agentes",
                if self.mcp_live.popover {
                    Variant::Selected
                } else {
                    Variant::Secondary
                },
                None,
                theme,
                cx.listener(|this, _, _, cx| {
                    this.mcp_live.popover = !this.mcp_live.popover;
                    this.edits.popover = false;
                    cx.notify();
                }),
            )
            .h(px(26.))
            .when(count > 0 || jit, |button| {
                button.child(pill(
                    if count > 0 {
                        count.to_string()
                    } else {
                        "JIT".into()
                    },
                    theme.warning,
                    theme.warning_bg,
                ))
            })
            .into_any_element(),
        )
    }

    pub(in crate::workspace) fn dismiss_mcp_activity(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.mcp_live.popover) {
            cx.notify();
        }
    }

    pub(in crate::workspace) fn mcp_live_overlay(
        &self,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.mcp_live.broker.state();
        if !state.config.enabled || self.dialog.is_some() || self.mcp.is_open() {
            return None;
        }
        let queue = state.pending();
        let index = self.mcp_live.selected.min(queue.len().saturating_sub(1));
        let activity = self
            .mcp_live
            .popover
            .then(|| self.agent_activity(&state, theme, window, cx));
        let approval = queue
            .get(index)
            .map(|item| self.approval_popup(item, index, queue.len(), theme, window, cx));
        let conflict = state
            .conflict
            .as_ref()
            .filter(|(until, _)| *until > Instant::now())
            .map(|(_, message)| {
                div()
                    .absolute()
                    .right(px(12.))
                    .when(approval.is_none(), |banner| banner.bottom(px(12.)))
                    .when(approval.is_some(), |banner| banner.top(px(94.)))
                    .w(px((f32::from(window.viewport_size().width) - 32.).min(456.)))
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(theme.warning_bg)
                    .text_color(theme.warning)
                    .child(
                        div()
                            .flex_none()
                            .mt(px(2.))
                            .child(icon(IconName::Warning).text_color(theme.warning)),
                    )
                    .child(div().min_w_0().flex_1().child(short(message, 220)))
            });
        Some(
            div()
                .absolute()
                .inset_0()
                .children(conflict)
                .children(approval)
                .children(activity)
                .into_any_element(),
        )
    }

    fn approval_popup(
        &self,
        item: &Approval,
        index: usize,
        count: usize,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = item.id;
        let preview = &item.preview;
        let target = preview
            .paths
            .iter()
            .chain(preview.scope_parameter.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        let width = window.viewport_size().width.min(px(500.)) - px(24.);
        let can_grant = (!preview.paths.is_empty() || preview.command.is_some())
            && !preview.operation.starts_with("Conectar por SSH");
        let decision = |label, action, variant| {
            let confirm = cx.listener(move |this, _, window, cx| {
                this.mcp_live.broker.answer(
                    id,
                    match action {
                        "mcp-session" => Answer::Session,
                        "mcp-save" => Answer::Save,
                        _ => Answer::Once,
                    },
                );
                this.close_mcp_denial(window, cx);
            });
            small_button(action, label, variant, theme, confirm)
                .debug_selector(move || action.into())
        };
        let deny_open = cx.listener(move |this, _, window, cx| {
            this.close_mcp_denial(window, cx);
            let previous = window.focused(cx);
            let input = cx.new(|cx| {
                TextInput::new(
                    "",
                    "Motivo opcional para el agente",
                    "Motivo opcional para el agente",
                    false,
                    cx,
                )
                .compact()
            });
            input.read(cx).focus_handle(cx).focus(window, cx);
            this.mcp_live.denial = Some((id, input, previous));
            cx.notify();
        });
        let note = if preview.operation.starts_with("Conectar por SSH") {
            "La conexión abre una sesión en Vela"
        } else if !matches!(
            preview.operation.as_str(),
            "fs_patch" | "fs_write" | "fs_delete"
        ) {
            "Esta operación no genera copia de rollback"
        } else if !item.rollback
            || preview
                .warnings
                .iter()
                .any(|warning| warning.starts_with("FileExceedsRollbackLimit"))
        {
            "Sin copia de rollback"
        } else {
            "Copia de rollback si existe el archivo y cabe en el límite"
        };
        let code = preview
            .unified_diff
            .as_deref()
            .or(preview.command.as_deref());
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(icon(IconName::Shield).text_color(theme.warning))
            .child(
                div()
                    .max_w(px(160.))
                    .min_w_0()
                    .font_weight(FontWeight::MEDIUM)
                    .truncate()
                    .child(short(&item.agent, 80)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.secondary_text)
                    .child(format!("en {}", short(&item.server, 60))),
            )
            .when(count > 1, |header| {
                header.child(
                    div().flex().items_center().gap_1().children(
                        [
                            (-1, "Anterior", IconName::ArrowLeft),
                            (1, "Siguiente", IconName::ArrowRight),
                        ]
                        .map(|(step, label, icon)| {
                            small_icon_button(
                                label,
                                label,
                                icon,
                                Variant::Secondary,
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    this.close_mcp_denial(window, cx);
                                    this.mcp_live.selected =
                                        (index as isize + step).rem_euclid(count as isize) as usize;
                                    cx.notify();
                                }),
                            )
                            .debug_selector(move || label.into())
                        })
                        .into_iter()
                        .enumerate()
                        .flat_map(|(position, button)| {
                            let number = (position == 0).then(|| {
                                div()
                                    .text_size(px(12.))
                                    .font_family(crate::theme::monospace())
                                    .child(format!("{} / {count}", index + 1))
                                    .into_any_element()
                            });
                            [Some(button.into_any_element()), number]
                                .into_iter()
                                .flatten()
                        }),
                    ),
                )
            });
        panel("mcp-approval", width, theme)
            .absolute()
            .right(px(12.))
            .bottom(px(12.))
            .p_3()
            .gap_2()
            .child(header)
            .child(
                div()
                    .font_family(crate::theme::monospace())
                    .text_size(px(12.))
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(format!(
                        "{}{}{}",
                        preview.operation,
                        if target.is_empty() { "" } else { " · " },
                        short(&target, 240)
                    )),
            )
            .when(!preview.warnings.is_empty(), |panel| {
                panel.child(div().flex().flex_wrap().gap_1().children(
                    preview.warnings.iter().take(8).map(|warning| {
                        pill(warning_label(warning), theme.warning, theme.warning_bg)
                    }),
                ))
            })
            .when_some(preview.agent_reason.as_ref(), |panel, reason| {
                panel.child(
                    div()
                        .text_color(theme.secondary_text)
                        .child(format!("«{}»", short(reason, 200))),
                )
            })
            .when(
                preview
                    .unified_diff
                    .as_ref()
                    .is_some_and(|diff| diff.lines().nth(160).is_some()),
                |panel| {
                    panel.child(
                        div()
                            .text_color(theme.warning)
                            .text_size(px(12.))
                            .child("Vista previa limitada a 160 líneas"),
                    )
                },
            )
            .when_some(code, |panel, code| {
                panel.child(code_box(id, code, preview.unified_diff.is_some(), theme))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_size(px(12.))
                    .text_color(theme.secondary_text)
                    .child(div().min_w_0().child(note))
                    .child(
                        div()
                            .flex_none()
                            .font_family(crate::theme::monospace())
                            .child(format!(
                                "Caduca en {}",
                                countdown(item.deadline.saturating_duration_since(Instant::now()))
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(decision("Permitir esta vez", "mcp-once", Variant::Primary))
                    .when(can_grant, |actions| {
                        actions
                            .child(decision(
                                "Sesión (15 min)",
                                "mcp-session",
                                Variant::Secondary,
                            ))
                            .child(
                                decision("Guardar regla", "mcp-save", Variant::Secondary)
                                    .border_color(theme.border),
                            )
                    })
                    .child(danger_button("mcp-deny", "Denegar", theme, deny_open).ml_auto()),
            )
            .when_some(
                self.mcp_live
                    .denial
                    .as_ref()
                    .filter(|(current, _, _)| *current == id),
                |panel, (_, input, _)| {
                    panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(input.clone()))
                            .child(danger_button(
                                "mcp-deny-send",
                                "Enviar",
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    let Some((current, input, _)) = &this.mcp_live.denial else {
                                        return;
                                    };
                                    if *current == id {
                                        this.mcp_live.broker.answer(
                                            id,
                                            Answer::Deny(
                                                input.read(cx).value().chars().take(500).collect(),
                                            ),
                                        );
                                        this.close_mcp_denial(window, cx);
                                    }
                                }),
                            )),
                    )
                },
            )
            .into_any_element()
    }

    fn close_mcp_denial(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, input, previous)) = self.mcp_live.denial.take()
            && input.read(cx).focus_handle(cx).is_focused(window)
            && let Some(previous) = previous
        {
            previous.focus(window, cx);
        }
        cx.notify();
    }

    fn agent_activity(
        &self,
        state: &crate::mcp_broker::State,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut agents = state
            .agents
            .values()
            .map(|agent| short(agent, 60))
            .collect::<Vec<_>>();
        agents.sort();
        let summary = if agents.is_empty() {
            "Ningún agente conectado".into()
        } else {
            format!("{} conectados · {}", agents.len(), agents.join(", "))
        };
        let width = (window.viewport_size().width - px(24.)).min(px(620.));
        let heading = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Actividad de agentes"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.secondary_text)
                            .truncate()
                            .child(summary),
                    ),
            )
            .child(danger_button(
                "mcp-kill",
                "Cortar agentes",
                theme,
                cx.listener(|this, _, window, cx| {
                    this.mcp_live.broker.kill();
                    this.close_mcp_denial(window, cx);
                }),
            ));
        let filters = div().flex().gap_1().px_3().py_2().children(
            [
                (Filter::All, "Todos"),
                (Filter::Approved, "Aprobados"),
                (Filter::Blocked, "Bloqueados"),
                (Filter::Rollback, "Con rollback"),
            ]
            .map(|(filter, label)| {
                small_button(
                    label,
                    label,
                    if filter == self.mcp_live.filter {
                        Variant::Selected
                    } else {
                        Variant::Secondary
                    },
                    theme,
                    cx.listener(move |this, _, _, cx| {
                        this.mcp_live.filter = filter;
                        cx.notify();
                    }),
                )
                .debug_selector(move || label.into())
            }),
        );
        let rows = state
            .history
            .iter()
            .filter(|row| self.mcp_live.filter.matches(row))
            .collect::<Vec<_>>();
        let list = div()
            .id("mcp-activity-list")
            .max_h(px(340.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(theme.border)
            .when(rows.is_empty(), |list| {
                list.child(
                    div()
                        .p_3()
                        .text_color(theme.secondary_text)
                        .child("Sin actividad para este filtro"),
                )
            })
            .children(
                rows.into_iter()
                    .map(|row| self.activity_row(row, theme, cx)),
            );
        let content = panel("mcp-activity", width, theme)
            .absolute()
            .right(px(12.))
            .top(px(68.))
            .child(heading)
            .children(self.jit_bar(state, theme, cx))
            .child(filters)
            .child(list);
        div()
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("mcp-activity-dismiss")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss_mcp_activity(cx))),
            )
            .child(content)
            .into_any_element()
    }

    fn jit_bar(
        &self,
        state: &crate::mcp_broker::State,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.selected.as_deref()?;
        let policy = state.config.servers.get(id)?;
        if policy.availability == ServerAvailability::Disabled {
            return None;
        }
        let server = self.servers.iter().find(|server| server.id == id)?;
        let now = SystemTime::now();
        let expiry = state.jit.profile_expiry(id, now);
        let profile = vela_mcp::presets::resolve_profile(&state.config, &policy.profile_id)
            .map(|profile| profile.name)
            .unwrap_or_else(|| policy.profile_id.clone());
        let (title, detail) = match expiry {
            Some(_) => (
                "Ventana JIT · Desarrollo",
                format!("{} volverá a «{profile}»", server.display_name()),
            ),
            None => (
                "Ventana JIT",
                format!(
                    "Eleva {} a «Desarrollo» temporalmente",
                    server.display_name()
                ),
            ),
        };
        let id = id.to_owned();
        Some(
            div()
                .mx_3()
                .px_2()
                .py_2()
                .rounded(px(6.))
                .border_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .gap_2()
                .when(expiry.is_some(), |bar| bar.bg(theme.warning_bg))
                .child(icon(IconName::Warning).text_color(theme.warning))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::MEDIUM).child(title))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(if expiry.is_some() {
                                    theme.warning
                                } else {
                                    theme.secondary_text
                                })
                                .truncate()
                                .child(detail),
                        ),
                )
                .when_some(expiry, |bar, expiry| {
                    let id = id.clone();
                    bar.child(
                        div()
                            .flex_none()
                            .font_family(crate::theme::monospace())
                            .text_size(px(12.))
                            .text_color(theme.warning)
                            .child(countdown(expiry.duration_since(now).unwrap_or_default())),
                    )
                    .child(
                        small_button(
                            "mcp-jit-revoke",
                            "Revocar ahora",
                            Variant::Secondary,
                            theme,
                            cx.listener(move |this, _, _, cx| {
                                this.mcp_live.broker.revoke(&id);
                                cx.notify();
                            }),
                        )
                        .debug_selector(|| "mcp-jit-revoke".into()),
                    )
                })
                .when(expiry.is_none(), |bar| {
                    bar.children([(15, "15 min"), (30, "30 min"), (60, "1 h")].map(
                        |(minutes, label)| {
                            let id = id.clone();
                            small_button(
                                label,
                                label,
                                Variant::Secondary,
                                theme,
                                cx.listener(move |this, _, _, cx| {
                                    this.mcp_live
                                        .broker
                                        .grant_profile(&id, Duration::from_secs(minutes * 60));
                                    cx.notify();
                                }),
                            )
                            .debug_selector(move || label.into())
                        },
                    ))
                })
                .into_any_element(),
        )
    }

    fn activity_row(&self, row: &Activity, theme: Theme, cx: &Context<Self>) -> AnyElement {
        let id = row.id;
        let (status, tone, bg) = match row.status {
            Status::Auto => ("Auto", theme.success, theme.hover),
            Status::Approved => ("Aprobado", theme.success, theme.hover),
            Status::Blocked => ("Bloqueado", theme.warning, theme.warning_bg),
            Status::Denied => ("Denegado", theme.danger, theme.hover),
            Status::Expired => ("Caducado", theme.secondary_text, theme.hover),
            Status::Cancelled => ("Cancelado", theme.secondary_text, theme.hover),
            Status::Error => ("Error", theme.danger, theme.hover),
            Status::Reverted => ("Revertido", theme.success, theme.hover),
        };
        let operation = if row.target.is_empty() {
            row.operation.clone()
        } else {
            format!("{} {}", row.operation, short(&row.target, 300))
        };
        let detail = [row.server.as_str(), row.detail.as_str()]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let restored = row.status == Status::Reverted;
        div()
            .id(("mcp-audit", id))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .min_h(px(42.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_none()
                    .font_family(crate::theme::monospace())
                    .text_size(px(12.))
                    .text_color(theme.secondary_text)
                    .child(local_time(row.time)),
            )
            .child(
                div()
                    .w(px(76.))
                    .flex_none()
                    .truncate()
                    .child(short(&row.agent, 80)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_family(crate::theme::monospace())
                            .text_size(px(12.))
                            .truncate()
                            .child(operation.clone()),
                    )
                    .when(!detail.is_empty(), |column| {
                        column.child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme.secondary_text)
                                .truncate()
                                .child(short(&detail, 240)),
                        )
                    }),
            )
            .child(pill(status.to_owned(), tone, bg))
            .when(row.backup_id.is_some(), |row_element| {
                if restored || row.restoring {
                    row_element.child(
                        div()
                            .flex_none()
                            .w(px(76.))
                            .text_center()
                            .text_size(px(12.))
                            .text_color(theme.secondary_text)
                            .child(if restored {
                                "Revertido"
                            } else {
                                "Revirtiendo…"
                            }),
                    )
                } else {
                    row_element.child(
                        small_button(
                            ("mcp-revert", id),
                            "Revertir",
                            Variant::Secondary,
                            theme,
                            cx.listener(move |this, _, window, cx| {
                                this.rollback_agent_change(id, window, cx)
                            }),
                        )
                        .w(px(76.)),
                    )
                }
            })
            .tooltip(move |_, cx| {
                cx.new(|_| Tooltip(format!("{operation} · {detail}").into()))
                    .into()
            })
            .into_any_element()
    }
}

fn panel(id: &'static str, width: gpui::Pixels, theme: Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .w(width)
        .min_w_0()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .occlude()
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised)
        .text_size(px(13.))
        .text_color(theme.text)
        .line_height(px(18.))
        .shadow(vec![gpui::BoxShadow {
            color: theme.overlay,
            inset: false,
            offset: point(px(0.), px(4.)),
            blur_radius: px(16.),
            spread_radius: px(0.),
        }])
}

fn pill(label: String, color: gpui::Rgba, background: gpui::Rgba) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(999.))
        .bg(background)
        .text_color(color)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn danger_button(
    id: &'static str,
    label: &'static str,
    theme: Theme,
    action: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    small_button(id, label, Variant::Secondary, theme, action)
        .debug_selector(move || id.into())
        .border_color(theme.border)
        .text_color(theme.danger)
}

fn code_box(id: u64, code: &str, diff: bool, theme: Theme) -> Stateful<Div> {
    let content = code
        .lines()
        .take(160)
        .map(|line| {
            let kind = if diff {
                classify_diff_line(line)
            } else {
                DiffLine::Context
            };
            let (color, bg) = match kind {
                DiffLine::Header => (theme.secondary_text, theme.panel),
                DiffLine::Added => (theme.success, theme.hover),
                DiffLine::Removed => (theme.danger, theme.hover),
                DiffLine::Context => (theme.text, theme.panel),
            };
            div()
                .px_2()
                .min_h(px(18.))
                .bg(bg)
                .text_color(color)
                .child(if diff {
                    short(line, 2048)
                } else {
                    line.to_owned()
                })
        })
        .collect::<Vec<_>>();
    div()
        .id(("mcp-approval-code", id))
        .max_h(px(144.))
        .overflow_y_scroll()
        .rounded(px(6.))
        .py_1()
        .border_1()
        .border_color(theme.border)
        .bg(theme.panel)
        .font_family(crate::theme::monospace())
        .text_size(px(12.))
        .children(content)
}

fn warning_label(warning: &str) -> String {
    let label = if warning.contains("ContainsSudo") {
        "Contiene sudo"
    } else if warning.contains("ShellChaining") {
        "Encadenamiento de shell"
    } else if warning.contains("Redirection") {
        "Redirección"
    } else if warning.contains("Subshell") {
        "Subshell"
    } else if warning.starts_with("FileExceedsRollbackLimit") {
        "Supera el límite de rollback"
    } else if warning.starts_with("OutOfScopePath") {
        "Ruta fuera del ámbito"
    } else {
        warning
    };
    short(label, 48)
}

fn countdown(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn local_time(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r writes a tm to a valid out pointer on success.
    unsafe {
        if libc::localtime_r(&seconds, time.as_mut_ptr()).is_null() {
            return "—".into();
        }
        let time = time.assume_init();
        format!("{:02}:{:02}:{:02}", time.tm_hour, time.tm_min, time.tm_sec)
    }
}

fn short(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let mut output: String = chars
        .by_ref()
        .take(max)
        .map(|character| {
            if character.is_control()
                || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                character
            }
        })
        .collect();
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn notify_approval(item: &Approval) {
    // Keep command text, paths, diffs and reasons in Vela.
    let body = format!(
        "{} solicita {} en {}",
        short(&item.agent, 50),
        short(&item.preview.operation, 40),
        short(&item.server, 50)
    );
    #[cfg(target_os = "linux")]
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let _ = notify_rust::Notification::new()
        .summary("Vela · Aprobación MCP")
        .body(&body)
        .show();
}

#[cfg(feature = "dev")]
impl Workspace {
    fn dev_mcp_live(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let mode = std::env::var("VELA_DEV_MCP_LIVE").unwrap_or_default();
        if !matches!(mode.as_str(), "patch" | "exec" | "conflict" | "activity")
            || crate::dev::uses_default_config()
        {
            return false;
        }
        if self.mcp_live.preview.is_some() {
            return true;
        }
        let Some(server) = self.servers.first() else {
            return false;
        };
        let name = server.display_name().to_owned();
        let id = server.id.clone();
        let live = &mut self.mcp_live;
        let mut config = McpConfig {
            enabled: true,
            ..Default::default()
        };
        config.servers.insert(
            id.clone(),
            ServerMcpPolicy {
                availability: ServerAvailability::ConnectedOnly,
                profile_id: "production_protected".into(),
                ..Default::default()
            },
        );
        live.broker.configure(config);
        {
            let mut state = live.broker.state();
            state.agents.insert(1, "Claude Code".into());
            state.agents.insert(2, "Codex".into());
        }
        let mut pending = Vec::new();
        let now = SystemTime::now();
        if matches!(mode.as_str(), "patch" | "exec") {
            let deadline = Instant::now() + Duration::from_secs(180);
            for (agent, preview) in [
                ("Claude Code", ApprovalPreview {
                    operation: "fs_patch".into(),
                    server_id: id.clone(),
                    paths: vec!["/etc/nginx/sites-available/api.conf".into()],
                    agent_reason: Some("Aumentar proxy_read_timeout a 60 s para evitar errores 504.".into()),
                    unified_diff: Some("@@ -1,3 +1,3 @@\n location /api/ {\n-    proxy_read_timeout 15s;\n+    proxy_read_timeout 60s;\n }\n".into()),
                    ..Default::default()
                }),
                ("Codex", ApprovalPreview {
                    operation: "exec_command".into(),
                    server_id: id.clone(),
                    paths: vec!["/etc/nginx".into()],
                    command: Some("sudo nginx -t && sudo systemctl reload nginx".into()),
                    agent_reason: Some("Validar la configuración y recargar Nginx.".into()),
                    warnings: vec!["ContainsSudo".into(), "ShellChaining".into()],
                    ..Default::default()
                }),
            ] {
                let (request, receive) = live.broker.preview_approval(Approval {
                    id: 0, agent: agent.into(), server: name.clone(), preview, deadline, rollback: true,
                });
                pending.push(receive);
                if mode == "exec" {
                    live.selected = 1;
                    let input = cx.new(|cx| TextInput::new("Valida primero la sintaxis", "Motivo opcional para el agente", "", false, cx).compact());
                    live.denial = Some((request, input, window.focused(cx)));
                }
            }
        }
        if mode == "activity" {
            live.broker.grant_profile(&id, Duration::from_secs(15 * 60));
            live.popover = true;
        }
        if matches!(mode.as_str(), "conflict" | "activity") {
            for (index, agent, operation, target, status, detail, backup) in [
                (
                    1,
                    "Codex",
                    "exec_command",
                    "nginx -t",
                    Status::Auto,
                    "Código de salida 0",
                    None,
                ),
                (
                    2,
                    "Claude Code",
                    "fs_write",
                    "/srv/api/config.toml",
                    Status::Blocked,
                    "Hay ediciones abiertas sin sincronizar.",
                    None,
                ),
                (
                    3,
                    "Claude Code",
                    "fs_patch",
                    "/etc/nginx/sites-available/api.conf",
                    Status::Approved,
                    "Copia de rollback de 4,2 KB",
                    Some("preview"),
                ),
            ] {
                live.broker.record(Activity {
                    id: index,
                    time: now - Duration::from_secs(70 - index * 15),
                    agent: agent.to_owned(),
                    server: name.clone(),
                    server_id: Some(id.clone()),
                    identity: None,
                    operation: operation.to_owned(),
                    target: target.to_owned(),
                    status,
                    detail: detail.to_owned(),
                    backup_id: backup.map(str::to_owned),
                    restoring: false,
                });
            }
            let mut state = live.broker.state();
            if mode == "activity" {
                state.conflict = None;
            } else if let Some((expiry, _)) = &mut state.conflict {
                *expiry = Instant::now() + Duration::from_secs(180);
            }
        }
        live.preview = Some(pending);
        true
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use vela_core::{Auth, config::Server};

    use super::*;

    fn server(dir: &std::path::Path) {
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.to_owned()));
        vela_core::config::save(
            dir,
            vec![Server {
                id: "srv".into(),
                name: "API".into(),
                host: "fixture.invalid".into(),
                port: 22,
                user: "vela".into(),
                auth: Auth::Password,
                initial_remote_path: None,
                pinned: false,
            }],
        )
        .unwrap();
    }

    fn fixture_config() -> McpConfig {
        McpConfig {
            enabled: true,
            servers: [(
                "srv".into(),
                ServerMcpPolicy {
                    availability: ServerAvailability::ConnectedOnly,
                    profile_id: "production_protected".into(),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        }
    }

    fn click(selector: &'static str, cx: &mut VisualTestContext) {
        let position = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("No element {selector}"))
            .center();
        cx.simulate_click(position, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn saves_from_settings_publish_the_new_config_to_the_running_broker(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        server(dir.path());
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, cx| {
            this.save_mcp(window, cx, false, |config| *config = fixture_config())
        });
        cx.run_until_parked();
        workspace.read_with(cx, |this, _| {
            assert!(this.mcp_live.worker.is_some());
            assert_eq!(this.mcp_live.broker.state().config, fixture_config());
        });
        workspace.update_in(cx, |this, window, cx| {
            this.save_mcp(window, cx, false, |config| {
                config.share_ui_context = true;
                config.approval_timeout_secs = 90;
                config.rollback.retention_hours = 12;
                config.servers.get_mut("srv").unwrap().availability =
                    ServerAvailability::OnDemandAsk;
            })
        });
        cx.run_until_parked();
        let config = vela_mcp::config::load(dir.path()).unwrap();
        assert_eq!(config.approval_timeout_secs, 90);
        assert!(config.share_ui_context);
        workspace.read_with(cx, |this, _| {
            assert_eq!(this.mcp_live.broker.state().config, config)
        });
    }

    struct WithEditor {
        workspace: Entity<Workspace>,
        editor: Entity<TextInput>,
    }

    impl gpui::Render for WithEditor {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.editor.clone())
                .child(div().flex_1().min_h_0().child(self.workspace.clone()))
        }
    }

    fn preview(broker: &Broker, target: &str) -> oneshot::Receiver<Answer> {
        let (_, reply) = broker.preview_approval(Approval {
            id: 0,
            agent: "Test agent".into(),
            server: "API".into(),
            deadline: Instant::now() + Duration::from_secs(180),
            rollback: true,
            preview: ApprovalPreview {
                server_id: "srv".into(),
                operation: "fs_write".into(),
                paths: vec![target.to_owned()],
                ..Default::default()
            },
        });
        reply
    }

    #[gpui::test]
    fn approvals_render_without_taking_keyboard_focus_and_queue_buttons_send_decisions(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        server(dir.path());
        cx.update(crate::components::text_input::bind_keys);
        let (root, cx) = cx.add_window_view(|window, cx| WithEditor {
            workspace: cx.new(|cx| Workspace::new(None, None, None, window, cx)),
            editor: cx.new(|cx| TextInput::new("local name", "Name", "", false, cx)),
        });
        let (workspace, editor) =
            root.read_with(cx, |root, _| (root.workspace.clone(), root.editor.clone()));
        let focus = editor.read_with(cx, |editor, cx| editor.focus_handle(cx));
        cx.update(|window, cx| focus.focus(window, cx));
        let broker = workspace.read_with(cx, |this, _| this.mcp_live.broker.clone());
        broker.configure(fixture_config());
        let mut first = preview(&broker, "/tmp/first");
        let mut second = preview(&broker, "/tmp/second");
        workspace.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(cx.debug_bounds("mcp-approval").is_some());
        cx.update(|window, _| assert!(focus.is_focused(window)));
        click("Siguiente", cx);
        workspace.read_with(cx, |this, _| assert_eq!(this.mcp_live.selected, 1));
        click("mcp-session", cx);
        assert!(matches!(second.try_recv(), Ok(Answer::Session)));
        click("mcp-once", cx);
        assert!(matches!(first.try_recv(), Ok(Answer::Once)));

        let mut denied = preview(&broker, "/tmp/deny");
        let mut saved = preview(&broker, "/tmp/save");
        workspace.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        click("mcp-save", cx);
        assert!(matches!(saved.try_recv(), Ok(Answer::Save)));
        click("mcp-deny", cx);
        workspace.update_in(cx, |this, window, cx| {
            this.mcp_live
                .denial
                .as_ref()
                .unwrap()
                .1
                .update(cx, |input, cx| {
                    input.set_value("Espera a mantenimiento", window, cx);
                });
        });
        click("mcp-deny-send", cx);
        assert!(
            matches!(denied.try_recv(), Ok(Answer::Deny(reason)) if reason == "Espera a mantenimiento")
        );
        assert!(broker.state().pending().is_empty());
    }

    #[gpui::test]
    fn activity_controls_filter_rows_and_revoke_jit_when_cutting_agents(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        server(dir.path());
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        let (broker, mut pending) = workspace.update(cx, |this, cx| {
            let broker = this.mcp_live.broker.clone();
            broker.configure(fixture_config());
            let pending = preview(&broker, "/tmp/kill");
            broker.record(Activity {
                id: 23,
                time: SystemTime::now(),
                agent: "Test agent".into(),
                server: "API".into(),
                server_id: Some("srv".into()),
                identity: None,
                operation: "fs_patch".into(),
                target: "/tmp/file".into(),
                status: Status::Blocked,
                detail: "Hay ediciones abiertas sin sincronizar.".into(),
                backup_id: None,
                restoring: false,
            });
            this.mcp_live.popover = true;
            cx.notify();
            (broker, pending)
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("mcp-activity").is_some());
        click("Bloqueados", cx);
        workspace.read_with(cx, |this, _| {
            assert!(this.mcp_live.filter == Filter::Blocked)
        });
        let row = broker.state().history.front().unwrap().clone();
        assert!(Filter::Blocked.matches(&row));
        assert!(!Filter::Approved.matches(&row));
        assert!(!Filter::Rollback.matches(&row));
        assert!(broker.state().conflict.is_some());

        click("15 min", cx);
        assert_eq!(
            broker
                .state()
                .jit
                .active_profile_override("srv", SystemTime::now()),
            Some("development")
        );
        click("mcp-jit-revoke", cx);
        assert_eq!(broker.state().jit.active_count(SystemTime::now()), 0);
        click("15 min", cx);
        click("mcp-kill", cx);
        assert!(matches!(
            pending.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert_eq!(broker.state().jit.active_count(SystemTime::now()), 0);
        assert!(
            broker
                .state()
                .history
                .iter()
                .any(|row| row.operation == "Cortar agentes")
        );
    }
}
