use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::Arc,
};

use gpui::{
    App, ClipboardEntry, Context, Div, ExternalDragPayload, ExternalPaths, FileDragPaths, Global,
    Stateful, Window, prelude::*,
};
use vela_core::{
    Session,
    transfer::{Cancel, Direction, Event, ItemResult, Spec, Status},
};

use super::{
    Pane, Source,
    operations::{Drag, Side},
};
use crate::{dev, file_metadata, staging, transfers_view::TransferOrigin};

#[cfg(target_os = "macos")]
use crate::pasteboard_macos as pasteboard;

#[cfg(not(target_os = "macos"))]
mod pasteboard {
    use std::{io, path::PathBuf};

    pub fn paths() -> Vec<PathBuf> {
        Vec::new()
    }

    pub fn write(_: &[(PathBuf, bool)]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "File clipboard unavailable",
        ))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    Copy,
    Drag,
}

impl Purpose {
    pub fn caption(self) -> &'static str {
        match self {
            Self::Copy => "Preparing to copy…",
            Self::Drag => "Preparing to drag…",
        }
    }
}

#[derive(Default)]
struct Request {
    cancelled: Cell<bool>,
    worker: RefCell<Option<Cancel>>,
}

impl Request {
    fn cancel(&self) {
        self.cancelled.set(true);
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker.cancel();
        }
    }

    fn attach(&self, worker: Cancel) {
        if self.cancelled.get() {
            worker.cancel();
        }
        self.worker.replace(Some(worker));
    }
}

#[derive(Default)]
pub(super) struct PaneFinder {
    copy: Option<(Weak<Request>, Arc<[PathBuf]>)>,
    drag: Option<(Rc<Request>, Arc<[PathBuf]>)>,
    ready: HashMap<Arc<[PathBuf]>, Staged>,
}

impl Drop for PaneFinder {
    fn drop(&mut self) {
        if let Some(request) = self
            .copy
            .as_ref()
            .and_then(|(request, _)| request.upgrade())
        {
            request.cancel();
        }
        if let Some((request, _)) = &self.drag {
            request.cancel();
        }
    }
}

struct Staged {
    directory: Rc<staging::Directory>,
    urls: Vec<(PathBuf, bool)>,
    sources: Vec<PathBuf>,
}

struct ClipboardCopy {
    staged: Staged,
    session: Session,
}

#[derive(Default)]
struct FinderClipboard {
    pending: Option<Rc<Request>>,
    current: Option<ClipboardCopy>,
    active: Vec<Weak<staging::Directory>>,
    drag_directories: Vec<Rc<staging::Directory>>,
    quitting: bool,
}

impl Global for FinderClipboard {}

pub fn initialize(cx: &mut App) {
    cx.set_global(FinderClipboard::default());
    cleanup(cx);
}

pub fn cancel_copy(cx: &mut App) {
    if let Some(request) = cx.global_mut::<FinderClipboard>().pending.take() {
        request.cancel();
    }
}

pub fn begin_quit(cx: &mut App) {
    cancel_copy(cx);
    cx.global_mut::<FinderClipboard>().quitting = true;
}

pub fn quit_cleanup(cx: &mut App) -> impl FnOnce() + use<> {
    begin_quit(cx);
    let root = staging::root().ok().filter(|path| path.is_dir());
    let paths = root.as_ref().map_or_else(Vec::new, |_| clipboard_paths(cx));
    let clipboard = cx.global_mut::<FinderClipboard>();
    let active: Vec<_> = clipboard
        .active
        .drain(..)
        .filter_map(|stage| stage.upgrade())
        .collect();
    clipboard.current = None;
    clipboard.drag_directories.clear();
    move || {
        for directory in active {
            if staging::referenced(directory.path(), &paths) {
                directory.keep();
            } else if let Err(error) = directory.remove() {
                dev::log(format_args!("Finder cache: {error}"));
            }
        }
        if let Some(root) = root
            && let Err(error) = staging::cleanup(&root, &paths)
        {
            dev::log(format_args!("Finder cache: {error}"));
        }
    }
}

