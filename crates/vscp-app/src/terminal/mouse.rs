// Adapted from Zed crates/terminal/src/mappings/mouse.rs (v1.27.0), GPL-3.0-or-later.

use std::iter::repeat_n;

/// Most of the code, and specifically the constants, in this are copied from Alacritty,
/// with modifications for our circumstances
use gpui::{Modifiers, MouseButton};

use alacritty_terminal::{index::Point, term::TermMode as Modes};

enum MouseFormat {
    Sgr,
    Normal(bool),
}

impl MouseFormat {
    fn from_mode(mode: Modes) -> Self {
        if mode.contains(Modes::SGR_MOUSE) {
            MouseFormat::Sgr
        } else if mode.contains(Modes::UTF8_MOUSE) {
            MouseFormat::Normal(true)
        } else {
            MouseFormat::Normal(false)
        }
    }
}

#[derive(Debug)]
enum MouseButtonCode {
    LeftButton = 0,
    MiddleButton = 1,
    RightButton = 2,
    LeftMove = 32,
    MiddleMove = 33,
    RightMove = 34,
    NoneMove = 35,
    ScrollUp = 64,
    ScrollDown = 65,
    Other = 99,
}

impl MouseButtonCode {
    fn from_move_button(e: Option<MouseButton>) -> Self {
        match e {
            Some(gpui::MouseButton::Left) => MouseButtonCode::LeftMove,
            Some(gpui::MouseButton::Middle) => MouseButtonCode::MiddleMove,
            Some(gpui::MouseButton::Right) => MouseButtonCode::RightMove,
            Some(gpui::MouseButton::Navigate(_)) => MouseButtonCode::Other,
            None => MouseButtonCode::NoneMove,
        }
    }

    fn from_button(e: MouseButton) -> Self {
        match e {
            gpui::MouseButton::Left => MouseButtonCode::LeftButton,
            gpui::MouseButton::Middle => MouseButtonCode::MiddleButton,
            gpui::MouseButton::Right => MouseButtonCode::RightButton,
            gpui::MouseButton::Navigate(_) => MouseButtonCode::Other,
        }
    }

    fn is_other(&self) -> bool {
        matches!(self, MouseButtonCode::Other)
    }
}

pub(crate) fn scroll_report(
    point: Point,
    lines: i32,
    modifiers: Modifiers,
    mode: Modes,
) -> Option<impl Iterator<Item = Vec<u8>>> {
    let button = if lines > 0 {
        MouseButtonCode::ScrollUp
    } else {
        MouseButtonCode::ScrollDown
    };
    mouse_report(point, button, true, modifiers, MouseFormat::from_mode(mode))
        .map(|report| repeat_n(report, lines.unsigned_abs() as usize))
}

pub(crate) fn mouse_button_report(
    point: Point,
    button: gpui::MouseButton,
    modifiers: Modifiers,
    pressed: bool,
    mode: Modes,
) -> Option<Vec<u8>> {
    let button = MouseButtonCode::from_button(button);
    if !button.is_other() && mode.intersects(Modes::MOUSE_MODE) {
        mouse_report(
            point,
            button,
            pressed,
            modifiers,
            MouseFormat::from_mode(mode),
        )
    } else {
        None
    }
}

pub(crate) fn mouse_moved_report(
    point: Point,
    button: Option<MouseButton>,
    modifiers: Modifiers,
    mode: Modes,
) -> Option<Vec<u8>> {
    let button = MouseButtonCode::from_move_button(button);

    if !button.is_other() && mode.intersects(Modes::MOUSE_MOTION | Modes::MOUSE_DRAG) {
        //Only drags are reported in drag mode, so block NoneMove.
        if mode.contains(Modes::MOUSE_DRAG) && matches!(button, MouseButtonCode::NoneMove) {
            None
        } else {
            mouse_report(point, button, true, modifiers, MouseFormat::from_mode(mode))
        }
    } else {
        None
    }
}

///Generate the bytes to send to the terminal, from the cell location, a mouse event, and the terminal mode
fn mouse_report(
    point: Point,
    button: MouseButtonCode,
    pressed: bool,
    modifiers: Modifiers,
    format: MouseFormat,
) -> Option<Vec<u8>> {
    if point.line.0 < 0 {
        return None;
    }

    let mut mods = 0;
    if modifiers.shift {
        mods += 4;
    }
    if modifiers.alt {
        mods += 8;
    }
    if modifiers.control {
        mods += 16;
    }

    match format {
        MouseFormat::Sgr => {
            Some(sgr_mouse_report(point, button as u8 + mods, pressed).into_bytes())
        }
        MouseFormat::Normal(utf8) => {
            if pressed {
                normal_mouse_report(point, button as u8 + mods, utf8)
            } else {
                normal_mouse_report(point, 3 + mods, utf8)
            }
        }
    }
}

fn normal_mouse_report(point: Point, button: u8, utf8: bool) -> Option<Vec<u8>> {
    let max_point = if utf8 { 2015 } else { 223 };

    if point.line.0 >= max_point || point.column.0 >= max_point as usize {
        return None;
    }

    let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

    let mouse_pos_encode = |pos: usize| -> Vec<u8> {
        let pos = 32 + 1 + pos;
        let first = 0xC0 + pos / 64;
        let second = 0x80 + (pos & 63);
        vec![first as u8, second as u8]
    };

    if utf8 && point.column.0 >= 95 {
        msg.append(&mut mouse_pos_encode(point.column.0));
    } else {
        msg.push(32 + 1 + point.column.0 as u8);
    }

    if utf8 && point.line >= 95 {
        msg.append(&mut mouse_pos_encode(point.line.0 as usize));
    } else {
        msg.push(32 + 1 + point.line.0 as u8);
    }

    Some(msg)
}

fn sgr_mouse_report(point: Point, button: u8, pressed: bool) -> String {
    let c = if pressed { 'M' } else { 'm' };

    format!(
        "\x1b[<{};{};{}{}",
        button,
        point.column.0 + 1,
        point.line.0 + 1,
        c
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};

    #[test]
    fn reports_sgr_utf8_and_legacy() {
        let point = Point::new(Line(2), Column(4));
        let modifiers = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(
            mouse_button_report(
                point,
                MouseButton::Left,
                modifiers,
                false,
                Modes::MOUSE_MODE | Modes::SGR_MOUSE
            )
            .unwrap(),
            b"\x1b[<16;5;3m",
        );
        assert_eq!(normal_mouse_report(point, 3, false).unwrap(), b"\x1b[M#%#");
        let wide = Point::new(Line(0), Column(100));
        assert_eq!(
            normal_mouse_report(wide, 0, true).unwrap(),
            b"\x1b[M \xc2\x85!"
        );
        assert_eq!(
            scroll_report(point, -3, modifiers, Modes::SGR_MOUSE)
                .unwrap()
                .count(),
            3
        );
    }
}
