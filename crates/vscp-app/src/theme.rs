use gpui::{App, Rgba, Window, rgba};
use vscp_core::appearance::{Colors, TerminalColors, VSCP_DARK, VSCP_LIGHT};

use crate::appearance::{Appearance, Variant, system_variant};

#[derive(Clone, Copy)]

pub struct Theme {
    pub background: Rgba,
    pub sidebar: Rgba,
    pub panel: Rgba,
    pub text: Rgba,
    pub secondary_text: Rgba,
    pub border: Rgba,
    pub selection: Rgba,
    pub accent: Rgba,
    pub control_border: Rgba,
    pub hover: Rgba,
    pub primary: Rgba,
    pub primary_text: Rgba,
    pub primary_hover: Rgba,
    pub overlay: gpui::Hsla,
    pub raised: Rgba,
    pub text_selection: gpui::Hsla,
    pub success: Rgba,
    pub warning: Rgba,
    pub warning_bg: Rgba,
    pub danger: Rgba,
    pub danger_bg: Rgba,
    pub terminal: TerminalColors,
}

impl Theme {
    pub fn from_colors(colors: &Colors) -> Self {
        Self {
            background: rgba(colors.background),
            sidebar: rgba(colors.sidebar),
            panel: rgba(colors.panel),
            text: rgba(colors.text),
            secondary_text: rgba(colors.secondary_text),
            border: rgba(colors.border),
            selection: rgba(colors.selection),
            accent: rgba(colors.accent),
            control_border: rgba(colors.control_border),
            hover: rgba(colors.hover),
            primary: rgba(colors.primary),
            primary_text: rgba(colors.primary_text),
            primary_hover: rgba(colors.primary_hover),
            overlay: rgba(colors.overlay).into(),
            raised: rgba(colors.raised),
            text_selection: rgba(colors.text_selection).into(),
            success: rgba(colors.success),
            warning: rgba(colors.warning),
            warning_bg: rgba(colors.warning_bg),
            danger: rgba(colors.danger),
            danger_bg: rgba(colors.danger_bg),
            terminal: colors.terminal,
        }
    }

    pub fn current(window: &Window, cx: &App) -> Self {
        if let Some(appearance) = cx.try_global::<Appearance>() {
            appearance.current_theme(window)
        } else {
            Self::from_colors(match system_variant(window.appearance()) {
                Variant::Light => &VSCP_LIGHT,
                Variant::Dark => &VSCP_DARK,
            })
        }
    }
}

/// Compares font family names case-insensitively, ignoring spaces and hyphens.
pub(crate) fn family_key(value: &str) -> String {
    value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'-')
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect()
}

pub fn monospace() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::sync::OnceLock;
        static FAMILY: OnceLock<String> = OnceLock::new();
        FAMILY.get_or_init(|| {
            let matched = std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "monospace:spacing=100"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .filter(|family| {
                    !family.is_empty() && !family.to_ascii_lowercase().contains("cjk")
                });
            if let Some(family) = matched {
                return family;
            }
            let installed = std::process::Command::new("fc-list")
                .args([":spacing=mono", "family"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
                .unwrap_or_default();
            const CANDIDATES: [&str; 10] = [
                "JetBrainsMono Nerd Font",
                "JetBrains Mono",
                "CaskaydiaCove Nerd Font Mono",
                "Cascadia Mono",
                "FiraCode Nerd Font",
                "Fira Code",
                "Hack",
                "DejaVu Sans Mono",
                "Noto Sans Mono",
                "Liberation Mono",
            ];
            CANDIDATES
                .iter()
                .copied()
                .find(|family| {
                    installed.lines().any(|line| {
                        line.split(',')
                            .any(|name| family_key(name) == family_key(family))
                    })
                })
                .unwrap_or("DejaVu Sans Mono")
                .to_owned()
        })
    }
}
