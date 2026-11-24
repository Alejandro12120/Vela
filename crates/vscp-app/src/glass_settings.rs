use super::*;

use gpui::{Bounds, DispatchPhase, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, canvas};
use vscp_core::appearance::Glass;

use crate::components::{slider, switch::switch};

#[derive(Clone, Copy)]
enum Zone {
    Chrome,
    Content,
}

impl Zone {
    fn label(self) -> &'static str {
        match self {
            Self::Chrome => "Barra lateral y cabecera",
            Self::Content => "Listas y terminal",
        }
    }

    fn value(self, glass: Glass) -> u8 {
        match self {
            Self::Chrome => glass.chrome,
            Self::Content => glass.content,
        }
    }

    fn set(self, glass: &mut Glass, value: u8) {
        match self {
            Self::Chrome => glass.chrome = value,
            Self::Content => glass.content = value,
        }
    }
}

pub(super) struct GlassDrag {
    zone: Zone,
    bounds: Bounds<Pixels>,
}

impl Workspace {
    pub(super) fn render_interface(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let appearance = cx.global::<Appearance>();
        let glass = appearance.glass();
        let reduced = appearance.reduce_transparency;
        let enabled = glass.enabled && !reduced && !self.settings_page.busy;
        section("Interfaz")
            .child(div().flex().justify_between().items_center().gap_3().py_1().child("Vidrio esmerilado")
                .child(switch("glass-enabled", "Vidrio esmerilado", glass.enabled, theme,
                    cx.listener(|this, _, window, cx| this.toggle_glass(window, cx)))))
            .when(reduced, |section| section.child(
                div().text_size(px(12.)).text_color(theme.secondary_text)
                    .child("macOS tiene activado “Reducir transparencia”: la ventana se mantiene opaca.")
            ))
            .children([Zone::Chrome, Zone::Content].map(|zone| self.glass_slider(zone, glass, enabled, theme, cx)))
            .child(self.glass_pointer(cx))
    }

    fn glass_slider(
        &self,
        zone: Zone,
        glass: Glass,
        enabled: bool,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Div {
        let label = zone.label();
        let value = zone.value(glass);
        let start = cx.listener(move |this, (x, bounds): &(Pixels, Bounds<Pixels>), _, cx| {
            this.settings_page.glass_drag = Some(GlassDrag {
                zone,
                bounds: *bounds,
            });
            this.update_glass_slider(zone, slider::value_at(*x, *bounds), cx);
        });
        let key = cx.listener(move |this, value: &u8, window, cx| {
            this.update_glass_slider(zone, *value, cx);
            this.save_glass(window, cx);
        });
        div()
            .flex()
            .flex_wrap()
            .justify_between()
            .items_center()
            .gap_2()
            .when(!enabled, |row| row.opacity(0.4))
            .child(div().min_w_0().child(label))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(48.))
                            .text_center()
                            .rounded(px(6.))
                            .bg(theme.hover)
                            .font_family(crate::theme::monospace())
                            .text_size(px(12.))
                            .child(format!("{value} %")),
                    )
                    .child(slider::slider(
                        label,
                        label,
                        value,
                        enabled,
                        theme,
                        move |x, bounds, window, cx| start(&(x, bounds), window, cx),
                        move |value, window, cx| key(&value, window, cx),
                    )),
            )
    }

    fn toggle_glass(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.settings_page.busy || self.settings_page.glass_drag.is_some() {
            return;
        }
        let mut glass = cx.global::<Appearance>().glass();
        glass.enabled = !glass.enabled;
        self.preview_glass(glass, cx);
        self.save_glass(window, cx);
    }

    fn update_glass_slider(&self, zone: Zone, value: u8, cx: &mut Context<Self>) {
        let mut glass = cx.global::<Appearance>().glass();
        zone.set(&mut glass, value);
        self.preview_glass(glass, cx);
    }

    fn preview_glass(&self, glass: Glass, cx: &mut Context<Self>) {
        cx.global_mut::<Appearance>().glass_preview = Some(glass);
        cx.refresh_windows();
        cx.notify();
    }

    fn save_glass(&mut self, window: &Window, cx: &mut Context<Self>) {
        let appearance = cx.global_mut::<Appearance>();
        let glass = appearance.glass();
        if let Some(override_value) = &mut appearance.dev_glass {
            *override_value = glass;
            appearance.glass_preview = None;
            cx.refresh_windows();
        } else if self.settings.appearance.glass == glass {
            appearance.glass_preview = None;
        } else {
            let mut next = self.settings.clone();
            next.appearance.glass = glass;
            self.save_settings(next, window, cx);
        }
        cx.notify();
    }

    pub(super) fn finish_glass_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.settings_page.glass_drag.take().is_some() {
            self.save_glass(window, cx);
        }
    }

    fn glass_pointer(&self, cx: &Context<Self>) -> impl IntoElement {
        let workspace = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let moving = workspace.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture
                        && event.pressed_button == Some(MouseButton::Left)
                    {
                        let _ = moving.update(cx, |this, cx| {
                            if let Some(drag) = &this.settings_page.glass_drag {
                                this.update_glass_slider(
                                    drag.zone,
                                    slider::value_at(event.position.x, drag.bounds),
                                    cx,
                                );
                            }
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        let _ = workspace.update(cx, |this, cx| {
                            if let Some(drag) = &this.settings_page.glass_drag {
                                this.update_glass_slider(
                                    drag.zone,
                                    slider::value_at(event.position.x, drag.bounds),
                                    cx,
                                );
                                this.finish_glass_drag(window, cx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, WindowBackgroundAppearance, point};

    #[gpui::test]
    fn glass_previews_until_release_and_respects_reduce_transparency(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            cx.global_mut::<Appearance>().reduce_transparency = false;
            workspace.open_settings(window, cx);
            workspace.settings_page.scroll.scroll_to_top_of_item(1);
            workspace.toggle_glass(window, cx);
        });
        cx.run_until_parked();
        workspace.read_with(cx, |_, cx| {
            assert_eq!(
                cx.global::<Appearance>().window_background(),
                WindowBackgroundAppearance::Blurred
            );
        });
        let track = cx
            .debug_bounds("Barra lateral y cabecera")
            .expect("a visible slider");
        let click = point(
            track.left() + track.size.width * (25. / 70.),
            track.center().y,
        );
        cx.simulate_mouse_down(click, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            let theme = Theme::current(window, cx);
            assert_eq!(theme.chrome(theme.background).a, 0.55);
        });
        assert_eq!(
            Settings::load(dir.path()).unwrap().appearance.glass.chrome,
            70
        );
        cx.simulate_mouse_up(click, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            Settings::load(dir.path()).unwrap().appearance.glass.chrome,
            55
        );
        cx.update(|window, cx| {
            let appearance = cx.global_mut::<Appearance>();
            appearance.reduce_transparency = true;
            assert_eq!(
                appearance.window_background(),
                WindowBackgroundAppearance::Opaque
            );
            let theme = Theme::current(window, cx);
            assert_eq!(theme.chrome(theme.background).a, 1.);
            assert_eq!(theme.content(theme.panel).a, 1.);
        });
    }
}
