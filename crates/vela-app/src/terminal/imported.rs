//! Configuration imported by the integrated terminal.

use std::{
    collections::{HashSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
};

use alacritty_terminal::vte::ansi::CursorShape;
use vela_core::settings::TerminalConfigSource;

use super::colors::Palette;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionAsAlt {
    #[default]
    No,
    Left,
    Right,
    Both,
}

#[derive(Clone)]
pub struct ImportedConfig {
    pub path: Option<PathBuf>,
    pub source: Option<TerminalConfigSource>,
    pub family: String,
    pub variants: [Option<String>; 3],
    pub size: f32,
    pub palette: Palette,
    pub cursor: CursorShape,
    pub history: usize,
    pub option_as_alt: OptionAsAlt,
    pub copy_on_select: bool,
}

impl Default for ImportedConfig {
    fn default() -> Self {
        // https://github.com/kovidgoyal/kitty/blob/master/kitty/options/definition.py
        // On macOS, kitty resolves its default “monospace” family to Menlo.
        // https://github.com/kovidgoyal/kitty/blob/master/kitty/fonts/core_text.py
        // On Linux the family comes from fontconfig.
        Self {
            path: None,
            source: None,
            family: crate::theme::monospace().into(),
            variants: Default::default(),
            size: 11.,
            palette: Palette::default(),
            cursor: CursorShape::Block,
            history: 5000,
            option_as_alt: OptionAsAlt::No,
            copy_on_select: false,
        }
    }
}

impl ImportedConfig {
    pub fn has_palette(&self) -> bool {
        self.path.is_some() && self.source != Some(TerminalConfigSource::Wezterm)
    }

    pub fn resolved_family(&self, names: &[String]) -> &str {
        let fallback = crate::theme::monospace();
        let key = crate::theme::family_key(&self.family);
        if key == crate::theme::family_key(fallback)
            || names
                .iter()
                .any(|name| crate::theme::family_key(name) == key)
        {
            &self.family
        } else {
            fallback
        }
    }

    pub fn resolve_family(&mut self, names: &[String]) {
        let family = self.resolved_family(names);
        if family != self.family {
            crate::dev::log(format!(
                "Terminal: font {} isn't installed. Using {family}.",
                self.family
            ));
            self.family = family.to_owned();
        }
    }
}

pub const SOURCES: &[(TerminalConfigSource, &str)] = &[
    (TerminalConfigSource::Auto, "Automatic"),
    (TerminalConfigSource::Kitty, "Kitty"),
    (TerminalConfigSource::Ghostty, "Ghostty"),
    (TerminalConfigSource::Alacritty, "Alacritty"),
    (TerminalConfigSource::Foot, "foot"),
    (TerminalConfigSource::Wezterm, "WezTerm (partial)"),
    (TerminalConfigSource::None, "None"),
];

impl gpui::Global for ImportedConfig {}

pub fn load(
    source: TerminalConfigSource,
    external: Option<TerminalConfigSource>,
) -> ImportedConfig {
    let override_file = crate::dev::terminal_conf().and_then(|(id, path)| {
        let source = match id.as_str() {
            "kitty" => TerminalConfigSource::Kitty,
            "ghostty" => TerminalConfigSource::Ghostty,
            "alacritty" => TerminalConfigSource::Alacritty,
            "foot" => TerminalConfigSource::Foot,
            "wezterm" => TerminalConfigSource::Wezterm,
            _ => return None,
        };
        Some((source, path))
    });
    select_config(source, external, |source| {
        if let Some((_, path)) = override_file.as_ref().filter(|(id, _)| *id == source) {
            read_config(source, std::slice::from_ref(path))
        } else if source == TerminalConfigSource::Kitty {
            ImportedConfig::load_kitty()
        } else {
            read_config(source, &paths(source))
        }
    })
}

fn select_config(
    source: TerminalConfigSource,
    external: Option<TerminalConfigSource>,
    mut read: impl FnMut(TerminalConfigSource) -> ImportedConfig,
) -> ImportedConfig {
    use TerminalConfigSource::*;
    let mut config = match source {
        None => return ImportedConfig::default(),
        Auto => {
            if let Some(external) = external.filter(|source| *source != Kitty) {
                let mut config = read(external);
                if config.path.is_some() {
                    config.source = Some(external);
                    return config;
                }
            }
            read(Kitty)
        }
        source => read(source),
    };
    config.source = config
        .path
        .as_ref()
        .map(|_| if source == Auto { Kitty } else { source });
    config
}

fn paths(source: TerminalConfigSource) -> Vec<PathBuf> {
    use TerminalConfigSource::*;

    let home = env::home_dir();
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let config_home = xdg
        .clone()
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    match source {
        Ghostty => {
            let mut directories = config_home
                .into_iter()
                .map(|root| root.join("ghostty"))
                .collect::<Vec<_>>();
            if cfg!(target_os = "macos")
                && let Some(home) = &home
            {
                directories.push(home.join("Library/Application Support/com.mitchellh.ghostty"));
            }
            directories
                .into_iter()
                .flat_map(|root| [root.join("config"), root.join("config.ghostty")])
                .collect()
        }
        Alacritty => xdg
            .into_iter()
            .flat_map(|root| {
                [
                    root.join("alacritty/alacritty.toml"),
                    root.join("alacritty.toml"),
                ]
            })
            .chain(home.into_iter().flat_map(|home| {
                [
                    home.join(".config/alacritty/alacritty.toml"),
                    home.join(".alacritty.toml"),
                ]
            }))
            .chain([PathBuf::from("/etc/alacritty/alacritty.toml")])
            .collect(),
        Foot => {
            let config_dirs = env::var_os("XDG_CONFIG_DIRS")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "/etc/xdg".into());
            config_home
                .into_iter()
                .chain(env::split_paths(&config_dirs))
                .map(|root| root.join("foot/foot.ini"))
                .collect()
        }
        Wezterm => {
            if let Some(path) = env::var_os("WEZTERM_CONFIG_FILE") {
                return vec![path.into()];
            }
            xdg.into_iter()
                .map(|root| root.join("wezterm/wezterm.lua"))
                .chain(home.into_iter().flat_map(|home| {
                    [
                        home.join(".config/wezterm/wezterm.lua"),
                        home.join(".wezterm.lua"),
                    ]
                }))
                .collect()
        }
        _ => Vec::new(),
    }
}