fn cleanup(cx: &App) {
    if cfg!(target_os = "macos") {
        let result = staging::root().and_then(|root| {
            if root.is_dir() {
                staging::cleanup(&root, &clipboard_paths(cx))?;
            }
            Ok(())
        });
        if let Err(error) = result {
            dev::log(format_args!("Finder cache: {error}"));
        }
    }
}

pub fn dev_drop_highlight(window: &Window, cx: &mut App) {
    let timer = cx
        .background_executor()
        .timer(std::time::Duration::from_millis(600));
    window
        .spawn(cx, async move |cx| {
            timer.await;
            let _ = cx.update(|window, cx| {
                let size = window.viewport_size();
                let event = gpui::FileDropEvent::Entered {
                    position: gpui::point(size.width * 0.86, size.height * 0.62),
                    paths: ExternalPaths(
                        [PathBuf::from("/vela-dev-drop-preview")]
                            .into_iter()
                            .collect(),
                    ),
                };
                window.dispatch_event(gpui::PlatformInput::FileDrop(event), cx);
            });
            cx.background_executor()
                .timer(std::time::Duration::from_secs(5))
                .await;
            let _ = cx.update(|window, cx| {
                window.dispatch_event(
                    gpui::PlatformInput::FileDrop(gpui::FileDropEvent::Exited),
                    cx,
                );
            });
        })
        .detach();
}

fn clipboard_paths(cx: &App) -> Vec<PathBuf> {
    let urls = pasteboard::paths();
    if !urls.is_empty() {
        return urls;
    }
    cx.read_from_clipboard()
        .into_iter()
        .flat_map(|item| item.entries)
        .filter_map(|entry| match entry {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.0),
            _ => None,
        })
        .flatten()
        .collect()
}

fn local_urls(paths: &[PathBuf]) -> Vec<(PathBuf, bool)> {
    paths
        .iter()
        .map(|path| (path.clone(), path.is_dir()))
        .collect()
}

pub(super) fn external_drop_target(
    remote: bool,
    paths: &[PathBuf],
    root: &Path,
    folder: Option<&Path>,
) -> Option<PathBuf> {
    (remote && absolute_paths(paths)).then(|| folder.unwrap_or(root).to_owned())
}

pub(super) fn valid_external_paths(paths: &ExternalPaths) -> bool {
    absolute_paths(paths.paths())
}

fn absolute_paths(paths: &[PathBuf]) -> bool {
    !paths.is_empty() && paths.iter().all(|path| path.is_absolute())
}

impl Pane {
    pub(super) fn check_copy_selection(&mut self) {
        if let Some((request, selection)) = &self.finder.copy
            && (request.upgrade().is_none() || !self.selection_matches(selection))
        {
            if let Some(request) = request.upgrade() {
                request.cancel();
            }
            self.finder.copy = None;
        }
    }

    fn selection_matches(&self, selection: &[PathBuf]) -> bool {
        self.tree.selected.len() == selection.len()
            && selection
                .iter()
                .all(|path| self.tree.selected.contains(path.as_path()))
    }

    pub(super) fn finder_copy(&mut self, window: &Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if self.source.side() == Side::Remote {
            self.begin_copy(paths, true, window, cx);
        } else {
            cancel_copy(cx);
            match pasteboard::write(&local_urls(&paths)) {
                Ok(()) => {
                    let clipboard = cx.global_mut::<FinderClipboard>();
                    if clipboard.current.as_ref().is_some_and(|copy| {
                        !staging::referenced(copy.staged.directory.path(), &paths)
                    }) {
                        clipboard.current = None;
                    }
                    cleanup(cx);
                }
                Err(error) => self.finder_error(error.to_string(), cx),
            }
        }
    }

    pub fn dev_copy(&mut self, paths: Vec<PathBuf>, window: &Window, cx: &mut Context<Self>) {
        self.begin_copy(paths.into(), false, window, cx);
    }

