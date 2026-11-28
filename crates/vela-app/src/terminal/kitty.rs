use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use alacritty_terminal::vte::ansi::CursorShape;

use super::imported::{ImportedConfig, OptionAsAlt};

impl ImportedConfig {
    pub(super) fn load_kitty() -> Self {
        let path = env::var_os("KITTY_CONFIG_DIRECTORY")
            .map(|dir| PathBuf::from(dir).join("kitty.conf"))
            .or_else(|| {
                env::var_os("XDG_CONFIG_HOME")
                    .map(|dir| PathBuf::from(dir).join("kitty/kitty.conf"))
            })
            .or_else(|| env::home_dir().map(|home| home.join(".config/kitty/kitty.conf")));
        path.as_deref().map(Self::read_kitty).unwrap_or_default()
    }

    pub(super) fn read_kitty(path: &Path) -> Self {
        let mut settings = Self::default();
        if settings.import(path, 0, &mut HashSet::new()) {
            settings.path = Some(path.to_owned());
        }
        settings
    }

    fn import(&mut self, path: &Path, depth: usize, active: &mut HashSet<PathBuf>) -> bool {
        if depth > 8 {
            return false;
        }
        let (Ok(canonical), Ok(text)) = (path.canonicalize(), fs::read_to_string(path)) else {
            return false;
        };
        if !active.insert(canonical.clone()) {
            return false;
        }
        let directory = path.parent().unwrap_or(Path::new("."));
        for line in text
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
        {
            let Some((key, value)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let value = value.trim();
            match key {
                "include" => {
                    self.import(&directory.join(value), depth + 1, active);
                }
                "globinclude" => {
                    for path in glob(&directory.join(value)) {
                        self.import(&path, depth + 1, active);
                    }
                }
                "font_family" => {
                    if let Some(name) = family(value) {
                        self.family = name;
                    }
                }
                "bold_font" => self.variants[0] = family(value),
                "italic_font" => self.variants[1] = family(value),
                "bold_italic_font" => self.variants[2] = family(value),
                "font_size" => {
                    if let Ok(size) = value.parse::<f32>()
                        && (4. ..=256.).contains(&size)
                    {
                        self.size = size;
                    }
                }
                "cursor_shape" => {
                    self.cursor = match value {
                        "block" => CursorShape::Block,
                        "beam" => CursorShape::Beam,
                        "underline" => CursorShape::Underline,
                        _ => self.cursor,
                    };
                }
                "scrollback_lines" => {
                    if let Ok(lines) = value.parse::<i64>() {
                        self.history = if lines < 0 {
                            10000
                        } else {
                            lines.clamp(1000, 10000) as usize
                        };
                    }
                }
                "copy_on_select" => self.copy_on_select = matches!(value, "clipboard" | "yes"),
                "macos_option_as_alt" => {
                    self.option_as_alt = match value {
                        "left" => OptionAsAlt::Left,
                        "right" => OptionAsAlt::Right,
                        "both" | "yes" => OptionAsAlt::Both,
                        "no" => OptionAsAlt::No,
                        _ => self.option_as_alt,
                    };
                }
                _ => self.palette.set(key, value),
            }
        }
        active.remove(&canonical);
        true
    }
}

fn family(value: &str) -> Option<String> {
    let value = if let Some(value) = value.strip_prefix("family=") {
        if let Some(quoted) = value.strip_prefix('"') {
            quoted.split_once('"')?.0
        } else if let Some(quoted) = value.strip_prefix('\'') {
            quoted.split_once('\'')?.0
        } else {
            value.split_whitespace().next()?
        }
    } else {
        value
    };
    match value {
        "" | "auto" => None,
        "monospace" => Some(crate::theme::monospace().into()),
        _ => Some(value.into()),
    }
}

fn glob(path: &Path) -> Vec<PathBuf> {
    use std::{
        ffi::{CStr, CString, OsStr},
        os::unix::ffi::OsStrExt,
    };

    let Ok(pattern) = CString::new(path.as_os_str().as_bytes()) else {
        return Vec::new();
    };
    // SAFETY: glob initializes the zeroed result and owns the returned paths
    // until globfree. Copy each path before releasing that allocation.
    unsafe {
        let mut matches: libc::glob_t = std::mem::zeroed();
        let result = if libc::glob(pattern.as_ptr(), 0, None, &mut matches) == 0 {
            (0..matches.gl_pathc as usize)
                .map(|index| CStr::from_ptr(*matches.gl_pathv.add(index)))
                .map(|path| PathBuf::from(OsStr::from_bytes(path.to_bytes())))
                .collect()
        } else {
            Vec::new()
        };
        libc::globfree(&mut matches);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_font_forms_and_size() {
        assert_eq!(family("JetBrains Mono").as_deref(), Some("JetBrains Mono"));
        assert_eq!(
            family("family=\"JetBrains Mono\" style=Regular").as_deref(),
            Some("JetBrains Mono")
        );
        assert_eq!(family("auto"), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kitty.conf");
        fs::write(
            &path,
            "font_size 13.5\nscrollback_lines 20000\nmacos_option_as_alt left",
        )
        .unwrap();
        let settings = ImportedConfig::read_kitty(&path);
        assert_eq!(settings.size, 13.5);
        assert_eq!(settings.history, 10000);
        assert_eq!(settings.option_as_alt, OptionAsAlt::Left);
    }

    #[test]
    fn nested_includes_cycles_missing_files_globs_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("kitty.conf"), "foreground #abc\ninclude sub/first.conf\ninclude missing.conf\nglobinclude sub/theme-*.conf\nfont_size 14").unwrap();
        fs::write(
            root.join("sub/first.conf"),
            "include ../kitty.conf\ninclude second.conf\nfont_family JetBrains Mono",
        )
        .unwrap();
        fs::write(
            root.join("sub/second.conf"),
            "background #123456\nfont_family Monaco",
        )
        .unwrap();
        fs::write(root.join("sub/theme-a.conf"), "color0 #123\nfont_size 20").unwrap();
        fs::write(root.join("sub/theme-b.conf"), "color0 #234\nfont_size 21").unwrap();

        let settings = ImportedConfig::read_kitty(&root.join("kitty.conf"));
        assert_eq!(settings.family, "JetBrains Mono");
        assert_eq!(settings.palette.foreground, 0xaabbcc);
        assert_eq!(settings.palette.background, 0x123456);
        assert_eq!(settings.palette.ansi[0], 0x223344);
        assert_eq!(settings.size, 14.);
    }

    #[test]
    fn hyde_include_layout_keeps_a_family_with_different_spacing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("kitty")).unwrap();
        fs::write(root.join("kitty/kitty.conf"), "include hyde.conf\n").unwrap();
        fs::write(
            root.join("kitty/hyde.conf"),
            "font_family Caskaydia Cove Nerd Font Mono\ninclude theme.conf\n",
        )
        .unwrap();
        fs::write(root.join("kitty/theme.conf"), "foreground #abc\n").unwrap();