fn read_config(source: TerminalConfigSource, paths: &[PathBuf]) -> ImportedConfig {
    use TerminalConfigSource::*;

    let mut config = ImportedConfig::default();
    let mut files = ConfigFiles::default();
    match source {
        Kitty => {
            if let Some(path) = paths.first() {
                return ImportedConfig::read_kitty(path);
            }
        }
        Ghostty => read_ghostty(&mut config, paths),
        Alacritty | Foot | Wezterm => {
            if let Some(path) = paths.iter().find(|path| path.is_file()) {
                let read = match source {
                    Alacritty => read_alacritty(&mut config, path, &mut files),
                    Foot => {
                        let mut foot = FootColors::default();
                        let read = read_foot(&mut config, &mut foot, path, &mut files);
                        config.palette = if foot.use_light {
                            foot.light
                        } else {
                            foot.dark
                        };
                        read
                    }
                    Wezterm => {
                        fs::read_to_string(path).is_ok_and(|text| read_wezterm(&mut config, &text))
                    }
                    _ => unreachable!(),
                };
                if read {
                    config.path = Some(path.clone());
                }
            }
        }
        _ => {}
    }
    config
}

#[derive(Default)]
struct ConfigFiles {
    active: HashSet<PathBuf>,
}

impl ConfigFiles {
    fn read(&mut self, path: &Path, parse: impl FnOnce(&str, &mut Self) -> bool) -> bool {
        if self.active.len() >= 16 {
            return false;
        }
        let Ok(canonical) = path.canonicalize() else {
            return false;
        };
        if !canonical.is_file() || !self.active.insert(canonical.clone()) {
            return false;
        }
        let read = fs::read_to_string(path).is_ok_and(|text| parse(&text, self));
        self.active.remove(&canonical);
        read
    }
}

