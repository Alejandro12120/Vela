use std::{collections::BTreeSet, io};

use gpui::{AppContext, DragMoveEvent, Point, WeakEntity};
use vela_core::transfer::{Direction, Spec};

use super::*;

use crate::{
    context_menu::{Action, Menu},
    rename::{self, InlineRename},
    transfers_view::Transfers,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Local,
    Remote,
}

impl Source {
    pub fn side(&self) -> Side {
        match self {
            Self::Local => Side::Local,
            Self::Remote(_) => Side::Remote,
        }
    }

    pub(crate) async fn rename(
        &self,
        from: PathBuf,
        to: PathBuf,
        executor: &BackgroundExecutor,
    ) -> Result<(), Error> {
        match self {
            Self::Local => {
                executor
                    .spawn(async move { local::rename(&from, &to).map_err(Error::Io) })
                    .await
            }
            Self::Remote(session) => session.rename(from, to).await,
        }
    }

    async fn create_folder(
        &self,
        parent: PathBuf,
        executor: &BackgroundExecutor,
    ) -> Result<PathBuf, Error> {
        for index in 1.. {
            let path = parent.join(rename::folder_name(index));
            match self {
                Self::Local => {
                    let candidate = path.clone();
                    match executor
                        .spawn(async move { std::fs::create_dir(candidate) })
                        .await
                    {
                        Ok(()) => return Ok(path),
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Self::Remote(session) => {
                    if let Err(error) = session.mkdir(path.clone()).await {
                        if session.stat(path.clone()).await.is_err() {
                            return Err(error);
                        }
                    } else {
                        return Ok(path);
                    }
                }
            }
        }
        unreachable!()
    }

    pub(crate) async fn create_file(
        &self,
        parent: PathBuf,
        executor: &BackgroundExecutor,
    ) -> Result<PathBuf, Error> {
        for index in 1.. {
            let path = parent.join(rename::file_name(index));
            let result = match self {
                Self::Local => {
                    let candidate = path.clone();
                    executor
                        .spawn(async move {
                            std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(candidate)
                                .map(|_| ())
                                .map_err(Error::Io)
                        })
                        .await
                }
                Self::Remote(session) => session.create_file(path.clone()).await,
            };
            match result {
                Ok(()) => return Ok(path),
                Err(Error::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }

    pub async fn delete(&self, path: PathBuf, executor: &BackgroundExecutor) -> Result<(), Error> {
        match self {
            Self::Local => {
                executor
                    .spawn(async move { local::trash(&path).map(|_| ()).map_err(Error::Io) })
                    .await
            }
            Self::Remote(session) => session.remove_tree(path).await,
        }
    }
}

pub(super) struct Operations {
    server: Arc<str>,
    other: WeakEntity<Pane>,
    pub(super) transfers: Entity<Transfers>,
}

#[derive(Clone)]
pub(super) struct Drag {
    pub side: Side,
    server: Arc<str>,
    pub(super) paths: Arc<[PathBuf]>,
    label: String,
    icon: IconName,
}

pub(super) struct DragChip {
    drag: Drag,
    offset: Point<Pixels>,
}

impl Render for DragChip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = Theme::current(window, cx);
        div()
            .pl(self.offset.x + px(8.))
            .pt(self.offset.y + px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .h(px(26.))
                    .max_w(px(220.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.accent)
                    .bg(theme.raised)
                    .text_color(theme.text)
                    .text_size(px(12.))
                    .child(icon(self.drag.icon).text_color(theme.secondary_text))
                    .child(div().truncate().child(self.drag.label.clone())),
            )
    }
}

impl Pane {
    pub fn wire(
        &mut self,
        server: Arc<str>,
        other: WeakEntity<Pane>,
        transfers: Entity<Transfers>,
    ) {
        self.operations = Some(Operations {
            server,
            other,
            transfers,
        });
    }

    pub fn root(&self) -> &Path {
        &self.tree.root
    }

    pub(super) fn selected_paths(&self) -> Arc<[PathBuf]> {
        self.tree
            .rows
            .iter()
            .filter(|row| self.tree.selected.contains(&row.path))
            .map(|row| row.path.to_path_buf())
            .collect()
    }

    pub(super) fn drag(
        &self,
        path: &Arc<Path>,
        selected: Arc<[PathBuf]>,
        name: &str,
        icon: IconName,
    ) -> Option<Drag> {
        let operations = self.operations.as_ref()?;
        let paths = if self.tree.selected.contains(path) {
            selected
        } else {
            Arc::from([path.to_path_buf()])
        };
        let label = if paths.len() == 1 {
            name.to_owned()
        } else {
            format!("{} items", paths.len())
        };
        Some(Drag {
            side: self.source.side(),
            server: operations.server.clone(),
            paths,
            label,
            icon,
        })
    }

    pub(super) fn external_drop_zone(
        &self,
        element: Stateful<Div>,
        target: Option<Arc<Path>>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let is_folder = target.is_some();
        element.when(
            cfg!(target_os = "macos")
                && self.source.side() == Side::Remote
                && self.operations.is_some(),
            |element| {
                element
                    .drag_over(move |style, paths: &gpui::ExternalPaths, _, _| {
                        if !super::finder::valid_external_paths(paths) {
                            return style;
                        }
                        style.bg(if is_folder {
                            theme.text_selection
                        } else {
                            theme.text_selection.opacity(0.35)
                        })
                    })
                    .on_drop(
                        cx.listener(move |pane, paths: &gpui::ExternalPaths, window, cx| {
                            pane.finder_drop(paths, target.as_deref(), window, cx);
                        }),
                    )
            },
        )
    }

    fn accepts_drag(&self, drag: &Drag) -> bool {
        self.operations
            .as_ref()
            .is_some_and(|operations| operations.server == drag.server)
    }

    pub(super) fn drag_move(
        &mut self,
        event: &DragMoveEvent<Drag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drag = event.drag(cx);
        let next = if event.bounds.contains(&event.event.position) && self.accepts_drag(drag) {
            // GPUI offsets rows upward by a negative Y as the list scrolls down.
            let scroll_offset_y = self.scroll.0.borrow().base_handle.offset().y;
            let y = event.event.position.y - event.bounds.top() - scroll_offset_y;
            let index = (y / ROW_HEIGHT).floor() as usize;
            let hovered = (index < self.tree.rows.len()).then_some(index);
            drop_target::resolve(
                &self.drop_rows(),
                self.root(),
                hovered,
                &drag.paths,
                drag.side == self.source.side(),
            )
        } else {
            None
        };
        if next == self.drop_target {
            return;
        }
        self.drop_target = next;
        self.spring = None;
        if let Some(DropTarget {
            folder,
            target: Target::Folder {
                expanded: false, ..
            },
        }) = &self.drop_target
        {
            let path: Arc<Path> = folder.as_path().into();
            let spring_path = path.clone();
            let timer = cx
                .background_executor()
                .timer(std::time::Duration::from_millis(1000));
            let task = cx.spawn_in(window, async move |this, cx| {
                timer.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if cx.has_active_drag()
                        && this.drop_target.as_ref().is_some_and(|drop| {
                            drop.folder == *spring_path
                                && matches!(
                                    drop.target,
                                    Target::Folder {
                                        expanded: false,
                                        ..
                                    }
                                )
                        })
                    {
                        this.set_expanded(spring_path, true, window, cx);
                    }
                });
            });
            self.spring = Some((path, task));
        }
        cx.notify();
    }

    pub(super) fn drop_files(&mut self, drag: &Drag, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.drop_target.take();
        self.spring = None;
        cx.notify();
        let Some(target) = target.filter(|_| self.accepts_drag(drag)) else {
            return;
        };
        let sources = drag.paths.to_vec();
        let folder = target.folder;
        if drag.side == self.source.side() {
            let move_files = MoveFiles { sources, folder };
            match self.source {
                Source::Local => self.move_files(move_files, window, cx),
                Source::Remote(_) => cx.emit(move_files),
            }
        } else if let Some(operations) = &self.operations {
            let direction = match drag.side {
                Side::Local => Direction::Upload,
                Side::Remote => Direction::Download,
            };
            let pane = cx.entity().downgrade();
            operations.transfers.update(cx, |queue, cx| {
                queue.enqueue(
                    Spec {
                        direction,
                        sources,
                        destination: folder,
                    },
                    pane,
                    window,
                    cx,
                );
            });
        }
    }

    pub fn move_files(&mut self, files: MoveFiles, window: &Window, cx: &mut Context<Self>) {
        if let Some(operations) = &self.operations {
            let pane = cx.entity().downgrade();
            let source = self.source.clone();
            self.path_error = None;
            operations.transfers.update(cx, |queue, cx| {
                queue.move_paths(source, files.sources, files.folder, pane, window, cx);
            });
            cx.notify();
        }
    }

    pub(super) fn drag_chip(
        drag: &Drag,
        offset: Point<Pixels>,
        _: &mut Window,
        cx: &mut App,
    ) -> Entity<DragChip> {
        cx.new(|_| DragChip {
            drag: drag.clone(),
            offset,
        })
    }

    pub(super) fn copy_to_other(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(operations) = &self.operations else {
            return;
        };
        let sources = self.selected_paths().to_vec();
        if sources.is_empty() {
            return;
        }
        let Some(other) = operations.other.upgrade() else {
            return;
        };
        let destination = other.read(cx).root().to_owned();
        let direction = if self.source.side() == Side::Local {
            Direction::Upload
        } else {
            Direction::Download
        };
        operations.transfers.update(cx, |queue, cx| {
            queue.enqueue(
                Spec {
                    direction,
                    sources,
                    destination,
                },
                other.downgrade(),
                window,
                cx,
            );
        });
    }

    pub(super) fn open_menu(
        &mut self,
        index: Option<usize>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list_focus.focus(window, cx);
        let mut folder = self.tree.root.clone();
        if let Some(index) = index {
            let row = &self.tree.rows[index];
            if row.entry().is_some_and(|entry| entry.is_directory()) {
                folder = row.path.to_path_buf();
            }
            if !self.tree.selected.contains(&row.path) {
                self.tree.select(index, Selection::Single);
                self.check_copy_selection();
            }
        }
        let first = cx.focus_handle().tab_stop(true);
        first.focus(window, cx);
        self.menu = Some(Menu {
            position,
            folder,
            on_row: index.is_some(),
            scope: cx.focus_handle(),
            first,
            last: cx.focus_handle().tab_stop(true),
        });
        cx.notify();
    }

    pub fn dismiss_menu(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        cx.notify();
    }

    pub fn menu_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let folder = self
            .menu
            .take()
            .map_or_else(|| self.tree.root.clone(), |menu| menu.folder);
        self.list_focus.focus(window, cx);
        match action {
            Action::NewFolder => self.new_folder(folder, window, cx),
            Action::NewFile => self.new_item(folder, false, window, cx),
            Action::Rename => self.start_selected_rename(window, cx),
            Action::Run => self.run_selected(false, window, cx),
            Action::RunInKitty => self.run_selected(true, window, cx),
            Action::Delete => self.delete_selected(cx),
            Action::Copy => self.copy_to_other(window, cx),
            Action::Refresh => self.refresh(window, cx),
        }
        cx.notify();
    }

    /// Shows `message` in the banner under the toolbar.
    pub fn report_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.path_error = Some(message);
        cx.notify();
    }

