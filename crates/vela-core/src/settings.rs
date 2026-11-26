//! Preferences shared by the desktop application.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::appearance::AppearanceSettings;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalColorSource {
    #[default]
    Theme,
    Kitty,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Settings {
    pub editor: Option<PathBuf>,
    /// Global SSH agent socket. When absent, the first detected Bitwarden
    /// socket is used, if any.
    pub agent_socket: Option<PathBuf>,
    /// Terminal font size in points. When absent, `kitty.conf` decides.
    pub terminal_font_size: Option<f32>,
    /// Whether selecting terminal text copies it. When absent, `kitty.conf` decides.
    pub terminal_copy_on_select: Option<bool>,
    /// Terminal palette source. Kitty falls back to the theme without a configuration file.
    pub terminal_colors: TerminalColorSource,
    pub appearance: AppearanceSettings,
}

impl Settings {
    pub fn load(dir: &Path) -> io::Result<Self> {
        match fs::read_to_string(dir.join("settings.toml")) {
            Ok(text) => {
                toml::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        Self::load(dir)?;
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        crate::config::save_private(dir, "settings.toml", &text)
    }

    pub(crate) fn import_defaults(&mut self, legacy: Self) {
        fn fill<T: PartialEq>(value: &mut T, legacy: T, default: T) {
            if *value == default {
                *value = legacy;
            }
        }

        let default = Self::default();
        fill(&mut self.editor, legacy.editor, default.editor);
        fill(
            &mut self.agent_socket,
            legacy.agent_socket,
            default.agent_socket,
        );
        fill(
            &mut self.terminal_font_size,
            legacy.terminal_font_size,
            default.terminal_font_size,
        );
        fill(
            &mut self.terminal_copy_on_select,
            legacy.terminal_copy_on_select,
            default.terminal_copy_on_select,
        );
        fill(
            &mut self.terminal_colors,
            legacy.terminal_colors,
            default.terminal_colors,
        );
        let (current, legacy, default) =
            (&mut self.appearance, legacy.appearance, default.appearance);
        fill(&mut current.scheme, legacy.scheme, default.scheme);
        fill(
            &mut current.light_theme,
            legacy.light_theme,
            default.light_theme,
        );
        fill(
            &mut current.dark_theme,
            legacy.dark_theme,
            default.dark_theme,
        );
        fill(
            &mut current.glass.enabled,
            legacy.glass.enabled,
            default.glass.enabled,
        );
        fill(
            &mut current.glass.chrome,
            legacy.glass.chrome,
            default.glass.chrome,
        );
        fill(
            &mut current.glass.content,
            legacy.glass.content,
            default.glass.content,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn editor_and_appearance_survive_separate_saves() {
        use crate::appearance::Scheme;

        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(
            dir.join("settings.toml"),
            "editor = '/Applications/First.app'",
        )
        .unwrap();
        let mut settings = Settings::load(dir).unwrap();
        assert_eq!(settings.appearance, AppearanceSettings::default());
        settings.appearance.scheme = Scheme::Dark;
        settings.appearance.light_theme = "oceano".into();
        settings.appearance.dark_theme = "bosque".into();
        settings.appearance.glass.enabled = true;
        settings.agent_socket = Some("/tmp/agente-global.sock".into());
        settings.terminal_font_size = Some(13.);
        settings.terminal_copy_on_select = Some(true);
        settings.save(dir).unwrap();
        let mut settings = Settings::load(dir).unwrap();
        assert_eq!(settings.editor, Some("/Applications/First.app".into()));
        let appearance = settings.appearance.clone();
        assert_eq!(
            settings.agent_socket,
            Some(PathBuf::from("/tmp/agente-global.sock"))
        );
        assert_eq!(settings.terminal_font_size, Some(13.));
        assert_eq!(settings.terminal_copy_on_select, Some(true));
        settings.editor = Some("/Applications/Second.app".into());
        settings.save(dir).unwrap();
        let settings = Settings::load(dir).unwrap();
        assert_eq!(settings.appearance, appearance);
        assert_eq!(settings.editor, Some("/Applications/Second.app".into()));
        assert_eq!(
            settings.agent_socket,
            Some(PathBuf::from("/tmp/agente-global.sock"))
        );
        assert_eq!(settings.terminal_font_size, Some(13.));
        assert_eq!(settings.terminal_copy_on_select, Some(true));
    }

    #[test]
    fn terminal_font_size_round_trips_and_defaults_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        assert!(Settings::load(path).unwrap().terminal_font_size.is_none());
        Settings {
            terminal_font_size: Some(20.),
            ..Settings::default()
        }
        .save(path)
        .unwrap();
        assert_eq!(Settings::load(path).unwrap().terminal_font_size, Some(20.));
    }

    #[test]
    fn terminal_colors_round_trip_as_lowercase() {
        let dir = tempfile::tempdir().unwrap();
        for (terminal_colors, value) in [
            (TerminalColorSource::Theme, "theme"),
            (TerminalColorSource::Kitty, "kitty"),
        ] {
            Settings {
                terminal_colors,
                ..Settings::default()
            }
            .save(dir.path())
            .unwrap();
            assert_eq!(
                Settings::load(dir.path()).unwrap().terminal_colors,
                terminal_colors
            );
            let text = fs::read_to_string(dir.path().join("settings.toml")).unwrap();
            assert!(text.contains(&format!("terminal_colors = \"{value}\"")));
        }
    }

    #[test]
    fn older_settings_default_to_theme_terminal_colors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("settings.toml"), "terminal_font_size = 14").unwrap();
        assert_eq!(
            Settings::load(dir.path()).unwrap().terminal_colors,
            TerminalColorSource::Theme
        );
    }

    #[test]
    fn older_builtin_theme_ids_and_new_fields_load_with_current_defaults() {
        let dir = tempfile::tempdir().unwrap();
        for appearance in [
            "theme = 'vscp'",
            "theme = 'vscp'\nlight_theme = 'vscp'",
            "light_theme = 'vscp'\ndark_theme = 'vscp'",
        ] {
            fs::write(
                dir.path().join("settings.toml"),
                format!("obsolete_field = 7\n[appearance]\n{appearance}\nobsolete = true\n"),
            )
            .unwrap();
            assert_eq!(Settings::load(dir.path()).unwrap(), Settings::default());
        }
    }

    #[test]
    fn agent_socket_round_trips_and_defaults_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        assert!(Settings::load(path).unwrap().agent_socket.is_none());
        Settings {
            agent_socket: Some("/tmp/bitwarden.sock".into()),
            ..Settings::default()
        }
        .save(path)
        .unwrap();
        assert_eq!(
            Settings::load(path).unwrap().agent_socket,
            Some(PathBuf::from("/tmp/bitwarden.sock"))
        );
        Settings::default().save(path).unwrap();
        assert!(Settings::load(path).unwrap().agent_socket.is_none());
    }

