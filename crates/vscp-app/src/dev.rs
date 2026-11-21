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

pub fn log(message: impl std::fmt::Display) {
    if value("VSCP_DEV_LOG").as_deref() == Some("1") {
        eprintln!("vSCP: {message}");
    }
}
