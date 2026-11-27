use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, FocusHandle, FontWeight, MouseButton, Rgba,
    SharedString, actions, div, ease_out_quint, point, prelude::*, px,
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
        .child(
            div()
                .id("dialog-body")
                .min_h_0()
                .overflow_y_scroll()
                .when_some(scroll, |body, scroll| body.track_scroll(scroll))
                .child(body),
        )
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
