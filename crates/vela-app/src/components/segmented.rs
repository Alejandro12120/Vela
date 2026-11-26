use std::rc::Rc;

use gpui::{
    App, ClickEvent, Div, FontWeight, KeyDownEvent, Role, Stateful, Window, div, prelude::*, px,
};

use crate::theme::Theme;

/// One Tab stop with arrow-key navigation, like a native radio group.
pub fn segmented(
    id: &'static str,
    label: &'static str,
    options: &[(&'static str, Option<&'static str>)],
    selected: usize,
    theme: Theme,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let on_select = Rc::new(on_select);
    let key_select = on_select.clone();
    let enabled: Vec<_> = options.iter().map(|(_, reason)| reason.is_none()).collect();
    div()
        .id(id)
        .role(Role::RadioGroup)
        .aria_label(label)
        .tab_index(0)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(7.))
        .border_1()
        .border_color(theme.control_border)
        .bg(theme.panel)
        .focus(move |style| style.border_color(theme.accent))
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            let Some(next) = step_segment(event.keystroke.key.as_str(), selected, &enabled) else {
                return;
            };
            cx.stop_propagation();
            key_select(next, window, cx);
        })
        .children(
            options
                .iter()
                .enumerate()
                .map(|(index, (label, disabled_reason))| {
                    let on_select = on_select.clone();
                    segment(
                        id,
                        index,
                        (label, *disabled_reason),
                        selected == index,
                        theme,
                        move |window, cx| on_select(index, window, cx),
                    )
                }),
        )
}

fn segment(
    group: &'static str,
    index: usize,
    (label, disabled_reason): (&'static str, Option<&'static str>),
    selected: bool,
    theme: Theme,
    on_select: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id((group, index))
        .role(Role::RadioButton)
        .aria_label(label)
        .aria_selected(selected)
        .flex_none()
        .px(px(10.))
        .py(px(4.))
        .rounded(px(5.))
        .text_size(px(13.))
        .when(selected, |segment| {
            segment
                .border_1()
                .border_color(theme.border)
                .bg(theme.hover)
                .text_color(theme.text)
        })
        .when(!selected && disabled_reason.is_none(), |segment| {
            segment
                .text_color(theme.secondary_text)
                .hover(move |style| style.bg(theme.hover))
        })
        .when_some(disabled_reason, |segment, reason| {
            segment
                .aria_description(reason)
                .text_color(theme.secondary_text)
                .opacity(0.55)
        })
        .when(disabled_reason.is_none(), |segment| {
            segment
                .cursor_pointer()
                .on_click(move |_: &ClickEvent, window, cx| on_select(window, cx))
        })
        .child(div().font_weight(FontWeight::MEDIUM).child(label))
}

/// Left/Right and Up/Down wrap around the group. Other keys do not move.
fn step_segment(key: &str, selected: usize, enabled: &[bool]) -> Option<usize> {
    let delta = match key {
        "right" | "down" => 1isize,
        "left" | "up" => -1,
        _ => return None,
    };
    (1..enabled.len())
        .map(|step| {
            (selected as isize + delta * step as isize).rem_euclid(enabled.len() as isize) as usize
        })
        .find(|&index| enabled[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_wrap_and_other_keys_do_not_move() {
        assert_eq!(step_segment("right", 0, &[true, true]), Some(1));
        assert_eq!(step_segment("left", 1, &[true, true]), Some(0));
        assert_eq!(step_segment("down", 1, &[true, true, true]), Some(2));
        assert_eq!(step_segment("up", 0, &[true, true, true]), Some(2));
        assert_eq!(step_segment("enter", 0, &[true, true]), None);
        assert_eq!(step_segment("right", 0, &[]), None);
    }

    #[test]
    fn arrows_skip_disabled_segments() {
        assert_eq!(step_segment("right", 0, &[true, false]), None);
        assert_eq!(step_segment("right", 0, &[true, false, true]), Some(2));
    }
}
