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
