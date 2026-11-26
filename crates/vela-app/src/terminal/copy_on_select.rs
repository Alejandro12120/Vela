use gpui::{App, Global};

/// The saved “Copy on Select” preference. `None` follows `kitty.conf`.
#[derive(Default)]
pub(crate) struct CopyOnSelect {
    value: Option<bool>,
}

impl Global for CopyOnSelect {}

fn ensure(cx: &mut App) {
    if !cx.has_global::<CopyOnSelect>() {
        cx.set_global(CopyOnSelect::default());
    }
}

pub(crate) fn set(value: Option<bool>, cx: &mut App) {
    ensure(cx);
    cx.global_mut::<CopyOnSelect>().value = value;
}

pub(crate) fn enabled(kitty_default: bool, cx: &App) -> bool {
    if !cx.has_global::<CopyOnSelect>() {
        return false;
    }
    super::view::should_copy(true, cx.global::<CopyOnSelect>().value, kitty_default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn saved_value_wins_and_none_follows_kitty(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(!enabled(true, cx));
            set(Some(false), cx);
            assert!(!enabled(true, cx));
            set(Some(true), cx);
            assert!(enabled(false, cx));
            set(None, cx);
            assert!(enabled(true, cx));
            assert!(!enabled(false, cx));
        });
    }
}
