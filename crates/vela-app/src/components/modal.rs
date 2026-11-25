use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, FocusHandle, FontWeight, Rgba, SharedString,
    actions, div, ease_out_quint, point, prelude::*, px,
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
    cx: &Context<T>,
) -> Div {
    let title = title.into();
    let overlay = theme.overlay;
    div()
        .absolute()
        .inset_0()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .child(
            // The backdrop fades in on its own, so the dialog is not dimmed by it.
            div().absolute().inset_0().with_animation(
                "modal-backdrop",
                Animation::new(motion::FAST).with_easing(ease_out_quint()),
                move |backdrop, t| backdrop.bg(overlay.opacity(t)),
            ),
        )
        .child(motion::appear(
            div()
                .id("modal")
                .role(gpui::Role::Dialog)
                .aria_label(title.clone())
                .tab_index(-1)
                .tab_group()
                .tab_stop(false)
                .track_focus(focus)
                .key_context("Dialog")
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
                .max_w(px(448.))
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
                        .child(body),
                )
                .child(footer),
            "modal-dialog",
            6.,
            motion::BASE,
        ))
}
