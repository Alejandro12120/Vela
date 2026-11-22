use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use gpui::prelude::*;
use gpui::{AnyElement, Div, ElementId, FontWeight, MouseButton, Task, anchored, deferred, point};
use vscp_core::edit::{
    self, DirectoryWatch, Edit, Index, OpenRemote, Schedule, Snapshot, Stamp, State, Target, Upload,
};
use vscp_core::{Session, config::Server};

use super::{
    Connection, Context, Dialog, Theme, Variant, Window, Workspace, button, dev, div, modal, px,
};
use crate::{
    components::button::icon_text_button,
    icons::{IconName, icon},
};

#[derive(Default)]
pub(super) struct Edits {
    pub index: Option<Index>,
    watch: Option<DirectoryWatch>,
    watch_task: Option<Task<()>>,
    activity: HashMap<Target, Activity>,
    banner: HashSet<String>,
    pub popover: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct Activity {
    schedule: Schedule,
    timer: Option<Task<()>>,
    notice: Option<(Instant, Notice)>,
    resume: bool,
}

enum Notice {
    Opening,
    Uploading,
    Pending,
    Uploaded(SystemTime),
    Error(String),
}

pub(super) enum EditDialog {
    Large(Target, OpenRemote),
    Conflict(Target, Option<Stamp>),
    Discard(Target),
}

impl Edits {
    pub fn shutdown(&mut self) -> Option<Index> {
        self.watch_task.take();
        self.watch.take();
        self.activity.clear();
        self.index.take()
    }

    fn rows(&self) -> &[Edit] {
        self.index.as_ref().map_or(&[], Index::edits)
    }

    pub fn has_server(&self, id: &str) -> bool {
        self.rows().iter().any(|row| row.target.server_id == id)
            || self
                .activity
                .iter()
                .any(|(target, activity)| target.server_id == id && activity.schedule.busy())
    }