    #[test]
    fn saves_and_resets_editor_in_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        assert!(Settings::load(path).unwrap().editor.is_none());
        let editor = PathBuf::from("/Applications/Visual Studio Code.app");
        Settings {
            editor: Some(editor.clone()),
            ..Settings::default()
        }
        .save(path)
        .unwrap();
        assert_eq!(Settings::load(path).unwrap().editor, Some(editor));
        assert_eq!(
            fs::metadata(path.join("settings.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        Settings::default().save(path).unwrap();
        assert!(Settings::load(path).unwrap().editor.is_none());
    }

    #[test]
    fn legacy_theme_selects_both_variants() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("settings.toml"),
            "[appearance]\ntheme = 'oceano'\n",
        )
        .unwrap();
        let settings = Settings::load(dir.path()).unwrap();
        assert_eq!(settings.appearance.light_theme, "oceano");
        assert_eq!(settings.appearance.dark_theme, "oceano");
    }

    #[test]
    fn distinct_themes_round_trip_without_the_legacy_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.appearance.light_theme = "oceano".into();
        settings.appearance.dark_theme = "frambuesa".into();
        settings.save(dir.path()).unwrap();
        let stored = Settings::load(dir.path()).unwrap();
        assert_eq!(stored.appearance, settings.appearance);
        let text = fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        let stored: toml::Table = toml::from_str(&text).unwrap();
        assert!(stored["appearance"].get("theme").is_none());
    }

    #[test]
    fn missing_theme_ids_fall_back_to_vela() {
        for (text, light, dark) in [
            ("[appearance]\nscheme = 'light'", "vela", "vela"),
            ("[appearance]\nlight_theme = 'oceano'", "oceano", "vela"),
            (
                "[appearance]\ndark_theme = 'frambuesa'",
                "vela",
                "frambuesa",
            ),
        ] {
            let settings: Settings = toml::from_str(text).unwrap();
            assert_eq!(settings.appearance.light_theme, light);
            assert_eq!(settings.appearance.dark_theme, dark);
        }
    }
}
