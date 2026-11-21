use gpui::{Div, Rgba, div, prelude::*, px};

pub fn status_dot(color: Rgba) -> Div {
    div().flex_none().size(px(7.)).rounded_full().bg(color)
}
