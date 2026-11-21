pub mod theme;

use std::time::Instant;

use gpui::{
    App, AppContext, Bounds, Context, FontWeight, IntoElement, KeyBinding, Menu, MenuItem,
    QuitMode, Render, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, size,
};

use theme::Theme;

actions!(vscp, [Quit]);

struct Workspace {
    trace_startup: Option<Instant>,
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(started_at) = self.trace_startup.take() {
            window.on_next_frame(move |_, _| {
                eprintln!(
                    "vSCP startup: {:.2} ms",
                    started_at.elapsed().as_secs_f64() * 1000.0
                );
            });
        }

        let theme = Theme::for_appearance(window.appearance());
        div()
            .flex()
            .size_full()
            .bg(theme.background)
            .font_family(".SystemUIFont")
            .text_size(px(13.0))
            .text_color(theme.text)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(232.0))
                    .border_r_1()
                    .border_color(theme.border)
                    .bg(theme.sidebar)
                    .p_4()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Servidores"),
                    )
                    .child(
                        div()
                            .text_color(theme.secondary_text)
                            .child("Sin servidores"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.panel)
                    .text_color(theme.secondary_text)
                    .child("Conecta un servidor para empezar"),
            )
    }
}

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "askpass")
    {
        std::process::exit(vscp_core::askpass::client_main());
    }

    let started_at = Instant::now();
    let trace_startup = std::env::var("VSCP_TRACE_STARTUP")
        .is_ok_and(|value| value == "1")
        .then_some(started_at);

    gpui_platform::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([Menu::new("vSCP").items([MenuItem::action("Salir de vSCP", Quit)])]);

            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1100.0), px(700.0)),
                        cx,
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("vSCP".into()),
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(640.0), px(420.0))),
                    window_background: WindowBackgroundAppearance::Opaque,
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
                            .detach();
                        Workspace { trace_startup }
                    })
                },
            )
            .expect("No se pudo abrir la ventana de vSCP");
            cx.activate(true);
        });
}
