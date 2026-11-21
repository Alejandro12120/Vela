use gpui::{
    App, ClickEvent, Div, ElementId, FontWeight, SharedString, Stateful, Window, div, prelude::*,
    px,
};

use crate::theme::Theme;

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
        .child(label.into())
        .on_click(on_click)
}
