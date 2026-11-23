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
        settings.appearance.theme = "bosque".into();
        settings.appearance.glass.enabled = true;
        settings.save(dir).unwrap();
        let mut settings = Settings::load(dir).unwrap();
        assert_eq!(settings.editor, Some("/Applications/First.app".into()));
        let appearance = settings.appearance.clone();
        settings.editor = Some("/Applications/Second.app".into());
        settings.save(dir).unwrap();
        let settings = Settings::load(dir).unwrap();
        assert_eq!(settings.appearance, appearance);
        assert_eq!(settings.editor, Some("/Applications/Second.app".into()));
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
}
