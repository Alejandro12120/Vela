use std::rc::Rc;

use gpui::{
    App, ClickEvent, Div, FontWeight, KeyDownEvent, Role, Stateful, Window, div, prelude::*, px,
};

use crate::theme::Theme;

/// One Tab stop with arrow-key navigation, like a native radio group.
pub fn segmented(
    id: &'static str,
    options: &[&'static str],
    selected: usize,
    theme: Theme,
    on_select: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let on_select = Rc::new(on_select);
    let key_select = on_select.clone();
    let len = options.len();
    div()
        .id(id)
        .role(Role::RadioGroup)
        .aria_label(options.join(" | "))
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
            let Some(next) = step_segment(event.keystroke.key.as_str(), selected, len) else {
                return;
            };
            cx.stop_propagation();
            key_select(next, window, cx);
        })
        .children(options.iter().enumerate().map(|(index, label)| {
            let on_select = on_select.clone();
            segment(
                id,
                index,
                label,
                selected == index,
                theme,
                move |window, cx| on_select(index, window, cx),
            )
        }))
}

fn segment(
    group: &'static str,
    index: usize,
    label: &'static str,
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
        .cursor_pointer()
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
        .when(!selected, |segment| {
            segment
                .text_color(theme.secondary_text)
                .hover(move |style| style.bg(theme.hover))
        })
        .child(div().font_weight(FontWeight::MEDIUM).child(label))
        .on_click(move |_: &ClickEvent, window, cx| on_select(window, cx))
}

/// Left/Right and Up/Down wrap around the group. Other keys do not move.
fn step_segment(key: &str, selected: usize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let delta = match key {
        "right" | "down" => 1isize,
        "left" | "up" => -1,
        _ => return None,
    };
    Some((selected as isize + delta).rem_euclid(len as isize) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_wrap_and_other_keys_do_not_move() {
        assert_eq!(step_segment("right", 0, 2), Some(1));
        assert_eq!(step_segment("left", 1, 2), Some(0));
        assert_eq!(step_segment("down", 1, 3), Some(2));
        assert_eq!(step_segment("up", 0, 3), Some(2));
        assert_eq!(step_segment("enter", 0, 2), None);
        assert_eq!(step_segment("right", 0, 0), None);
    }
}
