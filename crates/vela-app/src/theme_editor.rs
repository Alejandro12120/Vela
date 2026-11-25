#[path = "theme_picker.rs"]
mod theme_picker;
#[path = "theme_tokens.rs"]
mod theme_tokens;

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, App, AppContext, Bounds, BoxShadow, Entity, Focusable, Pixels, ScrollHandle,
    Subscription, canvas, checkerboard, point,
};
use unicode_segmentation::UnicodeSegmentation;
use vela_core::appearance::{contrast, export_theme, hex, parse_hex};

use self::{
    theme_picker::Picker,
    theme_tokens::{BASIC, TOKENS, Token},
};
use super::*;
use crate::{
    components::{modal, switch::switch, text_input::TextInput},
    terminal::colors::Palette,
};

pub(super) struct ThemeEditor {
    draft: ThemeDef,
    creating: bool,
    variant: ColorVariant,
    name: Entity<TextInput>,
    filter: Entity<TextInput>,
    advanced: bool,
    rows: HashMap<Token, ColorRow>,
    focus: FocusHandle,
    last_focus: FocusHandle,
    return_focus: FocusHandle,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
    picker: Option<Picker>,
    error: Option<String>,
}

struct ColorRow {
    input: Entity<TextInput>,
    swatch: FocusHandle,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl ThemeEditor {
    fn colors(&self) -> &Colors {
        self.variant.colors(&self.draft)
    }

    fn colors_mut(&mut self) -> &mut Colors {
        match self.variant {
            ColorVariant::Light => &mut self.draft.light,
            ColorVariant::Dark => &mut self.draft.dark,
        }
    }

    fn title(&self) -> &'static str {
        if self.creating {
            "Crear tema"
        } else {
            "Editar tema"
        }
    }

    fn name_error(&self, appearance: &Appearance, cx: &App) -> Option<&'static str> {
        let name = self.name.read(cx).value().trim();
        if name.is_empty() {
            Some("Escribe un nombre.")
        } else if appearance.themes.iter().any(|theme| {
            (self.creating || theme.id != self.draft.id) && names_match(&theme.name, name)
        }) {
            Some("Ya existe un tema con ese nombre.")
        } else {
            None
        }
    }

    fn editable_theme(&self, appearance: &Appearance, cx: &App) -> Option<ThemeDef> {
        if self.name_error(appearance, cx).is_some() {
            return None;
        }
        let mut theme = self.draft.clone();
        theme.name = self.name.read(cx).value().trim().into();
        Some(theme)
    }
}

