//! Placement and building blocks for menus anchored to a control.

use gpui::{Bounds, FocusHandle, Pixels, Role, Stateful, div, point, prelude::*, px};

use crate::{
    icons::{IconName, icon},
    theme::Theme,
};

gpui::actions!(editor_menu, [Dismiss]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Below,
    Above,
}

pub const GAP: f32 = 4.;
pub const ROW: f32 = 28.;

/// Where the menu goes, or None when it fits neither side or the trigger is not fully visible.
pub fn place(
    trigger: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    menu_height: Pixels,
) -> Option<Placement> {
    if trigger.top() < viewport.top() || trigger.bottom() > viewport.bottom() {
        return None;
    }
    let needed = menu_height + px(GAP);
    if viewport.bottom() - trigger.bottom() >= needed {
        Some(Placement::Below)
    } else if trigger.top() - viewport.top() >= needed {
        Some(Placement::Above)
    } else {
        None
    }
}

/// Rows × 28 + separators × 9 + 8 padding + 2 border, capped at 320.
pub fn menu_height(rows: usize, separators: usize) -> Pixels {
    px((rows as f32 * ROW + separators as f32 * 9. + 10.).min(320.))
}

pub fn dropdown_panel(id: impl Into<gpui::ElementId>, theme: Theme) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .p(px(4.))
        .min_w(px(220.))
        .max_h(px(320.))
        .overflow_y_scroll()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised)
        .shadow(vec![gpui::BoxShadow {
            color: theme.overlay,
            offset: point(px(0.), px(4.)),
            blur_radius: px(16.),
            inset: false,
            spread_radius: px(0.),
        }])
        .occlude()
}

pub fn dropdown_item(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    checked: bool,
    focus: &FocusHandle,
    theme: Theme,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::MenuItem)
        .tab_index(0)
        .track_focus(focus)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(ROW))
        .px(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(13.))
        .hover(move |style| style.bg(theme.hover))
        .focus(move |style| style.bg(theme.selection))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .when(checked, |row| {
            row.child(icon(IconName::Check).size(px(14.)).text_color(theme.accent))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn viewport() -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), size(px(400.), px(600.)))
    }

    fn trigger(top: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(20.), px(top)), size(px(200.), px(28.)))
    }

    #[test]
    fn menu_fits_below_when_there_is_room() {
        assert_eq!(
            place(trigger(100.), viewport(), menu_height(4, 0)),
            Some(Placement::Below)
        );
    }

    #[test]
    fn menu_goes_above_when_only_the_top_has_room() {
        let short = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(230.)));
        assert_eq!(
            place(trigger(200.), short, menu_height(2, 0)),
            Some(Placement::Above)
        );
    }

    #[test]
    fn menu_is_none_when_neither_side_fits() {
        let short = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(100.)));
        assert_eq!(place(trigger(50.), short, menu_height(2, 0)), None);
    }

    #[test]
    fn menu_is_none_when_the_trigger_is_partly_scrolled_out() {
        assert_eq!(place(trigger(-4.), viewport(), menu_height(2, 0)), None);
        assert_eq!(place(trigger(590.), viewport(), menu_height(2, 0)), None);
    }

    #[test]
    fn menu_height_is_capped_at_320() {
        assert_eq!(menu_height(3, 1), px(103.));
        assert_eq!(menu_height(100, 10), px(320.));
    }
}
