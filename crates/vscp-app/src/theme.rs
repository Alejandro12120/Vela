use gpui::{Rgba, Window, WindowAppearance, rgb, rgba};

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
}

impl Theme {
    pub fn light() -> Self {
        Self {
            background: rgb(0xffffff),
            sidebar: rgb(0xfafafa),
            panel: rgb(0xffffff),
            text: rgb(0x292929),
            secondary_text: rgb(0x5d5d5d),
            border: rgb(0xf2f2f2),
            selection: rgb(0xf5f5f5),
            accent: rgb(0x2563eb),
            control_border: rgb(0x8a8a8a),
            hover: rgb(0xf8f8f8),
            primary: rgb(0x292929),
            primary_text: rgb(0xffffff),
            primary_hover: rgb(0x414141),
            overlay: rgba(0x00000055).into(),
            raised: rgb(0xffffff),
            text_selection: rgba(0x2563eb40).into(),
            success: rgb(0x166534),
            warning: rgb(0x92400e),
            warning_bg: rgb(0xfffbeb),
            danger: rgb(0xb91c1c),
            danger_bg: rgb(0xfef2f2),
        }
    }

    pub fn dark() -> Self {
        Self {
            background: rgb(0x1c1c1c),
            sidebar: rgb(0x202020),
            panel: rgb(0x1c1c1c),
            text: rgb(0xe5e5e5),
            secondary_text: rgb(0xa1a1a1),
            border: rgb(0x282828),
            selection: rgb(0x2c2c2c),
            accent: rgb(0x60a5fa),
            control_border: rgb(0x747474),
            hover: rgb(0x282828),
            primary: rgb(0xe5e5e5),
            primary_text: rgb(0x1c1c1c),
            primary_hover: rgb(0xc9c9c9),
            overlay: rgba(0x00000088).into(),
            raised: rgb(0x252525),
            text_selection: rgba(0x60a5fa55).into(),
            success: rgb(0x86efac),
            warning: rgb(0xfcd34d),
            warning_bg: rgb(0x382d18),
            danger: rgb(0xfca5a5),
            danger_bg: rgb(0x3c2020),
        }
    }

    pub fn for_window(window: &Window) -> Self {
        Self::for_appearance(crate::dev::appearance().unwrap_or_else(|| window.appearance()))
    }

    fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
        }
    }
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
            std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "monospace"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .filter(|family| !family.is_empty())
                .unwrap_or_else(|| "DejaVu Sans Mono".into())
        })
    }
}
