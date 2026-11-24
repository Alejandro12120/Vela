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

#[cfg(test)]
thread_local! {
    /// Lets a test own the configuration directory without touching the
    /// process environment. GPUI tests run their tasks on the test thread.
    pub static TEST_CONFIG: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    pub static TEST_EDITOR_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    pub static TEST_EDITOR_PICKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn config_override() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG.with(|dir| dir.borrow().clone()) {
        return Some(dir);
    }
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

pub fn theme_ids() -> Option<(String, String)> {
    let value = value("VSCP_DEV_THEME_ID")?;
    Some(match value.split_once(',') {
        Some((light, dark)) => (light.into(), dark.into()),
        None => (value.clone(), value),
    })
}

pub fn settings_page() -> Option<String> {
    value("VSCP_DEV_SETTINGS")
}

pub fn hide_sidebar() -> bool {
    value("VSCP_DEV_HIDE_SIDEBAR").as_deref() == Some("1")
}

pub fn reduce_transparency() -> Option<bool> {
    match value("VSCP_DEV_REDUCE_TRANSPARENCY")?.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub fn glass() -> Option<vscp_core::appearance::Glass> {
    use crate::components::slider::{MAX, MIN};
    let value = value("VSCP_DEV_GLASS")?;
    let (chrome, content) = value.split_once(',')?;
    let (chrome, content) = (chrome.parse().ok()?, content.parse().ok()?);
    ((MIN..=MAX).contains(&chrome) && (MIN..=MAX).contains(&content)).then_some(
        vscp_core::appearance::Glass {
            enabled: true,
            chrome,
            content,
        },
    )
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

pub fn file_drag(side: crate::pane::Side) -> Option<(PathBuf, PathBuf)> {
    use crate::pane::Side;
    use std::path::Component;

    let value = value("VSCP_DEV_FILE_DRAG")?;
    let mut fields = value.splitn(3, ':');
    let preview_side = match fields.next()? {
        "local" => Side::Local,
        "remote" => Side::Remote,
        _ => return None,
    };
    let source = PathBuf::from(fields.next()?);
    let hover = PathBuf::from(fields.next()?);
    (preview_side == side
        && !source.as_os_str().is_empty()
        && !hover.as_os_str().is_empty()
        && source
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && (hover == std::path::Path::new(".")
            || hover
                .components()
                .all(|part| matches!(part, Component::Normal(_)))))
    .then_some((source, hover))
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

/// Opens the floating menu of the server with this display name at its row's centre at startup.
pub fn row_menu() -> Option<String> {
    value("VSCP_DEV_ROW_MENU")
}

/// `<name>:<dy>` shows the card of the server with that display name dragged
/// `dy` px from its slot and holds it there, for visual checks.
pub fn drag() -> Option<(String, f32)> {
    let value = value("VSCP_DEV_DRAG")?;
    let (name, dy) = value.rsplit_once(':')?;
    Some((name.to_owned(), dy.parse().ok()?))
}

/// `1` or `0` overrides the system "reduce motion" setting.
pub fn reduce_motion() -> Option<bool> {
    match value("VSCP_DEV_REDUCE_MOTION")?.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub enum FormRequest {
    New,
    Edit(String),
    Duplicate(String),
}

/// `new` opens the new-server form at startup, `edit:<name>` opens the form of
/// that server, and `duplicate:<name>` opens it prefilled as a new copy.
pub fn form() -> Option<FormRequest> {
    let value = value("VSCP_DEV_FORM")?;
    match value.split_once(':') {
        Some(("edit", name)) => Some(FormRequest::Edit(name.to_owned())),
        Some(("duplicate", name)) => Some(FormRequest::Duplicate(name.to_owned())),
        None if value == "new" => Some(FormRequest::New),
        _ => None,
    }
}

pub fn edit_view() -> Option<String> {
    value("VSCP_DEV_EDIT_VIEW")
}

pub fn form_file() -> Option<PathBuf> {
    value("VSCP_DEV_FORM_FILE").map(PathBuf::from)
}

pub fn form_key() -> Option<String> {
    value("VSCP_DEV_FORM_KEY")
}

pub fn form_advanced() -> bool {
    value("VSCP_DEV_FORM_ADVANCED").as_deref() == Some("1")
}

pub fn form_password() -> bool {
    value("VSCP_DEV_FORM_PASSWORD").as_deref() == Some("1")
}

pub fn agent_edit() -> bool {
    value("VSCP_DEV_AGENT_EDIT").as_deref() == Some("1")
}

pub fn editor_menu() -> bool {
    value("VSCP_DEV_EDITOR_MENU").as_deref() == Some("1")
}

pub fn empty_confirm() -> bool {
    value("VSCP_DEV_EMPTY_CONFIRM").as_deref() == Some("1")
}

/// The editor picker is always shown on macOS. On Linux it is a capture hook.
pub fn editor_picker() -> bool {
    #[cfg(test)]
    if TEST_EDITOR_PICKER.get() {
        return true;
    }
    cfg!(target_os = "macos") || value("VSCP_DEV_EDITOR_PICKER").as_deref() == Some("1")
}

/// Roots searched for editor bundles, in order.
pub fn editor_roots() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_EDITOR_ROOT.with(|root| root.borrow().clone()) {
        return vec![root];
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::home_dir();
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = home {
            roots.push(home.join("Applications"));
        }
        roots.push(PathBuf::from("/System/Applications"));
        roots
    }
    #[cfg(not(target_os = "macos"))]
    {
        value("VSCP_DEV_EDITOR_ROOT")
            .map(|root| vec![PathBuf::from(root)])
            .unwrap_or_default()
    }
}

/// Opens `path` the way a double click should. Text goes to the editor chosen
/// in Ajustes or, without one, to the default text editor (`open -t`). Anything
/// else opens with its default application. The real path, with symbolic links
/// resolved, decides whether the file is text.
pub fn terminal_view() -> bool {
    value("VSCP_DEV_VIEW").as_deref() == Some("terminal")
}

pub fn terminal_input() -> Option<String> {
    value("VSCP_DEV_TERMINAL_INPUT").map(|text| {
        let mut chars = text.chars().peekable();
        let mut input = String::new();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                let escaped = match chars.peek() {
                    Some('n') => Some('\n'),
                    Some('r') => Some('\r'),
                    Some('\\') => Some('\\'),
                    _ => None,
                };
                if let Some(escaped) = escaped {
                    chars.next();
                    input.push(escaped);
                    continue;
                }
            }
            input.push(ch);
        }
        input
    })
}

pub fn kitty_conf() -> Option<PathBuf> {
    value("VSCP_DEV_KITTY_CONF").map(PathBuf::from)
}

/// Initial terminal font size in points, for captures without physical keys.
pub fn terminal_font_size() -> Option<f32> {
    value("VSCP_DEV_TERMINAL_FONT_SIZE")
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|size| size.is_finite() && *size > 0.)
}

pub fn open_file(path: &std::path::Path, editor: Option<&std::path::Path>) -> io::Result<()> {
    if launches_suppressed() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
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
    #[cfg(not(target_os = "macos"))]
    {
        let _ = editor;
        run(
            std::process::Command::new("xdg-open").arg(path),
            "No se pudo abrir el archivo",
        )
    }
}

/// Opens `path` with its default application, whatever its type.
pub fn open_default(path: &std::path::Path) -> io::Result<()> {
    if launches_suppressed() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        run(
            std::process::Command::new("/usr/bin/open").arg(path),
            "No se pudo abrir el archivo",
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        run(
            std::process::Command::new("xdg-open").arg(path),
            "No se pudo abrir el archivo",
        )
    }
}

/// Starts a new Kitty running `ssh` with `args`.
pub fn open_kitty_ssh(args: &[std::ffi::OsString]) -> io::Result<()> {
    if launches_suppressed() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        run(
            std::process::Command::new("/usr/bin/open")
                .args(["-na", "kitty", "--args", "/usr/bin/ssh"])
                .args(args),
            "No se pudo abrir Kitty",
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        run(
            std::process::Command::new("kitty")
                .args(["--detach", "/usr/bin/ssh"])
                .args(args),
            "No se pudo abrir Kitty",
        )
    }
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
