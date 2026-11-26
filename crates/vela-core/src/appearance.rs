//! Theme files and color preferences shared by the desktop application.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use toml::{Table, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Glass {
    pub enabled: bool,
    pub chrome: u8,
    pub content: u8,
}

impl Default for Glass {
    fn default() -> Self {
        Self {
            enabled: false,
            chrome: 70,
            content: 92,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredAppearanceSettings")]
pub struct AppearanceSettings {
    pub scheme: Scheme,
    pub light_theme: String,
    pub dark_theme: String,
    pub glass: Glass,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct StoredAppearanceSettings {
    scheme: Scheme,
    theme: Option<String>,
    light_theme: Option<String>,
    dark_theme: Option<String>,
    glass: Glass,
}

impl From<StoredAppearanceSettings> for AppearanceSettings {
    fn from(stored: StoredAppearanceSettings) -> Self {
        let legacy = stored
            .theme
            .map(normalize_theme_id)
            .unwrap_or_else(|| "vela".into());
        Self {
            scheme: stored.scheme,
            light_theme: stored
                .light_theme
                .map(normalize_theme_id)
                .unwrap_or_else(|| legacy.clone()),
            dark_theme: stored.dark_theme.map(normalize_theme_id).unwrap_or(legacy),
            glass: stored.glass,
        }
    }
}

fn normalize_theme_id(id: String) -> String {
    if id == "vscp" { "vela".into() } else { id }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            scheme: Scheme::System,
            light_theme: "vela".into(),
            dark_theme: "vela".into(),
            glass: Glass::default(),
        }
    }
}

// Declare the tokens and their file keys together so new tokens are written
// and read in both variants.
macro_rules! colors {
    ($($field:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Colors {
            $(pub $field: u32,)+
            pub terminal: TerminalColors,
        }

        impl Colors {
            fn read(&mut self, table: &Table, variant: &str) -> io::Result<()> {
                $(if let Some(color) = read_color(table, variant, stringify!($field))? {
                    self.$field = color;
                })+
                if let Some(value) = table.get("terminal") {
                    let key = format!("{variant}.terminal");
                    let terminal = value.as_table().ok_or_else(|| invalid(&key))?;
                    self.terminal = TerminalColors::read(terminal, &key)?;
                }
                Ok(())
            }

            fn table(self) -> Table {
                let mut table = Table::new();
                $(table.insert(stringify!($field).into(), Value::String(hex(self.$field)));)+
                let terminal = self.terminal.table();
                if !terminal.is_empty() {
                    table.insert("terminal".into(), Value::Table(terminal));
                }
                table
            }
        }
    };
}

colors!(
    background,
    sidebar,
    panel,
    raised,
    overlay,
    text,
    secondary_text,
    primary_text,
    border,
    control_border,
    hover,
    selection,
    text_selection,
    accent,
    primary,
    primary_hover,
    success,
    warning,
    warning_bg,
    danger,
    danger_bg,
);

macro_rules! terminal_colors {
    ($($field:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct TerminalColors {
            $(pub $field: Option<u32>,)+
            pub ansi: [Option<u32>; 16],
        }

        impl TerminalColors {
            const EMPTY: Self = Self { $($field: None,)+ ansi: [None; 16] };

            fn read(table: &Table, variant: &str) -> io::Result<Self> {
                let mut colors = Self {
                    $($field: read_color(table, variant, stringify!($field))?,)+
                    ansi: [None; 16],
                };
                if let Some(value) = table.get("ansi") {
                    let key = format!("{variant}.ansi");
                    let entries = value.as_array().filter(|entries| entries.len() == 16)
                        .ok_or_else(|| invalid(&key))?;
                    for (index, entry) in entries.iter().enumerate() {
                        if entry.as_str() != Some("") {
                            colors.ansi[index] = Some(entry.as_str().and_then(parse_hex)
                                .ok_or_else(|| invalid(&format!("{key}[{index}]")))?);
                        }
                    }
                }
                Ok(colors)
            }

            fn table(self) -> Table {
                let mut table = Table::new();
                $(if let Some(color) = self.$field {
                    table.insert(stringify!($field).into(), Value::String(hex(color)));
                })+
                if self.ansi.iter().any(Option::is_some) {
                    table.insert("ansi".into(), Value::Array(self.ansi.into_iter()
                        .map(|color| Value::String(color.map(hex).unwrap_or_default())).collect()));
                }
                table
            }
        }
    };
}

terminal_colors!(
    foreground,
    background,
    cursor,
    cursor_text,
    selection_foreground,
    selection_background,
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeDef {
    pub id: String,
    pub name: String,
    pub light: Colors,
    pub dark: Colors,
    pub builtin: bool,
}

pub fn parse_hex(value: &str) -> Option<u32> {
    let digits = value.strip_prefix('#')?;
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let color = u32::from_str_radix(digits, 16).ok()?;
    match digits.len() {
        3 => Some(u32::from_be_bytes([
            ((color >> 8) as u8 & 15) * 17,
            ((color >> 4) as u8 & 15) * 17,
            (color as u8 & 15) * 17,
            255,
        ])),
        6 => Some(color << 8 | 255),
        8 => Some(color),
        _ => None,
    }
}

pub fn hex(color: u32) -> String {
    if color & 255 == 255 {
        format!("#{:06x}", color >> 8)
    } else {
        format!("#{color:08x}")
    }
}

fn invalid(key: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Invalid value for {key}."),
    )
}

fn read_color(table: &Table, variant: &str, key: &str) -> io::Result<Option<u32>> {
    table
        .get(key)
        .map(|value| {
            value
                .as_str()
                .and_then(parse_hex)
                .ok_or_else(|| invalid(&format!("{variant}.{key}")))
        })
        .transpose()
}

fn read_theme(path: &Path, id: String) -> io::Result<ThemeDef> {
    let table: Table = fs::read_to_string(path)?
        .parse()
        .map_err(io::Error::other)?;
    let name = table
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("name"))?;
    let mut light = VELA_LIGHT;
    let mut dark = VELA_DARK;
    for (key, colors) in [("light", &mut light), ("dark", &mut dark)] {
        if let Some(variant) = table.get(key) {
            colors.read(variant.as_table().ok_or_else(|| invalid(key))?, key)?;
        }
    }
    Ok(ThemeDef {
        id,
        name: name.into(),
        light,
        dark,
        builtin: false,
    })
}

const BUILTIN_IDS: [&str; 6] = ["vela", "frambuesa", "bosque", "oceano", "ambar", "lavanda"];

fn valid_user_id(id: &str) -> bool {
    !id.is_empty()
        && !matches!(id, "." | "..")
        && !id.contains(['/', '\0'])
        && !BUILTIN_IDS
            .iter()
            .any(|builtin| builtin.eq_ignore_ascii_case(id))
}

fn theme_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir.join("themes")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect())
}

pub fn load_user_themes(dir: &Path) -> (Vec<ThemeDef>, Vec<String>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    let mut files = match theme_files(dir) {
        Ok(files) => files,
        Err(error) => {
            return (
                themes,
                vec![format!("Couldn't read the themes folder: {error}")],
            );
        }
    };
    files.sort();
    for path in files {
        let id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let result = if valid_user_id(id) {
            read_theme(&path, id.into())
        } else {
            Err(invalid("id"))
        };
        match result {
            Ok(theme) => themes.push(theme),
            Err(error) => errors.push(format!(
                "Couldn't read {}: {error}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    themes.sort_by_cached_key(|theme| (theme.name.to_lowercase(), theme.id.clone()));
    (themes, errors)
}

pub fn save_user_theme(dir: &Path, theme: &ThemeDef) -> io::Result<()> {
    if theme.builtin || !valid_user_id(&theme.id) {
        return Err(invalid("id"));
    }
    export_theme(
        theme,
        &dir.join("themes").join(format!("{}.toml", theme.id)),
    )
}

pub fn delete_user_theme(dir: &Path, id: &str) -> io::Result<()> {
    if !valid_user_id(id) {
        return Err(invalid("id"));
    }
    fs::remove_file(dir.join("themes").join(format!("{id}.toml")))
}

pub fn import_theme(dir: &Path, source: &Path) -> io::Result<ThemeDef> {
    if !source
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Choose a .toml file.",
        ));
    }
    let (users, _) = load_user_themes(dir);
    let mut theme = read_theme(source, String::new())?;
    theme.id = unused_theme_id(
        dir,
        source
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    )?;
    let base = theme.name.clone();
    let names = builtin_themes()
        .into_iter()
        .chain(users)
        .map(|theme| theme.name.to_lowercase())
        .collect::<Vec<_>>();
    for suffix in 2.. {
        if !names.contains(&theme.name.to_lowercase()) {
            break;
        }
        theme.name = format!("{base} ({suffix})");
    }
    save_user_theme(dir, &theme)?;
    Ok(theme)
}

pub fn export_theme(theme: &ThemeDef, dest: &Path) -> io::Result<()> {
    let file = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("path"))?;
    let dir = dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let table = Table::from_iter([
        ("name".into(), Value::String(theme.name.clone())),
        ("light".into(), Value::Table(theme.light.table())),
        ("dark".into(), Value::Table(theme.dark.table())),
    ]);
    let text = toml::to_string_pretty(&table).map_err(io::Error::other)?;
    crate::config::save_private(dir, file, &text)
}

fn slug(name: &str) -> String {
    let normalized = name.to_ascii_lowercase();
    normalized
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn new_theme_id<'a>(name: &str, existing: impl IntoIterator<Item = &'a str>) -> String {
    let slug = slug(name);
    let base = if slug.is_empty() { "tema" } else { &slug };
    let existing: std::collections::HashSet<String> =
        existing.into_iter().map(str::to_ascii_lowercase).collect();
    let mut id = base.to_owned();
    for suffix in 2.. {
        if !BUILTIN_IDS.contains(&id.as_str()) && !existing.contains(id.as_str()) {
            break;
        }
        id = format!("{base}-{suffix}");
    }
    id
}

pub fn unused_theme_id(dir: &Path, name: &str) -> io::Result<String> {
    let files = theme_files(dir)?;
    Ok(new_theme_id(
        name,
        files.iter().filter_map(|path| path.file_stem()?.to_str()),
    ))
}

/// WCAG relative luminance, compositing translucent foregrounds over the background.
pub fn contrast(foreground: u32, background: u32) -> f32 {
    let [r, g, b, alpha] = foreground
        .to_be_bytes()
        .map(|component| f32::from(component) / 255.);
    let [br, bg, bb, _] = background
        .to_be_bytes()
        .map(|component| f32::from(component) / 255.);
    let luminance = |rgb: [f32; 3]| {
        let [r, g, b] = rgb.map(|c| {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    let a = luminance([
        r * alpha + br * (1. - alpha),
        g * alpha + bg * (1. - alpha),
        b * alpha + bb * (1. - alpha),
    ]);
    let b = luminance([br, bg, bb]);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
/// HSV with hue in degrees and saturation and value between 0 and 1.
#[derive(Clone, Copy, Debug)]
pub struct Hsv {
    pub hue: f32,
    pub saturation: f32,
    pub value: f32,
}

impl Hsv {
    pub fn from_rgba(color: u32) -> Self {
        let [r, g, b, _] = color
            .to_be_bytes()
            .map(|component| f32::from(component) / 255.);
        let value = r.max(g).max(b);
        let delta = value - r.min(g).min(b);
        let hue = if delta == 0. {
            0.
        } else if value == r {
            (g - b) / delta
        } else if value == g {
            2. + (b - r) / delta
        } else {
            4. + (r - g) / delta
        };
        Self {
            hue: (hue * 60.).rem_euclid(360.),
            saturation: if value == 0. { 0. } else { delta / value },
            value,
        }
    }

    pub fn rgba(self, alpha: u8) -> u32 {
        let sector = self.hue.rem_euclid(360.) / 60.;
        let chroma = self.saturation.clamp(0., 1.) * self.value.clamp(0., 1.);
        let x = chroma * (1. - (sector.rem_euclid(2.) - 1.).abs());
        let minimum = self.value.clamp(0., 1.) - chroma;
        let [r, g, b] = match sector as u8 {
            0 => [chroma, x, 0.],
            1 => [x, chroma, 0.],
            2 => [0., chroma, x],
            3 => [0., x, chroma],
            4 => [x, 0., chroma],
            _ => [chroma, 0., x],
        }
        .map(|channel| ((channel + minimum) * 255.).round() as u8);
        u32::from_be_bytes([r, g, b, alpha])
    }
}

pub const VELA_LIGHT: Colors = Colors {
    background: 0xffffffff,
    sidebar: 0xfafafaff,
    panel: 0xffffffff,
    text: 0x292929ff,
    secondary_text: 0x5d5d5dff,
    border: 0xf2f2f2ff,
    selection: 0xf5f5f5ff,
    accent: 0x2563ebff,
    control_border: 0x8a8a8aff,
    hover: 0xf8f8f8ff,
    primary: 0x292929ff,
    primary_text: 0xffffffff,
    primary_hover: 0x414141ff,
    overlay: 0x00000055,
    raised: 0xffffffff,
    text_selection: 0x2563eb40,
    success: 0x166534ff,
    warning: 0x92400eff,
    warning_bg: 0xfffbebff,
    danger: 0xb91c1cff,
    danger_bg: 0xfef2f2ff,
    terminal: TerminalColors::EMPTY,
};

pub const VELA_DARK: Colors = Colors {
    background: 0x1c1c1cff,
    sidebar: 0x202020ff,
    panel: 0x1c1c1cff,
    text: 0xe5e5e5ff,
    secondary_text: 0xa1a1a1ff,
    border: 0x282828ff,
    selection: 0x2c2c2cff,
    accent: 0x60a5faff,
    control_border: 0x747474ff,
    hover: 0x282828ff,
    primary: 0xe5e5e5ff,
    primary_text: 0x1c1c1cff,
    primary_hover: 0xc9c9c9ff,
    overlay: 0x00000088,
    raised: 0x252525ff,
    text_selection: 0x60a5fa55,
    success: 0x86efacff,
    warning: 0xfcd34dff,
    warning_bg: 0x382d18ff,
    danger: 0xfca5a5ff,
    danger_bg: 0x3c2020ff,
    terminal: TerminalColors::EMPTY,
};

pub fn builtin_themes() -> Vec<ThemeDef> {
    [
        ("Vela", VELA_LIGHT, VELA_DARK),
        (
            "Raspberry",
            tinted(VELA_LIGHT, [0xfcf6fa, 0xf6edf4, 0xfffafd, 0x99185f]),
            tinted(VELA_DARK, [0x1f1a24, 0x261e2a, 0x231d27, 0xf08dbd]),
        ),
        (
            "Forest",
            tinted(VELA_LIGHT, [0xf5faf6, 0xeaf3ed, 0xf9fdf9, 0x206641]),
            tinted(VELA_DARK, [0x171f1b, 0x1e2922, 0x1b251f, 0x76d5a2]),
        ),
        (
            "Ocean",
            tinted(VELA_LIGHT, [0xf3f9fc, 0xe9f2f8, 0xf9fcff, 0x195f99]),
            tinted(VELA_DARK, [0x171f2a, 0x1d2937, 0x1c2532, 0x80baf4]),
        ),
        (
            "Amber",
            tinted(VELA_LIGHT, [0xfcf8f1, 0xf6eee0, 0xfffcf7, 0x975019]),
            tinted(VELA_DARK, [0x241f18, 0x2c251c, 0x29231a, 0xf1b273]),
        ),
        (
            "Lavender",
            tinted(VELA_LIGHT, [0xf8f6fd, 0xf0ebf8, 0xfcfaff, 0x6547ab]),
            tinted(VELA_DARK, [0x201c2a, 0x282236, 0x251f30, 0xbda5fa]),
        ),
    ]
    .into_iter()
    .zip(BUILTIN_IDS)
    .map(|((name, light, dark), id)| ThemeDef {
        id: id.into(),
        name: name.into(),
        light,
        dark,
        builtin: true,
    })
    .collect()
}

fn tinted(base: Colors, [background, sidebar, panel, accent]: [u32; 4]) -> Colors {
    let opaque = |rgb: u32| (rgb << 8) | 255;
    let (background, sidebar, panel, accent) = (
        opaque(background),
        opaque(sidebar),
        opaque(panel),
        opaque(accent),
    );
    let dark = base.background == VELA_DARK.background;
    Colors {
        background,
        sidebar,
        panel,
        raised: mix(panel, accent, 0.06),
        border: mix(panel, base.text, 0.12),
        hover: mix(sidebar, accent, 0.08),
        selection: mix(sidebar, accent, 0.15),
        accent,
        control_border: if dark { 0x8b8591ff } else { 0x767176ff },
        primary: accent,
        primary_hover: mix(accent, base.primary_text, if dark { 0.1 } else { 0.16 }),
        text_selection: accent & !255 | 0x55,
        terminal: TerminalColors {
            background: Some(background),
            foreground: Some(base.text),
            cursor: Some(accent),
            cursor_text: Some(base.primary_text),
            selection_foreground: Some(base.text),
            selection_background: Some(mix(panel, accent, 0.24)),
            ..TerminalColors::EMPTY
        },
        ..base
    }
}

fn mix(a: u32, b: u32, weight: f32) -> u32 {
    let a = a.to_be_bytes();
    let b = b.to_be_bytes();
    u32::from_be_bytes(std::array::from_fn(|index| {
        (f32::from(a[index]) * (1. - weight) + f32::from(b[index]) * weight).round() as u8
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_preserves_rgb_and_alpha() {
        for color in [
            0xff0000ff, 0xaa009988, 0x00000000, 0xffffffff, 0x11223344, 0xf4a390ff, 0x1dcfe1ff,
        ] {
            assert_eq!(Hsv::from_rgba(color).rgba(color as u8), color);
        }
    }

    #[test]
    fn scheme_and_legacy_colors() {
        assert_eq!(parse_hex("#aBc"), Some(0xaabbccff));
        assert_eq!(parse_hex("#000000AA"), Some(0x000000aa));
        assert_eq!(parse_hex("#+fe"), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manual.toml");
        fs::write(&path, "name = 'Manual'\n[light]\nbackground = '#ABC'").unwrap();
        let theme = import_theme(dir.path(), &path).unwrap();
        assert_eq!(theme.light.background, 0xaabbccff);
        assert_eq!(theme.light.panel, VELA_LIGHT.panel);
        assert_eq!(theme.dark, VELA_DARK);
        fs::write(&path, "name = 'Manual'\n[dark]\ncontrol_border = '#oops'").unwrap();
        assert!(
            import_theme(dir.path(), &path)
                .unwrap_err()
                .to_string()
                .contains("dark.control_border")
        );
    }

    #[test]
    fn round_trip_and_skip_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut draft = builtin_themes().remove(0);
        draft.id = "demo".into();
        draft.builtin = false;
        draft.light.overlay = 0x12345622;
        draft.dark.terminal.cursor = Some(0x443322ff);
        draft.dark.terminal.ansi[15] = Some(0xabcd_efff);
        save_user_theme(dir.path(), &draft).unwrap();
        fs::write(dir.path().join("themes/error.toml"), "broken").unwrap();
        export_theme(&draft, &dir.path().join("themes/vela.toml")).unwrap();
        let (themes, errors) = load_user_themes(dir.path());
        assert_eq!(themes, vec![draft.clone()]);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("error.toml"));
        assert!(errors[1].contains("vela.toml"));
        delete_user_theme(dir.path(), &draft.id).unwrap();
        assert!(load_user_themes(dir.path()).0.is_empty());
    }

    #[test]
    fn import_uses_free_ids_and_names_including_reserved_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Vela.toml");
        export_theme(&builtin_themes()[0], &path).unwrap();
        let first = import_theme(dir.path(), &path).unwrap();
        let next = import_theme(dir.path(), &path).unwrap();
        assert_eq!(
            (first.id.as_str(), first.name.as_str()),
            ("vela-2", "Vela (2)")
        );
        assert_eq!(
            (next.id.as_str(), next.name.as_str()),
            ("vela-3", "Vela (3)")
        );
        assert_eq!(new_theme_id("✨", ["tema"]), "tema-2");
    }

    #[test]
    fn english_theme_names_keep_legacy_ids_reserved() {
        let themes = builtin_themes();
        assert_eq!(
            themes
                .iter()
                .map(|theme| theme.id.as_str())
                .collect::<Vec<_>>(),
            BUILTIN_IDS
        );
        assert_eq!(
            themes
                .iter()
                .map(|theme| theme.name.as_str())
                .collect::<Vec<_>>(),
            ["Vela", "Raspberry", "Forest", "Ocean", "Amber", "Lavender"]
        );
        assert_eq!(new_theme_id("OCEANO", []), "oceano-2");
        assert_eq!(new_theme_id("Ocean", ["ocean"]), "ocean-2");
    }

    #[test]
    fn builtin_contrast() {
        for theme in builtin_themes() {
            for (variant, colors) in [("light", theme.light), ("dark", theme.dark)] {
                let mut text_min = f32::INFINITY;
                let mut control_min = f32::INFINITY;
                for color in [colors.text, colors.secondary_text] {
                    for surface in [colors.background, colors.panel, colors.sidebar] {
                        text_min = text_min.min(contrast(color, surface));
                    }
                }
                for color in [colors.danger, colors.warning, colors.success] {
                    text_min = text_min.min(contrast(color, colors.background));
                }
                text_min = text_min.min(contrast(colors.primary_text, colors.primary));
                for color in [colors.accent, colors.control_border] {
                    control_min = control_min.min(contrast(color, colors.background));
                }
                println!(
                    "{} {variant} {} {} {} {} {text_min:.2} {control_min:.2}",
                    theme.name,
                    hex(colors.background),
                    hex(colors.panel),
                    hex(colors.text),
                    hex(colors.accent)
                );
                assert!(text_min >= 4.5, "{} {variant}: {text_min}", theme.name);
                assert!(control_min >= 3., "{} {variant}: {control_min}", theme.name);
            }
        }
    }
}
