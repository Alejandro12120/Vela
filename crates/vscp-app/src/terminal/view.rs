// Adapted from Zed crates/terminal_view/src/terminal_view.rs (v1.27.0), GPL-3.0-or-later.

use std::{ffi::OsString, ops::Range};

use alacritty_terminal::{
    grid::{Dimensions, Scroll},
    index::{Column, Direction, Line, Point as CellPoint},
    selection::{Selection, SelectionType},
    term::TermMode,
};
use gpui::{
    App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle, Font,
    FontFallbacks, FontFeatures, KeyBinding, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, Subscription, Task, UTF16Selection,
    Window, actions, div, font, prelude::*, px,
};

use crate::{
    components::button::{Variant, button},
    dev,
    theme::Theme,
};

use super::{
    GridSize, Terminal,
    colors::Palette,
    element::{Geometry, Surface},
    font_size, keys,
    kitty::{Kitty, OptionAsAlt},
    mouse,
};

actions!(
    terminal,
    [
        Tab,
        BackTab,
        Escape,
        Copy,
        Paste,
        SelectAll,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", Tab, Some("Terminal")),
        KeyBinding::new("shift-tab", BackTab, Some("Terminal")),
        KeyBinding::new("escape", Escape, Some("Terminal")),
    ]);
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        crate::keybindings::binding(cx, "cmd-c", Copy, Some("Terminal")),
        crate::keybindings::binding(cx, "cmd-v", Paste, Some("Terminal")),
        crate::keybindings::binding(cx, "cmd-a", SelectAll, Some("Terminal")),
        crate::keybindings::binding(cx, "cmd-+", IncreaseFontSize, Some("Terminal")),
        crate::keybindings::binding(cx, "cmd--", DecreaseFontSize, Some("Terminal")),
        crate::keybindings::binding(cx, "cmd-0", ResetFontSize, Some("Terminal")),
        crate::keybindings::binding(cx, "cmd-=", IncreaseFontSize, Some("Terminal")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        crate::keybindings::binding(cx, "ctrl-shift-c", Copy, Some("Terminal")),
        crate::keybindings::binding(cx, "ctrl-shift-v", Paste, Some("Terminal")),
        crate::keybindings::binding(cx, "ctrl-shift-a", SelectAll, Some("Terminal")),
        crate::keybindings::binding(cx, "ctrl-*", IncreaseFontSize, Some("Terminal")),
        crate::keybindings::binding(cx, "ctrl-+", IncreaseFontSize, Some("Terminal")),
        crate::keybindings::binding(cx, "ctrl-_", DecreaseFontSize, Some("Terminal")),
        crate::keybindings::binding(cx, "ctrl-=", ResetFontSize, Some("Terminal")),
    ]);
}

pub struct Restart;

pub(super) struct Marked {
    pub text: String,
    selection: Range<usize>,
}

pub struct TerminalView {
    pub(super) terminal: Option<Terminal>,
    pub(super) settings: Kitty,
    pub(super) palette: Palette,
    kitty_size: f32,
    pub(super) focus: FocusHandle,
    pub(super) fonts: [Font; 4],
    pub(super) geometry: Option<Geometry>,
    pub(super) ime_bounds: Option<Bounds<Pixels>>,
    pub(super) last_logged_size: Option<GridSize>,
    pub(super) marked: Option<Marked>,
    selected: bool,
    error: Option<String>,
    kitty_error: Option<String>,
    pending: Vec<String>,
    startup: Option<Task<()>>,
    ready_to_spawn: Option<Vec<OsString>>,
    redraw: Option<Task<()>>,
    activation: Option<Subscription>,
    scroll_remainder: f32,
    selecting: bool,
    mouse_press: Option<MouseButton>,
    last_report: Option<CellPoint>,
}

