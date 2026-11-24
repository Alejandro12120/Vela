use std::path::Path;

use gpui::{App, Global, Window, WindowAppearance, WindowBackgroundAppearance};
use vscp_core::{
    appearance::{
        AppearanceSettings, Colors, Glass, Scheme, ThemeDef, builtin_themes, load_user_themes,
    },
    settings::Settings,
};

use crate::{dev, theme::Theme};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Light,
    Dark,
}

impl Variant {
    pub fn colors(self, theme: &ThemeDef) -> &Colors {
        match self {
            Self::Light => &theme.light,
            Self::Dark => &theme.dark,
        }
    }
}

pub struct Appearance {
    pub settings: AppearanceSettings,
    pub themes: Vec<ThemeDef>,
    pub errors: Vec<String>,
    pub preview: Option<(ThemeDef, Variant)>,
    pub reduce_transparency: bool,
    pub dev_glass: Option<Glass>,
    pub glass_preview: Option<Glass>,
    dev_appearance: Option<WindowAppearance>,
    dev_theme: Option<String>,
    builtin_count: usize,
    render_light: Theme,
    render_dark: Theme,
}

impl Global for Appearance {}

impl Default for Appearance {
    fn default() -> Self {
        let themes = builtin_themes();
        let mut appearance = Self {
            settings: AppearanceSettings::default(),
            builtin_count: themes.len(),
            render_light: Theme::from_colors(&themes[0].light),
            render_dark: Theme::from_colors(&themes[0].dark),
            themes,
            errors: Vec::new(),
            preview: None,
            reduce_transparency: crate::accessibility::reduce_transparency(),
            dev_glass: dev::glass(),
            glass_preview: None,
            dev_appearance: dev::appearance(),
            dev_theme: dev::theme_id(),
        };
        appearance.rebuild();
        appearance
    }
}

impl Appearance {
    pub fn active(&self) -> &ThemeDef {
        let id = self.dev_theme.as_ref().unwrap_or(&self.settings.theme);
        self.themes
            .iter()
            .find(|theme| theme.id == *id)
            .unwrap_or(&self.themes[0])
    }

    pub fn replace_users(&mut self, themes: Vec<ThemeDef>, errors: Vec<String>) {
        self.themes.truncate(self.builtin_count);
        self.themes.extend(themes);
        self.errors = errors;
        self.rebuild();
    }

    pub fn variant_for(&self, window: &Window) -> Variant {
        match self.settings.scheme {
            Scheme::Light => Variant::Light,
            Scheme::Dark => Variant::Dark,
            Scheme::System => {
                system_variant(self.dev_appearance.unwrap_or_else(|| window.appearance()))
            }
        }
    }

    pub fn update_native_appearance(cx: &mut App) {
        let appearance = cx.global::<Self>();
        let variant =
            appearance.preview.as_ref().map(|(_, variant)| *variant).or(
                match appearance.settings.scheme {
                    Scheme::System => None,
                    Scheme::Light => Some(Variant::Light),
                    Scheme::Dark => Some(Variant::Dark),
                },
            );
        cx.set_window_appearance(
            variant
                .map(|variant| match variant {
                    Variant::Light => WindowAppearance::Light,
                    Variant::Dark => WindowAppearance::Dark,
                })
                .or(appearance.dev_appearance),
        );
    }

    pub fn set_settings(&mut self, settings: AppearanceSettings) {
        self.settings = settings;
        self.rebuild();
    }

    pub fn set_preview(&mut self, preview: Option<(ThemeDef, Variant)>) {
        self.preview = preview;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let (light, dark) = if let Some((draft, variant)) = &self.preview {
            let colors = variant.colors(draft);
            (colors, colors)
        } else {
            let active = self.active();
            (&active.light, &active.dark)
        };
        (self.render_light, self.render_dark) =
            (Theme::from_colors(light), Theme::from_colors(dark));
    }

    pub fn glass(&self) -> Glass {
        let mut glass = self
            .glass_preview
            .or(self.dev_glass)
            .unwrap_or(self.settings.glass);
        use crate::components::slider::{MAX, MIN};
        glass.chrome = glass.chrome.clamp(MIN, MAX);
        glass.content = glass.content.clamp(MIN, MAX);
        glass
    }

    pub fn initial_window_background(&self, loaded: Option<&Loaded>) -> WindowBackgroundAppearance {
        let glass = self
            .dev_glass
            .or_else(|| {
                loaded?
                    .settings
                    .as_ref()
                    .ok()
                    .map(|settings| settings.appearance.glass)
            })
            .unwrap_or_default();
        self.background_appearance(glass)
    }

    pub fn window_background(&self) -> WindowBackgroundAppearance {
        self.background_appearance(self.glass())
    }

    fn background_appearance(&self, glass: Glass) -> WindowBackgroundAppearance {
        if glass.enabled && !self.reduce_transparency {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        }
    }

    pub fn current_theme(&self, window: &Window) -> Theme {
        let mut glass = self.glass();
        glass.enabled &= !self.reduce_transparency;
        match self.variant_for(window) {
            Variant::Light => self.render_light,
            Variant::Dark => self.render_dark,
        }
        .with_glass(glass)
    }
}

pub fn system_variant(appearance: WindowAppearance) -> Variant {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Variant::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Variant::Dark,
    }
}

/// The appearance is read before the native window opens so its first frame uses
/// the chosen theme. Server configuration and edit recovery can finish afterwards.
pub struct Loaded {
    settings: std::io::Result<Settings>,
    users: Vec<ThemeDef>,
    errors: Vec<String>,
}

impl Loaded {
    pub fn read(dir: &Path) -> Self {
        let settings = Settings::load(dir);
        let (users, errors) = load_user_themes(dir);
        Self {
            settings,
            users,
            errors,
        }
    }

    pub fn install(self, cx: &mut App) -> (Settings, Option<String>) {
        let (settings, error) = match self.settings {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings::default(),
                Some(format!("No se pudo leer settings.toml. {error}")),
            ),
        };
        let appearance = cx.global_mut::<Appearance>();
        appearance.settings = settings.appearance.clone();
        appearance.replace_users(self.users, self.errors);
        Appearance::update_native_appearance(cx);
        cx.refresh_windows();
        (settings, error)
    }
}
