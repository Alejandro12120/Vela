use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Instant, SystemTime},
};

use gpui::{
    Context, Entity, FocusHandle, FontWeight, Render, Role, WeakEntity, Window, div, prelude::*, px,
};
use vscp_core::{
    Session,
    transfer::{
        self, Answer, Choice, Conflict, Direction, Event, ItemResult, JobId, Status, Totals,
    },
};

use crate::{
    components::{
        button::{Variant, button},
        modal,
    },
    dev, file_metadata,
    pane::{Pane, Source},
    pane_icons::Icon,
    theme::Theme,
};

struct Row {
    id: JobId,
    direction: Direction,
    status: Status,
    totals: Totals,
    file: Option<(usize, PathBuf)>,
    bytes: u64,
    started: Option<Instant>,
    speed: f64,
    errors: usize,
    details: Vec<String>,
    cancel: transfer::Cancel,
}

impl Row {
    fn update(&mut self, event: Event) {
        match event {
            Event::Status(status) => {
                self.status = status;
                if status == Status::Running {
                    self.started = Some(Instant::now());
                }
            }
            Event::Planned(totals) => self.totals = totals,
            Event::File { index, path } => self.file = Some((index, path)),
            Event::Progress { bytes } => {
                self.bytes = bytes;
                if let Some(started) = self.started {
                    self.speed = bytes as f64 / started.elapsed().as_secs_f64().max(0.001) / 1e6;
                }
            }
            Event::Item {
                source,
                result:
                    ItemResult::Failed(reason)
                    | ItemResult::Incomplete(reason)
                    | ItemResult::Skipped(reason),
                ..
            } => {
                self.details
                    .push(format!("{} · {reason}", source.display()));
            }
            Event::Finished(summary) => {
                self.status = summary.status;
                self.errors = summary.errors;
            }
            _ => {}
        }
    }

    fn progress(&self) -> f32 {
        ratio(
            self.bytes,
            self.totals,
            self.file
                .as_ref()
                .map_or(0, |(index, _)| index.saturating_sub(1)),
        )
    }

    fn active(&self) -> bool {
        matches!(self.status, Status::Preparing | Status::Running)
    }
}

fn ratio(bytes: u64, totals: Totals, files: usize) -> f32 {
    if totals.bytes > 0 {
        (bytes as f32 / totals.bytes as f32).clamp(0., 1.)
    } else if totals.files > 0 {
        (files as f32 / totals.files as f32).clamp(0., 1.)
    } else {
        0.
    }
}

fn status_label(status: Status, errors: usize) -> String {
    match status {
        Status::Queued => "En cola".into(),
        Status::Preparing => "Preparando…".into(),
        Status::Running => "En curso".into(),
        Status::Completed => "Completada".into(),
        Status::CompletedWithErrors => format!("Completada con errores ({errors})"),
        Status::Cancelled => "Cancelada".into(),
        Status::Error => "Error".into(),
    }
}

struct Deletion {
    source: Source,
    paths: Vec<PathBuf>,
    pane: WeakEntity<Pane>,
}

pub struct Transfers {
    session: Session,
    server: String,
    rows: Vec<Row>,
    expanded: bool,
    conflicts: Vec<(JobId, Conflict)>,
    deletion: Option<Deletion>,
    apply_all: bool,
    focus: FocusHandle,
    last: FocusHandle,
    previous: Option<FocusHandle>,
    error: Option<String>,
}

impl Transfers {
    pub fn new(session: Session, server: String, cx: &mut Context<Self>) -> Self {
        Self {
            session,
            server,
            rows: Vec::new(),
            expanded: dev::transfer_expanded(),
            conflicts: Vec::new(),
            deletion: None,
            apply_all: false,
            focus: cx.focus_handle(),
            last: cx.focus_handle().tab_stop(true),
            previous: None,
            error: None,
        }
    }

