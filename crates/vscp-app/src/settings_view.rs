#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use gpui::PathPromptOptions;
use gpui::prelude::*;
#[cfg(target_os = "macos")]
use vscp_core::settings::Settings;

use super::{AnyElement, Context, Dialog, Theme, Variant, Window, Workspace, button, div, modal};

impl Workspace {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir.is_none() || self.dialog.is_some() || self.quitting {
            return;
        }
        self.open_dialog(Dialog::Settings, window, cx);
        if self.active_terminal_settings(cx).is_none() {
            self.read_kitty(false, window, cx);
        }
    }

    fn active_terminal_settings<'a>(
        &'a self,
        cx: &'a gpui::App,
    ) -> Option<&'a crate::terminal::kitty::Kitty> {
        let id = self.selected.as_ref()?;
        let super::Connection::Connected { panes, .. } = self.connections.get(id)? else {
            return None;
        };
        panes.terminal.as_ref().map(|view| view.read(cx).settings())
    }

    pub(super) fn read_kitty(&mut self, apply: bool, window: &Window, cx: &mut Context<Self>) {
        let read = cx
            .background_executor()
            .spawn(async { crate::terminal::kitty::Kitty::load() });
        cx.spawn_in(window, async move |workspace, cx| {
            let mut settings = read.await;
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                settings.resolve_family(&cx.text_system().all_font_names());
                if apply {
                    for connection in workspace.connections.values() {
                        if let super::Connection::Connected { panes, .. } = connection
                            && let Some(view) = &panes.terminal
                        {
                            view.update(cx, |view, cx| view.reload(settings.clone(), window, cx));
                        }
                    }
                }
                workspace.kitty_settings = Some(settings);
                if window.is_window_active() {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn choose_editor(&mut self, window: &Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Elegir aplicación".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !matches!(this.dialog, Some(Dialog::Settings)) {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        match paths.into_iter().next().filter(|path| is_application(path)) {
                            Some(path) => this.save_editor(Some(path), window, cx),
                            None => this.settings_error = Some("Elige una aplicación .app.".into()),
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        this.settings_error =
                            Some("No se pudo abrir el selector de aplicaciones.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn save_editor(&mut self, editor: Option<PathBuf>, window: &Window, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        if self.saving {
            return;
        }
        self.saving = true;
        self.settings_error = None;
        cx.notify();
        let save = cx.background_executor().spawn(async move {
            let settings = Settings { editor };
            settings.save(&dir)?;
            Ok::<_, std::io::Error>(settings)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.saving = false;
                match result {
                    Ok(settings) => this.settings = settings,
                    Err(error) => {
                        this.settings_error =
                            Some(format!("No se pudo guardar settings.toml. {error}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn settings_dialog(&self, theme: Theme, cx: &Context<Self>) -> AnyElement {
        let terminal = self
            .active_terminal_settings(cx)
            .or(self.kitty_settings.as_ref());
        let mut body = div().flex().flex_col().gap_2().items_start();
        #[cfg(target_os = "macos")]
        {
            let editor = self.settings.editor.as_ref();
            body = body
                .child("Editor para archivos remotos")
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(button(
                            "settings-default",
                            "Aplicación predeterminada",
                            if editor.is_none() {
                                Variant::Selected
                            } else {
                                Variant::Secondary
                            },
                            theme,
                            cx.listener(|this, _, window, cx| this.save_editor(None, window, cx)),
                        ))
                        .child(button(
                            "settings-choose",
                            "Elegir aplicación…",
                            if editor.is_some() {
                                Variant::Selected
                            } else {
                                Variant::Secondary
                            },
                            theme,
                            cx.listener(|this, _, window, cx| {
                                if !this.saving {
                                    this.choose_editor(window, cx);
                                }
                            }),
                        )),
                )
                .when_some(editor, |body, editor| {
                    body.child(
                        div()
                            .text_color(theme.secondary_text)
                            .child(editor.display().to_string()),
                    )
                });
        }
        #[cfg(not(target_os = "macos"))]
        {
            body = body.child(
                div()
                    .text_color(theme.secondary_text)
                    .child("Los archivos se abren con la aplicación predeterminada (xdg-open)."),
            );
        }
        let body = body
            .when_some(self.settings_error.clone(), |body, error| {
                body.child(div().text_color(theme.danger).child(error))
            })
            .child(
                div().w_full().mt_2().pt_2().border_t_1().border_color(theme.border)
                    .flex().flex_col().items_start().gap_2().child("Terminal")
                    .when_some(terminal, |section, settings| {
                        let path = settings.path.as_ref().map_or_else(
                            || "No se encontró kitty.conf; se usan los valores de Kitty por defecto".into(),
                            |path| path.display().to_string(),
                        );
                        section.child(div().text_color(theme.secondary_text).child(path))
                            .child(div().text_color(theme.secondary_text).child(format!("{} · {} pt", settings.family, settings.size)))
                    })
                    .child(button("settings-kitty-reload", "Volver a leer", Variant::Secondary, theme,
                        cx.listener(|this, _, window, cx| this.read_kitty(true, window, cx)))),
            );
        let footer = div().flex().justify_end().child(
            button(
                "settings-close",
                if self.saving {
                    "Guardando…"
                } else {
                    "Cerrar"
                },
                Variant::Primary,
                theme,
                cx.listener(|this, _, window, cx| this.cancel_dialog(window, cx)),
            )
            .track_focus(&self.confirm_focus),
        );
        modal::overlay(
            "Ajustes",
            None,
            body.into_any_element(),
            footer.into_any_element(),
            &self.dialog_focus,
            &self.confirm_focus,
            theme,
            cx,
        )
        .into_any_element()
    }
}

#[cfg(target_os = "macos")]
fn is_application(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        && path.join("Contents/Info.plist").is_file()
}
