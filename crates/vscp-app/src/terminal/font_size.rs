use gpui::{App, Global};

use crate::dev;

const MIN: f32 = 6.;
const MAX: f32 = 48.;

#[derive(Default)]
pub(crate) struct FontSize {
    pub(crate) override_size: Option<f32>,
}

impl Global for FontSize {}

pub(crate) fn initialize(cx: &mut App) {
    ensure(cx);
    if let Some(size) = dev::terminal_font_size() {
        let size = size.clamp(MIN, MAX);
        cx.global_mut::<FontSize>().override_size = Some(size);
        dev::log(format!("Terminal: font size {size}"));
    }
}

pub(crate) fn ensure(cx: &mut App) {
    if !cx.has_global::<FontSize>() {
        cx.set_global(FontSize::default());
    }
}

pub(crate) fn effective(base: f32, cx: &App) -> f32 {
    cx.global::<FontSize>().override_size.unwrap_or(base)
}

pub(crate) fn increase(cx: &mut App, base: f32) {
    adjust(cx, base, 1.);
}

pub(crate) fn decrease(cx: &mut App, base: f32) {
    adjust(cx, base, -1.);
}

fn adjust(cx: &mut App, base: f32, delta: f32) {
    let current = cx.global::<FontSize>().override_size.unwrap_or(base);
    let next = (current + delta).clamp(MIN, MAX);
    if cx.global::<FontSize>().override_size == Some(next) {
        return;
    }
    cx.global_mut::<FontSize>().override_size = Some(next);
    dev::log(format!("Terminal: font size {next}"));
}

pub(crate) fn reset(cx: &mut App, base: f32) {
    if cx.global::<FontSize>().override_size.is_some() {
        cx.global_mut::<FontSize>().override_size = None;
        dev::log(format!("Terminal: font size {base}"));
    }
}