        let mut settings = ImportedConfig::read_kitty(&root.join("kitty/kitty.conf"));
        assert_eq!(settings.family, "Caskaydia Cove Nerd Font Mono");
        settings.resolve_family(&["CaskaydiaCove Nerd Font Mono".to_owned()]);
        assert_eq!(settings.family, "Caskaydia Cove Nerd Font Mono");

        settings.family = "JET-BRAINS MONO-nerd font".into();
        settings.resolve_family(&["JetBrainsMono Nerd Font".to_owned()]);
        assert_eq!(settings.family, "JET-BRAINS MONO-nerd font");
    }

    #[test]
    fn copy_on_select_only_accepts_clipboard_and_yes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kitty.conf");
        for (value, expected) in [("clipboard", true), ("no", false), ("a1", false)] {
            fs::write(&path, format!("copy_on_select {value}\n")).unwrap();
            assert_eq!(ImportedConfig::read_kitty(&path).copy_on_select, expected);
        }
        let settings = ImportedConfig::read_kitty(&dir.path().join("missing.conf"));
        assert!(!settings.copy_on_select);
    }

    #[test]
    fn absent_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = ImportedConfig::read_kitty(&dir.path().join("kitty.conf"));
        assert!(settings.path.is_none());
        assert_eq!(settings.family, crate::theme::monospace());
        assert_eq!(settings.size, 11.);
        assert_eq!(settings.history, 5000);
        assert_eq!(settings.palette.foreground, 0xdddddd);
        assert_eq!(settings.palette.ansi[9], 0xf2201f);
        assert_eq!(settings.option_as_alt, OptionAsAlt::No);
    }
}
