use gpui::{Div, div, prelude::*, px};

use crate::theme::Theme;

/// A 13 px selection circle: a thin ring when empty, a thick accent ring when
/// chosen. The centre stays transparent so the selected row's tint shows.
pub fn radio_dot(selected: bool, theme: Theme) -> Div {
    let dot = div().flex_none().size(px(13.)).rounded_full();
    if selected {
        dot.border(px(4.)).border_color(theme.accent)
    } else {
        dot.border(px(1.5)).border_color(theme.control_border)
    }
}