    fn row(&self, target: &Target) -> Option<&Edit> {
        self.index.as_ref()?.get(target)
    }
}

impl Workspace {
    pub(super) fn load_edits(
        &mut self,
        index: io::Result<Index>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        match index {
            Ok(index) => self.edits.index = Some(index),
            Err(error) => {
                self.edits.error = Some(format!("No se pudo leer el índice de ediciones. {error}"));
                return;
            }
        }
        match DirectoryWatch::new() {
            Ok((watch, events)) => {
                self.edits.watch = Some(watch);
                self.edits.watch_task = Some(cx.spawn_in(window, async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this
                            .update_in(cx, |this, window, cx| {
                                let targets = match event {
                                    Ok(event) => {
                                        dev::log(format_args!("edit watcher {event:?}"));
                                        this.edits
                                            .rows()
                                            .iter()
                                            .filter(|row| {
                                                DirectoryWatch::affects(&event, &row.local_path)
                                            })
                                            .map(|row| row.target.clone())
                                            .collect::<Vec<_>>()
                                    }
                                    Err(error) => {
                                        this.edits.error = Some(format!(
                                            "Error al vigilar las copias locales. {error}"
                                        ));
                                        cx.notify();
                                        this.edits
                                            .rows()
                                            .iter()
                                            .map(|row| row.target.clone())
                                            .collect()
                                    }
                                };
                                for target in targets {
                                    this.edit_changed(target, window, cx);
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(error) => {
                self.edits.error = Some(format!("No se pudo iniciar la vigilancia. {error}"))
            }
        }
        for row in self.edits.rows().to_vec() {
            self.watch_edit(&row);
            // Catch saves made while vSCP was closed, including replacements.
            self.edit_changed(row.target, window, cx);
        }
    }

    fn watch_edit(&mut self, row: &Edit) {
        if let Some(watch) = &mut self.edits.watch
            && let Err(error) = watch.add(&row.local_path)
        {
            self.edits.error = Some(format!("No se pudo vigilar {}. {error}", name(&row.target)));
        }
    }

    fn edit_session(&self, target: &Target) -> Option<Session> {
        if self.quitting || !self.servers.iter().any(|server| target.matches(server)) {
            return None;
        }
        match self.connections.get(&target.server_id) {
            Some(Connection::Connected { session, .. }) => Some(session.clone()),
            _ => None,
        }
    }

    pub(super) fn edits_connected(
        &mut self,
        server: &Server,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self
            .edits
            .rows()
            .iter()
            .filter(|row| row.target.matches(server) && row.state != State::Synced)
            .map(|row| (row.target.clone(), row.state))
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            self.edits.banner.insert(server.id.clone());
            for (target, state) in rows {
                if state == State::Pending {
                    self.request_upload(target, None, window, cx);
                }
            }
        }
        if dev::edit_view().as_deref() == Some("popover") {
            self.edits.popover = true;
        }
        if let Some(path) = dev::edit_path() {
            self.open_edit(Target::new(server, path), window, cx);
        }
    }

    pub(super) fn open_edit(&mut self, target: Target, window: &Window, cx: &mut Context<Self>) {
        if self.quitting || self.edits.index.is_none() {
            return;
        }
        let existing = self.edits.row(&target).cloned();
        let session = self.edit_session(&target);
        let activity = self.edits.activity.entry(target.clone()).or_default();
        if activity.schedule.busy()
            || existing
                .as_ref()
                .is_some_and(|row| row.state != State::Synced)
            || session.is_none()
        {
            if let Some(row) = existing {
                self.launch_edit(row.target, row.local_path, window, cx);
            }
            return;
        }
        activity.schedule.begin(Instant::now(), true);
        self.edit_notice(&target, Some(Notice::Opening), cx);
        let session = session.unwrap();
        cx.spawn_in(window, async move |this, cx| {
            let result = OpenRemote::inspect(&session, &target).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let activity = self::activity(this, &target);
                activity.schedule.finish();
                this.edit_notice(&target, None, cx);
                match result {
                    Ok(remote) if existing.is_none() && remote.stamp.size > edit::CONFIRM_SIZE => {
                        if this.dialog.is_none() {
                            this.open_dialog(
                                Dialog::Edit(EditDialog::Large(target.clone(), remote)),
                                window,
                                cx,
                            );
                        } else {
                            this.edit_error(
                                &target,
                                "Confirma la descarga cuando esté libre la ventana",
                                cx,
                            );
                        }
                    }
                    Ok(remote) => this.download_edit(target.clone(), remote, window, cx),
                    Err(error) => {
                        this.edit_error(&target, error, cx);
                        if let Some(row) = existing {
                            this.launch_edit(row.target, row.local_path, window, cx);
                        }
                    }
                }
                this.arm_edit_timer(target, window, cx);
            });
        })
        .detach();
    }

    fn download_edit(
        &mut self,
        target: Target,
        remote: OpenRemote,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.edit_session(&target) else {
            return;
        };
        let Some(index) = &self.edits.index else {
            return;
        };
        let existing = index.get(&target).cloned();
        if !activity(self, &target).schedule.begin(Instant::now(), true) {
            return;
        }
        let reopening = existing.is_some();
        let directory = if !reopening {
            match self.edits.index.as_ref().unwrap().directory(&target) {
                Ok(directory) => Some(directory),
                Err(error) => {
                    activity(self, &target).schedule.finish();
                    self.edit_error(&target, error, cx);
                    return;
                }
            }
        } else {
            None
        };
        self.edit_notice(&target, Some(Notice::Opening), cx);
        let key = target.clone();
        let download = cx.background_executor().spawn(async move {
            let result = if let Some(mut row) = existing {
                row.refresh(&session, remote).await.map(|baseline| {
                    if let Some(baseline) = baseline {
                        row.baseline = baseline;
                    }
                    row
                })
            } else {
                remote
                    .open(&session, key, directory.as_ref().unwrap().path())
                    .await
            };
            (result, directory)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (result, directory) = download.await;
            let _ = this.update_in(cx, |this, window, cx| {
                activity(this, &target).schedule.finish();
                this.edit_notice(&target, None, cx);
                match result {
                    Ok(row) if this.save_edit(row.clone(), cx) => {
                        if let Some(directory) = directory {
                            let _ = directory.keep();
                            this.watch_edit(&row);
                        }
                        this.launch_edit(target.clone(), row.local_path, window, cx);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        this.edit_error(&target, error, cx);
                        if let Some(row) = this.edits.row(&target) {
                            this.launch_edit(target.clone(), row.local_path.clone(), window, cx);
                        }
                    }
                }
                if reopening {
                    this.edit_changed(target, window, cx);
                }
            });
        })
        .detach();
    }

    fn save_edit(&mut self, row: Edit, cx: &mut Context<Self>) -> bool {
        let target = row.target.clone();
        match self.edits.index.as_mut().map(|index| index.put(row)) {
            Some(Ok(())) => {
                self.clear_resolved_banner(&target.server_id);
                cx.notify();
                true
            }
            Some(Err(error)) => {
                self.edit_error(
                    &target,
                    format!("No se pudo guardar edits.toml. {error}"),
                    cx,
                );
                false
            }
            None => false,
        }
    }

    fn clear_resolved_banner(&mut self, id: &str) {
        if self.edits.banner.contains(id)
            && !self
                .edits
                .rows()
                .iter()
                .any(|row| row.target.server_id == id && row.state != State::Synced)
        {
            self.edits.banner.remove(id);
        }
    }

    fn launch_edit(&self, target: Target, path: PathBuf, window: &Window, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        dev::log(format_args!(
            "edit opened {} -> {}",
            target.remote_path.display(),
            path.display()
        ));
        let editor = self.settings.editor.clone();
        let launch = cx
            .background_executor()
            .spawn(async move { dev::open_file(&path, editor.as_deref()) });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = launch.await {
                let _ = this.update_in(cx, |this, _, cx| this.edit_error(&target, error, cx));
            }
        })
        .detach();
    }

    fn edit_changed(&mut self, target: Target, window: &Window, cx: &mut Context<Self>) {
        activity(self, &target).schedule.changed(Instant::now());
        dev::log(format_args!(
            "edit debounce {}",
            target.remote_path.display()
        ));
        self.arm_edit_timer(target, window, cx);
    }

    fn arm_edit_timer(&mut self, target: Target, window: &Window, cx: &mut Context<Self>) {
        let Some(delay) = activity(self, &target).schedule.delay(Instant::now()) else {
            return;
        };
        let timer = cx.background_executor().timer(delay);
        let key = target.clone();
        activity(self, &key).timer = Some(cx.spawn_in(window, async move |this, cx| {
            timer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.scan_edit(target, false, None, window, cx)
            });
        }));
    }

