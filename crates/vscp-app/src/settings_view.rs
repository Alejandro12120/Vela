use std::path::{Path, PathBuf};

use gpui::{PathPromptOptions, prelude::*};
use vscp_core::settings::Settings;

use super::{AnyElement, Context, Dialog, Theme, Variant, Window, Workspace, button, div, modal};

impl Workspace {
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir.is_none() || self.dialog.is_some() || self.quitting {
            return;
        }
        self.open_dialog(Dialog::Settings, window, cx);
    }

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
        let editor = self.settings.editor.as_ref();
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .items_start()
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
            })
            .when_some(self.settings_error.clone(), |body, error| {
                body.child(div().text_color(theme.danger).child(error))
            });
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

fn is_application(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        && path.join("Contents/Info.plist").is_file()
}
