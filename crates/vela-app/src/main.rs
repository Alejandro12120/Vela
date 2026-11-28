mod accessibility;
mod appearance;
mod components;
mod context_menu;
mod dev;
mod drop_target;
mod file_metadata;
mod file_workspace;
mod form;
mod icons;
mod keybindings;
mod mcp_broker;
mod mcp_model;
mod move_paths;
mod pane;
#[cfg(target_os = "macos")]
mod pasteboard_macos;
mod rename;
mod sidebar;
mod staging;
mod terminal;
mod theme;
mod transfers_view;
mod tree;
mod workspace;

use std::time::Instant;

use gpui::{
    App, AppContext, Bounds, Menu, MenuItem, QuitMode, TitlebarOptions, WindowBounds, WindowKind,
    WindowOptions, actions, px, size,
};

use workspace::Workspace;

actions!(vela, [Quit]);

// AppKit's standard title bar on the reference Mac is 32 points high.
#[cfg(target_os = "macos")]
const MACOS_TITLEBAR_HEIGHT: f32 = 32.;

fn main() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "askpass")
    {
        std::process::exit(vela_core::askpass::client_main());
    }
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "mcp") {
        std::process::exit(vela_mcp::cli::main(std::env::args_os().skip(2)));
    }

    let started_at = Instant::now();
    let trace_startup = std::env::var("VELA_TRACE_STARTUP")
        .is_ok_and(|value| value == "1")
        .then_some(started_at);

    let client = std::env::current_exe()
        .and_then(vela_core::Client::new)
        .ok();
    gpui_platform::application()
        .with_assets(icons::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            if let Some(appearance) = dev::appearance() {
                cx.set_window_appearance(Some(appearance));
            }
            cx.set_reduce_motion(accessibility::reduce_motion());
            dev::trace_keystrokes(cx);
            terminal::font_size::initialize(cx);
            pane::finder::initialize(cx);
            workspace::bind_keys(cx);
            pane::bind_keys(cx);
            terminal::view::bind_keys(cx);
            components::text_input::bind_keys(cx);
            cx.bind_keys([keybindings::binding(cx, "secondary-q", Quit, None)]);
            cx.set_menus([Menu::new("Vela").items([MenuItem::action("Quit Vela", Quit)])]);

            cx.set_global(appearance::Appearance::default());
            let dir = dev::config_directory().ok();
            let migrate_legacy = dev::uses_default_config();
            let loading = cx.background_executor().spawn(async move {
                if migrate_legacy {
                    match vela_core::config::migrate_legacy_directory() {
                        Ok(true) => dev::log("migrated the vSCP configuration directory"),
                        Ok(false) => {}
                        Err(error) => dev::log(format_args!(
                            "couldn't migrate the vSCP configuration: {error}"
                        )),
                    }
                }
                dir.as_deref().map(appearance::Loaded::read)
            });
            cx.spawn(async move |cx| {
                let loaded = loading.await;
                cx.update(move |cx| {
                    let background = cx
                        .global::<appearance::Appearance>()
                        .initial_window_background(loaded.as_ref());
                    // FullSizeContentView includes the title bar in the requested height.
                    #[cfg(target_os = "macos")]
                    let (titlebar_height, traffic_lights) =
                        (MACOS_TITLEBAR_HEIGHT, Some(gpui::point(px(9.), px(9.))));
                    #[cfg(not(target_os = "macos"))]
                    let (titlebar_height, traffic_lights) = (0., None);
                    let window = cx
                        .open_window(
                            WindowOptions {
                                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                                    None,
                                    size(
                                        px(dev::window_width().unwrap_or(1100.)),
                                        px(700. + titlebar_height),
                                    ),
                                    cx,
                                ))),
                                kind: if dev::floating_window() {
                                    WindowKind::Floating
                                } else {
                                    WindowKind::Normal
                                },
                                titlebar: Some(TitlebarOptions {
                                    title: Some("Vela".into()),
                                    appears_transparent: cfg!(target_os = "macos"),
                                    traffic_light_position: traffic_lights,
                                }),
                                window_min_size: Some(size(px(640.0), px(420. + titlebar_height))),
                                window_background: background,
                                app_id: Some("vela".into()),
                                ..Default::default()
                            },
                            |window, cx| {
                                #[cfg(target_os = "linux")]
                                window.on_next_frame(|_, cx| {
                                    cx.background_executor()
                                        .spawn(async {
                                            let _ = crate::theme::monospace();
                                        })
                                        .detach();
                                });
                                let workspace = cx.new(|cx| {
                                    Workspace::new(client, trace_startup, loaded, window, cx)
                                });
                                let weak = workspace.downgrade();
                                window.on_window_should_close(cx, move |window, cx| {
                                    weak.update(cx, |workspace, cx| {
                                        workspace.request_quit(window, cx)
                                    })
                                    .is_err()
                                });
                                workspace
                            },
                        )
                        .expect("Couldn't open the Vela window");
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
            })
            .detach();
        });
}