    fn begin_copy(
        &mut self,
        paths: Arc<[PathBuf]>,
        watch: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let request = Rc::new(Request::default());
        if let Some(previous) = cx
            .global_mut::<FinderClipboard>()
            .pending
            .replace(request.clone())
        {
            previous.cancel();
        }
        if watch {
            self.finder.copy = Some((Rc::downgrade(&request), paths.clone()));
        }
        self.stage(paths, Purpose::Copy, request, window, cx);
    }

    pub(super) fn wire_finder_drag(
        &self,
        element: Stateful<Div>,
        drag: Drag,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let pane = cx.entity().downgrade();
        let resolver = pane.clone();
        element
            .on_drag(drag, move |drag, offset, window, cx| {
                if cfg!(target_os = "macos") {
                    let _ = pane.update(cx, |pane, cx| pane.start_drag(drag, window, cx));
                }
                Self::drag_chip(drag, offset, window, cx)
            })
            .when(cfg!(target_os = "macos"), |element| {
                // GPUI 0.3.11 offers Copy only for NSDraggingContext::OutsideApplication.
                element.external_drag_payload(move |drag: &Drag, window, cx| {
                    resolver
                        .update(cx, |pane, cx| pane.finder_drag_payload(drag, window, cx))
                        .ok()
                        .flatten()
                })
            })
    }

    pub(super) fn invalidate_drag_cache(&mut self) {
        if let Some((request, _)) = self.finder.drag.take() {
            request.cancel();
        }
        self.finder.ready.clear();
    }

    pub(super) fn start_drag(&mut self, drag: &Drag, window: &Window, cx: &mut Context<Self>) {
        if drag.side != Side::Remote {
            return;
        }
        let paths = &drag.paths;
        if let Some(ready) = self.finder.ready.get(paths)
            && ready.urls.iter().all(|(path, _)| path.exists())
        {
            return;
        }
        self.finder.ready.remove(paths);
        if self
            .finder
            .drag
            .as_ref()
            .is_some_and(|(request, pending)| paths == pending && !request.cancelled.get())
        {
            return;
        }
        if let Some((previous, _)) = self.finder.drag.take() {
            previous.cancel();
        }
        let request = Rc::new(Request::default());
        self.finder.drag = Some((request.clone(), paths.clone()));
        self.stage(paths.clone(), Purpose::Drag, request, window, cx);
    }

