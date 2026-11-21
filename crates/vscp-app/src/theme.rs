use gpui::{Rgba, WindowAppearance, rgb};

pub struct Theme {
    pub background: Rgba,
    pub sidebar: Rgba,
    pub panel: Rgba,
    pub text: Rgba,
    pub secondary_text: Rgba,
    pub border: Rgba,
    pub selection: Rgba,
    pub accent: Rgba,
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
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
        }
    }
}