fn included_path(parent: &Path, value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    if let Some(relative) = value.strip_prefix("~/") {
        return env::home_dir().map(|home| home.join(relative));
    }
    Some(parent.parent().unwrap_or(Path::new(".")).join(value))
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn key_value(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

fn hex(value: &str) -> Option<u32> {
    let value = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    (value.len() == 6)
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
}

fn color(slot: &mut u32, value: &str) {
    if let Some(value) = hex(value) {
        *slot = value;
    }
}

fn optional_color(slot: &mut Option<u32>, value: &str) {
    if let Some(value) = hex(value) {
        *slot = Some(value);
    } else if matches!(
        value,
        "CellForeground" | "CellBackground" | "cell-foreground" | "cell-background"
    ) {
        *slot = None;
    }
}

fn common_color(palette: &mut Palette, key: &str, value: &str) {
    match key {
        "foreground" => color(&mut palette.foreground, value),
        "background" => color(&mut palette.background, value),
        "cursor-color" => optional_color(&mut palette.cursor, value),
        "cursor-text" => optional_color(&mut palette.cursor_text, value),
        "selection-foreground" => optional_color(&mut palette.selection_foreground, value),
        "selection-background" => optional_color(&mut palette.selection_background, value),
        _ => {}
    }
}

fn size(config: &mut ImportedConfig, value: f32) {
    if (4. ..=256.).contains(&value) {
        config.size = value;
    }
}

fn family(config: &mut ImportedConfig, value: &str) {
    if !value.is_empty() {
        config.family = if value.eq_ignore_ascii_case("monospace") {
            crate::theme::monospace().into()
        } else {
            value.into()
        };
    }
}

fn option_as_alt(config: &mut ImportedConfig, value: &str) {
    config.option_as_alt = match value {
        "left" | "OnlyLeft" => OptionAsAlt::Left,
        "right" | "OnlyRight" => OptionAsAlt::Right,
        "true" | "Both" => OptionAsAlt::Both,
        "false" | "None" => OptionAsAlt::No,
        _ => config.option_as_alt,
    };
}

fn read_ghostty(config: &mut ImportedConfig, paths: &[PathBuf]) {
    let mut includes = VecDeque::new();
    let mut seen = HashSet::new();
    for path in paths {
        if let Some(canonical) = read_ghostty_file(config, path, &mut includes) {
            seen.insert(canonical);
            config.path = Some(path.clone());
        }
    }
    // Ghostty queues includes after the default layers and loads each include once.
    while let Some(path) = includes.pop_front() {
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if seen.insert(canonical) {
            read_ghostty_file(config, &path, &mut includes);
        }
    }
}

fn read_ghostty_file(
    config: &mut ImportedConfig,
    path: &Path,
    includes: &mut VecDeque<PathBuf>,
) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim_start_matches('\u{feff}');
    for (key, raw) in text.lines().filter_map(key_value) {
        let value = unquote(raw);
        match key {
            "config-file" => {
                let value = if raw.starts_with('"') {
                    value
                } else {
                    value.strip_prefix('?').unwrap_or(value)
                };
                includes.extend(included_path(path, value));
            }
            "font-family" => family(config, value),
            "font-family-bold" => config.variants[0] = Some(value.into()),
            "font-family-italic" => config.variants[1] = Some(value.into()),
            "font-family-bold-italic" => config.variants[2] = Some(value.into()),
            "font-size" => {
                if let Ok(value) = value.parse() {
                    size(config, value);
                }
            }
            "copy-on-select" => config.copy_on_select = matches!(value, "true" | "clipboard"),
            "macos-option-as-alt" => option_as_alt(config, value),
            "cursor-style" => set_cursor(config, value),
            "palette" => {
                if let Some((index, value)) = key_value(value)
                    && let Some(index) = palette_index(index)
                    && let Some(slot) = config.palette.ansi.get_mut(index)
                {
                    color(slot, value);
                }
            }
            _ => common_color(&mut config.palette, key, value),
        }
    }
    Some(canonical)
}

fn palette_index(value: &str) -> Option<usize> {
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(digits) = value.strip_prefix(prefix) {
            return usize::from_str_radix(digits, radix).ok();
        }
    }
    value.parse().ok()
}