impl EventEmitter<Restart> for TerminalView {}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        font_size::ensure(cx);
        cx.observe_global::<font_size::FontSize>(|view, cx| view.apply_font_size(cx))
            .detach();
        let mut view = Self {
            terminal: None,
            settings: Kitty::default(),
            palette: Palette::default(),
            kitty_size: Kitty::default().size,
            focus: cx.focus_handle(),
            fonts: std::array::from_fn(|_| font(crate::theme::monospace())),
            geometry: None,
            ime_bounds: None,
            last_logged_size: None,
            marked: None,
            selected: false,
            error: None,
            kitty_error: None,
            pending: Vec::new(),
            startup: None,
            ready_to_spawn: None,
            redraw: None,
            activation: None,
            scroll_remainder: 0.,
            selecting: false,
            mouse_press: None,
            last_report: None,
        };
        view.set_settings(Kitty::default(), cx);
        view
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    pub fn idle(&self) -> bool {
        self.startup.is_none()
            && self.ready_to_spawn.is_none()
            && self.terminal.as_ref().is_none_or(Terminal::ended)
    }

    pub fn started(&self) -> bool {
        self.startup.is_some()
            || self.ready_to_spawn.is_some()
            || self.terminal.is_some()
            || self.error.is_some()
    }

    pub fn alternate_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn command(&mut self, command: String) {
        if self.startup.is_some() || self.ready_to_spawn.is_some() {
            self.pending.push(command);
        } else if let Some(terminal) = &self.terminal {
            terminal.input(command);
        }
    }

    pub fn kitty_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.kitty_error = Some(format!("No se pudo abrir Kitty. {error}"));
        cx.notify();
    }

    pub fn stop(&mut self) {
        self.startup.take();
        self.ready_to_spawn.take();
        self.redraw.take();
        self.terminal.take();
    }

    pub fn reload(&mut self, settings: Kitty, window: &Window, cx: &mut Context<Self>) {
        self.set_settings(settings, cx);
        if self.selected && window.is_window_active() {
            cx.notify();
        }
    }

    pub fn set_selected(&mut self, selected: bool, window: &Window) {
        self.selected = selected;
        if let Some(terminal) = &self.terminal {
            terminal
                .listener
                .redraw
                .visible(selected && window.is_window_active());
        }
    }

    pub fn start(&mut self, args: Vec<OsString>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.idle() {
            return;
        }
        self.redraw.take();
        self.terminal.take();
        self.marked.take();
        self.error.take();
        self.kitty_error.take();
        self.pending.clear();
        self.last_logged_size.take();
        self.scroll_remainder = 0.;
        self.selecting = false;
        self.mouse_press = None;
        self.last_report = None;

        if self.activation.is_none() {
            self.activation = Some(cx.observe_window_activation(window, |view, window, cx| {
                view.set_selected(view.selected, window);
                if view.selected && window.is_window_active() {
                    cx.notify();
                }
            }));
        }
        let settings = cx.background_executor().spawn(async { Kitty::load() });
        self.startup = Some(cx.spawn_in(window, async move |view, cx| {
            let settings = settings.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.startup.take();
                view.set_settings(settings, cx);
                view.ready_to_spawn = Some(args);
                if view.selected && window.is_window_active() {
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// OpenSSH sends the initial PTY size while opening the channel. Start it
    /// with the measured size so it does not race installation of SIGWINCH.
    pub(super) fn spawn_measured(
        &mut self,
        size: GridSize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(args) = self.ready_to_spawn.take() else {
            return;
        };
        let mut settings = self.settings.clone();
        settings.palette = self.palette;
        let task = cx
            .background_executor()
            .spawn(async move { Terminal::spawn("/usr/bin/ssh", args, &settings, size) });
        self.startup = Some(cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.startup.take();
                match result {
                    Ok(terminal) => {
                        let signal = terminal.listener.redraw.clone();
                        for command in view.pending.drain(..) {
                            terminal.send(command);
                        }
                        terminal.set_palette(view.palette);
                        view.terminal = Some(terminal);
                        view.set_selected(view.selected, window);
                        view.redraw = Some(cx.spawn_in(window, async move |view, cx| {
                            loop {
                                signal.wait().await;
                                if view
                                    .update_in(cx, |view, window, cx| {
                                        if view.selected && window.is_window_active() {
                                            let signal = signal.clone();
                                            cx.on_next_frame(window, move |view, window, cx| {
                                                if signal.drain()
                                                    && view.selected
                                                    && window.is_window_active()
                                                {
                                                    cx.notify();
                                                }
                                            });
                                        } else {
                                            signal.drain();
                                        }
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }));
                        if let Some(input) = dev::terminal_input()
                            && let Some(terminal) = &view.terminal
                        {
                            terminal.send(input);
                        }
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                if view.selected && window.is_window_active() {
                    cx.notify();
                }
            });
        }));
    }

    pub fn set_settings(&mut self, mut settings: Kitty, cx: &App) {
        let names = cx.text_system().all_font_names();
        let configured = settings.family.clone();
        settings.resolve_family(&names);
        if crate::dev::logging() {
            let key = crate::theme::family_key(&configured);
            let present = names
                .iter()
                .any(|name| crate::theme::family_key(name) == key);
            let path = settings
                .path
                .as_ref()
                .map_or_else(|| "none".to_owned(), |path| path.display().to_string());
            crate::dev::log(format!(
                "Terminal: kitty.conf {path}; font_family {configured}; resolved {}; installed {}",
                settings.family,
                if present { "yes" } else { "no" }
            ));
        }
        let installed = |family: &str| names.iter().any(|name| name.eq_ignore_ascii_case(family));
        let fallback = names
            .iter()
            .find(|name| name.as_str() == "Symbols Nerd Font Mono")
            .or_else(|| names.iter().find(|name| name.contains("Nerd Font")))
            .map(|name| FontFallbacks(vec![name.clone()].into()));
        self.fonts = std::array::from_fn(|index| {
            let family = index
                .checked_sub(1)
                .and_then(|index| settings.variants[index].as_deref())
                .filter(|family| installed(family))
                .unwrap_or(&settings.family);
            let mut face = font(family.to_owned());
            face.features = FontFeatures::disable_ligatures();
            face.fallbacks = fallback.clone();
            if index & 1 != 0 {
                face = face.bold();
            }
            if index & 2 != 0 {
                face = face.italic();
            }
            face
        });
        self.kitty_size = settings.size;
        settings.size = font_size::effective(self.kitty_size, cx);
        if let Some(terminal) = &self.terminal {
            terminal.reconfigure(&settings);
        }
        self.settings = settings;
        self.geometry = None;
    }

    fn apply_font_size(&mut self, cx: &mut Context<Self>) {
        let size = font_size::effective(self.kitty_size, cx);
        if self.settings.size == size {
            return;
        }
        self.settings.size = size;
        self.geometry = None;
        cx.notify();
    }

    pub fn set_palette(&mut self, colors: vscp_core::appearance::TerminalColors) {
        let palette = self.settings.palette.with_theme(colors);
        if self.palette != palette {
            self.palette = palette;
            if let Some(terminal) = &self.terminal {
                terminal.set_palette(palette);
            }
        }
    }

    pub fn settings(&self) -> &Kitty {
        &self.settings
    }

    fn mode(&self) -> TermMode {
        self.terminal
            .as_ref()
            .map(|terminal| *terminal.grid.lock().mode())
            .unwrap_or_default()
    }

    fn key(&self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.marked.is_some() || event.keystroke.modifiers.platform {
            return;
        }
        let mode = self.mode();
        if let Some(bytes) = keys::keypad(mode).or_else(|| {
            keys::to_esc_str(
                &event.keystroke,
                mode,
                option_as_alt(self.settings.option_as_alt, &event.keystroke),
            )
        }) {
            self.special(bytes.as_bytes(), cx);
        }
    }

    fn special(&self, bytes: &[u8], cx: &mut Context<Self>) {
        if self.marked.is_none()
            && let Some(terminal) = &self.terminal
        {
            terminal.input(bytes);
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus(window, cx);
        let (Some(terminal), Some(geometry)) = (&self.terminal, self.geometry) else {
            return;
        };
        let mut grid = terminal.grid.lock();
        let (point, side) = geometry.cell(event.position, grid.grid().display_offset());
        let mode = *grid.mode();
        if !terminal.ended()
            && !event.modifiers.shift
            && mode.intersects(TermMode::MOUSE_MODE)
            && point.line.0 >= 0
        {
            if let Some(report) =
                mouse::mouse_button_report(point, event.button, event.modifiers, true, mode)
            {
                self.mouse_press = Some(event.button);
                terminal.send(report);
            }
        } else if event.button == MouseButton::Left {
            let kind = match event.click_count {
                1 => SelectionType::Simple,
                2 => SelectionType::Semantic,
                _ => SelectionType::Lines,
            };
            grid.selection = Some(Selection::new(kind, point, side));
            self.selecting = true;
            cx.notify();
        }
        self.last_report = Some(point);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(terminal), Some(geometry)) = (&self.terminal, self.geometry) else {
            return;
        };
        let mut grid = terminal.grid.lock();
        let (point, side) = geometry.cell(event.position, grid.grid().display_offset());
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            if let Some(selection) = grid.selection.as_mut() {
                selection.update(point, side);
                cx.notify();
            }
        } else if !event.modifiers.shift && !terminal.ended() && self.last_report != Some(point) {
            self.last_report = Some(point);
            if let Some(report) = mouse::mouse_moved_report(
                point,
                event.pressed_button,
                event.modifiers,
                *grid.mode(),
            ) {
                terminal.send(report);
            }
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.button == MouseButton::Left {
            self.selecting = false;
        }
        if self.mouse_press != Some(event.button) {
            return;
        }
        self.mouse_press = None;
        let (Some(terminal), Some(geometry)) = (&self.terminal, self.geometry) else {
            return;
        };
        let grid = terminal.grid.lock();
        let (point, _) = geometry.cell(event.position, grid.grid().display_offset());
        if let Some(report) =
            mouse::mouse_button_report(point, event.button, event.modifiers, false, *grid.mode())
        {
            terminal.send(report);
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(terminal), Some(geometry)) = (&self.terminal, self.geometry) else {
            return;
        };
        let delta = event.delta.pixel_delta(geometry.height);
        self.scroll_remainder += delta.y / geometry.height;
        let lines = self.scroll_remainder.trunc() as i32;
        self.scroll_remainder -= lines as f32;
        if lines == 0 {
            return;
        }
        let mut grid = terminal.grid.lock();
        let mode = *grid.mode();
        if !terminal.ended()
            && !event.modifiers.shift
            && mode.intersects(TermMode::MOUSE_MODE)
            && grid.grid().display_offset() == 0
        {
            let (point, _) = geometry.cell(event.position, 0);
            if let Some(reports) = mouse::scroll_report(point, lines, event.modifiers, mode) {
                terminal.send(reports.flatten().collect::<Vec<_>>());
            }
        } else if !terminal.ended()
            && !event.modifiers.shift
            && mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
        {
            let key = if lines > 0 { "up" } else { "down" };
            if let Ok(key) = Keystroke::parse(key)
                && let Some(bytes) = keys::to_esc_str(&key, mode, false)
            {
                terminal.send(bytes.as_bytes().repeat(lines.unsigned_abs() as usize));
            }
        } else {
            grid.scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
        cx.stop_propagation();
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(window, cx);
        self.set_palette(theme.terminal);
        let ended = self.terminal.as_ref().is_some_and(Terminal::ended) || self.error.is_some();
        let status = self
            .error
            .as_ref()
            .map(|error| format!("No se pudo abrir la terminal. {error}"))
            .unwrap_or_else(|| "La sesión de terminal ha terminado".into());
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(|view, event, _, cx| view.key(event, cx)))
            .on_action(cx.listener(|view, _: &Tab, _, cx| view.special(b"\t", cx)))
            .on_action(cx.listener(|view, _: &BackTab, _, cx| view.special(b"\x1b[Z", cx)))
            .on_action(cx.listener(|view, _: &Escape, _, cx| view.special(b"\x1b", cx)))
            .on_action(cx.listener(|view, _: &Copy, _, cx| {
                if let Some(text) = view
                    .terminal
                    .as_ref()
                    .and_then(|term| term.grid.lock().selection_to_string())
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|view, _: &Paste, _, cx| {
                if let Some(terminal) = &view.terminal
                    && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
                {
                    terminal.paste(&text);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|view, _: &SelectAll, _, cx| {
                if let Some(terminal) = &view.terminal {
                    let mut grid = terminal.grid.lock();
                    let first = CellPoint::new(Line(-(grid.history_size() as i32)), Column(0));
                    let last = CellPoint::new(
                        Line(grid.screen_lines() as i32 - 1),
                        Column(grid.columns() - 1),
                    );
                    let mut selection =
                        Selection::new(SelectionType::Simple, first, Direction::Left);
                    selection.update(last, Direction::Right);
                    grid.selection = Some(selection);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|view, _: &IncreaseFontSize, _, cx| {
                font_size::increase(cx, view.kitty_size);
            }))
            .on_action(cx.listener(|view, _: &DecreaseFontSize, _, cx| {
                font_size::decrease(cx, view.kitty_size);
            }))
            .on_action(cx.listener(|view, _: &ResetFontSize, _, cx| {
                font_size::reset(cx, view.kitty_size);
            }))
            .child(
                div()
                    .id("terminal-surface")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
                    .on_scroll(cx.listener(Self::scroll))
                    .child(Surface(cx.entity())),
            )
            .when_some(self.kitty_error.clone(), |view, error| {
                view.child(div().px_2().py_1().text_color(theme.danger).child(error))
            })
            .when(ended, |view| {
                view.child(
                    div()
                        .flex_none()
                        .h(px(40.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(theme.glassy, |status| status.bg(theme.content(theme.panel)))
                        .border_t_1()
                        .border_color(theme.border)
                        .text_color(theme.secondary_text)
                        .child(div().min_w_0().flex_1().truncate().child(status))
                        .child(button(
                            "terminal-restart",
                            "Abrir de nuevo",
                            Variant::Secondary,
                            theme,
                            cx.listener(|_, _, _, cx| cx.emit(Restart)),
                        )),
                )
            })
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let text: Vec<_> = self.marked.as_ref()?.text.encode_utf16().collect();
        let range = range.start.min(text.len())..range.end.min(text.len());
        *adjusted = Some(range.clone());
        Some(String::from_utf16_lossy(text.get(range)?))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self
                .marked
                .as_ref()
                .map_or(0..0, |marked| marked.selection.clone()),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        Some(0..self.marked.as_ref()?.text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked.take();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked.take();
        if let Some(terminal) = &self.terminal {
            terminal.input(text);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            self.marked = None;
        } else {
            self.marked = Some(Marked {
                text: text.into(),
                selection: selection.unwrap_or(0..0),
            });
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.ime_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn option_as_alt(option: OptionAsAlt, key: &Keystroke) -> bool {
    if !key.modifiers.alt {
        return false;
    }
    match option {
        OptionAsAlt::No => false,
        OptionAsAlt::Both => true,
        OptionAsAlt::Left => keys::native_key().is_some_and(|(_, flags)| flags & 0x20 != 0),
        OptionAsAlt::Right => keys::native_key().is_some_and(|(_, flags)| flags & 0x40 != 0),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::{file_workspace::SessionView, workspace::ShowFiles};

    type Sink = Arc<Mutex<Vec<u8>>>;

    struct Host {
        terminal: Entity<TerminalView>,
        view: SessionView,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .key_context("Workspace")
                .on_action(cx.listener(|host, _: &ShowFiles, _, cx| {
                    host.view = SessionView::Files;
                    cx.notify();
                }))
                .when(self.view == SessionView::Terminal, |host| {
                    host.child(self.terminal.clone())
                })
        }
    }

    fn open(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Host>,
        Entity<TerminalView>,
        Sink,
        &mut VisualTestContext,
    ) {
        cx.update(|cx| {
            crate::workspace::bind_keys(cx);
            bind_keys(cx);
        });
        let sink = Sink::default();
        let (host, cx) = cx.add_window_view({
            let sink = sink.clone();
            move |window, cx| {
                let terminal = cx.new(|cx| {
                    let mut view = TerminalView::new(cx);
                    let mut terminal = Terminal::new(&view.settings, GridSize::INITIAL);
                    terminal.sink = Some(sink);
                    view.terminal = Some(terminal);
                    view.focus(window, cx);
                    view
                });
                Host {
                    terminal,
                    view: SessionView::Terminal,
                }
            }
        });
        cx.run_until_parked();
        let terminal = host.read_with(cx, |host, _| host.terminal.clone());
        (host, terminal, sink, cx)
    }

    #[gpui::test]
    fn changing_theme_updates_a_running_pty_palette(cx: &mut TestAppContext) {
        let (_, terminal, _, cx) = open(cx);
        let grid = terminal.update(cx, |view, _| {
            let child = Terminal::spawn("/bin/cat", Vec::new(), &view.settings, GridSize::INITIAL)
                .expect("start an isolated PTY");
            let grid = child.grid.clone();
            view.terminal = Some(child);
            grid
        });

        cx.update(|_, cx| {
            let mut appearance = crate::appearance::Appearance::default();
            appearance.set_settings(vscp_core::appearance::AppearanceSettings {
                scheme: vscp_core::appearance::Scheme::Dark,
                dark_theme: "bosque".into(),
                ..Default::default()
            });
            cx.set_global(appearance);
            cx.refresh_windows();
        });
        cx.run_until_parked();

        terminal.read_with(cx, |view, _| {
            let child = view.terminal.as_ref().unwrap();
            assert!(Arc::ptr_eq(&child.grid, &grid));
            assert!(!child.ended());
            assert_eq!(view.palette.background, 0x171f1b);
            assert_eq!(
                child.listener.io.lock().unwrap().palette.background,
                0x171f1b
            );
        });

        terminal.update_in(cx, |view, window, cx| {
            let mut kitty = view.settings().clone();
            kitty.palette.set("color1", "#113355");
            view.reload(kitty, window, cx);
            // A standalone view normally refreshes when selected. In Workspace the
            // existing selected terminal is notified by its window on palette change.
            cx.notify();
        });
        cx.run_until_parked();
        terminal.read_with(cx, |view, _| {
            assert!(Arc::ptr_eq(&view.terminal.as_ref().unwrap().grid, &grid));
            assert_eq!(view.palette.ansi[1], 0x113355);
            assert_eq!(view.palette.background, 0x171f1b);
        });
    }
    #[gpui::test]
    fn keyboard_routes_control_text_and_workspace_shortcuts(cx: &mut TestAppContext) {
        let (host, _, sink, cx) = open(cx);
        cx.simulate_input("a");
        #[cfg(target_os = "macos")]
        let copy = "cmd-c";
        #[cfg(not(target_os = "macos"))]
        let copy = "ctrl-shift-c";
        cx.simulate_keystrokes(&format!("tab escape enter ctrl-c {copy}"));
        assert_eq!(*sink.lock().unwrap(), b"a\t\x1b\r\x03");
        cx.simulate_keystrokes("secondary-1");
        assert!(host.read_with(cx, |host, _| host.view == SessionView::Files));
        assert_eq!(*sink.lock().unwrap(), b"a\t\x1b\r\x03");
    }

    #[gpui::test]
    fn ime_sends_only_the_utf8_commit(cx: &mut TestAppContext) {
        let (_, terminal, sink, cx) = open(cx);
        terminal.update_in(cx, |view, window, cx| {
            view.replace_and_mark_text_in_range(None, "´", Some(1..1), window, cx);
            assert!(sink.lock().unwrap().is_empty());
            assert_eq!(view.marked_text_range(window, cx), Some(0..1));
            view.replace_text_in_range(None, "é", window, cx);
        });
        assert_eq!(*sink.lock().unwrap(), "é".as_bytes());
    }

    #[cfg(target_os = "macos")]
    const GROW_SPANISH: &str = "cmd-+";
    #[cfg(not(target_os = "macos"))]
    const GROW_SPANISH: &str = "ctrl-*";
    #[cfg(target_os = "macos")]
    const GROW_US: &str = "cmd-=";
    #[cfg(not(target_os = "macos"))]
    const GROW_US: &str = "ctrl-+";
    #[cfg(target_os = "macos")]
    const SHRINK: &str = "cmd--";
    #[cfg(not(target_os = "macos"))]
    const SHRINK: &str = "ctrl-_";
    #[cfg(target_os = "macos")]
    const RESET: &str = "cmd-0";
    #[cfg(not(target_os = "macos"))]
    const RESET: &str = "ctrl-=";

    fn size(cx: &mut VisualTestContext, terminal: &Entity<TerminalView>) -> f32 {
        terminal.read_with(cx, |view, _| view.settings.size)
    }

    #[gpui::test]
    fn font_size_shortcuts_grow_shrink_and_reset(cx: &mut TestAppContext) {
        let (_, terminal, sink, cx) = open(cx);
        let base = size(cx, &terminal);
        cx.simulate_keystrokes(GROW_SPANISH);
        assert_eq!(size(cx, &terminal), base + 1.);
        cx.simulate_keystrokes(SHRINK);
        assert_eq!(size(cx, &terminal), base);
        cx.simulate_keystrokes(GROW_US);
        assert_eq!(size(cx, &terminal), base + 1.);
        cx.simulate_keystrokes(RESET);
        assert_eq!(size(cx, &terminal), base);
        assert!(sink.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn font_size_clamps_at_both_ends(cx: &mut TestAppContext) {
        let (_, terminal, _, cx) = open(cx);
        cx.simulate_keystrokes(&vec![GROW_SPANISH; 60].join(" "));
        assert_eq!(size(cx, &terminal), 48.);
        cx.simulate_keystrokes(&vec![SHRINK; 60].join(" "));
        assert_eq!(size(cx, &terminal), 6.);
        cx.simulate_keystrokes(RESET);
        assert_eq!(size(cx, &terminal), 11.);
    }

    #[gpui::test]
    fn font_size_reset_uses_kitty_size_outside_clamp(cx: &mut TestAppContext) {
        let (_, terminal, _, cx) = open(cx);
        for base in [4.5, 60.] {
            terminal.update_in(cx, |view, _, cx| {
                view.kitty_size = base;
                view.apply_font_size(cx);
                assert_eq!(view.settings.size, base);
            });
            cx.simulate_keystrokes(GROW_SPANISH);
            assert_eq!(size(cx, &terminal), base.clamp(6., 48.));
            cx.simulate_keystrokes(RESET);
            assert_eq!(size(cx, &terminal), base);
        }
    }

    struct PairHost {
        terminals: [Entity<TerminalView>; 2],
    }

    impl Render for PairHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .key_context("Workspace")
                .child(self.terminals[0].clone())
                .child(self.terminals[1].clone())
        }
    }

    #[gpui::test]
    fn font_size_applies_to_all_terminals_and_new_ones(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::workspace::bind_keys(cx);
            bind_keys(cx);
        });
        let sink = Sink::default();
        let (host, cx) = cx.add_window_view({
            let sink = sink.clone();
            move |window, cx| {
                let second = cx.new(|cx| {
                    let mut view = TerminalView::new(cx);
                    let mut terminal = Terminal::new(&view.settings, GridSize::INITIAL);
                    terminal.sink = Some(sink.clone());
                    view.terminal = Some(terminal);
                    view
                });
                let first = cx.new(|cx| {
                    let mut view = TerminalView::new(cx);
                    let mut terminal = Terminal::new(&view.settings, GridSize::INITIAL);
                    terminal.sink = Some(sink.clone());
                    view.terminal = Some(terminal);
                    view.focus(window, cx);
                    view
                });
                PairHost {
                    terminals: [first, second],
                }
            }
        });
        cx.run_until_parked();
        let terminals = host.read_with(cx, |host, _| host.terminals.clone());
        cx.simulate_keystrokes(GROW_SPANISH);
        assert_eq!(size(cx, &terminals[0]), 12.);
        assert_eq!(size(cx, &terminals[1]), 12.);
        let later = cx.update(|_, cx| cx.new(TerminalView::new));
        assert_eq!(later.read_with(cx, |view, _| view.settings.size), 12.);
        cx.simulate_keystrokes(RESET);
        assert_eq!(size(cx, &terminals[0]), 11.);
        assert_eq!(size(cx, &terminals[1]), 11.);
    }

    #[gpui::test]
    fn unshifted_ctrl_minus_and_zero_are_not_bound(cx: &mut TestAppContext) {
        let (_, terminal, sink, cx) = open(cx);
        cx.simulate_keystrokes("ctrl-- ctrl-0");
        assert_eq!(size(cx, &terminal), 11.);
        assert!(sink.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn larger_font_shrinks_grid(cx: &mut TestAppContext) {
        let (_, terminal, _, cx) = open(cx);
        let before = terminal.read_with(cx, |view, _| {
            let terminal = view.terminal.as_ref().unwrap();
            (terminal.size.cols, terminal.size.rows)
        });
        cx.simulate_keystrokes(GROW_SPANISH);
        cx.run_until_parked();
        let after = terminal.read_with(cx, |view, _| {
            let terminal = view.terminal.as_ref().unwrap();
            (terminal.size.cols, terminal.size.rows)
        });
        assert!(after.0 < before.0, "cols {before:?} -> {after:?}");
        assert!(after.1 < before.1, "rows {before:?} -> {after:?}");
        let grid = terminal.read_with(cx, |view, _| {
            let terminal = view.terminal.as_ref().unwrap();
            let grid = terminal.grid.lock();
            (grid.columns(), grid.screen_lines())
        });
        assert_eq!(grid.0, usize::from(after.0));
        assert_eq!(grid.1, usize::from(after.1));
    }
}
