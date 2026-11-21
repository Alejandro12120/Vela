use std::{io, path::PathBuf};

use gpui::WindowAppearance;
use vscp_core::{ConnectionSpec, config};

fn value(key: &str) -> Option<String> {
    if cfg!(any(test, feature = "dev")) {
        std::env::var(key).ok()
    } else {
        None
    }
}

fn config_override() -> Option<PathBuf> {
    value("VSCP_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn config_directory() -> io::Result<PathBuf> {
    config_override().map_or_else(config::default_directory, Ok)
}

pub fn isolate_known_hosts(spec: &mut ConnectionSpec) {
    if let Some(dir) = config_override() {
        spec.known_hosts = Some(dir.join("known_hosts"));
    }
}

pub fn autoconnect() -> Option<String> {
    value("VSCP_DEV_AUTOCONNECT")
}

pub fn appearance() -> Option<WindowAppearance> {
    match value("VSCP_DEV_THEME").as_deref() {
        Some("light") => Some(WindowAppearance::Light),
        Some("dark") => Some(WindowAppearance::Dark),
        _ => None,
    }
}

pub fn logging() -> bool {
    value("VSCP_DEV_LOG").as_deref() == Some("1")
}

pub fn log(message: impl std::fmt::Display) {
    if logging() {
        eprintln!("vSCP: {message}");
    }
}

/// Prints each keystroke with its resolved action and key context stack when
/// `VSCP_DEV_LOG=1`. The subscription lives as long as the application.
pub fn trace_keystrokes(cx: &mut gpui::App) {
    if !cfg!(feature = "dev") || !logging() {
        return;
    }
    cx.observe_keystrokes(|event, _, _| {
        let action = event.action.as_ref().map_or("none", |action| action.name());
        eprintln!(
            "[keys] {:?} action={action} contexts={:?}",
            event.keystroke, event.context_stack
        );
    })
    .detach();
}

pub fn remote_delay() -> std::time::Duration {
    value("VSCP_DEV_REMOTE_DELAY_MS")
        .and_then(|value| value.parse().ok())
        .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis)
}

pub fn local_root() -> Option<PathBuf> {
    value("VSCP_DEV_LOCAL_ROOT")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn expanded_paths() -> Vec<PathBuf> {
    value("VSCP_DEV_EXPAND")
        .into_iter()
        .flat_map(|value| value.split(',').map(PathBuf::from).collect::<Vec<_>>())
        .filter(|path| {
            path.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        })
        .collect()
}

pub fn window_width() -> Option<f32> {
    value("VSCP_DEV_WINDOW_WIDTH")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|&width| width >= 640)
        .map(f32::from)
}

pub fn floating_window() -> bool {
    value("VSCP_DEV_WINDOW_FLOAT").as_deref() == Some("1")
}

pub fn transfer() -> Option<vscp_core::transfer::Spec> {
    use vscp_core::transfer::{Direction, Spec};
    let value = value("VSCP_DEV_TRANSFER")?;
    let mut parts = value.splitn(3, ':');
    let direction = match parts.next()? {
        "upload" => Direction::Upload,
        "download" => Direction::Download,
        _ => return None,
    };
    let source = PathBuf::from(parts.next()?);
    let destination = PathBuf::from(parts.next()?);
    (source.is_absolute() && destination.is_absolute()).then(|| Spec {
        direction,
        sources: vec![source],
        destination,
    })
}

pub fn transfer_expanded() -> bool {
    value("VSCP_DEV_TRANSFER_EXPANDED").as_deref() == Some("1")
}

pub fn file_state() -> Option<String> {
    value("VSCP_DEV_FILE_STATE")
}

pub fn transfer_delay() -> std::time::Duration {
    value("VSCP_DEV_TRANSFER_DELAY_MS")
        .and_then(|value| value.parse().ok())
        .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis)
}

pub fn copy_paths() -> Vec<PathBuf> {
    value("VSCP_DEV_COPY")
        .into_iter()
        .flat_map(|paths| paths.split(',').map(PathBuf::from).collect::<Vec<_>>())
        .collect()
}

pub fn paste_remote() -> bool {
    value("VSCP_DEV_PASTE").as_deref() == Some("remote")
}

pub fn staging_root() -> Option<PathBuf> {
    value("VSCP_DEV_STAGING_ROOT").map(PathBuf::from)
}

pub fn drop_highlight() -> bool {
    value("VSCP_DEV_DROP_HIGHLIGHT").as_deref() == Some("1")
}

pub fn edit_path() -> Option<PathBuf> {
    value("VSCP_DEV_EDIT")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn edit_root() -> io::Result<PathBuf> {
    if let Some(dir) = config_override() {
        return Ok(dir.join("edit-cache"));
    }
    std::env::home_dir()
        .map(|home| home.join("Library/Caches/vSCP/edit"))
        .ok_or_else(|| io::Error::other("No se encuentra la carpeta de caché"))
}

pub fn edit_view() -> Option<String> {
    value("VSCP_DEV_EDIT_VIEW")
}

/// Opens `path` the way a double click should. Text goes to the editor chosen
/// in Ajustes or, without one, to the default text editor (`open -t`). Anything
/// else opens with its default application. The real path, with symbolic links
/// resolved, decides whether the file is text.
pub fn open_file(path: &std::path::Path, editor: Option<&std::path::Path>) -> io::Result<()> {
    if launches_suppressed() {
        return Ok(());
    }
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut command = std::process::Command::new("/usr/bin/open");
    if vscp_core::local::is_text(&real)? {
        match editor {
            Some(editor) => command.arg("-a").arg(editor),
            None => command.arg("-t"),
        };
    }
    run(command.arg(path), "No se pudo abrir el archivo")
}

/// Opens `path` with its default application, whatever its type.
pub fn open_default(path: &std::path::Path) -> io::Result<()> {
    if launches_suppressed() {
        return Ok(());
    }
    run(
        std::process::Command::new("/usr/bin/open").arg(path),
        "No se pudo abrir el archivo",
    )
}

/// Starts a new Kitty running `ssh` with `args`.
pub fn open_kitty_ssh(args: &[std::ffi::OsString]) -> io::Result<()> {
    if launches_suppressed() {
        return Ok(());
    }
    run(
        std::process::Command::new("/usr/bin/open")
            .args(["-na", "kitty", "--args", "/usr/bin/ssh"])
            .args(args),
        "No se pudo abrir Kitty",
    )
}

fn launches_suppressed() -> bool {
    value("VSCP_DEV_EDITOR").as_deref() == Some("none")
}

// Reap the child and check Launch Services' exit status on the caller's
// background executor.
fn run(command: &mut std::process::Command, failure: &str) -> io::Result<()> {
    if command.status()?.success() {
        Ok(())
    } else {
        Err(io::Error::other(failure.to_owned()))
    }
}
