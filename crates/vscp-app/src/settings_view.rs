use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

use gpui::{
    Div, ElementId, FocusHandle, FontWeight, PathPromptOptions, ScrollHandle, Stateful,
    linear_color_stop, linear_gradient, prelude::*, px, rgba,
};
use vscp_core::{
    appearance::{
        Colors, Scheme, ThemeDef, delete_user_theme, import_theme, load_user_themes,
        save_user_theme, unused_theme_id,
    },
    settings::Settings,
};

use super::{Context, Dialog, Theme, Variant, Window, Workspace, button, div};
use crate::{
    appearance::{Appearance, Variant as ColorVariant},
    components::button::{icon_button, icon_text_button},
    icons::{IconName, icon},
};

pub(super) struct SettingsPage {
    previous_focus: Option<FocusHandle>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    schemes: [FocusHandle; 3],
    themes: HashMap<String, FocusHandle>,
    busy: bool,
    theme_error: Option<String>,
}

impl SettingsPage {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        Self {
            previous_focus: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            schemes: std::array::from_fn(|_| cx.focus_handle()),
            themes: HashMap::new(),
            busy: false,
            theme_error: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.previous_focus.is_some()
    }
}

impl Workspace {
    pub(crate) fn settings_open(&self) -> bool {
        self.settings_page.is_open()
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir.is_none() || self.dialog.is_some() || self.quitting {
            return;
        }
        if self.settings_open() {
            self.close_settings(window, cx);
            return;
        }
        self.dismiss_row_menu(window, cx);
        self.edits.popover = false;
        self.settings_page.previous_focus =
            Some(window.focused(cx).unwrap_or_else(|| self.focus.clone()));
        self.settings_page.focus.focus(window, cx);
        self.settings_page.scroll = ScrollHandle::new();
        if self.active_terminal_settings(cx).is_none() && self.kitty_settings.is_none() {
            self.read_kitty(false, window, cx);
        }
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previous) = self.settings_page.previous_focus.take() {
            previous.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn dev_open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = crate::dev::settings_page();
        if request.is_some() || crate::dev::edit_view().as_deref() == Some("settings") {
            self.open_settings(window, cx);
        }
        if request.as_deref() == Some("interface") {
            self.settings_page.scroll.scroll_to_top_of_item(1);
        }
        if let Some(id) = request
            .as_deref()
            .and_then(|request| request.strip_prefix("delete:"))
            && cx
                .global::<Appearance>()
                .themes
                .iter()
                .any(|theme| theme.id == id && !theme.builtin)
        {
            self.open_dialog(Dialog::DeleteTheme(id.into()), window, cx);
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
                if !this.settings_open() {
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
        let mut next = self.settings.clone();
        next.editor = editor;
        self.save_settings(next, window, cx);
    }

    fn save_settings(&mut self, next: Settings, window: &Window, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        if self.settings_page.busy {
            return;
        }
        self.settings_page.busy = true;
        self.settings_error = None;
        cx.notify();
        let save = cx.background_executor().spawn(async move {
            next.save(&dir)?;
            Ok::<_, io::Error>(next)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.settings_page.busy = false;
                match result {
                    Ok(settings) => {
                        cx.global_mut::<Appearance>().settings = settings.appearance.clone();
                        this.settings = settings;
                        cx.refresh_windows();
                    }
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

    fn set_scheme(&mut self, scheme: Scheme, window: &Window, cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        next.appearance.scheme = scheme;
        self.save_settings(next, window, cx);
    }

    fn activate_theme(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        next.appearance.theme = id.into();
        self.save_settings(next, window, cx);
    }

    fn change_themes(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
        on_success: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(dir) = self.dir.clone() else { return };
        if self.settings_page.busy {
            return;
        }
        self.settings_page.busy = true;
        self.settings_page.theme_error = None;
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            operation(&dir)?;
            Ok::<_, io::Error>(load_user_themes(&dir))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.settings_page.busy = false;
                match result {
                    Ok((users, errors)) => {
                        cx.global_mut::<Appearance>().replace_users(users, errors);
                        on_success(this, window, cx);
                        cx.refresh_windows();
                    }
                    Err(error) => this.settings_page.theme_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn duplicate_theme(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        let appearance = cx.global::<Appearance>();
        let Some(mut copy) = appearance
            .themes
            .iter()
            .find(|theme| theme.id == id)
            .cloned()
        else {
            return;
        };
        let base = format!("{} copia", copy.name);
        copy.name = base.clone();
        for suffix in 2.. {
            if !appearance
                .themes
                .iter()
                .any(|theme| names_match(&theme.name, &copy.name))
            {
                break;
            }
            copy.name = format!("{base} {suffix}");
        }
        copy.builtin = false;
        self.change_themes(
            window,
            cx,
            move |dir| {
                copy.id = unused_theme_id(dir, &copy.name)?;
                save_user_theme(dir, &copy)
            },
            |_, _, _| {},
        );
    }

    pub(super) fn remove_theme(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let deleted = id.clone();
        self.finish_dialog(window, cx);
        self.change_themes(
            window,
            cx,
            move |dir| delete_user_theme(dir, &deleted),
            move |this, window, cx| {
                if this.settings.appearance.theme == id {
                    this.activate_theme("vscp", window, cx);
                }
            },
        );
    }

    fn add_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.settings_page.busy {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Añadir tema".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.settings_open() {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.change_themes(
                                window,
                                cx,
                                move |dir| import_theme(dir, &path).map(|_| ()),
                                |_, _, _| {},
                            );
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        this.settings_page.theme_error =
                            Some("No se pudo abrir el selector de archivos.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_settings(
        &mut self,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        self.settings_page
            .themes
            .retain(|id, _| appearance.themes.iter().any(|theme| &theme.id == id));
        for theme in &appearance.themes {
            self.settings_page
                .themes
                .entry(theme.id.clone())
                .or_insert_with(|| cx.focus_handle());
        }
        let available = window.viewport_size().width.as_f32()
            - if self.sidebar_visible() {
                crate::sidebar::WIDTH
            } else {
                0.
            };
        let columns = if available < 608. { 2 } else { 3 };
        let active = appearance.active();
        let schemes = [
            ("Sistema", IconName::Monitor, Scheme::System),
            ("Claro", IconName::Sun, Scheme::Light),
            ("Oscuro", IconName::Moon, Scheme::Dark),
        ];
        let appearance_section = section("Apariencia")
            .child(subtitle("Esquema de color"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .children(schemes.into_iter().enumerate().map(
                        |(index, (label, icon, scheme))| {
                            self.scheme_card(index, label, icon, scheme, active, theme, cx)
                        },
                    )),
            )
            .child(
                div()
                    .mt_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(subtitle("Temas"))
                    .child(
                        icon_text_button(
                            "add-theme",
                            IconName::Plus,
                            "Añadir tema",
                            Variant::Secondary,
                            None,
                            theme,
                            cx.listener(|this, _, window, cx| this.add_theme(window, cx)),
                        )
                        .h(px(26.)),
                    ),
            )
            .when_some(self.settings_page.theme_error.clone(), |section, error| {
                section.child(
                    div()
                        .text_color(theme.danger)
                        .text_size(px(12.))
                        .child(error),
                )
            })
            .child(
                div().flex().flex_col().gap_3().children(
                    appearance
                        .themes
                        .chunks(columns)
                        .enumerate()
                        .map(|(row, themes)| {
                            div()
                                .flex()
                                .gap_3()
                                .children(themes.iter().enumerate().map(|(col, def)| {
                                    self.theme_card(row * columns + col, columns, def, theme, cx)
                                }))
                                .children((themes.len()..columns).map(|_| div().flex_1().min_w_0()))
                        }),
                ),
            )
            .children(appearance.errors.iter().cloned().map(|error| {
                div()
                    .text_color(theme.secondary_text)
                    .text_size(px(12.))
                    .child(error)
            }));
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex_col()
            .track_focus(&self.settings_page.focus)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(52.))
                    .flex_none()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(!self.sidebar_visible(), |row| {
                                row.child(self.sidebar_button(theme, cx))
                            })
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Ajustes"),
                            ),
                    )
                    .child(
                        icon_button(
                            "settings-close",
                            "Cerrar ajustes",
                            IconName::Close,
                            Variant::Secondary,
                            theme,
                            cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                        )
                        .size(px(28.))
                        .px_0(),
                    ),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .flex_col()
                    .items_center()
                    .overflow_y_scroll()
                    .track_scroll(&self.settings_page.scroll)
                    .child(appearance_section)
                    .child(
                        section("Interfaz").child(
                            div()
                                .text_color(theme.secondary_text)
                                .child("Vidrio esmerilado: próximamente."),
                        ),
                    )
                    .child(self.editor_section(theme, cx))
                    .child(self.terminal_section(theme, cx).pb_6()),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn scheme_card(
        &self,
        index: usize,
        label: &'static str,
        name: IconName,
        scheme: Scheme,
        active: &ThemeDef,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let focus = &self.settings_page.schemes[index];
        let selected = self.settings.appearance.scheme == scheme;
        let miniature = div()
            .flex()
            .h(px(120.))
            .w_full()
            .overflow_hidden()
            .rounded(px(6.))
            .when(scheme != Scheme::Dark, |row| {
                row.child(miniature(active.light))
            })
            .when(scheme != Scheme::Light, |row| {
                row.child(miniature(active.dark))
            });
        card(("scheme", index), focus, theme)
            .aria_selected(selected)
            .border_2()
            .when(selected, |card| card.border_color(theme.accent))
            .p_2()
            .gap_2()
            .child(miniature)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_1()
                    .text_size(px(12.))
                    .child(icon(name).size(px(12.)).text_color(theme.secondary_text))
                    .child(label),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.set_scheme(scheme, window, cx)))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if let Some(next) = arrow_target(index, 1, 3, &event.keystroke.key) {
                        this.settings_page.schemes[next].focus(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
    }

    fn theme_card(
        &self,
        index: usize,
        columns: usize,
        def: &ThemeDef,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let appearance = cx.global::<Appearance>();
        let active = appearance.active().id == def.id;
        let ring = |variant| {
            active.then_some(match (self.settings.appearance.scheme, variant) {
                (Scheme::Light, ColorVariant::Dark) | (Scheme::Dark, ColorVariant::Light) => 0.4,
                _ => 1.,
            })
        };
        let activate = def.id.clone();
        let duplicate = def.id.clone();
        let delete = def.id.clone();
        let focus = self.settings_page.themes[&def.id].clone();
        let key_focus = focus.clone();
        card(&focus, &focus, theme)
            .aria_label(def.name.clone())
            .aria_selected(active)
            .justify_between()
            .h(px(124.))
            .p_4()
            .gap_2()
            .child(
                div()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_6()
                    .child(orb(
                        def.light,
                        IconName::Sun,
                        ring(ColorVariant::Light),
                        theme,
                    ))
                    .child(orb(
                        def.dark,
                        IconName::Moon,
                        ring(ColorVariant::Dark),
                        theme,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(def.name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .flex_none()
                            .child(theme_action(
                                "duplicate",
                                "Duplicar",
                                IconName::Copy,
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.duplicate_theme(&duplicate, window, cx);
                                }),
                            ))
                            .when(!def.builtin, |row| {
                                row.child(theme_action(
                                    "delete",
                                    "Eliminar",
                                    IconName::Trash,
                                    theme,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        if !this.settings_page.busy {
                                            this.open_dialog(
                                                Dialog::DeleteTheme(delete.clone()),
                                                window,
                                                cx,
                                            );
                                        }
                                    }),
                                ))
                            }),
                    ),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.activate_theme(&activate, window, cx)),
            )
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if !key_focus.is_focused(window) {
                        return;
                    }
                    let themes = &cx.global::<Appearance>().themes;
                    if let Some(next) =
                        arrow_target(index, columns, themes.len(), &event.keystroke.key)
                    {
                        this.settings_page.themes[&themes[next].id].focus(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
    }

    fn editor_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
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
                                if !this.settings_page.busy {
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
        section("Editor")
            .child(body)
            .when_some(self.settings_error.clone(), |section, error| {
                section.child(div().text_color(theme.danger).child(error))
            })
    }

    fn terminal_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let terminal = self
            .active_terminal_settings(cx)
            .or(self.kitty_settings.as_ref());
        section("Terminal")
            .items_start()
            .when_some(terminal, |section, settings| {
                let path = settings.path.as_ref().map_or_else(
                    || "No se encontró kitty.conf; se usan los valores de Kitty por defecto".into(),
                    |path| path.display().to_string(),
                );
                section
                    .child(div().text_color(theme.secondary_text).child(path))
                    .child(
                        div()
                            .text_color(theme.secondary_text)
                            .child(format!("{} · {} pt", settings.family, settings.size)),
                    )
            })
            .child(button(
                "settings-kitty-reload",
                "Volver a leer",
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| this.read_kitty(true, window, cx)),
            ))
    }
}

fn names_match(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn arrow_target(index: usize, columns: usize, len: usize, key: &str) -> Option<usize> {
    let next = match key {
        "left" if !index.is_multiple_of(columns) || columns == 1 => index.checked_sub(1)?,
        "right" if !(index + 1).is_multiple_of(columns) || columns == 1 => index + 1,
        "up" => index.checked_sub(columns)?,
        "down" => index + columns,
        _ => return None,
    };
    (next < len).then_some(next)
}

fn section(label: &'static str) -> Div {
    div()
        .flex_none()
        .w_full()
        .max_w(px(760.))
        .px_6()
        .pt_6()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
}

fn subtitle(label: &'static str) -> Div {
    div()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn card(id: impl Into<ElementId>, focus: &FocusHandle, theme: Theme) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .tab_index(0)
        .track_focus(focus)
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .rounded(px(12.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.panel)
        .focus(move |style| style.border_color(theme.accent).bg(theme.hover))
        .hover(move |style| style.bg(theme.hover))
        .cursor_pointer()
}

fn theme_action(
    id: &'static str,
    label: &'static str,
    name: IconName,
    theme: Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    button(id, "", Variant::Secondary, theme, on_click)
        .aria_label(label)
        .size(px(24.))
        .px_0()
        .child(icon(name).size(px(14.)).text_color(theme.text))
}

fn orb(colors: Colors, name: IconName, ring: Option<f32>, theme: Theme) -> Div {
    div()
        .relative()
        .size(px(58.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .when_some(ring, |orb, alpha| {
            orb.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .border_2()
                    .border_color(gpui::Hsla::from(theme.accent).opacity(alpha)),
            )
        })
        .child(
            div()
                .size(px(48.))
                .rounded_full()
                .border_1()
                .border_color(rgba(colors.border))
                .bg(linear_gradient(
                    135.,
                    linear_color_stop(rgba(colors.accent), 0.),
                    linear_color_stop(rgba(colors.background), 1.),
                )),
        )
        .when(ring.is_some(), |orb| {
            orb.child(
                div()
                    .absolute()
                    .bottom(px(-2.))
                    .right(px(-2.))
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgba(colors.raised))
                    .border_1()
                    .border_color(rgba(colors.border))
                    .child(icon(name).size(px(10.)).text_color(rgba(colors.text))),
            )
        })
}

fn miniature(colors: Colors) -> Div {
    let line = |width, color| {
        div()
            .w(px(width))
            .max_w_full()
            .h(px(4.))
            .rounded_full()
            .bg(rgba(color))
    };
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .bg(rgba(colors.background))
        .border_1()
        .border_color(rgba(colors.border))
        .child(
            div()
                .w(gpui::relative(0.24))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .bg(rgba(colors.sidebar))
                .child(line(32., colors.secondary_text))
                .child(line(24., colors.control_border)),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(20.))
                        .bg(rgba(colors.raised))
                        .border_b_1()
                        .border_color(rgba(colors.border))
                        .px_2()
                        .flex()
                        .items_center()
                        .child(line(28., colors.text)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .p_2()
                        .child(line(56., colors.text))
                        .child(line(38., colors.secondary_text))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .p_1()
                                .w(px(64.))
                                .max_w_full()
                                .rounded(px(6.))
                                .bg(rgba(colors.raised))
                                .border_1()
                                .border_color(rgba(colors.border))
                                .children([colors.success, colors.warning, colors.danger].map(
                                    |color| div().size(px(6.)).rounded_full().bg(rgba(color)),
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .px_1()
                                .h(px(14.))
                                .w(px(80.))
                                .max_w_full()
                                .rounded(px(6.))
                                .bg(rgba(colors.panel))
                                .border_1()
                                .border_color(rgba(colors.control_border))
                                .child(div().size(px(5.)).rounded_full().bg(rgba(colors.accent))),
                        ),
                ),
        )
}

#[cfg(target_os = "macos")]
fn is_application(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        && path.join("Contents/Info.plist").is_file()
}
