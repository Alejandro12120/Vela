use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, Context, Div, FocusHandle, FontWeight,
    MouseButton, Rgba, ScrollHandle, SharedString, actions, canvas, div, ease_out_quint, fill,
    linear_color_stop, linear_gradient, point, prelude::*, px, size,
};

use super::motion;
use crate::{
    icons::{IconName, icon},
    theme::Theme,
};

actions!(dialog, [Confirm, Cancel, Next, Previous]);

/// An 18 px icon before the title. The tone matches the dialog: danger for
/// deletions, warning for conflicts, accent or neutral for prompts.
pub type Lead = Option<(IconName, Rgba)>;

/// The backdrop intercepts pointer input. Tab wraps within the active dialog.
#[allow(clippy::too_many_arguments)]
pub fn overlay<T: 'static>(
    title: impl Into<SharedString>,
    lead: Lead,
    body: AnyElement,
    footer: AnyElement,
    focus: &FocusHandle,
    last: &FocusHandle,
    theme: Theme,
    dismiss_on_backdrop: bool,
    cx: &Context<T>,
) -> Div {
    overlay_with_width(
        title,
        lead,
        body,
        footer,
        focus,
        last,
        theme,
        dismiss_on_backdrop,
        px(448.),
        "Dialog",
        None,
        cx,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn overlay_with_width<T: 'static>(
    title: impl Into<SharedString>,
    lead: Lead,
    body: AnyElement,
    footer: AnyElement,
    focus: &FocusHandle,
    last: &FocusHandle,
    theme: Theme,
    dismiss_on_backdrop: bool,
    width: gpui::Pixels,
    key_context: &'static str,
    scroll: Option<&gpui::ScrollHandle>,
    cx: &Context<T>,
) -> Div {
    let title = title.into();
    let overlay = theme.overlay;
    let reduce_motion = cx.reduce_motion();
    let backdrop = div()
        .absolute()
        .inset_0()
        .when(dismiss_on_backdrop, |backdrop| {
            backdrop.on_mouse_down(MouseButton::Left, |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(Cancel), cx);
            })
        });
    let backdrop = if reduce_motion {
        backdrop.bg(overlay).into_any_element()
    } else {
        backdrop
            .with_animation(
                "modal-backdrop",
                Animation::new(motion::FAST).with_easing(ease_out_quint()),
                move |backdrop, t| backdrop.bg(overlay.opacity(t)),
            )
            .into_any_element()
    };
    let scroller = div().id("dialog-body").min_h_0().overflow_y_scroll();
    let body = if let Some(scroll) = scroll {
        div()
            .relative()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                scroller
                    .track_scroll(scroll)
                    .pr_3()
                    .child(div().pb_4().child(body)),
            )
            .child(scroll_indicator(scroll, theme, true))
            .into_any_element()
    } else {
        scroller.child(body).into_any_element()
    };
    let panel = div()
        .id("modal")
        .when(dismiss_on_backdrop, |panel| panel.occlude())
        .role(gpui::Role::Dialog)
        .aria_label(title.clone())
        .tab_index(-1)
        .tab_group()
        .tab_stop(false)
        .track_focus(focus)
        .key_context(key_context)
        .on_action({
            let scope = focus.clone();
            cx.listener(move |_, _: &Next, window, cx| {
                window.focus_next(cx);
                if !scope.contains_focused(window, cx) {
                    scope.focus(window, cx);
                    window.focus_next(cx);
                }
            })
        })
        .on_action({
            let scope = focus.clone();
            let last = last.clone();
            cx.listener(move |_, _: &Previous, window, cx| {
                window.focus_prev(cx);
                if !scope.contains_focused(window, cx) {
                    last.focus(window, cx);
                }
            })
        })
        .w_full()
        .max_w(width)
        .max_h_full()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised)
        .shadow(vec![gpui::BoxShadow {
            color: theme.overlay,
            inset: false,
            offset: point(px(0.), px(4.)),
            blur_radius: px(16.),
            spread_radius: px(0.),
        }])
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(16.))
                .when_some(lead, |row, (name, color)| {
                    row.child(icon(name).size(px(18.)).text_color(color))
                })
                .child(div().min_w_0().child(title)),
        )
        .child(body)
        .child(footer);
    let panel = if reduce_motion {
        panel.into_any_element()
    } else {
        motion::appear(panel, "modal-dialog", 6., motion::BASE).into_any_element()
    };
    div()
        .absolute()
        .inset_0()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .child(backdrop)
        .child(panel)
}

pub fn scroll_indicator(scroll: &ScrollHandle, theme: Theme, fade_edges: bool) -> impl IntoElement {
    let scroll = scroll.clone();
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let overflow = scroll.max_offset().y;
            let height = bounds.size.height;
            if overflow <= px(0.) || height <= px(0.) {
                return;
            }
            let bar_width = px(4.);
            let bar_left = bounds.right() - bar_width;
            let thumb =
                (height * (height / (height + overflow))).clamp(px(20.).min(height), height);
            let offset = (-scroll.offset().y).clamp(px(0.), overflow);
            let progress = offset / overflow;
            let top = bounds.top() + (height - thumb) * progress;
            window.paint_quad(fill(
                Bounds::new(point(bar_left, bounds.top()), size(bar_width, height)),
                theme.selection,
            ));
            window.paint_quad(fill(
                Bounds::new(point(bar_left, top), size(bar_width, thumb)),
                theme.secondary_text,
            ));
            if fade_edges {
                let fade_height = px(12.).min(height / 2.);
                let fade_size = size(bounds.size.width - bar_width, fade_height);
                for (visible, y, angle) in [
                    (offset > px(0.), bounds.top(), 180.),
                    (offset < overflow, bounds.bottom() - fade_height, 0.),
                ] {
                    if visible {
                        window.paint_quad(fill(
                            Bounds::new(point(bounds.left(), y), fade_size),
                            linear_gradient(
                                angle,
                                linear_color_stop(theme.raised, 0.),
                                linear_color_stop(theme.raised.opacity(0.), 1.),
                            ),
                        ));
                    }
                }
            }
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .right_0()
    .when(fade_edges, |canvas| canvas.left_0())
    .when(!fade_edges, |canvas| canvas.w(px(4.)))
}
