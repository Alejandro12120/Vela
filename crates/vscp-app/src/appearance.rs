use std::path::Path;

use gpui::{App, Global, Window, WindowAppearance};
use vscp_core::{
    appearance::{AppearanceSettings, Colors, Scheme, ThemeDef, builtin_themes, load_user_themes},
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
    dev_appearance: Option<WindowAppearance>,
    dev_theme: Option<String>,
    builtin_count: usize,
}

impl Global for Appearance {}

impl Default for Appearance {
    fn default() -> Self {
        let themes = builtin_themes();
        Self {
            settings: AppearanceSettings::default(),
            builtin_count: themes.len(),
            themes,
            errors: Vec::new(),
            dev_appearance: dev::appearance(),
            dev_theme: dev::theme_id(),
        }
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

    pub fn current_theme(&self, window: &Window) -> Theme {
        Theme::from_colors(self.variant_for(window).colors(self.active()))
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
        cx.refresh_windows();
        (settings, error)
    }
}
