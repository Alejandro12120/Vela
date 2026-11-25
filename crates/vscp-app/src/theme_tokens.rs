use vscp_core::appearance::Colors;

macro_rules! tokens {
    (
        ui { $($token:ident: $field:ident = $label:literal @ $group:literal,)+ }
        terminal { $($term:ident: $term_field:ident = $term_label:literal,)+ }
    ) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Token { $($token,)+ $($term,)+ Ansi(usize) }

        pub const TOKENS: &[Token] = &[
            $(Token::$token,)+ $(Token::$term,)+
            Token::Ansi(0), Token::Ansi(1), Token::Ansi(2), Token::Ansi(3),
            Token::Ansi(4), Token::Ansi(5), Token::Ansi(6), Token::Ansi(7),
            Token::Ansi(8), Token::Ansi(9), Token::Ansi(10), Token::Ansi(11),
            Token::Ansi(12), Token::Ansi(13), Token::Ansi(14), Token::Ansi(15),
        ];

        impl Token {
            pub fn get(self, colors: &Colors) -> Option<u32> {
                match self {
                    $(Self::$token => Some(colors.$field),)+
                    $(Self::$term => colors.terminal.$term_field,)+
                    Self::Ansi(index) => colors.terminal.ansi[index],
                }
            }

            pub fn set(self, colors: &mut Colors, value: Option<u32>) {
                match self {
                    $(Self::$token => if let Some(value) = value { colors.$field = value },)+
                    $(Self::$term => colors.terminal.$term_field = value,)+
                    Self::Ansi(index) => colors.terminal.ansi[index] = value,
                }
            }

            pub fn group(self) -> &'static str {
                match self {
                    $(Self::$token => $group,)+
                    _ => "Terminal",
                }
            }

            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$token => $label,)+
                    $(Self::$term => $term_label,)+
                    Self::Ansi(index) => ANSI[index],
                }
            }

            pub fn key(self) -> &'static str {
                match self {
                    $(Self::$token => stringify!($field),)+
                    $(Self::$term => concat!("terminal_", stringify!($term_field)),)+
                    Self::Ansi(index) => ANSI_KEYS[index],
                }
            }
        }
    };
}

tokens! {
    ui {
        Background: background = "Fondo" @ "Superficies",
        Sidebar: sidebar = "Barra lateral" @ "Superficies",
        Panel: panel = "Panel" @ "Superficies",
        Raised: raised = "Elevado" @ "Superficies",
        Overlay: overlay = "Velo" @ "Superficies",
        Text: text = "Texto" @ "Texto",
        SecondaryText: secondary_text = "Texto secundario" @ "Texto",
        PrimaryText: primary_text = "Texto sobre primario" @ "Texto",
        Border: border = "Borde" @ "Bordes e interacción",
        ControlBorder: control_border = "Borde de control" @ "Bordes e interacción",
        Hover: hover = "Hover" @ "Bordes e interacción",
        Selection: selection = "Selección" @ "Bordes e interacción",
        TextSelection: text_selection = "Selección de texto" @ "Bordes e interacción",
        Accent: accent = "Acento" @ "Acción",
        Primary: primary = "Primario" @ "Acción",
        PrimaryHover: primary_hover = "Primario en hover" @ "Acción",
        Success: success = "Correcto" @ "Estados",
        Warning: warning = "Aviso" @ "Estados",
        WarningBg: warning_bg = "Fondo de aviso" @ "Estados",
        Danger: danger = "Error" @ "Estados",
        DangerBg: danger_bg = "Fondo de error" @ "Estados",
    }
    terminal {
        TerminalBackground: background = "Fondo",
        TerminalText: foreground = "Texto",
        Cursor: cursor = "Cursor",
        CursorText: cursor_text = "Texto bajo el cursor",
        SelectionText: selection_foreground = "Texto seleccionado",
        SelectionBg: selection_background = "Fondo de selección",
    }
}

pub const BASIC: &[Token] = &[
    Token::Background,
    Token::Sidebar,
    Token::Panel,
    Token::Text,
    Token::SecondaryText,
    Token::Accent,
];

impl Token {
    pub fn terminal(self) -> bool {
        self.group() == "Terminal"
    }

    pub fn has_alpha(self, color: u32) -> bool {
        matches!(self, Self::Overlay | Self::TextSelection) || color & 255 != 255
    }
}

const ANSI: [&str; 16] = [
    "Color 0 · Negro",
    "Color 1 · Rojo",
    "Color 2 · Verde",
    "Color 3 · Amarillo",
    "Color 4 · Azul",
    "Color 5 · Magenta",
    "Color 6 · Cian",
    "Color 7 · Blanco",
    "Color 8 · Negro brillante",
    "Color 9 · Rojo brillante",
    "Color 10 · Verde brillante",
    "Color 11 · Amarillo brillante",
    "Color 12 · Azul brillante",
    "Color 13 · Magenta brillante",
    "Color 14 · Cian brillante",
    "Color 15 · Blanco brillante",
];

const ANSI_KEYS: [&str; 16] = [
    "color0", "color1", "color2", "color3", "color4", "color5", "color6", "color7", "color8",
    "color9", "color10", "color11", "color12", "color13", "color14", "color15",
];