    pub(super) fn finder_drag_payload(
        &self,
        drag: &Drag,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ExternalDragPayload> {
        let entries = if drag.side == Side::Local {
            local_urls(&drag.paths)
        } else if let Some(ready) = self.finder.ready.get(&drag.paths) {
            // A native drag can outlive the pane's cached view of this selection.
            let directories = &mut cx.global_mut::<FinderClipboard>().drag_directories;
            if !directories
                .iter()
                .any(|directory| Rc::ptr_eq(directory, &ready.directory))
            {
                directories.push(ready.directory.clone());
            }
            ready.urls.clone()
        } else {
            let pending =
                self.finder.drag.as_ref().is_some_and(|(request, paths)| {
                    !request.cancelled.get() && paths == &drag.paths
                });
            dev::log(format_args!(
                "remote drag out: no payload (preparing={pending})"
            ));
            if pending && let Some(operations) = &self.operations {
                operations.transfers.update(cx, |queue, cx| {
                    queue.finder_notice("Preparing… try dragging again when ready", window, cx);
                });
            }
            return None;
        };
        dev::log(format_args!(
            "drag out {:?}: {} paths",
            drag.side,
            entries.len()
        ));
        Some(ExternalDragPayload::Files(FileDragPaths::new(entries)))
    }

    pub fn finder_paste(&mut self, window: &Window, cx: &mut Context<Self>) {
        let paths = clipboard_paths(cx);
        if !absolute_paths(&paths) {
            return;
        }
        let Some(operations) = &self.operations else {
            return;
        };
        let folder = self
            .tree
            .cursor_index()
            .map(|index| &self.tree.rows[index])
            .filter(|row| {
                self.tree.selected.contains(&row.path)
                    && row.entry().is_some_and(|entry| entry.is_directory())
            })
            .map(|row| row.path.as_ref());
        let destination = folder.unwrap_or(self.root()).to_owned();
        let target = cx.entity().downgrade();
        if self.source.side() == Side::Remote {
            let origin = cx
                .global::<FinderClipboard>()
                .current
                .as_ref()
                .filter(|copy| staging::referenced(copy.staged.directory.path(), &paths))
                .map_or(TransferOrigin::Pane, |copy| {
                    TransferOrigin::StagedUpload(copy.staged.directory.clone())
                });
            operations.transfers.update(cx, |queue, cx| {
                queue.enqueue_using(
                    origin,
                    Spec {
                        direction: Direction::Upload,
                        sources: paths,
                        destination,
                    },
                    target,
                    window,
                    cx,
                );
            });
        } else {
            let copied = cx
                .global::<FinderClipboard>()
                .current
                .as_ref()
                .filter(|copy| copy.staged.urls.iter().map(|(path, _)| path).eq(&paths))
                .map(|copy| (copy.session.clone(), copy.staged.sources.clone()));
            operations.transfers.update(cx, |queue, cx| match copied {
                Some((session, sources)) => queue.enqueue_using(
                    TransferOrigin::RemoteCopy(session),
                    Spec {
                        direction: Direction::Download,
                        sources,
                        destination,
                    },
                    target,
                    window,
                    cx,
                ),
                None => queue.finder_notice("Pasting local files here is unavailable", window, cx),
            });
        }
    }

    pub(super) fn finder_drop(
        &self,
        paths: &ExternalPaths,
        folder: Option<&Path>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if let (Some(destination), Some(operations)) = (
            external_drop_target(
                self.source.side() == Side::Remote,
                paths.paths(),
                self.root(),
                folder,
            ),
            &self.operations,
        ) {
            let target = cx.entity().downgrade();
            operations.transfers.update(cx, |queue, cx| {
                queue.enqueue(
                    Spec {
                        direction: Direction::Upload,
                        sources: paths.paths().to_vec(),
                        destination,
                    },
                    target,
                    window,
                    cx,
                );
            });
        }
    }

    fn finder_error(&self, error: String, cx: &mut Context<Self>) {
        if let Some(operations) = &self.operations {
            operations
                .transfers
                .update(cx, |queue, cx| queue.finder_error(error, cx));
        }
    }

    fn stage(
        &self,
        paths: Arc<[PathBuf]>,
        purpose: Purpose,
        request: Rc<Request>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let (Source::Remote(session), Some(operations)) = (&self.source, &self.operations) else {
            request.cancel();
            return;
        };
        let session = session.clone();
        let queue = operations.transfers.downgrade();
        let clipboard = cx.global_mut::<FinderClipboard>();
        if clipboard.quitting {
            request.cancel();
            return;
        }
        let directory = match staging::root().and_then(|root| staging::create(&root)) {
            Ok(directory) => Rc::new(directory),
            Err(error) => {
                request.cancel();
                self.finder_error(error.to_string(), cx);
                return;
            }
        };
        clipboard
            .active
            .retain(|directory| directory.strong_count() > 0);
        clipboard.active.push(Rc::downgrade(&directory));
        cx.spawn_in(window, async move |pane, cx| {
            let result = async {
                if request.cancelled.get() {
                    return Ok(None);
                }
                let job = session
                    .enqueue_staging(
                        paths.to_vec(),
                        directory.path().to_owned(),
                        staging::BYTE_LIMIT,
                    )
                    .await?;
                request.attach(job.cancel.clone());
                let id = job.id;
                if queue
                    .update(cx, |queue, cx| queue.track_stage(&job, purpose, cx))
                    .is_err()
                {
                    request.cancel();
                }
                let mut staged = Staged {
                    directory,
                    urls: Vec::new(),
                    sources: Vec::new(),
                };
                let mut too_large = None;
                while let Ok(event) = job.events.recv().await {
                    dev::log(format_args!("Finder {id}: {event:?}"));
                    match &event {
                        Event::Item {
                            source,
                            destination: Some(destination),
                            result,
                        } if destination.parent() == Some(staged.directory.path())
                            && matches!(result, ItemResult::Copied | ItemResult::Directory) =>
                        {
                            staged
                                .urls
                                .push((destination.clone(), *result == ItemResult::Directory));
                            staged.sources.push(source.clone());
                        }
                        Event::SizeLimit(bytes) => too_large = Some(*bytes),
                        _ => {}
                    }
                    let status = match &event {
                        Event::Finished(summary) => Some(summary.status),
                        _ => None,
                    };
                    if queue
                        .update(cx, |queue, cx| queue.stage_event(id, event, cx))
                        .is_err()
                    {
                        request.cancel();
                    }
                    if let Some(status) = status {
                        if let Some(bytes) = too_large {
                            return Err(size_message(bytes).into());
                        }
                        return match status {
                            Status::Cancelled => Ok(None),
                            Status::Completed if !staged.urls.is_empty() => Ok(Some(staged)),
                            _ => Err("Couldn't prepare the files for Finder".into()),
                        };
                    }
                }
                Err("The session ended while preparing the files".into())
            }
            .await;
            let _ = pane.update_in(cx, |pane, window, cx| {
                pane.finish_stage(paths, purpose, &request, session, result, (window, cx));
            });
        })
        .detach();
    }

    fn finish_stage(
        &mut self,
        paths: Arc<[PathBuf]>,
        purpose: Purpose,
        request: &Rc<Request>,
        session: Session,
        result: Result<Option<Staged>, Box<dyn std::error::Error>>,
        (window, cx): (&Window, &mut Context<Self>),
    ) {
        self.check_copy_selection();
        let Some(queue) = self
            .operations
            .as_ref()
            .map(|operations| operations.transfers.clone())
        else {
            return;
        };
        if purpose == Purpose::Copy {
            let clipboard = cx.global_mut::<FinderClipboard>();
            if !clipboard
                .pending
                .as_ref()
                .is_some_and(|pending| Rc::ptr_eq(request, pending))
            {
                return;
            }
            clipboard.pending = None;
            self.finder.copy = None;
        } else if self
            .finder
            .drag
            .as_ref()
            .is_some_and(|(pending, _)| Rc::ptr_eq(request, pending))
        {
            self.finder.drag = None;
        } else {
            return;
        }
        if request.cancelled.get() {
            return;
        }
        let staged = match result {
            Ok(Some(staged)) => staged,
            Ok(None) => return,
            Err(error) => {
                self.finder_error(error.to_string(), cx);
                return;
            }
        };
        let message = if purpose == Purpose::Copy {
            if let Err(error) = pasteboard::write(&staged.urls) {
                self.finder_error(error.to_string(), cx);
                return;
            }
            dev::log(format_args!(
                "Finder: clipboard ready ({} paths)",
                staged.urls.len()
            ));
            cx.global_mut::<FinderClipboard>().current = Some(ClipboardCopy { staged, session });
            cleanup(cx);
            "Ready to paste in Finder"
        } else {
            self.finder.ready.insert(paths, staged);
            "Ready to drag"
        };
        queue.update(cx, |queue, cx| queue.finder_notice(message, window, cx));
    }
}

fn size_message(bytes: u64) -> String {
    let size = file_metadata::format_size(bytes);
    format!("Too large to copy to the clipboard ({size}). Use Download.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_drops_resolve_folder_or_remote_root_only() {
        let paths = [PathBuf::from("/Finder/item")];
        let root = Path::new("/remote");
        let folder = Path::new("/remote/folder");
        assert_eq!(
            external_drop_target(true, &paths, root, Some(folder)),
            Some(folder.into())
        );
        assert_eq!(
            external_drop_target(true, &paths, root, None),
            Some(root.into())
        );
        assert_eq!(external_drop_target(false, &paths, root, None), None);
        assert_eq!(external_drop_target(true, &[], root, None), None);
        assert_eq!(
            external_drop_target(true, &[PathBuf::from("relative")], root, None),
            None
        );
    }
}
