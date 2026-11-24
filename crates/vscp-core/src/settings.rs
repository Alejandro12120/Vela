//! Preferences shared by the desktop application.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::appearance::AppearanceSettings;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Settings {
    pub editor: Option<PathBuf>,
    /// Global SSH agent socket. When absent, the first detected Bitwarden
    /// socket is used, if any.
    #[serde(default)]
    pub agent_socket: Option<PathBuf>,
    #[serde(default)]
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
        settings.save(dir).unwrap();
        let mut settings = Settings::load(dir).unwrap();
        assert_eq!(settings.editor, Some("/Applications/First.app".into()));
        let appearance = settings.appearance.clone();
        assert_eq!(
            settings.agent_socket,
            Some(PathBuf::from("/tmp/agente-global.sock"))
        );
        settings.editor = Some("/Applications/Second.app".into());
        settings.save(dir).unwrap();
        let settings = Settings::load(dir).unwrap();
        assert_eq!(settings.appearance, appearance);
        assert_eq!(settings.editor, Some("/Applications/Second.app".into()));
        assert_eq!(
            settings.agent_socket,
            Some(PathBuf::from("/tmp/agente-global.sock"))
        );
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
    fn missing_theme_ids_fall_back_to_vscp() {
        for (text, light, dark) in [
            ("[appearance]\nscheme = 'light'", "vscp", "vscp"),
            ("[appearance]\nlight_theme = 'oceano'", "oceano", "vscp"),
            (
                "[appearance]\ndark_theme = 'frambuesa'",
                "vscp",
                "frambuesa",
            ),
        ] {
            let settings: Settings = toml::from_str(text).unwrap();
            assert_eq!(settings.appearance.light_theme, light);
            assert_eq!(settings.appearance.dark_theme, dark);
        }
    }
}
