use gpui::{
    AnyElement, Context, Div, FocusHandle, FontWeight, SharedString, actions, div, point,
    prelude::*, px,
};

use crate::theme::Theme;

actions!(dialog, [Confirm, Cancel, Next, Previous]);

/// The backdrop intercepts pointer input. Tab wraps within the active dialog.
pub fn overlay<T: 'static>(
    title: impl Into<SharedString>,
    body: AnyElement,
    footer: AnyElement,
    focus: &FocusHandle,
    last: &FocusHandle,
    theme: Theme,
    cx: &Context<T>,
) -> Div {
    let title = title.into();
    div()
        .absolute()
        .inset_0()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.overlay)
        .occlude()
        .child(
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
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(16.))
                        .child(title),
                )
                .child(
                    div()
                        .id("dialog-body")
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                )
                .child(footer),
        )
}
