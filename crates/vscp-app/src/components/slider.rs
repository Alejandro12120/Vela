use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Bounds, BoxShadow, Div, ElementId, KeyDownEvent, MouseButton, MouseDownEvent, Pixels,
    Stateful, Window, canvas, div, point, prelude::*, px, relative,
};

use crate::theme::Theme;

pub const MIN: u8 = 30;
pub const MAX: u8 = 100;
const STEP: u8 = 5;
const WIDTH: f32 = 200.;

pub fn value_at(x: Pixels, bounds: Bounds<Pixels>) -> u8 {
    let fraction = ((x - bounds.left()) / bounds.size.width).clamp(0., 1.);
    let steps = (MAX - MIN) / STEP;
    MIN + (fraction * f32::from(steps)).round() as u8 * STEP
}

#[allow(clippy::too_many_arguments)]
pub fn slider(
    id: impl Into<ElementId>,
    label: &'static str,
    value: u8,
    enabled: bool,
    theme: Theme,
    on_start: impl Fn(Pixels, Bounds<Pixels>, &mut Window, &mut App) + 'static,
    on_key: impl Fn(u8, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let track_bounds = Rc::new(Cell::new(None));
    let layout = track_bounds.clone();
    let fraction = f32::from(value - MIN) / f32::from(MAX - MIN);
    div()
        .id(id)
        .debug_selector(move || label.to_owned())
        .role(gpui::Role::Slider)
        .aria_label(label)
        .aria_numeric_value(f64::from(value))
        .aria_min_numeric_value(f64::from(MIN))
        .aria_max_numeric_value(f64::from(MAX))
        .aria_numeric_value_step(f64::from(STEP))
        .aria_value(format!("{value} %"))
        .relative()
        .h(px(24.))
        .w(px(WIDTH))
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(6.))
        .when(enabled, |slider| {
            slider
                .tab_index(0)
                .cursor_pointer()
                .focus(move |style| {
                    style.shadow(vec![BoxShadow {
                        color: theme.accent.into(),
                        inset: false,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(1.),
                    }])
                })
                .on_mouse_down(
                    MouseButton::Left,
                    move |event: &MouseDownEvent, window, cx| {
                        if let Some(rect) = track_bounds.get() {
                            on_start(event.position.x, rect, window, cx);
                        }
                    },
                )
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    let next = match event.keystroke.key.as_str() {
                        "left" => value.saturating_sub(STEP).max(MIN),
                        "right" => value.saturating_add(STEP).min(MAX),
                        "home" => MIN,
                        "end" => MAX,
                        _ => return,
                    };
                    on_key(next, window, cx);
                    cx.stop_propagation();
                })
        })
        .child(
            div()
                .relative()
                .w_full()
                .h(px(4.))
                .rounded_full()
                .bg(theme.border)
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(theme.accent),
                )
                .child(
                    div()
                        .absolute()
                        .left(relative(fraction))
                        .top(px(-5.))
                        .ml(px(-7.))
                        .size(px(14.))
                        .rounded_full()
                        .border_2()
                        .border_color(theme.accent)
                        .bg(theme.raised),
                )
                .child(
                    canvas(
                        move |bounds, _, _| layout.set(Some(bounds)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn pointer_clamps_and_rounds_to_five_percent() {
        let bounds = Bounds::new(point(px(100.), px(20.)), size(px(200.), px(4.)));
        assert_eq!(value_at(px(0.), bounds), 30);
        assert_eq!(value_at(px(207.), bounds), 65);
        assert_eq!(value_at(px(400.), bounds), 100);
    }
}
