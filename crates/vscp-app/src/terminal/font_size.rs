use std::time::Duration;

use gpui::{App, Global};

use crate::dev;

const MIN: f32 = 6.;
const MAX: f32 = 48.;

#[derive(Default)]
pub(crate) struct FontSize {
    pub(crate) override_size: Option<f32>,
    dev_override: bool,
}
impl Global for FontSize {}

pub(crate) fn initialize(cx: &mut App) {
    seed(dev::terminal_font_size(), cx);
}

fn seed(dev_size: Option<f32>, cx: &mut App) {
    ensure(cx);
    if let Some(size) = dev_size {
        let size = size.clamp(MIN, MAX);
        cx.global_mut::<FontSize>().override_size = Some(size);
        cx.global_mut::<FontSize>().dev_override = true;
        dev::log(format!("Terminal: font size {size}"));
    }
}

/// Applies the saved preference before the first terminal opens. A dev override
/// wins for the run and is never written back.
pub(crate) fn apply_saved(saved: Option<f32>, cx: &mut App) {
    ensure(cx);
    if cx.global::<FontSize>().dev_override {
        return;
    }
    let Some(saved) = saved else { return };
    cx.global_mut::<FontSize>().override_size = Some(saved.clamp(MIN, MAX));
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
    schedule_save(Some(next), cx);
}

pub(crate) fn reset(cx: &mut App, base: f32) {
    if cx.global::<FontSize>().override_size.is_some() {
        cx.global_mut::<FontSize>().override_size = None;
        dev::log(format!("Terminal: font size {base}"));
        schedule_save(None, cx);
    }
}

/// Writes `settings.toml` after a short debounce. The retry covers a
/// concurrent settings save; the last write wins with the newest value.
fn schedule_save(size: Option<f32>, cx: &mut App) {
    let Ok(dir) = dev::config_directory() else {
        return;
    };
    let executor = cx.background_executor().clone();
    let timer = executor.clone();
    executor
        .spawn(async move {
            timer.timer(Duration::from_millis(500)).await;
            loop {
                let mut settings = vscp_core::settings::Settings::load(&dir).unwrap_or_default();
                if settings.terminal_font_size == size {
                    break;
                }
                settings.terminal_font_size = size;
                if settings.save(&dir).is_ok() {
                    break;
                }
                timer.timer(Duration::from_millis(200)).await;
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn saved_sizes_are_clamped_and_dev_overrides_win(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            initialize(cx);
            apply_saved(Some(100.), cx);
            assert_eq!(cx.global::<FontSize>().override_size, Some(48.));
            apply_saved(Some(9.), cx);
            assert_eq!(cx.global::<FontSize>().override_size, Some(9.));
        });
    }

    #[gpui::test]
    fn shortcuts_and_reset_update_the_saved_override(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            ensure(cx);
            apply_saved(Some(13.), cx);
            increase(cx, 11.);
            assert_eq!(cx.global::<FontSize>().override_size, Some(14.));
            decrease(cx, 11.);
            assert_eq!(cx.global::<FontSize>().override_size, Some(13.));
            reset(cx, 11.);
            assert_eq!(cx.global::<FontSize>().override_size, None);
        });
    }

    #[gpui::test]
    fn a_dev_override_is_not_replaced_by_the_saved_size(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            seed(Some(20.), cx);
            apply_saved(Some(13.), cx);
            assert_eq!(cx.global::<FontSize>().override_size, Some(20.));
        });
    }
}
