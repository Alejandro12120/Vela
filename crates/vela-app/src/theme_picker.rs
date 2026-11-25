use super::*;

use gpui::{
    DispatchPhase, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Point, anchored,
    deferred, relative, rgb,
};
use vela_core::appearance::Hsv;

const WIDTH: f32 = 208.;
const SQUARE_HEIGHT: f32 = 140.;

pub(super) struct Picker {
    pub(super) token: Token,
    pub(super) hex: Entity<TextInput>,
    pub(super) fallback: u32,
    pub(super) scope: FocusHandle,
    pub(super) done: FocusHandle,
    saturation: FocusHandle,
    hue: FocusHandle,
    opacity: FocusHandle,
    color: Hsv,
    alpha: f32,
    show_alpha: bool,
    drag: Option<(Axis, Bounds<Pixels>)>,
    anchor: Point<Pixels>,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
enum Axis {
    Saturation,
    Hue,
    Opacity,
}

impl Axis {
    fn label(self) -> &'static str {
        match self {
            Self::Saturation => "Saturación y brillo",
            Self::Hue => "Tono",
            Self::Opacity => "Opacidad",
        }
    }
}

impl Picker {
    pub(super) fn update_color(&mut self, color: u32) {
        let hsv = Hsv::from_rgba(color);
        if hsv.saturation != 0. {
            self.color.hue = hsv.hue;
        }
        self.color.saturation = hsv.saturation;
        self.color.value = hsv.value;
        self.alpha = f32::from(color as u8) / 255.;
        self.show_alpha |= self.token.has_alpha(color);
    }

    fn rgba(&self) -> u32 {
        self.color.rgba((self.alpha * 255.).round() as u8)
    }
}

impl Workspace {
    pub(super) fn dev_open_picker(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(token) = TOKENS.iter().copied().find(|token| token.key() == key) else {
            return;
        };
        if let Some(editor) = &mut self.settings_page.editor
            && !BASIC.contains(&token)
        {
            editor.advanced = true;
            let mut group = "";
            let mut index = 1;
            for &entry in TOKENS {
                if entry.group() != group {
                    group = entry.group();
                    index += if entry.terminal() { 2 } else { 1 };
                }
                if entry == token {
                    break;
                }
                index += 1;
            }
            editor.scroll.scroll_to_item(index);
        }
        cx.on_next_frame(window, move |_, window, cx| {
            cx.on_next_frame(window, move |this, window, cx| {
                this.open_picker(token, window, cx)
            });
            cx.notify();
        });
    }

