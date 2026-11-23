// Adapted from Zed crates/terminal/src/terminal.rs (v1.27.0), GPL-3.0-or-later.

mod colors;
mod element;
pub(crate) mod font_size;
mod keys;
pub(crate) mod kitty;
mod mouse;
mod pty;
pub(crate) mod view;

use std::{
    ffi::OsString,
    future::poll_fn,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::EventLoopSender,
    grid::{Dimensions, Scroll},
    sync::FairMutex,
    term::{Config, Term, TermMode},
    vte::ansi::CursorStyle,
};

use self::{
    colors::{Palette, to_rgb},
    kitty::Kitty,
    pty::Process,
};

type Grid = Arc<FairMutex<Term<Listener>>>;

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
}

impl GridSize {
    #[cfg(test)]
    const INITIAL: Self = Self {
        cols: 80,
        rows: 24,
        cell_width: 7,
        cell_height: 14,
    };

    fn window(self) -> WindowSize {
        WindowSize {
            num_cols: self.cols,
            num_lines: self.rows,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

#[derive(Default)]
struct RedrawState {
    visible: bool,
    dirty: bool,
    scheduled: bool,
    waiter: Option<Waker>,
}

/// The PTY thread can emit many Wakeups per frame. Only the first dirty wakeup
/// while visible wakes the UI task, which drains the flag on the next frame.
#[derive(Default)]
struct Redraw(Mutex<RedrawState>);

impl Redraw {
    fn change(&self, change: impl FnOnce(&mut RedrawState)) {
        let waiter = {
            let mut state = self.0.lock().unwrap();
            change(&mut state);
            if state.visible && state.dirty && !state.scheduled {
                state.waiter.take()
            } else {
                None
            }
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    fn visible(&self, visible: bool) {
        self.change(|state| {
            state.dirty |= visible && !state.visible;
            state.visible = visible;
        });
    }

    async fn wait(&self) {
        poll_fn(|cx| {
            let mut state = self.0.lock().unwrap();
            if state.visible && state.dirty && !state.scheduled {
                state.scheduled = true;
                Poll::Ready(())
            } else {
                state.waiter = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    fn drain(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        state.scheduled = false;
        let dirty = std::mem::take(&mut state.dirty);
        state.visible && dirty
    }
}

struct PtyReplies {
    sender: Option<EventLoopSender>,
    palette: Palette,
    size: WindowSize,
}

#[derive(Clone)]
struct Listener {
    redraw: Arc<Redraw>,
    io: Arc<Mutex<PtyReplies>>,
    ended: Arc<AtomicBool>,
}

impl Listener {
    fn finish(&self) {
        self.ended.store(true, Ordering::Release);
        self.io.lock().unwrap().sender = None;
        self.redraw.change(|state| state.dirty = true);
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => self.redraw.change(|state| state.dirty = true),
            Event::PtyWrite(bytes) => pty::input(&self.io.lock().unwrap().sender, bytes),
            Event::ColorRequest(index, format) => {
                let io = self.io.lock().unwrap();
                let rgb = to_rgb(io.palette.index(index, &Default::default()));
                pty::input(&io.sender, format(rgb));
            }
            Event::TextAreaSizeRequest(format) => {
                let io = self.io.lock().unwrap();
                pty::input(&io.sender, format(io.size));
            }
            _ => {}
        }
    }
}

struct Terminal {
    grid: Grid,
    listener: Listener,
    process: Option<Process>,
    size: GridSize,
    #[cfg(test)]
    sink: Option<Arc<Mutex<Vec<u8>>>>,
}

impl Terminal {
    fn new(settings: &Kitty, size: GridSize) -> Self {
        let listener = Listener {
            redraw: Arc::default(),
            io: Arc::new(Mutex::new(PtyReplies {
                sender: None,
                palette: settings.palette.clone(),
                size: size.window(),
            })),
            ended: Arc::default(),
        };
        let grid = Arc::new(FairMutex::new(Term::new(
            Self::config(settings),
            &size,
            listener.clone(),
        )));
        Self {
            grid,
            listener,
            process: None,
            size,
            #[cfg(test)]
            sink: None,
        }
    }

    fn spawn(
        program: &str,
        args: Vec<OsString>,
        settings: &Kitty,
        size: GridSize,
    ) -> io::Result<Self> {
        let mut terminal = Self::new(settings, size);
        terminal.process = Some(Process::spawn(
            program,
            args,
            terminal.grid.clone(),
            terminal.listener.clone(),
            terminal.size.window(),
        )?);
        Ok(terminal)
    }

    fn config(settings: &Kitty) -> Config {
        Config {
            scrolling_history: settings.history,
            default_cursor_style: CursorStyle {
                shape: settings.cursor,
                blinking: false,
            },
            ..Config::default()
        }
    }

    fn reconfigure(&self, settings: &Kitty) {
        self.listener.io.lock().unwrap().palette = settings.palette.clone();
        self.grid.lock().set_options(Self::config(settings));
    }

    fn ended(&self) -> bool {
        self.listener.ended.load(Ordering::Acquire)
    }

    fn send(&self, bytes: impl Into<Vec<u8>>) {
        let bytes = bytes.into();
        #[cfg(test)]
        if let Some(sink) = &self.sink {
            sink.lock().unwrap().extend_from_slice(&bytes);
            return;
        }
        if let Some(process) = &self.process {
            process.write(bytes);
        }
    }

    fn input(&self, bytes: impl Into<Vec<u8>>) {
        {
            let mut grid = self.grid.lock();
            grid.selection = None;
            grid.scroll_display(Scroll::Bottom);
        }
        self.send(bytes);
    }

    fn paste(&self, text: &str) {
        let text = text.replace('\x1b', "");
        if self.grid.lock().mode().contains(TermMode::BRACKETED_PASTE) {
            self.input(format!("\x1b[200~{text}\x1b[201~"));
        } else {
            self.input(text.replace("\r\n", "\r").replace('\n', "\r"));
        }
    }

    fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.grid.lock().resize(size);
        self.listener.io.lock().unwrap().size = size.window();
        if let Some(process) = &self.process {
            process.resize(size.window());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn pty_output_reaches_grid_and_can_resize() {
        let mut terminal = Terminal::spawn(
            "/bin/sh",
            vec!["-c".into(), "printf 'hola\\n'; sleep 0.2".into()],
            &Kitty::default(),
            GridSize::INITIAL,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text: String = terminal
                .grid
                .lock()
                .grid()
                .display_iter()
                .map(|item| item.cell.c)
                .collect();
            if text.contains("hola") {
                break;
            }
            assert!(Instant::now() < deadline, "missing PTY output");
            std::thread::sleep(Duration::from_millis(10));
        }
        terminal.resize(GridSize {
            cols: 100,
            rows: 30,
            ..GridSize::INITIAL
        });
        let grid = terminal.grid.lock();
        assert_eq!((grid.columns(), grid.screen_lines()), (100, 30));
    }

    #[test]
    fn pty_retains_capped_history_after_exit() {
        let settings = Kitty {
            history: 1000,
            ..Kitty::default()
        };
        let terminal = Terminal::spawn(
            "/bin/sh",
            vec!["-c".into(), "seq 1 20000".into()],
            &settings,
            GridSize::INITIAL,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !terminal.ended() {
            assert!(Instant::now() < deadline, "PTY did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut grid = terminal.grid.lock();
        assert_eq!(grid.history_size(), 1000);
        let text: String = grid.grid().display_iter().map(|item| item.cell.c).collect();
        assert!(text.contains("20000"));
        grid.scroll_display(Scroll::Top);
        assert_eq!(grid.grid().display_offset(), 1000);
    }

    #[test]
    fn dropping_pty_reaps_child_that_ignores_hangup() {
        let terminal = Terminal::spawn(
            "/bin/sh",
            vec![
                "-c".into(),
                "trap '' HUP; printf 'ready\\n'; read line".into(),
            ],
            &Kitty::default(),
            GridSize::INITIAL,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text: String = terminal
                .grid
                .lock()
                .grid()
                .display_iter()
                .map(|item| item.cell.c)
                .collect();
            if text.contains("ready") {
                break;
            }
            assert!(Instant::now() < deadline, "missing PTY readiness marker");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(terminal);
    }
}