fn set_cursor(config: &mut ImportedConfig, value: &str) {
    config.cursor = match value {
        "block" | "Block" => CursorShape::Block,
        "bar" | "beam" | "Beam" => CursorShape::Beam,
        "hollow" => CursorShape::HollowBlock,
        "underline" | "Underline" => CursorShape::Underline,
        _ => config.cursor,
    };
}

fn read_alacritty(config: &mut ImportedConfig, path: &Path, files: &mut ConfigFiles) -> bool {
    files.read(path, |text, files| {
        let Ok(table) = text.parse::<toml::Table>() else {
            return false;
        };
        let value = |keys: &[&str]| -> Option<&toml::Value> {
            let (first, rest) = keys.split_first()?;
            rest.iter()
                .try_fold(table.get(*first)?, |value, key| value.get(*key))
        };
        for import in value(&["general", "import"])
            .or_else(|| value(&["import"]))
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
        {
            if let Some(include) = included_path(path, import) {
                read_alacritty(config, &include, files);
            }
        }
        let string = |keys: &[&str]| value(keys).and_then(toml::Value::as_str);
        if let Some(name) = string(&["font", "normal", "family"]) {
            family(config, name);
        }
        for (index, kind) in ["bold", "italic", "bold_italic"].into_iter().enumerate() {
            if let Some(name) = string(&["font", kind, "family"]) {
                config.variants[index] = Some(name.into());
            }
        }
        if let Some(value) = value(&["font", "size"])
            && let Some(number) = value
                .as_float()
                .or_else(|| value.as_integer().map(|n| n as f64))
        {
            size(config, number as f32);
        }
        if let Some(enable) =
            value(&["selection", "save_to_clipboard"]).and_then(toml::Value::as_bool)
        {
            config.copy_on_select = enable;
        }
        if let Some(option) = string(&["window", "option_as_alt"]) {
            option_as_alt(config, option);
        }
        if let Some(shape) =
            string(&["cursor", "style", "shape"]).or_else(|| string(&["cursor", "style"]))
        {
            set_cursor(config, shape);
        }
        if let Some(lines) = value(&["scrolling", "history"]).and_then(toml::Value::as_integer) {
            config.history = lines.clamp(0, 10000) as usize;
        }
        for (group, key, target) in [
            ("primary", "foreground", "foreground"),
            ("primary", "background", "background"),
            ("cursor", "cursor", "cursor-color"),
            ("cursor", "text", "cursor-text"),
            ("selection", "text", "selection-foreground"),
            ("selection", "background", "selection-background"),
        ] {
            if let Some(value) = string(&["colors", group, key]) {
                common_color(&mut config.palette, target, value);
            }
        }
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for (offset, group) in [(0, "normal"), (8, "bright")] {
            for (index, name) in names.iter().enumerate() {
                if let Some(value) = string(&["colors", group, name]) {
                    color(&mut config.palette.ansi[index + offset], value);
                }
            }
        }
        true
    })
}

#[derive(Default)]
struct FootColors {
    dark: Palette,
    light: Palette,
    use_light: bool,
}