    pub(super) fn open_picker(
        &mut self,
        token: Token,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kitty = self
            .editor_kitty(cx)
            .map_or_else(Palette::default, |kitty| kitty.palette)
            .as_theme();
        let Some(editor) = self.settings_page.editor.as_mut() else {
            return;
        };
        let fallback = fallback_color(token, &kitty);
        let stored = token.get(editor.colors());
        let color = stored.unwrap_or(fallback);
        let hex = color_input(token, &stored.map(hex).unwrap_or_default(), cx);
        let subscription = cx.observe_in(&hex, window, move |this, input, window, cx| {
            this.edit_hex(token, &input, window, cx);
        });
        let anchor = editor.rows[&token].bounds.get().map_or_else(
            || point(window.viewport_size().width - px(244.), px(210.)),
            |bounds| point(bounds.right() - px(232.), bounds.bottom() + px(4.)),
        );
        let saturation = cx.focus_handle();
        saturation.focus(window, cx);
        editor.picker = Some(Picker {
            token,
            hex,
            fallback,
            anchor,
            color: Hsv::from_rgba(color),
            alpha: f32::from(color as u8) / 255.,
            show_alpha: token.has_alpha(color),
            drag: None,
            scope: cx.focus_handle(),
            done: cx.focus_handle(),
            saturation,
            hue: cx.focus_handle(),
            opacity: cx.focus_handle(),
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &mut self.settings_page.editor else {
            return false;
        };
        let Some(picker) = editor.picker.take() else {
            return false;
        };
        editor.rows[&picker.token].swatch.focus(window, cx);
        cx.notify();
        true
    }

    fn set_picker_rgb(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.settings_page.editor else {
            return;
        };
        let Some(picker) = &editor.picker else { return };
        let (token, value, popup_input) = (picker.token, picker.rgba(), picker.hex.clone());
        token.set(editor.colors_mut(), Some(value));
        let hex = hex(value);
        for input in [&editor.rows[&token].input, &popup_input] {
            input.update(cx, |input, cx| {
                input.set_invalid(false, cx);
                if input.value() != hex {
                    input.set_value(&hex, window, cx);
                }
            });
        }
        self.update_preview(cx);
    }

    fn set_picker_pointer(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self
            .settings_page
            .editor
            .as_mut()
            .and_then(|editor| editor.picker.as_mut())
        else {
            return;
        };
        let Some((axis, bounds)) = picker.drag else {
            return;
        };
        let ratio =
            |offset: Pixels, extent: Pixels| (offset.as_f32() / extent.as_f32()).clamp(0., 1.);
        let x = ratio(position.x - bounds.left(), bounds.size.width);
        match axis {
            Axis::Saturation => {
                picker.color.saturation = x;
                picker.color.value = 1. - ratio(position.y - bounds.top(), bounds.size.height);
            }
            Axis::Hue => picker.color.hue = x * 359.99,
            Axis::Opacity => picker.alpha = x,
        }
        self.set_picker_rgb(window, cx);
    }

    fn picker_key(
        &mut self,
        axis: Axis,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self
            .settings_page
            .editor
            .as_mut()
            .and_then(|editor| editor.picker.as_mut())
        else {
            return;
        };
        let (x, y) = match event.keystroke.key.as_str() {
            "left" => (-1., 0.),
            "right" => (1., 0.),
            "up" if matches!(axis, Axis::Saturation) => (0., 1.),
            "down" if matches!(axis, Axis::Saturation) => (0., -1.),
            _ => return,
        };
        let step = if event.keystroke.modifiers.shift {
            10.
        } else {
            1.
        };
        match axis {
            Axis::Saturation => {
                picker.color.saturation = (picker.color.saturation + x * step / 100.).clamp(0., 1.);
                picker.color.value = (picker.color.value + y * step / 100.).clamp(0., 1.);
            }
            Axis::Hue => picker.color.hue = (picker.color.hue + x * step).rem_euclid(360.),
            Axis::Opacity => picker.alpha = (picker.alpha + x * step / 100.).clamp(0., 1.),
        }
        self.set_picker_rgb(window, cx);
        cx.stop_propagation();
    }

    pub(in super::super) fn render_picker(
        &self,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement> {
        let picker = self.settings_page.editor.as_ref()?.picker.as_ref()?;
        let colors = picker.color;
        let opaque = colors.rgba(255);
        let pure = Hsv {
            saturation: 1.,
            value: 1.,
            ..colors
        }
        .rgba(255);
        let square = div()
            .absolute()
            .inset_0()
            .rounded(px(6.))
            .overflow_hidden()
            .bg(rgba(pure))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(6.))
                    .bg(gradient(90., 0xffffffff, 0xffffff00)),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(6.))
                    .bg(gradient(180., 0x00000000, 0x000000ff)),
            );
        let steps = [
            0xff0000ff, 0xffff00ff, 0x00ff00ff, 0x00ffffff, 0x0000ffff, 0xff00ffff, 0xff0000ff,
        ];
        let hue = div()
            .absolute()
            .inset_0()
            .flex()
            .rounded_full()
            .overflow_hidden()
            .children(
                steps
                    .windows(2)
                    .map(|pair| div().flex_1().h_full().bg(gradient(90., pair[0], pair[1]))),
            );
        let opacity = div()
            .absolute()
            .inset_0()
            .rounded_full()
            .overflow_hidden()
            .bg(checkerboard(theme.secondary_text, 4.))
            .child(div().absolute().inset_0().rounded_full().bg(gradient(
                90.,
                opaque & !255,
                opaque,
            )));
        let mouse = self.picker_mouse_tracker(cx);
        let panel = div()
            .id("color-picker")
            .role(gpui::Role::Dialog)
            .aria_label("Selector de color")
            .track_focus(&picker.scope)
            .tab_group()
            .tab_stop(false)
            .key_context("ThemeEditor")
            .on_action(cx.listener(|this, _: &modal::Next, window, cx| {
                this.theme_editor_tab(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &modal::Previous, window, cx| {
                this.theme_editor_tab(true, window, cx);
            }))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.close_picker(window, cx);
            }))
            .relative()
            .occlude()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .w(px(232.))
            .rounded(px(8.))
            .bg(theme.raised)
            .shadow(vec![BoxShadow {
                color: theme.overlay,
                inset: false,
                offset: point(px(0.), px(4.)),
                blur_radius: px(16.),
                spread_radius: px(0.),
            }])
            .child(
                self.picker_control(Axis::Saturation, &picker.saturation, theme, cx)
                    .h(px(SQUARE_HEIGHT))
                    .child(square)
                    .child(thumb(colors.saturation, 1. - colors.value, opaque)),
            )
            .child(
                self.picker_control(Axis::Hue, &picker.hue, theme, cx)
                    .h(px(12.))
                    .child(hue)
                    .child(thumb(colors.hue / 360., 0.5, pure)),
            )
            .when(picker.show_alpha, |panel| {
                panel.child(
                    self.picker_control(Axis::Opacity, &picker.opacity, theme, cx)
                        .h(px(12.))
                        .child(opacity)
                        .child(thumb(picker.alpha, 0.5, opaque)),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().w(px(96.)).child(picker.hex.clone()))
                    .child(
                        button(
                            "picker-done",
                            "Hecho",
                            Variant::Primary,
                            theme,
                            cx.listener(|this, _, window, cx| {
                                this.close_picker(window, cx);
                            }),
                        )
                        .track_focus(&picker.done)
                        .h(px(24.)),
                    ),
            )
            .child(mouse);
        Some(
            deferred(
                anchored()
                    .position(picker.anchor)
                    .snap_to_window_with_margin(px(8.))
                    .child(panel),
            )
            .with_priority(3),
        )
    }

