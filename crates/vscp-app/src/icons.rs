//! Lucide icons (ISC), embedded in the binary. See `assets/icons/README.md`.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString, Styled, Svg, px, svg};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconName {
    Back,
    Forward,
    Up,
    Refresh,
    Eye,
    EyeOff,
    Sidebar,
    ChevronRight,
    ChevronDown,
    Folder,
    SquareTerminal,
    FolderOpen,
    File,
    FileText,
    Symlink,
    ArrowRight,
    ArrowLeft,
    Upload,
    Download,
    Pin,
    PinOff,
    Check,
    Plus,
    Settings,
    Close,
    Trash,
    Pencil,
    FolderPlus,
    Play,
    ExternalLink,
    Undo,
    Warning,
    Error,
    Key,
    Lock,
    Plug,
    Unplug,
    Spinner,
    Edits,
}

impl IconName {
    #[cfg(test)]
    pub const ALL: &'static [IconName] = &[
        IconName::Back,
        IconName::Forward,
        IconName::Up,
        IconName::Refresh,
        IconName::Eye,
        IconName::EyeOff,
        IconName::Sidebar,
        IconName::ChevronRight,
        IconName::ChevronDown,
        IconName::Folder,
        IconName::SquareTerminal,
        IconName::FolderOpen,
        IconName::File,
        IconName::FileText,
        IconName::Symlink,
        IconName::ArrowRight,
        IconName::ArrowLeft,
        IconName::Upload,
        IconName::Download,
        IconName::Pin,
        IconName::PinOff,
        IconName::Check,
        IconName::Plus,
        IconName::Settings,
        IconName::Close,
        IconName::Trash,
        IconName::Pencil,
        IconName::FolderPlus,
        IconName::Play,
        IconName::ExternalLink,
        IconName::Undo,
        IconName::Warning,
        IconName::Error,
        IconName::Key,
        IconName::Lock,
        IconName::Plug,
        IconName::Unplug,
        IconName::Spinner,
        IconName::Edits,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Self::Back => "icons/chevron-left.svg",
            Self::Forward => "icons/chevron-right.svg",
            Self::Up => "icons/arrow-up.svg",
            Self::Refresh => "icons/rotate-cw.svg",
            Self::Eye => "icons/eye.svg",
            Self::EyeOff => "icons/eye-off.svg",
            Self::Sidebar => "icons/panel-left.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::SquareTerminal => "icons/square-terminal.svg",
            Self::Folder => "icons/folder.svg",
            Self::FolderOpen => "icons/folder-open.svg",
            Self::File => "icons/file.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Symlink => "icons/file-symlink.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::Upload => "icons/upload.svg",
            Self::Download => "icons/download.svg",
            Self::Pin => "icons/pin.svg",
            Self::PinOff => "icons/pin-off.svg",
            Self::Check => "icons/check.svg",
            Self::Plus => "icons/plus.svg",
            Self::Settings => "icons/settings.svg",
            Self::Close => "icons/x.svg",
            Self::Trash => "icons/trash-2.svg",
            Self::Pencil => "icons/pencil.svg",
            Self::FolderPlus => "icons/folder-plus.svg",
            Self::Play => "icons/play.svg",
            Self::ExternalLink => "icons/external-link.svg",
            Self::Undo => "icons/undo-2.svg",
            Self::Warning => "icons/triangle-alert.svg",
            Self::Error => "icons/circle-alert.svg",
            Self::Key => "icons/key-round.svg",
            Self::Lock => "icons/lock.svg",
            Self::Plug => "icons/plug.svg",
            Self::Unplug => "icons/unplug.svg",
            Self::Spinner => "icons/loader-circle.svg",
            Self::Edits => "icons/file-pen.svg",
        }
    }
}