fn read_foot(
    config: &mut ImportedConfig,
    foot: &mut FootColors,
    path: &Path,
    files: &mut ConfigFiles,
) -> bool {
    files.read(path, |text, files| {
        let mut section = "main";
        for line in text.lines().map(str::trim) {
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                section = name;
                continue;
            }
            let Some((key, value)) = key_value(line) else {
                continue;
            };
            // foot allows inline comments after whitespace.
            let value = value
                .split_once(" #")
                .map_or(value, |(value, _)| value)
                .trim();
            let value = unquote(value);
            match (section, key) {
                ("main", "include") if value.starts_with('/') || value.starts_with("~/") => {
                    if let Some(include) = included_path(path, value) {
                        read_foot(config, foot, &include, files);
                    }
                }
                ("main", "font") => {
                    let pattern = value.split(',').next().unwrap_or_default();
                    let mut parts = pattern.split(':');
                    family(config, parts.next().unwrap_or_default().trim());
                    if let Some(points) = parts
                        .filter_map(|part| part.strip_prefix("size="))
                        .find_map(|value| value.parse().ok())
                    {
                        size(config, points);
                    }
                }
                ("main", "font-bold" | "font-italic" | "font-bold-italic") => {
                    let index = match key {
                        "font-bold" => 0,
                        "font-italic" => 1,
                        _ => 2,
                    };
                    let name = value.split([':', ',']).next().unwrap_or_default().trim();
                    if !name.is_empty() {
                        config.variants[index] = Some(name.into());
                    }
                }
                ("main", "initial-color-theme") => foot.use_light = value == "light",
                ("main", "selection-target") => {
                    config.copy_on_select = matches!(value, "clipboard" | "both");
                }
                ("scrollback", "lines") => {
                    if let Ok(lines) = value.parse::<usize>() {
                        config.history = lines.min(10000);
                    }
                }
                ("cursor", "style") => set_cursor(config, value),
                ("cursor", "color") => {
                    foot_cursor(&mut foot.dark, value);
                    foot_cursor(&mut foot.light, value);
                }
                ("colors" | "colors-dark" | "colors-light", _) => {
                    let palette = if section == "colors-light" {
                        &mut foot.light
                    } else {
                        &mut foot.dark
                    };
                    let index = key
                        .strip_prefix("regular")
                        .and_then(ansi_index)
                        .or_else(|| {
                            key.strip_prefix("bright")
                                .and_then(ansi_index)
                                .map(|index| index + 8)
                        });
                    if let Some(index) = index {
                        color(&mut palette.ansi[index], value);
                    } else if key == "cursor" {
                        foot_cursor(palette, value);
                    } else {
                        common_color(palette, key, value);
                    }
                }
                _ => {}
            }
        }
        true
    })
}

fn ansi_index(value: &str) -> Option<usize> {
    value.parse().ok().filter(|index| *index < 8)
}

fn foot_cursor(palette: &mut Palette, value: &str) {
    let mut colors = value.split_whitespace();
    if let (Some(text), Some(cursor)) = (colors.next(), colors.next()) {
        optional_color(&mut palette.cursor_text, text);
        optional_color(&mut palette.cursor, cursor);
    }
}

// A deliberately small subset of Lua for standalone assignments. The file is
// never evaluated, and config expressions and block syntax are not interpreted.
fn read_wezterm(config: &mut ImportedConfig, text: &str) -> bool {
    let mut found = false;
    let mut alt = [false; 2];
    for line in text.lines().map(str::trim) {
        let Some((key, value)) = line.strip_prefix("config.").and_then(key_value) else {
            continue;
        };
        let value = value.trim_end_matches(';').trim_end();
        match key {
            "font_size" => {
                let Ok(points) = value.parse() else { continue };
                size(config, points);
            }
            "font" => {
                let Some(literal) =
                    value
                        .strip_prefix("wezterm.font")
                        .map(str::trim)
                        .and_then(|argument| {
                            let argument = argument
                                .strip_prefix('(')
                                .and_then(|argument| argument.strip_suffix(')'))
                                .unwrap_or(argument);
                            lua_string(argument)
                        })
                else {
                    continue;
                };
                family(config, literal);
            }
            "scrollback_lines" => {
                let Ok(lines) = value.parse::<usize>() else {
                    continue;
                };
                config.history = lines.min(10000);
            }
            "send_composed_key_when_left_alt_is_pressed"
            | "send_composed_key_when_right_alt_is_pressed" => {
                let enabled = match value {
                    "false" => true,
                    "true" => false,
                    _ => continue,
                };
                alt[usize::from(key.contains("_right_"))] = enabled;
            }
            _ => continue,
        }
        found = true;
    }
    config.option_as_alt = match alt {
        [false, false] => OptionAsAlt::No,
        [true, false] => OptionAsAlt::Left,
        [false, true] => OptionAsAlt::Right,
        [true, true] => OptionAsAlt::Both,
    };
    found
}