    /// The only selected row when it is a regular file.
    fn selected_file(&self) -> Option<&tree::Row> {
        let mut rows = self
            .tree
            .rows
            .iter()
            .filter(|row| self.tree.selected.contains(&row.path));
        let row = rows.next()?;
        if rows.next().is_some() {
            return None;
        }
        row.entry()
            .is_some_and(|entry| entry.kind == EntryKind::File)
            .then_some(row)
    }

    /// A remote file runs when it has an execute bit or ends with `.sh`.
    pub(super) fn run_state(&self) -> RunState {
        let Some(row) = self.selected_file() else {
            return RunState::Hidden;
        };
        match self.source {
            Source::Local => RunState::Enabled,
            Source::Remote(_) if remote_runnable(row) => RunState::Remote,
            Source::Remote(_) => RunState::Disabled,
        }
    }

    fn run_selected(&mut self, kitty: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.run_state(), RunState::Enabled | RunState::Remote) {
            return;
        }
        let Some(row) = self.selected_file() else {
            return;
        };
        let path = row.path.to_path_buf();
        let executable = row
            .entry()
            .and_then(|entry| entry.mode)
            .is_some_and(|mode| mode & 0o111 != 0);
        let (failure, launch): (Option<&str>, Box<dyn FnOnce() -> io::Result<()> + Send>) =
            match &self.source {
                Source::Local => (None, Box::new(move || dev::open_default(&path))),
                Source::Remote(session) => {
                    let (Some(directory), Some(name)) = (
                        path.parent().and_then(Path::to_str),
                        path.file_name().and_then(std::ffi::OsStr::to_str),
                    ) else {
                        self.report_error(
                            "The filename can't be passed to the shell as text.".into(),
                            cx,
                        );
                        return;
                    };
                    if !kitty {
                        cx.emit(super::RunRemote {
                            directory: directory.into(),
                            name: name.into(),
                            executable,
                        });
                        return;
                    }
                    let command = vela_core::run_script_command(directory, name, executable);
                    let args = session.terminal_command(&command);
                    (
                        Some("Couldn't open Kitty"),
                        Box::new(move || dev::open_kitty_ssh(&args)),
                    )
                }
            };
        let task = cx.background_executor().spawn(async move { launch() });
        cx.spawn_in(window, async move |pane, cx| {
            if let Err(error) = task.await {
                let message = failure.map_or_else(|| error.to_string(), str::to_owned);
                let _ = pane.update_in(cx, |pane, _, cx| pane.report_error(message, cx));
            }
        })
        .detach();
    }

    pub(super) fn start_selected_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree.selected.len() == 1
            && let Some(path) = self.tree.selected.iter().next()
        {
            self.start_rename(path.to_path_buf(), window, cx);
        }
    }

    fn start_rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self
            .tree
            .rows
            .iter()
            .position(|row| row.path.as_ref() == path)
        else {
            return;
        };
        let directory = self.tree.rows[index]
            .entry()
            .is_some_and(|entry| entry.kind == EntryKind::Directory);
        self.tree.select(index, Selection::Single);
        self.check_copy_selection();
        self.scroll_to_cursor();
        self.rename = InlineRename::new(path, directory, window, cx);
        if self.rename.is_none() {
            self.path_error = Some("The name can't be edited as text.".into());
        }
        cx.notify();
    }

    pub(super) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.as_ref().is_some_and(|rename| !rename.busy) {
            self.rename = None;
            self.list_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.as_mut().filter(|rename| !rename.busy) else {
            return;
        };
        let input = rename.input.read(cx);
        if input.is_composing() {
            return;
        }
        let value = input.value();
        if !rename::validate(value) {
            rename.error = Some("Invalid name");
            cx.notify();
            return;
        }
        let from = rename.path.clone();
        let to = from.with_file_name(value);
        if from == to {
            self.cancel_rename(window, cx);
            return;
        }
        rename.busy = true;
        rename.error = None;
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = source.rename(from.clone(), to.clone(), &executor).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this
                    .rename
                    .as_ref()
                    .is_some_and(|rename| rename.path == from)
                {
                    match &result {
                        Ok(()) => {
                            this.rename = None;
                            this.list_focus.focus(window, cx);
                        }
                        Err(error) => {
                            let rename = this.rename.as_mut().expect("active rename");
                            rename.busy = false;
                            rename.error = Some(match error {
                                Error::Io(error)
                                    if error.kind() == io::ErrorKind::AlreadyExists =>
                                {
                                    "That name is already in use"
                                }
                                _ => "Couldn't rename the item",
                            });
                        }
                    }
                }
                if result.is_ok()
                    && let Some(parent) = from.parent()
                {
                    this.refresh_changed([parent.to_owned()], None, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn new_folder(
        &mut self,
        parent: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.new_item(parent, true, window, cx);
    }

    fn new_item(
        &mut self,
        parent: PathBuf,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        let revision = self.tree_revision;
        cx.spawn_in(window, async move |this, cx| {
            let result = if directory {
                source.create_folder(parent.clone(), &executor).await
            } else {
                source.create_file(parent.clone(), &executor).await
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.tree_revision != revision {
                    return;
                }
                match result {
                    Ok(path) => {
                        if parent != this.tree.root {
                            this.set_expanded(parent.as_path().into(), true, window, cx);
                        }
                        this.refresh_changed([parent], Some(path), window, cx);
                    }
                    Err(error) => {
                        let item = if directory { "folder" } else { "file" };
                        this.path_error = Some(format!("Couldn't create the {item}: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(operations) = &self.operations {
            let pane = cx.entity().downgrade();
            let source = self.source.clone();
            let paths = self.selected_paths().to_vec();
            operations.transfers.update(cx, |queue, cx| {
                queue.confirm_delete(source, paths, pane, cx)
            });
        }
    }

    pub fn refresh_changed(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        rename: Option<PathBuf>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_changed_selecting(paths, rename, Vec::new(), window, cx);
    }

    pub fn refresh_after_move(
        &mut self,
        mut parents: BTreeSet<PathBuf>,
        moved: Vec<(PathBuf, PathBuf)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // A replaced folder may have been listed already, including its open descendants.
        parents.extend(
            self.tree
                .expanded
                .iter()
                .filter(|path| moved.iter().any(|(_, dest)| path.starts_with(dest)))
                .map(|path| path.to_path_buf()),
        );
        self.refresh_changed_selecting(parents, None, moved, window, cx);
    }

    fn refresh_changed_selecting(
        &mut self,
        paths: impl IntoIterator<Item = PathBuf>,
        rename: Option<PathBuf>,
        moved: Vec<(PathBuf, PathBuf)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.invalidate_drag_cache();
        let paths: BTreeSet<_> = paths
            .into_iter()
            .filter(|path| self.tree.has_branch(path) || path == &self.tree.root)
            .map(Arc::<Path>::from)
            .collect();
        if paths.is_empty() {
            return;
        }
        self.branch_generation += 1;
        let generation = self.branch_generation;
        for path in &paths {
            self.branch_revisions.insert(path.clone(), generation);
        }
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        let revision = self.tree_revision;
        cx.spawn_in(window, async move |this, cx| {
            let mut listings = Vec::new();
            for path in paths {
                let listing = source.list(&path, &executor).await;
                listings.push((path, listing));
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if this.tree_revision == revision {
                    listings
                        .retain(|(path, _)| this.branch_revisions.get(path) == Some(&generation));
                    let moved_expansion: Vec<Arc<Path>> = this
                        .tree
                        .expanded
                        .iter()
                        .filter_map(|path| {
                            moved.iter().find_map(|(from, to)| {
                                path.strip_prefix(from).ok().map(|suffix| {
                                    if suffix.as_os_str().is_empty() {
                                        to.as_path().into()
                                    } else {
                                        to.join(suffix).into()
                                    }
                                })
                            })
                        })
                        .collect();
                    this.tree.expanded.extend(moved_expansion);
                    this.tree.refresh_branches(listings);
                    this.remember();
                    this.load_expanded(window, cx);
                    if !moved.is_empty() {
                        this.tree.selected = moved
                            .into_iter()
                            .filter_map(|(_, dest)| {
                                dest.starts_with(this.root()).then(|| dest.into())
                            })
                            .collect();
                        this.check_copy_selection();
                    }
                    if let Some(path) = rename {
                        this.start_rename(path, window, cx);
                    }
                    this.report_rows(window);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn dev_state(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tree.rows.iter().position(|row| row.entry().is_some()) {
            self.tree.select(index, Selection::Single);
            self.check_copy_selection();
            self.list_focus.focus(window, cx);
            match action {
                "delete" => self.delete_selected(cx),
                "rename" => self.start_selected_rename(window, cx),
                "menu" => {
                    let sidebar = if self.sidebar.get() {
                        px(crate::sidebar::WIDTH)
                    } else {
                        px(0.)
                    };
                    let pane_width = (window.viewport_size().width - sidebar) / 2.;
                    let origin = sidebar
                        + if self.source.side() == Side::Remote {
                            pane_width
                        } else {
                            px(0.)
                        };
                    self.open_menu(
                        Some(index),
                        gpui::point(origin + px(54.), px(125.)),
                        window,
                        cx,
                    );
                }
                _ => {}
            }
            cx.notify();
        }
    }
}

fn remote_runnable(row: &tree::Row) -> bool {
    row.entry()
        .is_some_and(|entry| runnable(entry.mode, entry.name.to_str().unwrap_or_default()))
}

/// A file runs on the server when it has any execute bit or ends with `.sh`.
fn runnable(mode: Option<u32>, name: &str) -> bool {
    mode.is_some_and(|mode| mode & 0o111 != 0) || name.ends_with(".sh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_files_with_an_execute_bit_or_a_sh_name() {
        assert!(runnable(Some(0o100755), "herramienta"));
        assert!(runnable(Some(0o100010), "x"));
        assert!(runnable(Some(0o100644), "deploy.sh"));
        assert!(runnable(None, "deploy.sh"));
        assert!(!runnable(Some(0o100644), "notas.txt"));
        assert!(!runnable(None, "notas"));
    }
}
