use gpui::{App, ClickEvent, Div, ElementId, Rgba, Stateful, Window, div, prelude::*, px};

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
        .child(
            div()
                .size(px(12.))
                .rounded_full()
                .bg(thumb_color(checked, theme)),
        )
        .when(on_click.is_none(), |track| {
            track.opacity(0.55).aria_description("No disponible")
        })
        .when_some(on_click, |track, on_click| {
            track.tab_index(0).cursor_pointer().on_click(on_click)
        })
}

fn thumb_color(checked: bool, theme: Theme) -> Rgba {
    if checked {
        theme.raised
    } else {
        theme.secondary_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vela_core::appearance::{builtin_themes, contrast};

    #[test]
    fn off_thumb_contrasts_with_settings_background_and_track() {
        for definition in builtin_themes() {
            for (variant, colors) in [("light", definition.light), ("dark", definition.dark)] {
                let thumb = thumb_color(false, Theme::from_colors(&colors)).into();
                for (surface, background) in [("row", colors.sidebar), ("track", colors.hover)] {
                    let ratio = contrast(thumb, background);
                    assert!(
                        ratio >= 3.,
                        "{} {variant} {surface}: {ratio:.2}:1",
                        definition.name
                    );
                }
            }
        }
    }
}