impl Workspace {
    pub(super) fn open_theme_editor(
        &mut self,
        id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings_open() || self.settings_page.busy || self.settings_page.editor.is_some() {
            return;
        }
        self.settings_page.theme_error = None;
        let appearance = cx.global::<Appearance>();
        let variant = appearance.variant_for(window);
        let creating = id.is_none();
        let draft = if let Some(id) = id {
            let Some(theme) = appearance
                .themes
                .iter()
                .find(|theme| theme.id == id && !theme.builtin)
            else {
                return;
            };
            theme.clone()
        } else {
            let mut theme = appearance.selected(variant).clone();
            theme.light = appearance.selected(ColorVariant::Light).light;
            theme.dark = appearance.selected(ColorVariant::Dark).dark;
            theme.id.clear();
            theme.name.clear();
            theme.builtin = false;
            theme
        };
        let name = cx.new(|cx| TextInput::new(&draft.name, "Nombre", "p. ej. Aurora", false, cx));
        let filter = cx.new(|cx| {
            TextInput::new("", "Filtrar colores", "Filtrar colores", false, cx).compact()
        });
        let return_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.settings_page.focus.clone());
        let mut subscriptions = vec![
            cx.observe(&name, |_, _, cx| cx.notify()),
            cx.observe(&filter, |_, _, cx| cx.notify()),
        ];
        let rows = TOKENS
            .iter()
            .map(|&token| {
                let hex = current_hex(token, variant.colors(&draft));
                let input = color_input(token, &hex, cx);
                subscriptions.push(cx.observe_in(
                    &input,
                    window,
                    move |this, input, window, cx| {
                        this.edit_hex(token, &input, window, cx);
                    },
                ));
                (
                    token,
                    ColorRow {
                        input,
                        swatch: cx.focus_handle(),
                        bounds: Rc::default(),
                    },
                )
            })
            .collect();
        name.read(cx).focus_handle(cx).focus(window, cx);
        self.settings_page.editor = Some(ThemeEditor {
            draft,
            creating,
            variant,
            name,
            filter,
            advanced: false,
            rows,
            focus: cx.focus_handle(),
            last_focus: cx.focus_handle(),
            return_focus,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
            picker: None,
            error: None,
        });
        self.update_preview(cx);
    }

    pub(super) fn dev_theme_editor(
        &mut self,
        request: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match request.split_once(':') {
            Some(("editor", id)) => self.open_theme_editor(Some(id), window, cx),
            Some(("picker", key)) => {
                self.open_theme_editor(None, window, cx);
                self.dev_open_picker(key, window, cx);
            }
            Some(("create", "terminal")) => {
                self.open_theme_editor(None, window, cx);
                if let Some(editor) = &mut self.settings_page.editor {
                    let groups_before_terminal = 5;
                    let index = 1
                        + groups_before_terminal
                        + TOKENS.iter().take_while(|token| !token.terminal()).count();
                    editor.advanced = true;
                    editor.scroll.scroll_to_top_of_item(index.saturating_sub(3));
                    cx.notify();
                }
            }
            Some(("create", "contrast")) => {
                self.open_theme_editor(None, window, cx);
                self.set_variant(ColorVariant::Light, window, cx);
                if let Some(editor) = &self.settings_page.editor {
                    editor.rows[&Token::SecondaryText]
                        .input
                        .update(cx, |input, cx| input.set_value("#888888", window, cx));
                }
            }
            Some(("create", "dark")) => {
                self.open_theme_editor(None, window, cx);
                self.set_variant(ColorVariant::Dark, window, cx);
            }
            None if request == "create" => self.open_theme_editor(None, window, cx),
            _ => {}
        }
    }

    pub(super) fn update_preview(&self, cx: &mut Context<Self>) {
        let preview = self
            .settings_page
            .editor
            .as_ref()
            .map(|editor| (editor.draft.clone(), editor.variant));
        let changed_variant = cx
            .global::<Appearance>()
            .preview
            .as_ref()
            .map(|(_, variant)| *variant)
            != preview.as_ref().map(|(_, variant)| *variant);
        cx.global_mut::<Appearance>().set_preview(preview);
        if changed_variant {
            Appearance::update_native_appearance(cx);
        }
        cx.refresh_windows();
        cx.notify();
    }

    pub(super) fn close_theme_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = self.settings_page.editor.take() else {
            return false;
        };
        editor.return_focus.focus(window, cx);
        self.update_preview(cx);
        true
    }

    pub(in crate::workspace) fn dismiss_theme_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.close_picker(window, cx) {
            return true;
        }
        self.close_theme_editor(window, cx)
    }

    fn set_variant(&mut self, variant: ColorVariant, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_page.editor.as_mut() else {
            return;
        };
        editor.variant = variant;
        editor.picker = None;
        Self::sync_inputs(editor, TOKENS, window, cx);
        self.update_preview(cx);
    }

    fn sync_inputs(
        editor: &ThemeEditor,
        tokens: &[Token],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for &token in tokens {
            let value = current_hex(token, editor.colors());
            editor.rows[&token].input.update(cx, |input, cx| {
                input.set_invalid(false, cx);
                if input.value() != value {
                    input.set_value(&value, window, cx);
                }
            });
        }
    }

    fn edit_hex(
        &mut self,
        token: Token,
        source: &Entity<TextInput>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.settings_page.editor.as_mut() else {
            return;
        };
        let source_input = source.read(cx);
        if source_input.is_composing() {
            return;
        }
        let text = source_input.value();
        let value = parse_hex(text);
        let valid = value.is_some() || (token.terminal() && text.is_empty());
        source.update(cx, |input, cx| input.set_invalid(!valid, cx));
        if !valid || token.get(editor.colors()) == value {
            return;
        }
        token.set(editor.colors_mut(), value);
        let picker = editor
            .picker
            .as_mut()
            .filter(|picker| picker.token == token);
        if let Some(picker) = picker {
            picker.update_color(value.unwrap_or(picker.fallback));
            let other = if source == &picker.hex {
                &editor.rows[&token].input
            } else {
                &picker.hex
            };
            let value = value.map(hex).unwrap_or_default();
            other.update(cx, |input, cx| {
                input.set_invalid(false, cx);
                input.set_value(&value, window, cx);
            });
        }
        self.update_preview(cx);
    }

    fn import_kitty(&mut self, use_kitty: bool, window: &mut Window, cx: &mut Context<Self>) {
        let palette = self
            .editor_kitty(cx)
            .filter(|kitty| kitty.path.is_some())
            .map(|kitty| kitty.palette);
        let Some(editor) = self.settings_page.editor.as_mut() else {
            return;
        };
        if use_kitty && palette.is_none() {
            return;
        }
        editor.colors_mut().terminal = if use_kitty {
            palette.unwrap_or_default().as_theme()
        } else {
            Default::default()
        };
        editor.picker = None;
        let first_terminal = TOKENS.iter().position(|token| token.terminal()).unwrap();
        Self::sync_inputs(editor, &TOKENS[first_terminal..], window, cx);
        self.update_preview(cx);
    }

    fn editor_kitty<'a>(&'a self, cx: &'a App) -> Option<&'a crate::terminal::kitty::Kitty> {
        self.kitty_settings
            .as_ref()
            .or_else(|| self.active_terminal_settings(cx))
    }

    fn save_theme_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_page.editor.as_ref() else {
            return;
        };
        let Some(mut draft) = editor.editable_theme(cx.global::<Appearance>(), cx) else {
            return;
        };
        let creating = editor.creating;
        self.change_themes(
            window,
            cx,
            move |dir| {
                if creating {
                    draft.id = unused_theme_id(dir, &draft.name)?;
                }
                save_user_theme(dir, &draft)?;
                Ok(draft.id)
            },
            |this, id, window, cx| {
                this.activate_theme(&id, None, window, cx);
                this.close_theme_editor(window, cx);
            },
        );
    }

    fn export_theme_draft(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_page.editor.as_ref() else {
            return;
        };
        let Some(draft) = editor.editable_theme(cx.global::<Appearance>(), cx) else {
            return;
        };
        let Some(dir) = self.dir.as_ref() else { return };
        let id = if editor.creating {
            vela_core::appearance::new_theme_id(
                &draft.name,
                cx.global::<Appearance>()
                    .themes
                    .iter()
                    .map(|theme| theme.id.as_str()),
            )
        } else {
            draft.id.clone()
        };
        let path = cx.prompt_for_new_path(dir, Some(&format!("{id}.toml")));
        cx.spawn_in(window, async move |this, cx| {
            let result = match path.await {
                Ok(Ok(Some(path))) => {
                    let task = cx
                        .background_executor()
                        .spawn(async move { export_theme(&draft, &path) });
                    task.await.map_err(|error| error.to_string())
                }
                Ok(Ok(None)) => return,
                _ => Err("No se pudo abrir el selector de archivos.".into()),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                if let Some(editor) = &mut this.settings_page.editor {
                    editor.error = result.err();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn render_theme_editor(
        &self,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        let editor = self.settings_page.editor.as_ref()?;
        let title = editor.title();
        let name_error = editor.name_error(cx.global::<Appearance>(), cx);
        let valid = name_error.is_none() && !self.settings_page.busy;
        let filter = search_key(editor.filter.read(cx).value().trim());
        let kitty = self.editor_kitty(cx);
        let palette = kitty.map_or_else(Palette::default, |kitty| kitty.palette);
        let defaults = palette.with_theme(editor.colors().terminal).as_theme();
        let mut group = "";
        let mut color_list: Vec<AnyElement> = Vec::new();
        let tokens = if editor.advanced { TOKENS } else { BASIC };
        for &token in tokens {
            if !search_key(token.label()).contains(&filter) {
                continue;
            }
            if editor.advanced && token.group() != group {
                group = token.group();
                color_list.push(
                    div()
                        .flex_none()
                        .mx_3()
                        .pt_4()
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .font_weight(FontWeight::MEDIUM)
                        .child(group)
                        .into_any_element(),
                );
                if token.terminal() {
                    color_list.push(
                        self.kitty_actions(
                            kitty.is_some_and(|kitty| kitty.path.is_some()),
                            theme,
                            cx,
                        )
                        .px_3()
                        .flex_none()
                        .into_any_element(),
                    );
                }
            }
            color_list.push(
                div()
                    .px_3()
                    .flex_none()
                    .child(self.render_color_row(editor, token, &defaults, theme, cx))
                    .into_any_element(),
            );
        }
        let sidebar = if self.sidebar_visible() {
            crate::sidebar::WIDTH
        } else {
            0.
        };
        let width = (window.viewport_size().width - px(sidebar + 24.)).min(px(420.));
        let focus = editor.focus.clone();
        let last = editor.last_focus.clone();
        Some(
            div()
                .id("theme-editor")
                .role(gpui::Role::Dialog)
                .aria_label(title)
                .track_focus(&focus)
                .tab_group()
                .tab_stop(false)
                .key_context("ThemeEditor")
                .on_action(cx.listener(move |this, _: &modal::Next, window, cx| {
                    this.theme_editor_tab(false, window, cx);
                }))
                .on_action(cx.listener(move |this, _: &modal::Previous, window, cx| {
                    this.theme_editor_tab(true, window, cx);
                }))
                .absolute()
                .right_3()
                .top_3()
                .bottom_3()
                .w(width)
                .flex()
                .flex_col()
                .occlude()
                .rounded(px(12.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.raised)
                .shadow(vec![BoxShadow {
                    color: theme.overlay,
                    inset: false,
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(16.),
                    spread_radius: px(0.),
                }])
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .flex_none()
                        .p_3()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme.secondary_text)
                                        .child("Elige un color"),
                                ),
                        )
                        .child(
                            icon_button(
                                "editor-close",
                                "Cerrar",
                                IconName::Close,
                                Variant::Secondary,
                                theme,
                                cx.listener(|this, _, window, cx| {
                                    this.close_theme_editor(window, cx);
                                }),
                            )
                            .size(px(26.))
                            .px_0(),
                        ),
                )
                .child(
                    div()
                        .id("theme-editor-scroll")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&editor.scroll)
                        .child(
                            div()
                                .flex_none()
                                .px_3()
                                .pt_3()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(editor_row(
                                    "Nombre",
                                    div().w_full().child(editor.name.clone()),
                                ))
                                .when_some(
                                    name_error.filter(|_| !editor.name.read(cx).value().is_empty()),
                                    |form, message| {
                                        form.child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(theme.danger)
                                                .child(message),
                                        )
                                    },
                                )
                                .child(editor_row(
                                    "Apariencia",
                                    self.variant_switch(editor, theme, cx),
                                ))
                                .child(
                                    div()
                                        .pt_2()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Colores"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .min_w_0()
                                                .flex_1()
                                                .child(
                                                    icon(IconName::Search)
                                                        .size(px(14.))
                                                        .text_color(theme.secondary_text),
                                                )
                                                .child(editor.filter.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .flex_none()
                                                .child("Avanzado")
                                                .child(switch(
                                                    "advanced",
                                                    "Avanzado",
                                                    editor.advanced,
                                                    theme,
                                                    cx.listener(|this, _, window, cx| {
                                                        this.close_picker(window, cx);
                                                        if let Some(editor) =
                                                            &mut this.settings_page.editor
                                                        {
                                                            editor.advanced = !editor.advanced;
                                                            cx.notify();
                                                        }
                                                    }),
                                                )),
                                        ),
                                ),
                        )
                        .children(color_list)
                        .child(div().h_3().flex_none()),
                )
                .when_some(
                    editor
                        .error
                        .as_ref()
                        .or(self.settings_page.theme_error.as_ref()),
                    |panel, error| {
                        panel.child(
                            div()
                                .px_3()
                                .text_size(px(12.))
                                .text_color(theme.danger)
                                .child(error.clone()),
                        )
                    },
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_between()
                        .gap_1()
                        .p_3()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            button(
                                "theme-export",
                                "Exportar…",
                                Variant::Secondary,
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    if valid {
                                        this.export_theme_draft(window, cx)
                                    }
                                }),
                            )
                            .when(!valid, |button| button.tab_stop(false).opacity(0.4)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    button(
                                        "theme-cancel",
                                        "Cancelar",
                                        Variant::Secondary,
                                        theme,
                                        cx.listener(|this, _, window, cx| {
                                            this.close_theme_editor(window, cx);
                                        }),
                                    )
                                    .when(!valid, |button| button.track_focus(&last)),
                                )
                                .child(
                                    icon_text_button(
                                        "theme-save",
                                        IconName::Palette,
                                        if editor.creating {
                                            "Crear tema"
                                        } else {
                                            "Guardar"
                                        },
                                        Variant::Primary,
                                        None,
                                        theme,
                                        cx.listener(move |this, _, window, cx| {
                                            if valid {
                                                this.save_theme_draft(window, cx)
                                            }
                                        }),
                                    )
                                    .when(valid, |button| button.track_focus(&last))
                                    .when(!valid, |button| button.tab_stop(false).opacity(0.4)),
                                ),
                        ),
                ),
        )
    }

    pub(in crate::workspace) fn theme_editor_tab(
        &self,
        backwards: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = self.settings_page.editor.as_ref() else {
            return false;
        };
        let focus = editor
            .picker
            .as_ref()
            .map_or(&editor.focus, |picker| &picker.scope);
        let last = editor
            .picker
            .as_ref()
            .map_or(&editor.last_focus, |picker| &picker.done);
        if backwards {
            window.focus_prev(cx);
            if !focus.contains_focused(window, cx) {
                last.focus(window, cx);
            }
        } else {
            window.focus_next(cx);
            if !focus.contains_focused(window, cx) {
                focus.focus(window, cx);
                window.focus_next(cx);
            }
        }
        cx.stop_propagation();
        true
    }

    fn variant_switch(&self, editor: &ThemeEditor, theme: Theme, cx: &Context<Self>) -> Div {
        div().flex().gap_1().children(
            [
                (ColorVariant::Light, "Claro"),
                (ColorVariant::Dark, "Oscuro"),
            ]
            .map(|(variant, label)| {
                button(
                    label,
                    label,
                    if editor.variant == variant {
                        Variant::Selected
                    } else {
                        Variant::Secondary
                    },
                    theme,
                    cx.listener(move |this, _, window, cx| this.set_variant(variant, window, cx)),
                )
                .h(px(24.))
            }),
        )
    }

    fn kitty_actions(&self, found: bool, theme: Theme, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                button(
                    "clear-kitty",
                    "Usar colores de kitty.conf",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.import_kitty(false, window, cx)),
                )
                .h(px(24.))
                .text_size(px(12.)),
            )
            .child(
                button(
                    "import-kitty",
                    "Importar de kitty.conf",
                    Variant::Secondary,
                    theme,
                    cx.listener(move |this, _, window, cx| {
                        if found {
                            this.import_kitty(true, window, cx)
                        }
                    }),
                )
                .h(px(24.))
                .text_size(px(12.))
                .when(!found, |button| button.tab_stop(false).opacity(0.4)),
            )
            .when(!found, |row| {
                row.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .child("No se encontró kitty.conf"),
                )
            })
    }

    fn render_color_row(
        &self,
        editor: &ThemeEditor,
        token: Token,
        defaults: &vela_core::appearance::TerminalColors,
        theme: Theme,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let row = &editor.rows[&token];
        let color = token.get(editor.colors());
        let fallback = fallback_color(token, defaults);
        let display = color.unwrap_or(fallback);
        let bounds = row.bounds.clone();
        let swatch = div()
            .id(&row.swatch)
            .track_focus(&row.swatch)
            .role(gpui::Role::Button)
            .aria_label(token.label())
            .tab_index(0)
            .relative()
            .size(px(20.))
            .flex_none()
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(if color.is_none() {
                theme.secondary_text
            } else {
                theme.border
            })
            .focus(move |style| style.border_color(theme.accent))
            .cursor_pointer()
            .when_else(
                display & 255 != 255,
                |swatch| {
                    swatch
                        .bg(checkerboard(theme.secondary_text, 4.))
                        .child(div().absolute().inset_0().rounded_full().bg(rgba(display)))
                },
                |swatch| swatch.bg(rgba(display)),
            )
            .child(
                canvas(move |rect, _, _| bounds.set(Some(rect)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_picker(token, window, cx)));
        div()
            .flex_none()
            .flex()
            .justify_between()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .min_w_0()
                    .items_center()
                    .child(div().child(token.label()))
                    .when_some(
                        contrast_warning(token, editor.colors(), defaults),
                        |row, warning| {
                            row.child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_1()
                                    .text_color(theme.warning)
                                    .text_size(px(12.))
                                    .child(
                                        icon(IconName::Warning)
                                            .size(px(12.))
                                            .text_color(theme.warning),
                                    )
                                    .child(warning),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(swatch)
                    .child(div().w(px(96.)).child(row.input.clone())),
            )
    }
}

fn editor_row(label: &'static str, control: Div) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().w(px(132.)).flex_none().child(label))
        .child(div().min_w_0().flex_1().child(control))
}

fn color_input(token: Token, value: &str, cx: &mut Context<Workspace>) -> Entity<TextInput> {
    cx.new(|cx| {
        TextInput::new(
            value,
            token.label(),
            if token.terminal() { "kitty" } else { "#rrggbb" },
            false,
            cx,
        )
        .compact()
        .monospace()
    })
}

fn current_hex(token: Token, colors: &Colors) -> String {
    token.get(colors).map(hex).unwrap_or_default()
}

fn fallback_color(token: Token, defaults: &vela_core::appearance::TerminalColors) -> u32 {
    let mut colors = vela_core::appearance::VELA_LIGHT;
    colors.terminal = *defaults;
    token.get(&colors).unwrap_or_else(|| match token {
        Token::Cursor | Token::SelectionText => defaults.foreground.unwrap_or(0xffffffff),
        _ => defaults.background.unwrap_or(0x000000ff),
    })
}

fn search_key(text: &str) -> String {
    // The editor labels are Spanish. Drop combining marks and normalize the
    // precomposed vowels so typing either form gives the same results.
    text.graphemes(true)
        .flat_map(|part| {
            part.to_lowercase().chars().next().map(|c| match c {
                'á' => 'a',
                'é' => 'e',
                'í' => 'i',
                'ó' => 'o',
                'ú' | 'ü' => 'u',
                'ñ' => 'n',
                _ => c,
            })
        })
        .collect()
}

fn contrast_warning(
    token: Token,
    colors: &Colors,
    effective: &vela_core::appearance::TerminalColors,
) -> Option<String> {
    let ratio = match token {
        Token::Text | Token::SecondaryText => [colors.background, colors.sidebar, colors.panel]
            .into_iter()
            .map(|surface| contrast(token.get(colors).unwrap(), surface))
            .fold(f32::INFINITY, f32::min),
        Token::PrimaryText => contrast(colors.primary_text, colors.primary),
        Token::TerminalText => contrast(
            fallback_color(Token::TerminalText, effective),
            fallback_color(Token::TerminalBackground, effective),
        ),
        _ => return None,
    };
    (ratio < 4.5).then(|| format_ratio(ratio))
}

fn format_ratio(ratio: f32) -> String {
    format!("{ratio:.1}:1").replace('.', ",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn workspace<'a>(
        dir: &Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workspace>, &'a mut VisualTestContext) {
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.to_path_buf()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        (workspace, cx)
    }

    #[gpui::test]
    fn creating_a_theme_saves_activates_and_keeps_the_editor_preference(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            editor: Some("/Applications/Editor.app".into()),
            ..Default::default()
        };
        settings.save(dir.path()).unwrap();
        let (workspace, cx) = workspace(dir.path(), cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_settings(window, cx);
            workspace.open_theme_editor(None, window, cx);
            let editor = workspace.settings_page.editor.as_ref().unwrap();
            editor
                .name
                .update(cx, |input, cx| input.set_value("Aurora", window, cx));
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.save_theme_draft(window, cx)
        });
        cx.run_until_parked();
        let settings = Settings::load(dir.path()).unwrap();
        assert_eq!(settings.appearance.light_theme, "aurora");
        assert_eq!(settings.appearance.dark_theme, "aurora");
        assert_eq!(settings.editor, Some("/Applications/Editor.app".into()));
        let (themes, errors) = load_user_themes(dir.path());
        assert!(errors.is_empty());
        assert_eq!(themes[0].name, "Aurora");
        workspace.read_with(cx, |workspace, cx| {
            assert!(workspace.settings_page.editor.is_none());
            assert!(cx.global::<Appearance>().preview.is_none());
            let appearance = cx.global::<Appearance>();
            assert_eq!(appearance.selected(ColorVariant::Light).id, "aurora");
            assert_eq!(appearance.selected(ColorVariant::Dark).id, "aurora");
        });
    }

    #[gpui::test]
    fn live_preview_ignores_scheme_and_cancelling_restores_it(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        Settings {
            appearance: vela_core::appearance::AppearanceSettings {
                scheme: Scheme::Light,
                ..Default::default()
            },
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let (workspace, cx) = workspace(dir.path(), cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_settings(window, cx);
            let active = Theme::current(window, cx);
            workspace.open_theme_editor(None, window, cx);
            workspace.set_variant(ColorVariant::Dark, window, cx);
            assert_ne!(Theme::current(window, cx).background, active.background);
            assert!(workspace.close_theme_editor(window, cx));
            assert_eq!(Theme::current(window, cx).background, active.background);
        });
        assert!(!dir.path().join("themes").exists());
    }

    #[gpui::test]
    fn invalid_hex_keeps_the_last_valid_color_and_duplicate_names_are_rejected(
        cx: &mut TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = workspace(dir.path(), cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_settings(window, cx);
            workspace.open_theme_editor(None, window, cx);
            let editor = workspace.settings_page.editor.as_ref().unwrap();
            editor
                .name
                .update(cx, |input, cx| input.set_value("frAMbueSa", window, cx));
            editor.rows[&Token::Accent]
                .input
                .update(cx, |input, cx| input.set_value("#135", window, cx));
            assert!(editor.name_error(cx.global::<Appearance>(), cx).is_some());
        });
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = workspace.settings_page.editor.as_ref().unwrap();
            assert_eq!(editor.colors().accent, 0x113355ff);
            editor.rows[&Token::Accent]
                .input
                .update(cx, |input, cx| input.set_value("#w", window, cx));
            workspace.save_theme_draft(window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, _| {
            let editor = workspace.settings_page.editor.as_ref().unwrap();
            assert_eq!(editor.colors().accent, 0x113355ff);
        });
        assert!(!dir.path().join("themes").exists());
    }

    #[test]
    fn formats_contrast_and_filters_spanish_labels() {
        assert_eq!(format_ratio(3.2), "3,2:1");
        assert_eq!(search_key("SELECCIÓN"), "seleccion");
        assert_eq!(search_key("seleccio\u{301}n"), "seleccion");
    }
}
