use gpui::AppContext;
use vela_core::{run_script_command, shell_quote, terminal_start_command};

use crate::{
    file_workspace::{Panes, SessionView},
    pane::RunRemote,
    terminal::view::{Restart, TerminalView},
};

use super::{Connection, Context, Window, Workspace, dev};

impl Workspace {
    pub(super) fn terminal_for(
        panes: &mut Panes,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<TerminalView> {
        panes
            .terminal
            .get_or_insert_with(|| {
                let terminal = cx.new(TerminalView::new);
                let id = id.to_owned();
                cx.subscribe_in(
                    &terminal,
                    window,
                    move |this, _, _: &Restart, window, cx| {
                        let Some(Connection::Connected { panes, session }) =
                            this.connections.get(&id)
                        else {
                            return;
                        };
                        let command = terminal_start_command(
                            &panes.remote().read(cx).directory().to_string_lossy(),
                        );
                        let args = session.terminal_command(&command);
                        if let Some(terminal) = &panes.terminal {
                            terminal.update(cx, |terminal, cx| {
                                terminal.start(args, window, cx);
                                terminal.focus(window, cx);
                            });
                        }
                    },
                )
                .detach();
                terminal
            })
            .clone()
    }

    pub(super) fn switch_session_view(
        &mut self,
        view: SessionView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_open() || self.dialog.is_some() || self.quitting {
            return;
        }
        let Some(id) = self.selected.as_deref() else {
            return;
        };
        let Some(Connection::Connected { panes, session }) = self.connections.get_mut(id) else {
            return;
        };
        if panes.view != view {
            panes.view = view;
            panes.switched = (!cx.reduce_motion()).then(std::time::Instant::now);
        }
        if view == SessionView::Terminal {
            let terminal = Self::terminal_for(panes, id, window, cx);
            if !terminal.read(cx).started() {
                let command =
                    terminal_start_command(&panes.remote().read(cx).directory().to_string_lossy());
                let args = session.terminal_command(&command);
                terminal.update(cx, |terminal, cx| terminal.start(args, window, cx));
            }
        }
        panes.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn select_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(window, cx);
        self.select(id);
        if let Some(Connection::Connected { panes, .. }) = self.connections.get(id) {
            panes.focus(window, cx);
        }
    }

    pub(super) fn run_remote(
        &mut self,
        id: &str,
        run: &RunRemote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Connection::Connected { panes, session }) = self.connections.get_mut(id) else {
            return;
        };
        let terminal = Self::terminal_for(panes, id, window, cx);
        let runner = if run.executable { "" } else { "sh " };
        let view = terminal.read(cx);
        if view.idle() {
            let command = run_script_command(&run.directory, &run.name, run.executable);
            let args = session.terminal_command(&format!("export COLORTERM=truecolor; {command}"));
            terminal.update(cx, |terminal, cx| terminal.start(args, window, cx));
        } else {
            let error = if view.alternate_screen() {
                Some("The terminal is busy")
            } else if run
                .directory
                .chars()
                .chain(run.name.chars())
                .any(char::is_control)
            {
                Some("The filename contains control characters. Use “Run in Kitty.”")
            } else {
                None
            };
            if let Some(error) = error {
                panes
                    .remote()
                    .update(cx, |pane, cx| pane.report_error(error.into(), cx));
                return;
            }
            let command = format!(
                "cd {} && {runner}{}\r",
                shell_quote(&run.directory),
                shell_quote(&format!("./{}", run.name)),
            );
            terminal.update(cx, |terminal, _| terminal.command(command));
        }
        self.switch_session_view(SessionView::Terminal, window, cx);
    }

    pub(super) fn open_terminal_in_kitty(&self, window: &Window, cx: &mut Context<Self>) {
        let Some(id) = &self.selected else { return };
        let Some(Connection::Connected { panes, session }) = self.connections.get(id) else {
            return;
        };
        let command =
            terminal_start_command(&panes.remote().read(cx).directory().to_string_lossy());
        let args = session.terminal_command(&command);
        let terminal = panes.terminal.as_ref().map(gpui::Entity::downgrade);
        let result = cx
            .background_executor()
            .spawn(async move { dev::open_kitty_ssh(&args) });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = result.await
                && let Some(terminal) = terminal
            {
                let _ = terminal.update(cx, |terminal, cx| {
                    terminal.kitty_error(error.to_string(), cx)
                });
            }
        })
        .detach();
    }
}
