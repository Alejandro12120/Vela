use gpui::{Div, FontWeight, div, prelude::*, px};

use crate::{
    icons::{IconName, icon},
    theme::Theme,
};

/// One row of a menu: a 16 px icon, the label, and the shortcut at the right.
/// Callers add the id, role, focus handling and click handler.
pub fn menu_item(
    name: IconName,
    label: &'static str,
    shortcut: &'static str,
    enabled: bool,
    danger: bool,
    theme: Theme,
) -> Div {
    let (text, icon_color) = match (enabled, danger) {
        (false, _) => (theme.control_border, theme.control_border),
        (true, true) => (theme.danger, theme.danger),
        (true, false) => (theme.text, theme.secondary_text),
    };
    div()
        .h(px(28.))
        .px_2()
        .rounded(px(6.))
        .flex()
        .items_center()
        .gap_2()
        .text_color(text)
        .child(icon(name).text_color(icon_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
        .when(!shortcut.is_empty(), |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(if enabled {
                        theme.secondary_text
                    } else {
                        theme.control_border
                    })
                    .child(shortcut),
            )
        })
        .when(enabled, |row| {
            row.cursor_pointer()
                .hover(move |style| style.bg(theme.hover))
                .focus(move |style| style.bg(theme.selection))
        })
}
