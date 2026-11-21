use gpui::{
    App, ClickEvent, Div, ElementId, FontWeight, Rgba, SharedString, Stateful, Window, div,
    prelude::*, px,
};

use crate::{
    icons::{IconName, icon},
    theme::Theme,
};

#[derive(Clone, Copy)]
pub enum Variant {
    Primary,
    Secondary,
    Selected,
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: Variant,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    shell(id, variant, theme, on_click).child(label.into())
}

/// A button holding only an icon. `aria_label` names it for assistive technology.
pub fn icon_button(
    id: impl Into<ElementId>,
    aria_label: impl Into<SharedString>,
    name: IconName,
    variant: Variant,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    shell(id, variant, theme, on_click)
        .aria_label(aria_label)
        .child(icon(name).text_color(text_color(variant, theme)))
}

/// A button with a leading icon and a text label. `tone` overrides the colour
/// of the icon, for example the danger colour.
pub fn icon_text_button(
    id: impl Into<ElementId>,
    name: IconName,
    label: impl Into<SharedString>,
    variant: Variant,
    tone: Option<Rgba>,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    shell(id, variant, theme, on_click)
        .gap(px(6.))
        .child(icon(name).text_color(tone.unwrap_or_else(|| text_color(variant, theme))))
        .child(label.into())
        .when_some(tone, |button, tone| button.text_color(tone))
}

fn text_color(variant: Variant, theme: Theme) -> Rgba {
    match variant {
        Variant::Primary => theme.primary_text,
        Variant::Secondary | Variant::Selected => theme.text,
    }
}

fn shell(
    id: impl Into<ElementId>,
    variant: Variant,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (bg, text, border, hover) = match variant {
        Variant::Primary => (
            theme.primary,
            theme.primary_text,
            theme.primary,
            theme.primary_hover,
        ),
        Variant::Secondary => (theme.panel, theme.text, theme.control_border, theme.hover),
        Variant::Selected => (theme.selection, theme.text, theme.accent, theme.selection),
    };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .tab_index(0)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(30.))
        .px_2()
        .rounded(px(6.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(text)
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(theme.selection).text_color(theme.text))
        .focus(move |style| style.border_color(theme.accent))
        .on_click(on_click)
}
