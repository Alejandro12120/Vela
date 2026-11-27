use gpui::ClipboardItem;
use vela_mcp::harness::{self, HarnessError};

use super::*;
use crate::mcp_model::{DiffLine, classify_diff_line, harness_error, harness_result, harness_row};

impl Workspace {
    pub(super) fn open_mcp_harnesses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() || self.mcp.is_open() {
            return;
        }
        self.mcp.dialog = Some(View::Harnesses);
        self.mcp.results.clear();
        self.start_mcp(window, cx);
        self.scan_mcp_harnesses(window, cx);
    }

    pub(in crate::workspace) fn scan_mcp_harnesses(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mcp.scanning || self.mcp.applying {
            return;
        }
        self.mcp.scanning = true;
        self.mcp.scan_error = None;
        self.mcp.pending = PendingChanges::default();
        self.mcp.diff = None;
        #[cfg(test)]
        let dir = self.dir.clone().unwrap_or_default();
        let task = cx.background_executor().spawn(async move {
            #[cfg(test)]
            let env = harness::HarnessEnv::isolated(
                dir.join("mcp-test-harnesses"),
                harness::TargetOs::Linux,
            );
            #[cfg(not(test))]
            let env = harness::HarnessEnv::from_process()?;
            HarnessScan::load(env)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.mcp.scanning = false;
                this.set_harness_scan(result);
                if matches!(this.mcp.dialog, Some(View::Harnesses))
                    && let Some(preview) = crate::dev::mcp_diff()
                    && let Some((id, action)) = this.mcp.scan.as_ref().and_then(|scan| {
                        scan.statuses
                            .iter()
                            .find(|(id, _)| id.id() == preview)
                            .and_then(|(id, status)| {
                                harness_row(status)
                                    .action
                                    .filter(|action| *action != Change::Replace)
                                    .map(|action| (*id, action))
                            })
                    })
                {
                    this.toggle_harness_change(id, action, cx);
                    this.preview_harness_change(id, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_harness_scan(&mut self, result: io::Result<HarnessScan>) {
        match result {
            Ok(scan) => self.mcp.scan = Some(scan),
            Err(error) => {
                self.mcp.scan = None;
                self.mcp.scan_error = Some(format!(
                    "No se pudo detectar la configuración de los agentes. {error}"
                ));
            }
        }
    }

    pub(super) fn toggle_harness_change(
        &mut self,
        id: HarnessId,
        action: Change,
        cx: &mut Context<Self>,
    ) {
        self.mcp.pending.toggle(id, action);
        if self.mcp.diff.as_ref().is_some_and(|(open, _)| *open == id) {
            self.mcp.diff = None;
        }
        cx.notify();
    }

    fn queue_harness_change(
        &mut self,
        id: HarnessId,
        action: Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mcp.applying || self.mcp.scanning {
            return;
        }
        if action == Change::Replace && self.mcp.pending.get(id).is_none() {
            self.mcp.confirmation = Some(Confirmation::Replace(id));
            self.start_mcp(window, cx);
        } else {
            self.toggle_harness_change(id, action, cx);
        }
    }

    fn preview_harness_change(
        &mut self,
        id: HarnessId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mcp.diff.as_ref().is_some_and(|(open, _)| *open == id) {
            self.mcp.diff = None;
            cx.notify();
            return;
        }
        let (Some(action), Some(scan)) = (self.mcp.pending.get(id), &self.mcp.scan) else {
            return;
        };
        let (env, cmd) = (scan.env.clone(), scan.command.clone());
        self.mcp.diff = Some((id, None));
        self.mcp.diff_scroll = gpui::ScrollHandle::new();
        let task = cx.background_executor().spawn(async move {
            action.run(id, &env, &cmd, true).map_or_else(
                |error| harness_error(&error),
                |report| {
                    if report.diff.is_empty() {
                        "El archivo ya refleja este cambio.".into()
                    } else {
                        report.diff
                    }
                },
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let diff = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some((open, text)) = &mut this.mcp.diff
                    && *open == id
                    && this.mcp.pending.get(id) == Some(action)
                {
                    *text = Some(diff);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn apply_mcp_harnesses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .mcp
            .pending
            .can_apply(self.mcp.scanning || self.mcp.applying)
        {
            return;
        }
        let Some(scan) = self.mcp.scan.clone() else {
            return;
        };
        let pending = self.mcp.pending.ordered();
        self.mcp.applying = true;
        self.mcp.results.clear();
        let task = cx.background_executor().spawn(async move {
            let results = pending
                .into_iter()
                .map(|(id, action)| {
                    let observed = harness::detect(id, &scan.env, &scan.command);
                    let result = match scan.statuses.iter().find(|(candidate, _)| *candidate == id)
                    {
                        Some((_, before)) if *before == observed => {
                            action.run(id, &scan.env, &scan.command, false)
                        }
                        _ => Err(HarnessError::ConcurrentModification {
                            config_path: harness_row(&observed)
                                .path
                                .unwrap_or(scan.command.program())
                                .to_owned(),
                        }),
                    };
                    (id, harness_result(result, &scan))
                })
                .collect();
            let statuses = harness::detect_all(&scan.env, &scan.command);
            (results, HarnessScan { statuses, ..scan })
        });
        cx.spawn_in(window, async move |this, cx| {
            let (results, scan) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.mcp.applying = false;
                this.mcp.pending = PendingChanges::default();
                this.mcp.diff = None;
                this.mcp.results = results;
                this.mcp.scan = Some(scan);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn copy_mcp_snippet(
        &mut self,
        key: u64,
        snippet: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(snippet));
        self.mcp.copied = Some(key);
        self.mcp.copy_generation += 1;
        let generation = self.mcp.copy_generation;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = this.update(cx, |this, cx| {
                if this.mcp.copy_generation == generation {
                    this.mcp.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn harnesses_body(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let busy = self.mcp.scanning || self.mcp.applying;
        let header = div().flex().items_start().gap_3().justify_between()
            .child(muted("Detecta los harnesses instalados y añade vela mcp sin tocar el resto de tus servidores MCP.", theme))
            .child(if busy {
                disabled_button(if self.mcp.applying { "Aplicando…" } else { "Detectando…" }, theme).into_any_element()
            } else {
                small("detect-mcp", "Detectar", theme, cx.listener(|this, _, window, cx| this.scan_mcp_harnesses(window, cx))).into_any_element()
            });
        let mut body = stack().child(header);
        if let Some(error) = &self.mcp.scan_error {
            body = body.child(div().text_color(theme.danger).child(error.clone()));
        }
        let Some(scan) = &self.mcp.scan else {
            return body;
        };
        body = body.when(scan.detected_count() == 0, |body| {
            body.child(
                muted(
                    "No se detectó ningún agente. Usa la configuración manual de abajo.",
                    theme,
                )
                .px_3()
                .py_2()
                .rounded(px(6.))
                .bg(theme.warning_bg)
                .text_color(theme.warning),
            )
        });
        body = body.child(super::super::settings_view::settings_group(
            theme,
            scan.statuses
                .iter()
                .enumerate()
                .map(|(index, (id, status))| {
                    let id = *id;
                    let row = harness_row(status);
                    let pending = self.mcp.pending.get(id);
                    let (label, tone) = pending.map_or((row.label, row.tone), Change::pending);
                    let info = stack()
                        .gap_0()
                        .flex_1()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1()
                                .child(heading(id.display_name()))
                                .child(badge(label, tone, theme)),
                        )
                        .when(pending.is_some(), |info| {
                            info.child(muted(row.label, theme))
                        })
                        .children(row.path.map(|path| {
                            ellipsis(
                                ("harness-path", index),
                                format!("{} · stdio", scan.display_path(path)),
                                theme,
                            )
                            .font_family(crate::theme::monospace())
                        }))
                        .children(
                            row.detail
                                .map(|detail| ellipsis(("harness-detail", index), detail, theme)),
                        )
                        .children(self.mcp.results.get(&id).map(|(result, tone)| {
                            ellipsis(("harness-result", index), result.clone(), theme)
                                .text_color(tone_color(*tone, theme))
                        }));
                    let actions = div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .children(row.action.map(|action| {
                            let label = if pending.is_some() {
                                "Desmarcar"
                            } else {
                                action.label()
                            };
                            if busy {
                                return disabled_button(label, theme).into_any_element();
                            }
                            small(
                                ("harness-change", index),
                                label,
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    this.queue_harness_change(id, action, window, cx)
                                }),
                            )
                            .into_any_element()
                        }))
                        .when(pending.is_some(), |actions| {
                            let open = self.mcp.diff.as_ref().is_some_and(|(open, _)| *open == id);
                            let label = if open {
                                "Ocultar cambios"
                            } else {
                                "Ver cambios"
                            };
                            if busy {
                                return actions.child(disabled_button(label, theme));
                            }
                            actions.child(small(
                                ("harness-show-diff", index),
                                label,
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    this.preview_harness_change(id, window, cx)
                                }),
                            ))
                        });
                    let mut element = stack().px_3().py_2().child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(info)
                            .child(actions),
                    );
                    if let Some(snippet) = row.snippet {
                        let expanded = self.mcp.manual_preview == Some(id);
                        let label = if expanded {
                            "Ocultar configuración"
                        } else {
                            "Ver configuración"
                        };
                        element = element.child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_1()
                                .child(if busy {
                                    disabled_button(label, theme)
                                } else {
                                    small(
                                        ("show-snippet", index),
                                        label,
                                        theme,
                                        cx.listener(move |this, _, _, cx| {
                                            this.mcp.manual_preview =
                                                if expanded { None } else { Some(id) };
                                            cx.notify();
                                        }),
                                    )
                                })
                                .child(self.copy_mcp_button(
                                    index as u64 + 1,
                                    "Copiar configuración",
                                    snippet.to_owned(),
                                    theme,
                                    cx,
                                )),
                        );
                        if expanded {
                            element = element.child(code_box(
                                ("harness-snippet", index),
                                snippet.to_owned(),
                                None,
                                theme,
                            ));
                        }
                    }
                    if let Some((open, text)) = &self.mcp.diff
                        && *open == id
                        && let (Some(change), Some(path)) = (pending, row.path)
                    {
                        element = element.child(
                            stack()
                                .gap_1()
                                .child(muted(change.summary(&scan.display_path(path)), theme))
                                .child(code_box(
                                    ("harness-diff", index),
                                    text.as_deref().unwrap_or("Calculando cambios…").to_owned(),
                                    Some(&self.mcp.diff_scroll),
                                    theme,
                                )),
                        );
                    }
                    element
                })
                .collect(),
        ));
        let snippet = harness::snippet(HarnessId::Cursor, &scan.command);
        body.child(ellipsis("harness-program", format!("Ejecutable: {}", scan.command.program().display()), theme).font_family(crate::theme::monospace()))
            .when(scan.development_program, |body| body.child(muted("Esta ruta es de una compilación de desarrollo; los agentes dejarán de encontrarla si la borras.", theme)
                .text_color(theme.warning)))
            .child(stack().gap_1().child(div().flex().items_center().justify_between().gap_2()
                .child(muted("Configuración manual para otros clientes", theme))
                .child(self.copy_mcp_button(0, "Copiar JSON", snippet.clone(), theme, cx)))
                .child(code_box("harness-manual-json", snippet, None, theme)))
    }

    fn copy_mcp_button(
        &self,
        key: u64,
        label: &'static str,
        snippet: String,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        if self.mcp.applying || self.mcp.scanning {
            return disabled_button(label, theme);
        }
        small(
            ("copy-mcp", key as usize),
            if self.mcp.copied == Some(key) {
                "Copiado"
            } else {
                label
            },
            theme,
            cx.listener(move |this, _, window, cx| {
                this.copy_mcp_snippet(key, snippet.clone(), window, cx)
            }),
        )
    }
}

fn code_box(
    id: impl Into<ElementId>,
    text: String,
    scroll: Option<&gpui::ScrollHandle>,
    theme: Theme,
) -> Div {
    let diff = scroll.is_some();
    div()
        .relative()
        .flex()
        .flex_col()
        .max_h(px(if diff { 152. } else { 190. }))
        .rounded(px(6.))
        .bg(theme.panel)
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .id(id)
                .min_h_0()
                .overflow_scroll()
                .p_2()
                .when_some(scroll, |code, scroll| code.track_scroll(scroll).pr_4())
                .font_family(crate::theme::monospace())
                .text_size(px(11.))
                .line_height(px(15.))
                .children(text.lines().map(|line| {
                    let kind = if diff {
                        classify_diff_line(line)
                    } else {
                        DiffLine::Context
                    };
                    let color = match kind {
                        DiffLine::Header => theme.secondary_text,
                        DiffLine::Added => theme.success,
                        DiffLine::Removed => theme.danger,
                        DiffLine::Context => theme.text,
                    };
                    div()
                        .whitespace_nowrap()
                        .text_color(color)
                        .when(matches!(kind, DiffLine::Added | DiffLine::Removed), |row| {
                            row.font_weight(FontWeight::MEDIUM)
                        })
                        .child(line.to_owned())
                })),
        )
        .when_some(scroll, |code, scroll| {
            code.child(modal::scroll_indicator(scroll, theme, false))
        })
}
