mod components;
mod dev;
mod file_metadata;
mod file_workspace;
mod form;
mod pane;
mod pane_icons;
mod sidebar;
mod theme;
mod tree;
mod workspace;

use std::time::Instant;

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, QuitMode, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, px, size,
};

use workspace::Workspace;

actions!(vscp, [Quit]);

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

    let client = std::env::current_exe()
        .and_then(vscp_core::Client::new)
        .ok();
    gpui_platform::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            if let Some(appearance) = dev::appearance() {
                cx.set_window_appearance(Some(appearance));
            }
            workspace::bind_keys(cx);
            pane::bind_keys(cx);
            components::text_input::bind_keys(cx);
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([Menu::new("vSCP").items([MenuItem::action("Salir de vSCP", Quit)])]);

            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                            None,
                            size(px(dev::window_width().unwrap_or(1100.)), px(700.)),
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
                        let workspace =
                            cx.new(|cx| Workspace::new(client, trace_startup, window, cx));
                        let weak = workspace.downgrade();
                        window.on_window_should_close(cx, move |window, cx| {
                            weak.update(cx, |workspace, cx| workspace.request_quit(window, cx))
                                .is_err()
                        });
                        workspace
                    },
                )
                .expect("No se pudo abrir la ventana de vSCP");
            cx.on_action(move |_: &Quit, cx| {
                if window
                    .update(cx, |workspace, window, cx| {
                        workspace.request_quit(window, cx)
                    })
                    .is_err()
                {
                    cx.quit();
                }
            });
            cx.activate(true);
        });
}
