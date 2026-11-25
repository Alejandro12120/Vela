use std::{ffi::OsString, io, thread::JoinHandle};

use alacritty_terminal::{
    event::WindowSize,
    event_loop::{EventLoop, EventLoopSender, Msg},
    tty::{self, Options, Shell},
};

use super::{Grid, Listener};

pub struct Process {
    sender: EventLoopSender,
    reader: Option<JoinHandle<()>>,
}

impl Process {
    pub fn spawn(
        program: &str,
        args: Vec<OsString>,
        grid: Grid,
        listener: Listener,
        size: WindowSize,
    ) -> io::Result<Self> {
        let args = args
            .into_iter()
            .map(|arg| {
                arg.into_string()
                    .map_err(|_| io::Error::other("Argumento SSH no válido en UTF-8"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let options = Options {
            shell: Some(Shell::new(program.into(), args)),
            env: [("TERM".into(), "xterm-256color".into())].into(),
            drain_on_exit: true,
            ..Options::default()
        };
        let pty = tty::new(&options, size, 0)?;
        let pid = pty.child().id();
        let event_loop = EventLoop::new(grid, listener.clone(), pty, options.drain_on_exit, false)?;
        let sender = event_loop.channel();
        listener.io.lock().unwrap().sender = Some(sender.clone());
        let reader = event_loop.spawn();
        // Keep ownership of cleanup even when the loop exits with an I/O error.
        let cleanup = std::thread::spawn(move || {
            if let Ok(state) = reader.join() {
                // A multiplexed ssh can stay in a blocking read after SIGHUP.
                // Terminate any unreaped child before Pty::drop waits for it.
                terminate_child(pid);
                drop(state);
            }
            listener.finish();
        });
        Ok(Self {
            sender,
            reader: Some(cleanup),
        })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&self, size: WindowSize) {
        let _ = self.sender.send(Msg::Resize(size));
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

pub(super) fn input(sender: &Option<EventLoopSender>, bytes: String) {
    if let Some(sender) = sender {
        let _ = sender.send(Msg::Input(bytes.into_bytes().into()));
    }
}

fn terminate_child(pid: u32) {
    // The reader has joined, so nothing else reaps this Child. Confirm that
    // it is still ours without reaping it or signalling a recycled PID.
    loop {
        // SAFETY: info is writable and waitid only inspects this process's child.
        let result = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // SAFETY: waitid established that this PID is still our child.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            return;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}