    fn request_upload(
        &mut self,
        target: Target,
        expected: Option<Stamp>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.scan_edit(target, true, expected, window, cx);
    }

    fn scan_edit(
        &mut self,
        target: Target,
        immediate: bool,
        expected: Option<Stamp>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.edits.row(&target).cloned() else {
            return;
        };
        let activity = activity(self, &target);
        if !activity.schedule.begin(Instant::now(), immediate) {
            activity.resume |= immediate && expected.is_none();
            return;
        }
        activity.timer.take();
        let scan = cx.background_executor().spawn(async move {
            let result = row.snapshot();
            (row, result)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (mut row, result) = scan.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Some(snapshot)) => {
                        if this.send_edit(row, snapshot, immediate, expected, window, cx) {
                            return;
                        }
                    }
                    Ok(None) => {
                        dev::log(format_args!(
                            "edit unchanged {}",
                            target.remote_path.display()
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        this.edit_changed(target.clone(), window, cx);
                    }
                    Err(error) => {
                        if row.state == State::Synced {
                            row.state = State::Pending;
                            this.save_edit(row, cx);
                        }
                        this.edit_error(&target, error, cx);
                    }
                }
                this.finish_edit_job(target, window, cx);
            });
        })
        .detach();
    }

    fn send_edit(
        &mut self,
        mut row: Edit,
        snapshot: Snapshot,
        immediate: bool,
        expected: Option<Stamp>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if row.state == State::Conflict && !immediate {
            return false;
        }
        let target = row.target.clone();
        if row.state == State::Synced {
            row.state = State::Pending;
            if !self.save_edit(row.clone(), cx) {
                return false;
            }
        }
        let Some(session) = self.edit_session(&target) else {
            dev::log(format_args!(
                "edit pending offline {}",
                target.remote_path.display()
            ));
            self.edit_notice(&target, Some(Notice::Pending), cx);
            return false;
        };
        self.edit_notice(&target, Some(Notice::Uploading), cx);
        dev::log(format_args!(
            "edit upload start {}",
            target.remote_path.display()
        ));
        cx.spawn_in(window, async move |this, cx| {
            let result = row
                .upload(&session, snapshot, expected.unwrap_or(row.baseline.remote))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Upload::Synced(baseline)) => {
                        dev::log(format_args!(
                            "edit uploaded {} sha256={}",
                            target.remote_path.display(),
                            baseline.sha256
                        ));
                        row.baseline = baseline;
                        row.state = State::Synced;
                        if this.save_edit(row, cx) {
                            this.edit_notice(
                                &target,
                                Some(Notice::Uploaded(SystemTime::now())),
                                cx,
                            );
                        }
                    }
                    Ok(Upload::Conflict(stamp)) => {
                        dev::log(format_args!(
                            "edit conflict {}",
                            target.remote_path.display()
                        ));
                        row.state = State::Conflict;
                        if this.save_edit(row, cx) {
                            this.edit_notice(&target, None, cx);
                            if this.dialog.is_none()
                                && this.selected.as_deref() == Some(&target.server_id)
                            {
                                this.open_dialog(
                                    Dialog::Edit(EditDialog::Conflict(target.clone(), stamp)),
                                    window,
                                    cx,
                                );
                            }
                        }
                    }
                    Err(error) => this.edit_error(&target, error, cx),
                }
                this.finish_edit_job(target, window, cx);
            });
        })
        .detach();
        true
    }

    fn finish_edit_job(&mut self, target: Target, window: &Window, cx: &mut Context<Self>) {
        let activity = activity(self, &target);
        activity.schedule.finish();
        if std::mem::take(&mut activity.resume) {
            self.request_upload(target, None, window, cx);
        } else {
            self.arm_edit_timer(target, window, cx);
        }
        cx.notify();
    }

    fn edit_notice(&mut self, target: &Target, notice: Option<Notice>, cx: &mut Context<Self>) {
        activity(self, target).notice = notice.map(|notice| (Instant::now(), notice));
        cx.notify();
    }

    fn edit_error(
        &mut self,
        target: &Target,
        error: impl std::fmt::Display,
        cx: &mut Context<Self>,
    ) {
        dev::log(format_args!(
            "edit error {}: {error}",
            target.remote_path.display()
        ));
        self.edit_notice(target, Some(Notice::Error(error.to_string())), cx);
    }

    fn later_copy(&mut self, target: Target, window: &Window, cx: &mut Context<Self>) {
        let Some(row) = self.edits.row(&target).cloned() else {
            return;
        };
        let Some(session) = self.edit_session(&target) else {
            return;
        };
        if !activity(self, &target).schedule.begin(Instant::now(), true) {
            return;
        }
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = match row.save_remote_copy(&session).await {
                Ok(path) => executor.spawn(async move { reveal(&path) }).await,
                Err(error) => Err(io::Error::other(error)),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result {
                    this.edit_error(&target, error, cx);
                }
                this.finish_edit_job(target, window, cx);
            });
        })
        .detach();
    }

    fn remove_edit(
        &mut self,
        target: Target,
        discard: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.edits.row(&target).cloned() else {
            return;
        };
        let task = activity(self, &target);
        if task.schedule.busy() {
            return;
        }
        let Some(index) = &mut self.edits.index else {
            return;
        };
        match index.remove(&target, discard) {
            Ok(true) => {
                self.edits.activity.remove(&target);
                self.clear_resolved_banner(&target.server_id);
                if let Some(watcher) = &mut self.edits.watch {
                    let _ = watcher.remove(&row.local_path);
                }
            }
            Ok(false) => self.edit_changed(target, window, cx),
            Err(error) => self.edit_error(&target, error, cx),
        }
        cx.notify();
    }

    pub(super) fn confirm_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Edit(dialog)) = self.dialog.take() else {
            unreachable!()
        };
        self.finish_dialog(window, cx);
        match dialog {
            EditDialog::Large(target, remote) => self.download_edit(target, remote, window, cx),
            EditDialog::Discard(target) => self.remove_edit(target, true, window, cx),
            EditDialog::Conflict(target, Some(stamp)) => {
                self.request_upload(target, Some(stamp), window, cx)
            }
            EditDialog::Conflict(_, None) => {}
        }
    }

    pub(super) fn edit_dialog(
        &self,
        dialog: &EditDialog,
        theme: Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (title, body, confirm, cancel) = match dialog {
            EditDialog::Large(_, remote) => (
                "Descargar para editar",
                format!(
                    "Este archivo pesa {}. ¿Descargarlo para editar?",
                    crate::file_metadata::format_size(remote.stamp.size)
                ),
                Some("Descargar"),
                "Cancelar",
            ),
            EditDialog::Discard(target) => (
                "Descartar cambios",
                format!(
                    "¿Eliminar la copia local de «{}» y descartar sus cambios?",
                    name(target)
                ),
                Some("Descartar cambios"),
                "Cancelar",
            ),
            EditDialog::Conflict(target, stamp) => (
                "El archivo remoto ha cambiado desde que lo abriste",
                target.remote_path.display().to_string(),
                stamp.as_ref().map(|_| "Sobrescribir remoto"),
                "Más tarde",
            ),
        };
        let conflict = matches!(dialog, EditDialog::Conflict(..));
        let copy = if let EditDialog::Conflict(target, _) = dialog {
            let target = target.clone();
            Some(
                button(
                    "edit-save-copy",
                    "Guardar remoto como copia",
                    Variant::Secondary,
                    theme,
                    cx.listener(move |this, _, window, cx| {
                        this.finish_dialog(window, cx);
                        this.later_copy(target.clone(), window, cx);
                    }),
                )
                .text_size(px(12.)),
            )
        } else {
            None
        };
        let footer = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap_1()
            .child(
                button(
                    "edit-dialog-cancel",
                    cancel,
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.finish_dialog(window, cx)),
                )
                .when(conflict, |button| button.text_size(px(12.))),
            )
            .children(copy)
            .when_some(confirm, |footer, label| {
                footer.child(
                    button(
                        "edit-dialog-confirm",
                        label,
                        if conflict {
                            Variant::Secondary
                        } else {
                            Variant::Primary
                        },
                        theme,
                        cx.listener(|this, _, window, cx| this.confirm_edit(window, cx)),
                    )
                    .when(conflict, |button| {
                        button.text_size(px(12.)).text_color(theme.danger)
                    })
                    .track_focus(&self.confirm_focus),
                )
            });
        let lead = match dialog {
            EditDialog::Large(..) => None,
            EditDialog::Discard(_) => Some((IconName::Undo, theme.danger)),
            EditDialog::Conflict(..) => Some((IconName::Warning, theme.warning)),
        };
        modal::overlay(
            title,
            lead,
            div().child(body).into_any_element(),
            footer.into_any_element(),
            &self.dialog_focus,
            &self.confirm_focus,
            theme,
            cx,
        )
        .into_any_element()
    }

    pub(super) fn edit_header(
        &self,
        id: &str,
        theme: Theme,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let count = self
            .edits
            .rows()
            .iter()
            .filter(|row| row.target.server_id == id)
            .count();
        div().relative().flex_none().child(
            icon_text_button(
                "edits",
                IconName::Edits,
                "Ediciones",
                if self.edits.popover {
                    Variant::Selected
                } else {
                    Variant::Secondary
                },
                None,
                theme,
                cx.listener(|this, _, _, cx| {
                    this.edits.popover = !this.edits.popover;
                    cx.notify();
                }),
            )
            .h(px(26.))
            .child(
                div()
                    .px_1()
                    .rounded(px(4.))
                    .bg(theme.hover)
                    .text_size(px(12.))
                    .text_color(theme.secondary_text)
                    .child(count.to_string()),
            ),
        )
    }

    pub(super) fn edit_popover(
        &self,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.edits.popover || self.dialog.is_some() {
            return None;
        }
        let id = self.selected.as_ref()?;
        let rows = self
            .edits
            .rows()
            .iter()
            .filter(|row| &row.target.server_id == id);
        let mut list = div().id("edit-list").max_h(px(360.)).overflow_y_scroll();
        let mut empty = true;
        for (i, row) in rows.enumerate() {
            empty = false;
            list = list.child(self.edit_row(row, i, theme, cx));
        }
        if empty {
            list = list.child(
                div()
                    .p_3()
                    .text_color(theme.secondary_text)
                    .child("Sin archivos en edición"),
            );
        }
        let panel = div()
            .id("edit-popover")
            .w(px(380.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(theme.raised)
            .border_1()
            .border_color(theme.border)
            .occlude()
            .shadow(vec![gpui::BoxShadow {
                color: theme.overlay,
                inset: false,
                offset: point(px(0.), px(4.)),
                blur_radius: px(16.),
                spread_radius: px(0.),
            }])
            .child(
                div()
                    .px_3()
                    .py_2()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Ediciones"),
            )
            .child(list)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let panel = crate::components::motion::appear(
            panel,
            "edit-popover-appear",
            4.,
            crate::components::motion::FAST,
        );
        Some(
            div()
                .id("edit-popover-layer")
                .absolute()
                .inset_0()
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edits.popover = false;
                    cx.notify();
                }))
                .child(
                    deferred(
                        anchored()
                            .position(point(window.viewport_size().width - px(388.), px(40.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(panel),
                    )
                    .with_priority(1),
                )
                .into_any_element(),
        )
    }

    fn edit_row(&self, row: &Edit, i: usize, theme: Theme, cx: &Context<Self>) -> Div {
        let target = &row.target;
        let state = match row.state {
            State::Synced => ("Sincronizado", theme.success, Some(IconName::Check)),
            State::Pending => ("Cambios sin subir", theme.warning, None),
            State::Conflict => ("Conflicto", theme.danger, Some(IconName::Warning)),
        };
        let busy = self
            .edits
            .activity
            .get(target)
            .is_some_and(|a| a.schedule.busy());
        let action =
            |id,
             name,
             label,
             tone,
             action: fn(&mut Workspace, Target, &mut Window, &mut Context<Workspace>)| {
                let target = target.clone();
                icon_text_button(
                    ElementId::named_usize(id, i),
                    name,
                    label,
                    Variant::Secondary,
                    tone,
                    theme,
                    cx.listener(move |this, _, window, cx| {
                        action(this, target.clone(), window, cx)
                    }),
                )
                .h(px(24.))
                .text_size(px(12.))
            };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .p_3()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(name(target)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(12.))
                            .text_color(state.1)
                            .when_some(state.2, |label, name| {
                                label.child(icon(name).size(px(14.)).text_color(state.1))
                            })
                            .child(state.0),
                    ),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(theme.secondary_text)
                    .child(target.remote_path.display().to_string()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .mt_1()
                    .child(action(
                        "edit-open",
                        IconName::ExternalLink,
                        "Abrir",
                        None,
                        |this, target, window, cx| this.open_edit(target, window, cx),
                    ))
                    .when(!busy && self.edit_session(target).is_some(), |bar| {
                        bar.child(action(
                            "edit-upload",
                            IconName::Upload,
                            "Subir ahora",
                            None,
                            |this, target, window, cx| {
                                this.request_upload(target, None, window, cx)
                            },
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(action(
                        "edit-reveal",
                        IconName::FolderOpen,
                        if cfg!(target_os = "macos") {
                            "Mostrar en Finder"
                        } else {
                            "Mostrar en la carpeta"
                        },
                        None,
                        Workspace::reveal_edit,
                    ))
                    .when(!busy && row.state == State::Synced, |bar| {
                        bar.child(action(
                            "edit-stop",
                            IconName::Close,
                            "Dejar de editar",
                            None,
                            |this, target, window, cx| this.remove_edit(target, false, window, cx),
                        ))
                    })
                    .when(!busy && row.state != State::Synced, |bar| {
                        bar.child(action(
                            "edit-discard",
                            IconName::Undo,
                            "Descartar cambios…",
                            Some(theme.danger),
                            |this, target, window, cx| {
                                this.open_dialog(
                                    Dialog::Edit(EditDialog::Discard(target)),
                                    window,
                                    cx,
                                )
                            },
                        ))
                    }),
            )
    }

    fn reveal_edit(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.edits.row(&target) else {
            return;
        };
        let path = row.local_path.clone();
        let open = cx.background_executor().spawn(async move { reveal(&path) });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = open.await {
                let _ = this.update_in(cx, |this, _, cx| this.edit_error(&target, error, cx));
            }
        })
        .detach();
    }

    pub(super) fn edit_status(&self, id: &str, theme: Theme, cx: &Context<Self>) -> Div {
        let mut strip = div().flex_none();
        if let Some(error) = &self.edits.error {
            strip = strip.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .text_color(theme.danger)
                    .child(
                        icon(IconName::Error)
                            .size(px(14.))
                            .mt(px(2.))
                            .text_color(theme.danger),
                    )
                    .child(div().min_w_0().child(error.clone())),
            );
        }
        let pending = self
            .edits
            .rows()
            .iter()
            .filter(|row| row.target.server_id == id && row.state != State::Synced)
            .count();
        if pending > 0 && self.edits.banner.contains(id) {
            let id = id.to_owned();
            strip = strip.child(crate::components::motion::appear(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .bg(theme.warning_bg)
                    .text_color(theme.warning)
                    .child(
                        icon(IconName::Warning)
                            .size(px(14.))
                            .text_color(theme.warning),
                    )
                    .child(if pending == 1 {
                        "1 archivo con cambios sin subir".into()
                    } else {
                        format!("{pending} archivos con cambios sin subir")
                    })
                    .child(
                        button(
                            "edit-review",
                            "Revisar",
                            Variant::Secondary,
                            theme,
                            cx.listener(move |this, _, _, cx| {
                                this.edits.banner.remove(&id);
                                this.edits.popover = true;
                                cx.notify();
                            }),
                        )
                        .h(px(24.)),
                    ),
                "edit-banner",
                -4.,
                crate::components::motion::FAST,
            ));
        }
        if let Some((target, (_, notice))) = self
            .edits
            .activity
            .iter()
            .filter(|(target, _)| target.server_id == id)
            .filter_map(|(target, row)| row.notice.as_ref().map(|notice| (target, notice)))
            .max_by_key(|(_, (when, notice))| (matches!(notice, Notice::Uploading), *when))
        {
            let (caption, color, glyph) = match notice {
                Notice::Opening => (
                    format!("Abriendo {}…", name(target)),
                    theme.secondary_text,
                    IconName::Spinner,
                ),
                Notice::Uploading => (
                    format!("Subiendo cambios de {}…", name(target)),
                    theme.secondary_text,
                    IconName::Upload,
                ),
                Notice::Pending => (
                    format!("{} · Cambios sin subir", name(target)),
                    theme.warning,
                    IconName::Warning,
                ),
                Notice::Uploaded(time) => (
                    format!(
                        "{} subido · {}",
                        name(target),
                        crate::file_metadata::clock(*time)
                    ),
                    theme.success,
                    IconName::Check,
                ),
                Notice::Error(error) => (
                    format!("{} · {error}", name(target)),
                    theme.danger,
                    IconName::Error,
                ),
            };
            let target = target.clone();
            strip = strip.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .px_2()
                    .py_1()
                    .text_size(px(12.))
                    .text_color(color)
                    .child({
                        let glyph_icon = icon(glyph).size(px(14.)).text_color(color);
                        if glyph == IconName::Spinner {
                            crate::components::motion::spin(glyph_icon, "edit-spinner")
                                .into_any_element()
                        } else {
                            glyph_icon.into_any_element()
                        }
                    })
                    .child(
                        div()
                            .id("edit-caption")
                            .min_w_0()
                            .truncate()
                            .child(caption.clone())
                            .aria_label(caption),
                    )
                    .when(
                        matches!(notice, Notice::Error(_)) && self.edit_session(&target).is_some(),
                        |bar| {
                            bar.child(
                                button(
                                    "edit-retry",
                                    "Reintentar",
                                    Variant::Secondary,
                                    theme,
                                    cx.listener(move |this, _, window, cx| {
                                        if this
                                            .edits
                                            .row(&target)
                                            .is_some_and(|row| row.state != State::Synced)
                                        {
                                            this.request_upload(target.clone(), None, window, cx);
                                        } else {
                                            this.open_edit(target.clone(), window, cx);
                                        }
                                    }),
                                )
                                .h(px(24.)),
                            )
                        },
                    ),
            );
        }
        strip
    }
}

fn activity<'a>(workspace: &'a mut Workspace, target: &Target) -> &'a mut Activity {
    workspace.edits.activity.entry(target.clone()).or_default()
}

fn name(target: &Target) -> String {
    target
        .remote_path
        .file_name()
        .unwrap_or(target.remote_path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn reveal(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .status()?
            .success()
        {
            Ok(())
        } else {
            Err(io::Error::other("No se pudo mostrar la copia en Finder"))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let folder = path.parent().unwrap_or(path);
        if std::process::Command::new("xdg-open")
            .arg(folder)
            .status()?
            .success()
        {
            Ok(())
        } else {
            Err(io::Error::other("No se pudo mostrar la carpeta"))
        }
    }
}