fn lua_string(value: &str) -> Option<&str> {
    let value = value.trim();
    for quote in ['\'', '"'] {
        if let Some(literal) = value
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
            && !literal.contains([quote, '\\'])
        {
            return Some(literal);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use TerminalConfigSource::{Alacritty, Auto, Foot, Ghostty, Kitty, Wezterm};

    fn write(dir: &Path, file: &str, content: &str) -> PathBuf {
        let path = dir.join(file);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn ghostty_reads_layers_then_uses_includes_after_their_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let global = write(
            root,
            "config.ghostty",
            "font-family = Global\nfont-size = 12",
        );
        let local = write(
            root,
            "config.ghostty",
            r#"
            config-file = ?missing
            config-file = "included config"
            font-family = Local
            font-size = 14.5
            foreground = #123456
            background = aabbcc
            cursor-color = #234567
            cursor-text = #456789
            palette = 0xf=#ffeeaa
            copy-on-select = clipboard
            macos-option-as-alt = left
        "#,
        );
        write(
            root,
            "included config",
            r#"
            config-file = config.ghostty
            font-family = "JetBrains Mono"
            font-family-bold = Bold Mono
            selection-foreground = #fedcba
            selection-background = 112233
            cursor-style = bar
        "#,
        );
        let config = read_config(Ghostty, &[global, local.clone()]);
        assert_eq!(config.path, Some(local));
        assert_eq!(config.family, "JetBrains Mono");
        assert_eq!(config.variants[0].as_deref(), Some("Bold Mono"));
        assert_eq!(config.size, 14.5);
        assert_eq!(config.palette.foreground, 0x123456);
        assert_eq!(config.palette.background, 0xaabbcc);
        assert_eq!(config.palette.cursor, Some(0x234567));
        assert_eq!(config.palette.cursor_text, Some(0x456789));
        assert_eq!(config.palette.ansi[15], 0xffeeaa);
        assert_eq!(config.palette.selection_foreground, Some(0xfedcba));
        assert_eq!(config.palette.selection_background, Some(0x112233));
        assert_eq!(config.cursor, CursorShape::Beam);
        assert!(config.copy_on_select);
        assert_eq!(config.option_as_alt, OptionAsAlt::Left);
    }

    #[test]
    fn alacritty_imports_in_order_then_applies_the_root_toml() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let main = write(
            root,
            "alacritty.toml",
            r##"
            [general]
            import = ["missing", "base.toml", "last.toml"]
            [font]
            size = 16
            normal.family = "First Font"
            italic.family = "Italic Font"
            [colors.primary]
            background = "0x101010"
            [colors.selection]
            text = "CellForeground"
            background = "#334455"
            [window]
            option_as_alt = "OnlyRight"
            [selection]
            save_to_clipboard = true
            [cursor.style]
            shape = "Underline"
        "##,
        );
        write(
            root,
            "base.toml",
            r##"
            import = ["alacritty.toml"]
            [font]
            size = 15.0
            [colors.primary]
            foreground = "#111111"
            background = "0x202020"
            [colors.cursor]
            cursor = "#123456"
            text = "#234567"
            [colors.normal]
            red = "#312345"
        "##,
        );
        write(
            root,
            "last.toml",
            r##"
            [colors.primary]
            foreground = "#f1f2f3"
            [colors.bright]
            magenta = "#f0e0d0"
            [scrolling]
            history = 2345
        "##,
        );
        let config = read_config(Alacritty, std::slice::from_ref(&main));
        assert_eq!(config.path, Some(main));
        assert_eq!(config.family, "First Font");
        assert_eq!(config.variants[1].as_deref(), Some("Italic Font"));
        assert_eq!(config.size, 16.);
        assert_eq!(config.palette.foreground, 0xf1f2f3);
        assert_eq!(config.palette.background, 0x101010);
        assert_eq!(config.palette.ansi[1], 0x312345);
        assert_eq!(config.palette.ansi[13], 0xf0e0d0);
        assert_eq!(config.palette.cursor, Some(0x123456));
        assert_eq!(config.palette.cursor_text, Some(0x234567));
        assert_eq!(config.palette.selection_foreground, None);
        assert_eq!(config.palette.selection_background, Some(0x334455));
        assert!(config.copy_on_select);
        assert_eq!(config.option_as_alt, OptionAsAlt::Right);
        assert_eq!(config.cursor, CursorShape::Underline);
        assert_eq!(config.history, 2345);
        let invalid = write(root, "invalid.toml", "font = [");
        assert_eq!(read_config(Alacritty, &[invalid]).path, None);
    }

    #[test]
    fn foot_processes_includes_in_place_and_preserves_the_callers_section() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let main = write(
            root,
            "foot.ini",
            &format!(
                r#"
            font=Before:size=8
            include={}/included.ini
            include={}/missing
            font=Final Mono:size=13.5, Noto Emoji:size=11
            font-bold=Bold Mono:size=13
            initial-color-theme=light
            selection-target=clipboard
            [scrollback]
            lines=1234
            [cursor]
            style=beam
            [colors-dark]
            foreground=010203
            [colors-light]
            regular0=a1a2a3
            selection-foreground=112233
            selection-background=bbccdd
        "#,
                root.display(),
                root.display()
            ),
        );
        write(
            root,
            "included.ini",
            &format!(
                r#"
            include={}
            font=Included:size=16
            [colors-light]
            foreground=102030
            background=f0e0d0
            bright7=f1e1d1
            cursor=123456 abcdef
        "#,
                main.display()
            ),
        );
        let config = read_config(Foot, std::slice::from_ref(&main));
        assert_eq!(config.path, Some(main));
        assert_eq!(config.family, "Final Mono");
        assert_eq!(config.variants[0].as_deref(), Some("Bold Mono"));
        assert_eq!(config.size, 13.5);
        assert_eq!(config.palette.foreground, 0x102030);
        assert_eq!(config.palette.background, 0xf0e0d0);
        assert_eq!(config.palette.ansi[0], 0xa1a2a3);
        assert_eq!(config.palette.ansi[15], 0xf1e1d1);
        assert_eq!(config.palette.selection_foreground, Some(0x112233));
        assert_eq!(config.palette.selection_background, Some(0xbbccdd));
        assert_eq!(config.palette.cursor, Some(0xabcdef));
        assert_eq!(config.palette.cursor_text, Some(0x123456));
        assert_eq!(config.cursor, CursorShape::Beam);
        assert_eq!(config.history, 1234);
        assert!(config.copy_on_select);
    }

    #[test]
    fn wezterm_reads_standalone_literals_and_does_not_evaluate_lua() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "wezterm.lua",
            r#"
            local wezterm = require 'wezterm'
            config.font_size = 18
            config.font_size = 4 + 40
            config.font = wezterm.font 'Family Name'
            config.scrollback_lines = 2200
            config.send_composed_key_when_left_alt_is_pressed = false
            config.send_composed_key_when_right_alt_is_pressed = true
            config.colors = require 'colors'
            return config
        "#,
        );
        let config = select_config(Wezterm, None, |source| {
            read_config(source, std::slice::from_ref(&path))
        });
        assert_eq!(config.path, Some(path));
        assert_eq!(config.family, "Family Name");
        assert_eq!(config.size, 18.);
        assert_eq!(config.history, 2200);
        assert_eq!(config.option_as_alt, OptionAsAlt::Left);
        assert!(!config.has_palette());
        let path = write(
            dir.path(),
            "unsupported.lua",
            "return require('other.config')",
        );
        assert!(read_config(Wezterm, &[path]).path.is_none());
    }

    #[test]
    fn automatic_import_uses_available_config_then_kitty_then_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let kitty = write(
            dir.path(),
            "kitty.conf",
            "font_size 14\ncopy_on_select clipboard",
        );
        let ghostty = write(dir.path(), "config.ghostty", "font-size = 19");
        let absent = dir.path().join("missing");
        let load = |source| {
            read_config(
                source,
                &[match source {
                    Kitty => kitty.clone(),
                    Ghostty => ghostty.clone(),
                    _ => absent.clone(),
                }],
            )
        };

        let config = select_config(Auto, Some(Ghostty), load);
        assert_eq!(config.source, Some(Ghostty));
        assert_eq!(config.size, 19.);
        for external in [Some(Foot), Some(Kitty), None] {
            let config = select_config(Auto, external, load);
            assert_eq!(config.source, Some(Kitty));
            assert_eq!(config.size, 14.);
            assert!(config.copy_on_select);
        }

        for source in [TerminalConfigSource::None, Foot] {
            assert!(select_config(source, Some(Kitty), load).path.is_none());
        }
        assert!(
            select_config(Auto, Some(Alacritty), |source| read_config(
                source,
                std::slice::from_ref(&absent)
            ))
            .path
            .is_none()
        );
    }
}
