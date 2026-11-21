use std::{collections::BTreeSet, io};

use gpui::{AppContext, Point, WeakEntity};
use vscp_core::transfer::{Direction, Spec};

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

    async fn rename(
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
    transfers: Entity<Transfers>,
}

#[derive(Clone)]
pub(super) struct Drag {
    pub side: Side,
    server: Arc<str>,
    paths: Arc<[PathBuf]>,
    label: String,
    icon: Icon,
}

pub(super) struct DragChip {
    drag: Drag,
    offset: Point<Pixels>,
}

impl Render for DragChip {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = Theme::for_window(window);
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
                    .child(self.drag.icon.draw(theme.secondary_text))
                    .child(div().truncate().child(self.drag.label.clone())),
            )
    }
}

fn resolve_drop(
    same_server: bool,
    from: Side,
    to: Side,
    root: &Path,
    folder: Option<&Path>,
) -> Option<PathBuf> {
    (same_server && from != to).then(|| folder.unwrap_or(root).to_owned())
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
        icon: Icon,
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
            format!("{} elementos", paths.len())
        };
        Some(Drag {
            side: self.source.side(),
            server: operations.server.clone(),
            paths,
            label,
            icon,
        })
    }

    pub(super) fn drop_zone(
        &self,
        element: Stateful<Div>,
        target: Option<Arc<Path>>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let Some(operations) = &self.operations else {
            return element;
        };
        let server = operations.server.clone();
        let side = self.source.side();
        let is_folder = target.is_some();
        element
            .drag_over(move |style, drag: &Drag, _, _| {
                if drag.side != side && drag.server == server {
                    if is_folder {
                        style.bg(theme.text_selection)
                    } else {
                        style.bg(theme.text_selection.opacity(0.35))
                    }
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |pane, drag: &Drag, window, cx| {
                let Some(operations) = &pane.operations else {
                    return;
                };
                if let Some(destination) = resolve_drop(
                    drag.server == operations.server,
                    drag.side,
                    pane.source.side(),
                    pane.root(),
                    target.as_deref(),
                ) {
                    let direction = if drag.side == Side::Local {
                        Direction::Upload
                    } else {
                        Direction::Download
                    };
                    let spec = Spec {
                        direction,
                        sources: drag.paths.to_vec(),
                        destination,
                    };
                    let target = cx.entity().downgrade();
                    operations
                        .transfers
                        .update(cx, |queue, cx| queue.enqueue(spec, target, window, cx));
                }
            }))
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
            Action::Rename => self.start_selected_rename(window, cx),
            Action::Delete => self.delete_selected(cx),
            Action::Copy => self.copy_to_other(window, cx),
            Action::Refresh => self.refresh(window, cx),
        }
        cx.notify();
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
        self.scroll_to_cursor();
        self.rename = InlineRename::new(path, directory, window, cx);
        if self.rename.is_none() {
            self.path_error = Some("El nombre no se puede editar como texto.".into());
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
            rename.error = Some("Nombre no válido");
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
                                    "Ya existe ese nombre"
                                }
                                _ => "No se pudo renombrar",
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
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        let revision = self.tree_revision;
        cx.spawn_in(window, async move |this, cx| {
            let result = source.create_folder(parent.clone(), &executor).await;
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
                        this.path_error = Some(format!("No se pudo crear la carpeta: {error}"))
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
                    this.tree.refresh_branches(listings);
                    this.remember();
                    this.load_expanded(window, cx);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_opposite_pane_on_same_server_is_a_drop_target() {
        let root = Path::new("/root");
        let folder = Path::new("/root/folder");
        assert_eq!(
            resolve_drop(true, Side::Local, Side::Remote, root, Some(folder)).as_deref(),
            Some(folder)
        );
        assert_eq!(
            resolve_drop(true, Side::Remote, Side::Local, root, None).as_deref(),
            Some(root)
        );
        assert_eq!(
            resolve_drop(true, Side::Remote, Side::Remote, root, None),
            None
        );
        assert_eq!(
            resolve_drop(false, Side::Remote, Side::Local, root, None),
            None
        );
    }
}