    pub fn enqueue(
        &mut self,
        spec: transfer::Spec,
        target: WeakEntity<Pane>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let direction = spec.direction;
            let mut changed = BTreeSet::from([spec.destination.clone()]);
            let job = match session.enqueue_transfer(spec).await {
                Ok(job) => job,
                Err(error) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        this.error = Some(error.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            let id = job.id;
            if this
                .update_in(cx, |this, _, cx| {
                    this.rows.push(Row {
                        id,
                        direction,
                        status: Status::Queued,
                        totals: Totals::default(),
                        file: None,
                        bytes: 0,
                        started: None,
                        speed: 0.,
                        errors: 0,
                        details: Vec::new(),
                        cancel: job.cancel.clone(),
                    });
                    cx.notify();
                })
                .is_err()
            {
                job.cancel.cancel();
                return;
            }

            while let Ok(event) = job.events.recv().await {
                if dev::logging() {
                    dev::log(format_args!("trabajo {id}: {event:?}"));
                }
                if let Event::Item {
                    destination: Some(path),
                    ..
                } = &event
                    && let Some(parent) = path.parent()
                {
                    changed.insert(parent.to_owned());
                }
                let finished = matches!(event, Event::Finished(_));
                if this
                    .update_in(cx, |this, window, cx| {
                        if let Event::Conflict(conflict) = event {
                            this.conflicts.push((id, conflict));
                        } else if let Some(row) = this.rows.iter_mut().find(|row| row.id == id) {
                            row.update(event);
                        }
                        if finished {
                            this.conflicts.retain(|(job, _)| *job != id);
                            this.restore_focus(window, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    job.cancel.cancel();
                    return;
                }
                if finished {
                    break;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(row) = this.rows.iter_mut().find(|row| row.id == id)
                    && matches!(
                        row.status,
                        Status::Queued | Status::Preparing | Status::Running
                    )
                {
                    row.status = Status::Error;
                    row.details
                        .push("La sesión terminó durante la transferencia.".into());
                    this.conflicts.retain(|(job, _)| *job != id);
                    this.restore_focus(window, cx);
                    cx.notify();
                }
                let _ = target.update(cx, |pane, cx| {
                    pane.refresh_changed(changed, None, window, cx)
                });
            });
        })
        .detach();
    }

    pub fn confirm_delete(
        &mut self,
        source: Source,
        paths: Vec<PathBuf>,
        pane: WeakEntity<Pane>,
        cx: &mut Context<Self>,
    ) {
        if !paths.is_empty() && self.deletion.is_none() {
            self.deletion = Some(Deletion {
                source,
                paths,
                pane,
            });
            cx.notify();
        }
    }

    fn restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deletion.is_none() && self.conflicts.is_empty() {
            let previous = self.previous.take();
            if self.focus.contains_focused(window, cx)
                && let Some(previous) = previous
            {
                previous.focus(window, cx);
            }
        }
    }

    fn answer(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if !self.conflicts.is_empty() {
            let (_, conflict) = self.conflicts.remove(0);
            conflict.answer(Answer {
                choice,
                apply_to_all: std::mem::take(&mut self.apply_all),
            });
        }
        self.restore_focus(window, cx);
        cx.notify();
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut deletion) = self.deletion.take() else {
            return;
        };
        self.restore_focus(window, cx);
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut errors = Vec::new();
            let mut parents = BTreeSet::new();
            deletion.paths.sort();
            let mut removed: Vec<PathBuf> = Vec::new();
            for path in deletion.paths {
                if let Some(parent) = path.parent() {
                    parents.insert(parent.to_owned());
                }
                if removed.iter().any(|parent| path.starts_with(parent)) {
                    continue;
                }
                match deletion.source.delete(path.clone(), &executor).await {
                    Ok(()) => removed.push(path),
                    Err(error) => errors.push(format!("{} · {error}", path.display())),
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.error = (!errors.is_empty()).then(|| errors.join("\n"));
                let _ = deletion.pane.update(cx, |pane, cx| {
                    pane.refresh_changed(parents, None, window, cx)
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.conflicts.is_empty() {
            self.answer(Choice::Cancel, window, cx);
        } else {
            self.deletion = None;
            self.restore_focus(window, cx);
            cx.notify();
        }
    }

    fn dialog_button(
        id: &'static str,
        label: &'static str,
        theme: Theme,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + Clone + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let click = action.clone();
        button(
            id,
            label,
            Variant::Secondary,
            theme,
            cx.listener(move |this, _, window, cx| click(this, window, cx)),
        )
        .on_action(cx.listener(move |this, _: &modal::Confirm, window, cx| {
            cx.stop_propagation();
            action(this, window, cx);
        }))
    }

    fn toggle_policy(&mut self, cx: &mut Context<Self>) {
        self.apply_all = !self.apply_all;
        cx.notify();
    }

    fn dialog(&self, theme: Theme, cx: &Context<Self>) -> Option<gpui::Div> {
        let (title, body, footer) = if let Some((_, conflict)) = self.conflicts.first() {
            let now = SystemTime::now();
            let metadata = |label, entry: &vscp_core::Entry| {
                div()
                    .flex()
                    .justify_between()
                    .gap_2()
                    .child(div().font_weight(FontWeight::MEDIUM).child(label))
                    .child(
                        div()
                            .min_w_0()
                            .text_color(theme.secondary_text)
                            .child(format!(
                                "{} · {}",
                                file_metadata::size(entry),
                                file_metadata::modified(entry.mtime, now),
                            )),
                    )
            };
            let body = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(metadata("Origen", &conflict.source))
                .child(metadata("Destino", &conflict.existing))
                .child(
                    div()
                        .id("apply-all")
                        .role(Role::CheckBox)
                        .aria_toggled(if self.apply_all {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tab_index(0)
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2()
                        .hover(move |s| s.bg(theme.hover))
                        .focus(move |s| s.bg(theme.selection))
                        .child(
                            div()
                                .w(px(16.))
                                .h(px(16.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(theme.control_border)
                                .child(if self.apply_all { "✓" } else { "" }),
                        )
                        .child("Aplicar a todos los conflictos de este trabajo")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_policy(cx)))
                        .on_action(cx.listener(|this, _: &modal::Confirm, _, cx| {
                            cx.stop_propagation();
                            this.toggle_policy(cx);
                        })),
                );
            let footer = div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div().flex().flex_wrap().justify_end().gap_2().children(
                        [
                            ("Sobrescribir", Choice::Overwrite),
                            ("Omitir", Choice::Skip),
                            ("Renombrar", Choice::Rename),
                        ]
                        .map(|(label, choice)| {
                            Self::dialog_button(label, label, theme, cx, move |this, window, cx| {
                                this.answer(choice, window, cx)
                            })
                        }),
                    ),
                )
                .child(
                    div().flex().justify_end().child(
                        Self::dialog_button(
                            "cancel-transfer",
                            "Cancelar trabajo",
                            theme,
                            cx,
                            |this, window, cx| this.answer(Choice::Cancel, window, cx),
                        )
                        .track_focus(&self.last),
                    ),
                );
            (
                format!(
                    "Ya existe «{}» en el destino",
                    conflict.existing.name.to_string_lossy()
                ),
                body,
                footer,
            )
        } else if let Some(deletion) = &self.deletion {
            let count = deletion.paths.len();
            let items = if count == 1 {
                "1 elemento".into()
            } else {
                format!("{count} elementos")
            };
            let remote = matches!(deletion.source, Source::Remote(_));
            let title = if remote {
                format!("¿Eliminar {items} de {}?", self.server)
            } else {
                format!("¿Mover {items} a la Papelera?")
            };
            let body = div().when(remote, |body| {
                body.text_color(theme.danger)
                    .child("Esta acción no se puede deshacer.")
            });
            let footer = div()
                .flex()
                .justify_end()
                .gap_2()
                .child(Self::dialog_button(
                    "cancel-delete",
                    "Cancelar",
                    theme,
                    cx,
                    Self::escape,
                ))
                .child(
                    Self::dialog_button(
                        "confirm-delete",
                        if remote {
                            "Eliminar"
                        } else {
                            "Mover a la Papelera"
                        },
                        theme,
                        cx,
                        Self::delete,
                    )
                    .text_color(theme.danger)
                    .track_focus(&self.last),
                );
            (title, body, footer)
        } else {
            return None;
        };

        Some(
            modal::overlay(
                title,
                body.into_any_element(),
                footer.into_any_element(),
                &self.focus,
                &self.last,
                theme,
                cx,
            )
            .on_action(cx.listener(|this, _: &modal::Cancel, window, cx| {
                cx.stop_propagation();
                this.escape(window, cx);
            }))
            .on_action(cx.listener(|_, _: &modal::Confirm, _, cx| cx.stop_propagation())),
        )
    }

    fn strip(&self, theme: Theme, cx: &Context<Self>) -> gpui::Div {
        let active = self.rows.iter().find(|row| row.active());
        let progress = active.map_or(0., Row::progress);
        let caption = active.map_or_else(
            || {
                self.rows.last().map_or_else(
                    || "Transferencias".into(),
                    |row| status_label(row.status, row.errors),
                )
            },
            |row| match &row.file {
                Some((index, file)) => {
                    let verb = match row.direction {
                        Direction::Upload => "Subiendo",
                        Direction::Download => "Descargando",
                    };
                    let name = file
                        .file_name()
                        .unwrap_or(file.as_os_str())
                        .to_string_lossy();
                    let speed = format!("{:.1}", row.speed).replace('.', ",");
                    format!(
                        "{verb} {index} de {} · {name} · {speed} MB/s · {:.0} %",
                        row.totals.files,
                        progress * 100.
                    )
                }
                None => status_label(row.status, row.errors),
            },
        );
        div()
            .flex()
            .flex_col()
            .flex_none()
            .when(!self.rows.is_empty() || self.error.is_some(), |strip| {
                strip
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
            })
            .when(!self.rows.is_empty(), |strip| {
                strip.child(
                    div()
                        .h(px(36.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .child(
                            div()
                                .id("transfer-disclosure")
                                .role(Role::Button)
                                .aria_label("Cola de transferencias")
                                .aria_expanded(self.expanded)
                                .tab_index(0)
                                .cursor_pointer()
                                .size(px(26.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .hover(move |s| s.bg(theme.hover))
                                .child(
                                    if self.expanded {
                                        Icon::Down
                                    } else {
                                        Icon::Forward
                                    }
                                    .draw(theme.secondary_text),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.expanded = !this.expanded;
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(caption))
                        .when_some(active, |header, row| {
                            let cancel = row.cancel.clone();
                            header.child(
                                button(
                                    "stop-job",
                                    "Cancelar",
                                    Variant::Secondary,
                                    theme,
                                    move |_, _, _| cancel.cancel(),
                                )
                                .h(px(25.)),
                            )
                        }),
                )
            })
            .when(active.is_some(), |strip| {
                strip.child(
                    div()
                        .mx_2()
                        .mb_1()
                        .h(px(3.))
                        .rounded(px(3.))
                        .bg(theme.selection)
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(progress))
                                .rounded(px(3.))
                                .bg(theme.accent),
                        ),
                )
            })
            .when(self.expanded && !self.rows.is_empty(), |strip| {
                strip
                    .child(
                        div()
                            .id("transfer-history")
                            .max_h(px(170.))
                            .overflow_y_scroll()
                            .px_2()
                            .children(self.rows.iter().map(|row| {
                                let direction = match row.direction {
                                    Direction::Upload => "Subida",
                                    Direction::Download => "Descarga",
                                };
                                let files = if row.totals.files == 1 {
                                    "1 archivo".into()
                                } else {
                                    format!("{} archivos", row.totals.files)
                                };
                                let bytes = file_metadata::size(&vscp_core::Entry {
                                    name: Default::default(),
                                    kind: vscp_core::EntryKind::File,
                                    symlink_target_kind: None,
                                    size: Some(row.totals.bytes),
                                    mtime: None,
                                    mode: None,
                                });
                                div()
                                    .border_t_1()
                                    .border_color(theme.border)
                                    .py_1()
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div().min_w_0().truncate().child(format!(
                                                    "{direction} · {files} · {bytes}"
                                                )),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_color(theme.secondary_text)
                                                    .child(status_label(row.status, row.errors)),
                                            ),
                                    )
                                    .children(row.details.iter().map(|detail| {
                                        div()
                                            .text_size(px(12.))
                                            .text_color(theme.secondary_text)
                                            .child(detail.clone())
                                    }))
                            })),
                    )
                    .child(
                        div().flex().justify_end().p_2().child(
                            button(
                                "clear-jobs",
                                "Limpiar",
                                Variant::Secondary,
                                theme,
                                cx.listener(|this, _, _, cx| {
                                    this.rows
                                        .retain(|row| row.active() || row.status == Status::Queued);
                                    cx.notify();
                                }),
                            )
                            .h(px(25.)),
                        ),
                    )
            })
            .when_some(self.error.clone(), |strip, error| {
                strip.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .text_color(theme.danger)
                        .child(error)
                        .child(button(
                            "dismiss-error",
                            "Cerrar",
                            Variant::Secondary,
                            theme,
                            cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            }),
                        )),
                )
            })
    }
}

impl Render for Transfers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.strip(Theme::for_window(window), cx)
    }
}

pub struct TransferOverlay {
    transfers: Entity<Transfers>,
}

impl TransferOverlay {
    pub fn new(transfers: Entity<Transfers>, cx: &mut Context<Self>) -> Self {
        cx.observe(&transfers, |_, _, cx| cx.notify()).detach();
        Self { transfers }
    }
}

impl Render for TransferOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        self.transfers.update(cx, |queue, cx| {
            let Some(dialog) = queue.dialog(theme, cx) else {
                return div().into_any_element();
            };
            if !queue.focus.contains_focused(window, cx) {
                let weak = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    let _ = weak.update(cx, |queue, cx| {
                        if queue.deletion.is_some() || !queue.conflicts.is_empty() {
                            queue.previous = window.focused(cx);
                            queue.focus.focus(window, cx);
                        }
                    });
                });
            }
            gpui::deferred(dialog).with_priority(2).into_any_element()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_status_and_progress() {
        assert_eq!(status_label(Status::Queued, 0), "En cola");
        assert_eq!(
            status_label(Status::CompletedWithErrors, 2),
            "Completada con errores (2)"
        );
        assert_eq!(
            ratio(
                40,
                Totals {
                    files: 1,
                    bytes: 100
                },
                0
            ),
            0.4
        );
        assert_eq!(ratio(0, Totals { files: 2, bytes: 0 }, 1), 0.5);
        assert_eq!(
            ratio(
                100,
                Totals {
                    files: 1,
                    bytes: 50
                },
                0
            ),
            1.
        );
    }
}
