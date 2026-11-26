use vela_core::appearance::Colors;

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
        Background: background = "Background" @ "Surfaces",
        Sidebar: sidebar = "Sidebar" @ "Surfaces",
        Panel: panel = "Panel" @ "Surfaces",
        Raised: raised = "Raised" @ "Surfaces",
        Overlay: overlay = "Overlay" @ "Surfaces",
        Text: text = "Text" @ "Text",
        SecondaryText: secondary_text = "Secondary Text" @ "Text",
        PrimaryText: primary_text = "Text on Primary" @ "Text",
        Border: border = "Border" @ "Borders and Interaction",
        ControlBorder: control_border = "Control Border" @ "Borders and Interaction",
        Hover: hover = "Hover" @ "Borders and Interaction",
        Selection: selection = "Selection" @ "Borders and Interaction",
        TextSelection: text_selection = "Text Selection" @ "Borders and Interaction",
        Accent: accent = "Accent" @ "Action",
        Primary: primary = "Primary" @ "Action",
        PrimaryHover: primary_hover = "Primary Hover" @ "Action",
        Success: success = "Success" @ "Status",
        Warning: warning = "Warning" @ "Status",
        WarningBg: warning_bg = "Warning Background" @ "Status",
        Danger: danger = "Error" @ "Status",
        DangerBg: danger_bg = "Error Background" @ "Status",
    }
    terminal {
        TerminalBackground: background = "Background",
        TerminalText: foreground = "Text",
        Cursor: cursor = "Cursor",
        CursorText: cursor_text = "Text Under Cursor",
        SelectionText: selection_foreground = "Selected Text",
        SelectionBg: selection_background = "Selection Background",
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
    "Color 0 · Black",
    "Color 1 · Red",
    "Color 2 · Green",
    "Color 3 · Yellow",
    "Color 4 · Blue",
    "Color 5 · Magenta",
    "Color 6 · Cyan",
    "Color 7 · White",
    "Color 8 · Bright Black",
    "Color 9 · Bright Red",
    "Color 10 · Bright Green",
    "Color 11 · Bright Yellow",
    "Color 12 · Bright Blue",
    "Color 13 · Bright Magenta",
    "Color 14 · Bright Cyan",
    "Color 15 · Bright White",
];

const ANSI_KEYS: [&str; 16] = [
    "color0", "color1", "color2", "color3", "color4", "color5", "color6", "color7", "color8",
    "color9", "color10", "color11", "color12", "color13", "color14", "color15",
];
