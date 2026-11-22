use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color, NamedColor, Rgb},
};

#[derive(Clone)]
pub struct Palette {
    pub foreground: u32,
    pub background: u32,
    pub cursor: Option<u32>,
    pub cursor_text: Option<u32>,
    pub selection_foreground: Option<u32>,
    pub selection_background: Option<u32>,
    pub ansi: [u32; 16],
}

impl Default for Palette {
    fn default() -> Self {
        // https://github.com/kovidgoyal/kitty/blob/master/kitty/options/definition.py
        Self {
            foreground: 0xdddddd,
            background: 0x000000,
            cursor: Some(0xcccccc),
            cursor_text: Some(0x111111),
            selection_foreground: Some(0x000000),
            selection_background: Some(0xfffacd),
            ansi: [
                0x000000, 0xcc0403, 0x19cb00, 0xcecb00, 0x0d73cc, 0xcb1ed1, 0x0dcdcd, 0xdddddd,
                0x767676, 0xf2201f, 0x23fd00, 0xfffd00, 0x1a8fff, 0xfd28ff, 0x14ffff, 0xffffff,
            ],
        }
    }
}

impl Palette {
    pub fn set(&mut self, key: &str, value: &str) {
        let color = parse_color(value);
        match key {
            "cursor" if value == "none" || color.is_some() => self.cursor = color,
            "cursor_text_color" if value == "background" || color.is_some() => {
                self.cursor_text = color
            }
            "selection_foreground" if value == "none" || color.is_some() => {
                self.selection_foreground = color
            }
            "selection_background" if value == "none" || color.is_some() => {
                self.selection_background = color
            }
            _ => {
                let Some(color) = color else { return };
                match key {
                    "foreground" => self.foreground = color,
                    "background" => self.background = color,
                    _ => {
                        if let Some(index) = key
                            .strip_prefix("color")
                            .and_then(|index| index.parse::<usize>().ok())
                            && let Some(entry) = self.ansi.get_mut(index)
                        {
                            *entry = color;
                        }
                    }
                }
            }
        }
    }

    pub fn index(&self, index: usize, overrides: &Colors) -> u32 {
        let value = match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let index = (index - 16) as u32;
                let component = |n| if n == 0 { 0 } else { 55 + 40 * n };
                (component(index / 36) << 16)
                    | (component(index / 6 % 6) << 8)
                    | component(index % 6)
            }
            232..=255 => (8 + 10 * (index as u32 - 232)) * 0x010101,
            257 => self.background,
            258 => self.cursor.unwrap_or(self.foreground),
            _ => self.foreground,
        };
        if index < alacritty_terminal::term::color::COUNT {
            overrides[index].map_or(value, rgb_value)
        } else {
            value
        }
    }

    pub fn color(&self, color: Color, overrides: &Colors) -> u32 {
        match color {
            Color::Named(NamedColor::DimForeground) => self.index(256, overrides),
            Color::Named(NamedColor::BrightForeground) => self.index(256, overrides),
            Color::Named(name) => self.index(name as usize, overrides),
            Color::Indexed(index) => self.index(index as usize, overrides),
            Color::Spec(rgb) => rgb_value(rgb),
        }
    }
}

fn rgb_value(rgb: Rgb) -> u32 {
    u32::from_be_bytes([0, rgb.r, rgb.g, rgb.b])
}

pub fn to_rgb(value: u32) -> Rgb {
    let [_, r, g, b] = value.to_be_bytes();
    Rgb { r, g, b }
}

fn parse_color(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        3 => {
            let r = ((value >> 8) & 15) * 17;
            let g = ((value >> 4) & 15) * 17;
            let b = (value & 15) * 17;
            Some((r << 16) | (g << 8) | b)
        }
        6 => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_cube_grey_and_truecolor() {
        let palette = Palette::default();
        let overrides = Colors::default();
        assert_eq!(
            palette.color(Color::Named(NamedColor::Red), &overrides),
            0xcc0403
        );
        assert_eq!(palette.color(Color::Indexed(15), &overrides), 0xffffff);
        assert_eq!(palette.color(Color::Indexed(202), &overrides), 0xff5f00);
        assert_eq!(palette.color(Color::Indexed(232), &overrides), 0x080808);
        assert_eq!(palette.color(Color::Indexed(255), &overrides), 0xeeeeee);
        assert_eq!(
            palette.color(
                Color::Spec(Rgb {
                    r: 15,
                    g: 64,
                    b: 200
                }),
                &overrides
            ),
            0x0f40c8
        );
    }
}
