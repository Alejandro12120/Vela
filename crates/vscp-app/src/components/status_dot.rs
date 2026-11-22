use gpui::{AnyElement, Rgba, div, prelude::*, px};

use super::motion;

/// A 7 px status dot. It pulses while `pulse_id` is set, which is only while connecting.
pub fn status_dot(color: Rgba, pulse_id: Option<String>) -> AnyElement {
    let dot = div().flex_none().size(px(7.)).rounded_full().bg(color);
    match pulse_id {
        Some(id) => motion::pulse(dot, format!("pulse-{id}")).into_any_element(),
        None => dot.into_any_element(),
    }
}
