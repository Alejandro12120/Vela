//! Shared motion constants and helpers. Transitions are short and never delay
//! input. `with_animation` and `with_spring` already render the end state when
//! the system asks for reduced motion.

use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt, AnyElement, ElementId, IntoElement, SpringConfig,
    Styled, Svg, Transformation, ease_out_quint, percentage, px,
};

/// Colour and opacity.
pub const FAST: Duration = Duration::from_millis(120);
/// Position and size.
pub const BASE: Duration = Duration::from_millis(180);
/// Roughly critically damped, settles in about 250 ms.
pub const SPRING: SpringConfig = SpringConfig::new(520., 44., 1.);
/// One turn of a spinner.
const SPIN: Duration = Duration::from_millis(900);
/// Period of the connecting pulse.
const PULSE: Duration = Duration::from_millis(1200);

/// Fades the element in. `rise` is the starting offset in px along y.
/// The element must not be absolutely positioned, since the offset is relative.
pub fn appear<E>(
    element: E,
    id: impl Into<ElementId>,
    rise: f32,
    duration: Duration,
) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out_quint()),
        move |element, t| element.opacity(t).relative().top(px(rise * (1. - t))),
    )
}

/// Fades the element in without moving it, which suits absolutely positioned ones.
pub fn fade<E>(element: E, id: impl Into<ElementId>, duration: Duration) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out_quint()),
        |element, t| element.opacity(t),
    )
}

/// Rotates the icon continuously, one turn per 0.9 s.
pub fn spin(icon: Svg, id: impl Into<ElementId>) -> AnimationElement<Svg> {
    icon.with_animation(id, Animation::new(SPIN).repeat(), |icon, t| {
        icon.with_transformation(Transformation::rotate(percentage(t)))
    })
}

/// Opacity pulse between 0.35 and 1 with a 1.2 s period.
pub fn pulse<E>(element: E, id: impl Into<ElementId>) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(
        id,
        Animation::new(PULSE)
            .repeat()
            .with_easing(gpui::pulsating_between(0.35, 1.)),
        |element, opacity| element.opacity(opacity),
    )
}

/// A right-pointing chevron that is a quarter turn clockwise when `expanded`.
/// With `animate` it turns over `FAST`, otherwise it is drawn at its final angle.
pub fn chevron(icon: Svg, id: impl Into<ElementId>, expanded: bool, animate: bool) -> AnyElement {
    let end = if expanded { 0.25 } else { 0. };
    if !animate {
        return icon
            .with_transformation(Transformation::rotate(percentage(end)))
            .into_any_element();
    }
    // The id carries the state, so every toggle starts a new turn.
    let id: ElementId = (id.into(), if expanded { "open" } else { "closed" }).into();
    icon.with_animation(
        id,
        Animation::new(FAST).with_easing(ease_out_quint()),
        move |icon, t| {
            let turns = if expanded { 0.25 * t } else { 0.25 * (1. - t) };
            icon.with_transformation(Transformation::rotate(percentage(turns)))
        },
    )
    .into_any_element()
}