const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/square-terminal.svg",
        include_bytes!("../assets/icons/square-terminal.svg"),
    ),
    (
        "icons/chevron-left.svg",
        include_bytes!("../assets/icons/chevron-left.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/arrow-up.svg",
        include_bytes!("../assets/icons/arrow-up.svg"),
    ),
    (
        "icons/rotate-cw.svg",
        include_bytes!("../assets/icons/rotate-cw.svg"),
    ),
    ("icons/eye.svg", include_bytes!("../assets/icons/eye.svg")),
    (
        "icons/eye-off.svg",
        include_bytes!("../assets/icons/eye-off.svg"),
    ),
    (
        "icons/panel-left.svg",
        include_bytes!("../assets/icons/panel-left.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/folder.svg",
        include_bytes!("../assets/icons/folder.svg"),
    ),
    (
        "icons/folder-open.svg",
        include_bytes!("../assets/icons/folder-open.svg"),
    ),
    ("icons/file.svg", include_bytes!("../assets/icons/file.svg")),
    (
        "icons/file-text.svg",
        include_bytes!("../assets/icons/file-text.svg"),
    ),
    (
        "icons/file-symlink.svg",
        include_bytes!("../assets/icons/file-symlink.svg"),
    ),
    (
        "icons/arrow-right.svg",
        include_bytes!("../assets/icons/arrow-right.svg"),
    ),
    (
        "icons/arrow-left.svg",
        include_bytes!("../assets/icons/arrow-left.svg"),
    ),
    (
        "icons/upload.svg",
        include_bytes!("../assets/icons/upload.svg"),
    ),
    (
        "icons/download.svg",
        include_bytes!("../assets/icons/download.svg"),
    ),
    ("icons/pin.svg", include_bytes!("../assets/icons/pin.svg")),
    (
        "icons/pin-off.svg",
        include_bytes!("../assets/icons/pin-off.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../assets/icons/check.svg"),
    ),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    (
        "icons/settings.svg",
        include_bytes!("../assets/icons/settings.svg"),
    ),
    ("icons/x.svg", include_bytes!("../assets/icons/x.svg")),
    (
        "icons/trash-2.svg",
        include_bytes!("../assets/icons/trash-2.svg"),
    ),
    (
        "icons/pencil.svg",
        include_bytes!("../assets/icons/pencil.svg"),
    ),
    (
        "icons/folder-plus.svg",
        include_bytes!("../assets/icons/folder-plus.svg"),
    ),
    ("icons/play.svg", include_bytes!("../assets/icons/play.svg")),
    (
        "icons/external-link.svg",
        include_bytes!("../assets/icons/external-link.svg"),
    ),
    (
        "icons/undo-2.svg",
        include_bytes!("../assets/icons/undo-2.svg"),
    ),
    (
        "icons/triangle-alert.svg",
        include_bytes!("../assets/icons/triangle-alert.svg"),
    ),
    (
        "icons/circle-alert.svg",
        include_bytes!("../assets/icons/circle-alert.svg"),
    ),
    (
        "icons/key-round.svg",
        include_bytes!("../assets/icons/key-round.svg"),
    ),
    ("icons/lock.svg", include_bytes!("../assets/icons/lock.svg")),
    ("icons/plug.svg", include_bytes!("../assets/icons/plug.svg")),
    (
        "icons/unplug.svg",
        include_bytes!("../assets/icons/unplug.svg"),
    ),
    (
        "icons/loader-circle.svg",
        include_bytes!("../assets/icons/loader-circle.svg"),
    ),
    (
        "icons/file-pen.svg",
        include_bytes!("../assets/icons/file-pen.svg"),
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

/// A 16 px icon. Callers set the colour with `.text_color(...)` and may resize it with `.size(...)`.
pub fn icon(name: IconName) -> Svg {
    svg().path(name.path()).size(px(16.)).flex_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_as_thin_svg() {
        for &name in IconName::ALL {
            let bytes = Assets
                .load(name.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{} is missing", name.path()));
            let text = std::str::from_utf8(&bytes).unwrap();
            // Lucide files open with a license comment before the root element.
            let body = match text.strip_prefix("<!--") {
                Some(rest) => rest.split_once("-->").unwrap().1.trim_start(),
                None => text,
            };
            assert!(body.starts_with("<svg"), "{}", name.path());
            assert!(text.contains("stroke-width=\"1.75\""), "{}", name.path());
        }
    }

    #[test]
    fn unknown_path_is_none() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert!(Assets.load("").unwrap().is_none());
    }

    #[test]
    fn list_filters_by_prefix() {
        assert_eq!(Assets.list("icons/").unwrap().len(), ICONS.len());
        assert!(Assets.list("other/").unwrap().is_empty());
    }
}