    fn picker_control(
        &self,
        axis: Axis,
        focus: &FocusHandle,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let bounds = Rc::new(Cell::new(None));
        let layout = bounds.clone();
        div()
            .id(focus)
            .track_focus(focus)
            .role(gpui::Role::Slider)
            .aria_label(axis.label())
            .tab_index(0)
            .relative()
            .w(px(WIDTH))
            .flex_none()
            .rounded(px(6.))
            .focus(move |style| {
                style.shadow(vec![BoxShadow {
                    color: theme.accent.into(),
                    inset: false,
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(2.),
                }])
            })
            .cursor_pointer()
            .child(
                canvas(move |rect, _, _| layout.set(Some(rect)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if let Some((picker, bounds)) = this
                        .settings_page
                        .editor
                        .as_mut()
                        .and_then(|editor| editor.picker.as_mut())
                        .zip(bounds.get())
                    {
                        picker.drag = Some((axis, bounds));
                        this.set_picker_pointer(event.position, window, cx);
                    }
                }),
            )
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    this.picker_key(axis, event, window, cx)
                }),
            )
    }

    fn picker_mouse_tracker(&self, cx: &Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let moving = weak.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture
                        && event.pressed_button == Some(MouseButton::Left)
                    {
                        let _ = moving.update(cx, |this, cx| {
                            this.set_picker_pointer(event.position, window, cx)
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        let _ = weak.update(cx, |this, cx| {
                            this.set_picker_pointer(event.position, window, cx);
                            if let Some(picker) = this
                                .settings_page
                                .editor
                                .as_mut()
                                .and_then(|editor| editor.picker.as_mut())
                            {
                                picker.drag = None;
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size(px(0.))
    }
}

fn gradient(angle: f32, from: u32, to: u32) -> gpui::Background {
    linear_gradient(
        angle,
        linear_color_stop(rgba(from), 0.),
        linear_color_stop(rgba(to), 1.),
    )
}

fn thumb(x: f32, y: f32, color: u32) -> Div {
    div()
        .absolute()
        .left(relative(x))
        .top(relative(y))
        .ml(px(-6.))
        .mt(px(-6.))
        .size(px(12.))
        .rounded_full()
        .bg(rgba(color))
        .border_2()
        .border_color(rgb(0xffffff))
        .shadow(vec![BoxShadow {
            color: rgba(0x00000099).into(),
            inset: false,
            offset: point(px(0.), px(0.)),
            blur_radius: px(1.),
            spread_radius: px(1.),
        }])
}
