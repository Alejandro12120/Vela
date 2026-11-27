use gpui::{App, ClickEvent, Div, ElementId, Stateful, Window, div, prelude::*, px};

use crate::theme::Theme;

pub fn switch(
    id: impl Into<ElementId>,
    label: &'static str,
    checked: bool,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    switch_with_state(id, label, checked, theme, Some(on_click))
}

pub fn switch_with_state(
    id: impl Into<ElementId>,
    label: &'static str,
    checked: bool,
    theme: Theme,
    on_click: Option<impl Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Switch)
        .aria_label(label)
        .aria_toggled(if checked {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .flex()
        .items_center()
        .justify_between()
        .w(px(28.))
        .h(px(16.))
        .px(px(1.))
        .rounded_full()
        .border_1()
        .border_color(if checked {
            theme.accent
        } else {
            theme.control_border
        })
        .bg(if checked { theme.accent } else { theme.hover })
        .focus(move |style| style.border_color(theme.text))
        .when(checked, |track| track.justify_end())
        .child(div().size(px(12.)).rounded_full().bg(if checked {
            theme.raised
        } else {
            theme.secondary_text
        }))
        .when(on_click.is_none(), |track| {
            track.opacity(0.55).aria_description("No disponible")
        })
        .when_some(on_click, |track, on_click| {
            track.tab_index(0).cursor_pointer().on_click(on_click)
        })
}
