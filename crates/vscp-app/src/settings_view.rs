#[path = "glass_settings.rs"]
mod glass_settings;
#[path = "theme_editor.rs"]
mod theme_editor;

use std::{cell::Cell, collections::HashMap, io, path::Path, rc::Rc};

#[cfg(target_os = "macos")]
use std::path::PathBuf;

use gpui::{
    Bounds, Div, ElementId, FocusHandle, FontWeight, PathPromptOptions, Pixels, ScrollHandle,
    Stateful, canvas, linear_color_stop, linear_gradient, point, prelude::*, px, rgba,
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
    context_menu::Tooltip,
    icons::{IconName, icon},
};

struct ThemeCardFocus {
    card: FocusHandle,
    light: FocusHandle,
    dark: FocusHandle,
}

impl ThemeCardFocus {
    fn new(cx: &Context<Workspace>) -> Self {
        Self {
            card: cx.focus_handle().tab_stop(true),
            light: cx.focus_handle().tab_stop(true),
            dark: cx.focus_handle().tab_stop(true),
        }
    }
}

pub(super) struct SettingsPage {
    previous_focus: Option<FocusHandle>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    schemes: [FocusHandle; 3],
    themes: HashMap<String, ThemeCardFocus>,
    busy: bool,
    theme_error: Option<String>,
    editor: Option<theme_editor::ThemeEditor>,
    glass_drag: Option<glass_settings::GlassDrag>,
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
            editor: None,
            glass_drag: None,
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
        self.finish_glass_drag(window, cx);
        self.close_theme_editor(window, cx);
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
        if let Some(request) = request.as_deref() {
            self.dev_theme_editor(request, window, cx);
        }
        if request.as_deref() == Some("focus-moon") {
            self.settings_page
                .themes
                .entry("vscp".into())
                .or_insert_with(|| ThemeCardFocus::new(cx))
                .dark
                .focus(window, cx);
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
        if self.settings_page.busy || self.settings_page.glass_drag.is_some() {
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
                let glass_changed = cx.global_mut::<Appearance>().glass_preview.take().is_some();
                if glass_changed {
                    cx.refresh_windows();
                }
                match result {
                    Ok(settings) => {
                        cx.global_mut::<Appearance>()
                            .set_settings(settings.appearance.clone());
                        this.settings = settings;
                        Appearance::update_native_appearance(cx);
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
        if self.settings_page.editor.is_some() {
            return;
        }
        let mut next = self.settings.clone();
        next.appearance.scheme = scheme;
        self.save_settings(next, window, cx);
    }

    fn activate_theme(
        &mut self,
        id: &str,
        variant: Option<ColorVariant>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.settings.clone();
        if variant != Some(ColorVariant::Dark) {
            next.appearance.light_theme = id.into();
        }
        if variant != Some(ColorVariant::Light) {
            next.appearance.dark_theme = id.into();
        }
        self.save_settings(next, window, cx);
    }

    fn change_themes<R: Send + 'static>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(&Path) -> io::Result<R> + Send + 'static,
        on_success: impl FnOnce(&mut Self, R, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(dir) = self.dir.clone() else { return };
        if self.settings_page.busy || self.settings_page.glass_drag.is_some() {
            return;
        }
        self.settings_page.busy = true;
        self.settings_page.theme_error = None;
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let result = operation(&dir)?;
            Ok::<_, io::Error>((result, load_user_themes(&dir)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.settings_page.busy = false;
                match result {
                    Ok((result, (users, errors))) => {
                        cx.global_mut::<Appearance>().replace_users(users, errors);
                        on_success(this, result, window, cx);
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
                save_user_theme(dir, &copy)?;
                Ok(copy.id)
            },
            |this, id, window, cx| this.open_theme_editor(Some(&id), window, cx),
        );
    }

    pub(super) fn remove_theme(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let deleted = id.clone();
        self.finish_dialog(window, cx);
        self.change_themes(
            window,
            cx,
            move |dir| delete_user_theme(dir, &deleted),
            move |this, _, window, cx| {
                let mut next = this.settings.clone();
                for selection in [
                    &mut next.appearance.light_theme,
                    &mut next.appearance.dark_theme,
                ] {
                    if *selection == id {
                        *selection = "vscp".into();
                    }
                }
                if next.appearance != this.settings.appearance {
                    this.save_settings(next, window, cx);
                }
            },
        );
    }

    fn add_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.settings_page.busy || self.settings_page.editor.is_some() {
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
                                |_, _, _, _| {},
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
                .or_insert_with(|| ThemeCardFocus::new(cx));
        }
        let available = window.viewport_size().width.as_f32()
            - if self.sidebar_visible() {
                crate::sidebar::WIDTH
            } else {
                0.
            };
        let columns = if available < 608. { 2 } else { 3 };
        let light = appearance.selected(ColorVariant::Light);
        let dark = appearance.selected(ColorVariant::Dark);
        let colors = appearance
            .preview
            .as_ref()
            .map_or((light.light, dark.dark), |(draft, _)| {
                (draft.light, draft.dark)
            });
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
                            self.scheme_card(index, label, icon, scheme, colors, theme, cx)
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
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                icon_text_button(
                                    "create-theme",
                                    IconName::Palette,
                                    "Crear tema",
                                    Variant::Secondary,
                                    None,
                                    theme,
                                    cx.listener(|this, _, window, cx| {
                                        this.open_theme_editor(None, window, cx)
                                    }),
                                )
                                .h(px(26.)),
                            )
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
                                    div().flex_1().min_w_0().child(self.theme_card(
                                        row * columns + col,
                                        columns,
                                        def,
                                        (light.id == def.id, dark.id == def.id),
                                        theme,
                                        cx,
                                    ))
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
            .relative()
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
                    .when(theme.glassy, |header| {
                        header.bg(theme.chrome(theme.background))
                    })
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
                    .when(theme.glassy, |scroll| {
                        scroll.bg(theme.chrome(theme.background))
                    })
                    .child(appearance_section)
                    .child(self.render_interface(theme, cx))
                    .child(self.editor_section(theme, cx))
                    .child(self.terminal_section(theme, cx).pb_6()),
            )
            .children(self.render_theme_editor(theme, window, cx))
            .children(self.render_picker(theme, cx))
    }

    #[allow(clippy::too_many_arguments)]
    fn scheme_card(
        &self,
        index: usize,
        label: &'static str,
        name: IconName,
        scheme: Scheme,
        colors: (Colors, Colors),
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
            .when(scheme != Scheme::Dark, |row| row.child(miniature(colors.0)))
            .when(scheme != Scheme::Light, |row| {
                row.child(miniature(colors.1))
            });
        card(("scheme", index), focus, theme)
            .flex_1()
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
        selected: (bool, bool),
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let theme_focus = &self.settings_page.themes[&def.id];
        let variant_button = |variant: ColorVariant| {
            let (id, name, mode, focus, selected) = match variant {
                ColorVariant::Light => (
                    "sun",
                    IconName::Sun,
                    "claro",
                    &theme_focus.light,
                    selected.0,
                ),
                ColorVariant::Dark => (
                    "moon",
                    IconName::Moon,
                    "oscuro",
                    &theme_focus.dark,
                    selected.1,
                ),
            };
            let ring = selected.then_some(match (self.settings.appearance.scheme, variant) {
                (Scheme::Light, ColorVariant::Dark) | (Scheme::Dark, ColorVariant::Light) => 0.4,
                _ => 1.,
            });
            let label = format!("Usar «{}» en modo {mode}", def.name);
            let accessible_label = if selected {
                format!("{label} (seleccionado)")
            } else {
                label.clone()
            };
            let tooltip = gpui::SharedString::from(label);
            let activate = def.id.clone();
            orb(id, *variant.colors(def), name, ring, theme)
                .role(gpui::Role::Button)
                .aria_label(accessible_label)
                .track_focus(focus)
                .tooltip(move |_, cx| cx.new(|_| Tooltip(tooltip.clone())).into())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if this.settings_page.editor.is_none() {
                        this.activate_theme(&activate, Some(variant), window, cx);
                    }
                }))
        };
        let activate = def.id.clone();
        let duplicate = def.id.clone();
        let delete = def.id.clone();
        let edit = def.id.clone();
        let focus = theme_focus.card.clone();
        let key_focus = focus.clone();
        let bounds = Rc::new(Cell::new(None));
        let layout = bounds.clone();
        card(&focus, &focus, theme)
            .w_full()
            .relative()
            .child(
                canvas(move |rect, _, _| layout.set(Some(rect)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .aria_label(def.name.clone())
            .aria_selected(selected.0 || selected.1)
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
                    .child(variant_button(ColorVariant::Light))
                    .child(variant_button(ColorVariant::Dark)),
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
                            .when(!def.builtin, |row| {
                                row.child(theme_action(
                                    "edit",
                                    "Editar",
                                    IconName::Pencil,
                                    theme,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_theme_editor(Some(&edit), window, cx);
                                    }),
                                ))
                            })
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
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.settings_page.editor.is_none() {
                    this.activate_theme(&activate, None, window, cx);
                }
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if !key_focus.is_focused(window) {
                        return;
                    }
                    let themes = &cx.global::<Appearance>().themes;
                    if let Some(next) =
                        arrow_target(index, columns, themes.len(), &event.keystroke.key)
                    {
                        this.settings_page.themes[&themes[next].id]
                            .card
                            .focus(window, cx);
                        if let Some(rect) = bounds.get() {
                            let rows = (next / columns) as f32 - (index / columns) as f32;
                            reveal_theme_card(&this.settings_page.scroll, rect, rows);
                            cx.notify();
                        }
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

fn reveal_theme_card(scroll: &ScrollHandle, rect: Bounds<Pixels>, rows: f32) {
    let viewport = scroll.bounds();
    let distance = rows * (rect.size.height + px(12.));
    let top = rect.top() + distance;
    let bottom = rect.bottom() + distance;
    let shift = if top < viewport.top() {
        viewport.top() - top
    } else if bottom > viewport.bottom() {
        viewport.bottom() - bottom
    } else {
        return;
    };
    let offset = scroll.offset();
    scroll.set_offset(point(offset.x, (offset.y + shift).min(px(0.))));
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
        .min_w_0()
        .rounded(px(12.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.content(theme.panel))
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

fn orb(
    id: &'static str,
    colors: Colors,
    name: IconName,
    ring: Option<f32>,
    theme: Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .size(px(58.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(gpui::transparent_black())
        .hover(move |style| style.bg(gpui::Hsla::from(theme.accent).opacity(0.10)))
        .focus(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .when_some(ring, |orb, alpha| {
            orb.child(
                div()
                    .absolute()
                    .inset(px(1.))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn press(cx: &mut gpui::VisualTestContext, key: &'static str) {
        cx.simulate_keystrokes(key);
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn keyboard_selects_appearance_cards_and_restores_focus(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(workspace.settings_open());
            workspace.settings_page.schemes[0].focus(window, cx);
        });
        cx.simulate_keystrokes("right");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(workspace.settings_page.schemes[1].is_focused(window));
        });
        press(cx, "enter");
        assert_eq!(
            Settings::load(dir.path()).unwrap().appearance.scheme,
            Scheme::Light
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.settings_page.themes["vscp"]
                .card
                .focus(window, cx);
        });
        cx.simulate_keystrokes("right");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["frambuesa"]
                    .card
                    .is_focused(window)
            );
        });
        press(cx, "space");
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "frambuesa");
        assert_eq!(settings.dark_theme, "frambuesa");
        cx.simulate_keystrokes("escape");
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(!workspace.settings_open());
            assert_eq!(window.focused(cx), Some(workspace.focus_handle()));
        });
    }

    #[gpui::test]
    fn keyboard_selects_light_and_dark_orbs_independently(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.appearance.scheme = Scheme::Dark;
        settings.save(dir.path()).unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.settings_page.themes["oceano"]
                .card
                .focus(window, cx);
        });
        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["oceano"]
                    .light
                    .is_focused(window)
            );
        });
        press(cx, "enter");
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "oceano");
        assert_eq!(settings.dark_theme, "vscp");
        assert_eq!(settings.scheme, Scheme::Dark);

        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["oceano"]
                    .dark
                    .is_focused(window)
            );
        });
        press(cx, "tab");
        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["ambar"]
                    .card
                    .is_focused(window)
            );
        });
        press(cx, "tab");
        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["ambar"]
                    .dark
                    .is_focused(window)
            );
        });
        press(cx, "space");
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "oceano");
        assert_eq!(settings.dark_theme, "ambar");
        assert_eq!(settings.scheme, Scheme::Dark);
    }

    #[gpui::test]
    fn deleting_the_dark_theme_preserves_the_light_choice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut theme = vscp_core::appearance::builtin_themes().remove(0);
        theme.id = "noche".into();
        theme.name = "Noche".into();
        theme.builtin = false;
        save_user_theme(dir.path(), &theme).unwrap();
        let mut settings = Settings::default();
        settings.appearance.light_theme = "oceano".into();
        settings.appearance.dark_theme = theme.id.clone();
        settings.save(dir.path()).unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.remove_theme(theme.id, window, cx);
        });
        cx.run_until_parked();
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "oceano");
        assert_eq!(settings.dark_theme, "vscp");
        assert!(load_user_themes(dir.path()).0.is_empty());
    }
}
