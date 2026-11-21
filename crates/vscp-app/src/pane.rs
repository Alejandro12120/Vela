use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::{Arc, LazyLock},
    time::{Instant, SystemTime},
};

use gpui::{
    App, BackgroundExecutor, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, Focusable,
    FontFeatures, FontWeight, KeyBinding, MouseButton, MouseDownEvent, Pixels, Render, Role,
    ScrollStrategy, Stateful, UniformListScrollHandle, Window, actions, div, prelude::*, px,
    uniform_list,
};
use vscp_core::{EntryKind, Error, Session, local};

use crate::{
    components::text_input::TextInput,
    dev, file_metadata,
    pane_icons::Icon,
    theme::Theme,
    tree::{self, Child, History, Listing, Navigation, PathSet, RowState, Selection, Tree},
};

#[path = "pane_operations.rs"]
mod operations;

#[path = "finder.rs"]
pub mod finder;

use crate::{
    context_menu::{Menu, Tooltip},
    rename::InlineRename,
};
use operations::Operations;

actions!(
    file_pane,
    [
        MoveUp,
        MoveDown,
        ExtendUp,
        ExtendDown,
        Expand,
        Collapse,
        OpenFolder,
        Parent,
        Back,
        Forward,
        Refresh,
        ToggleHidden,
        SelectAll,
        SwitchPane,
        SubmitPath,
        CopyOther,
        FinderCopy,
        FinderPaste,
        StartRename,
        CommitRename,
        CancelRename,
        NewFolder,
        DeleteFiles,
        DismissFileMenu
    ]
);

pub fn bind_keys(cx: &mut App) {
    use crate::components::modal::{Next, Previous};
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-c", FinderCopy, Some("FileList")),
            KeyBinding::new("cmd-v", FinderPaste, Some("FileList")),
        ]);
    }
    cx.bind_keys([
        KeyBinding::new("tab", Next, Some("FileMenu")),
        KeyBinding::new("down", Next, Some("FileMenu")),
        KeyBinding::new("shift-tab", Previous, Some("FileMenu")),
        KeyBinding::new("up", Previous, Some("FileMenu")),
        KeyBinding::new("f5", CopyOther, Some("FileList")),
        KeyBinding::new("enter", StartRename, Some("FileList")),
        KeyBinding::new("enter", CommitRename, Some("FileRename > TextInput")),
        KeyBinding::new("escape", CancelRename, Some("FileRename > TextInput")),
        KeyBinding::new("escape", DismissFileMenu, Some("FilePane")),
        KeyBinding::new("cmd-shift-n", NewFolder, Some("FileList")),
        KeyBinding::new("cmd-backspace", DeleteFiles, Some("FileList")),
        KeyBinding::new("up", MoveUp, Some("FileList")),
        KeyBinding::new("down", MoveDown, Some("FileList")),
        KeyBinding::new("shift-up", ExtendUp, Some("FileList")),
        KeyBinding::new("shift-down", ExtendDown, Some("FileList")),
        KeyBinding::new("right", Expand, Some("FileList")),
        KeyBinding::new("left", Collapse, Some("FileList")),
        KeyBinding::new("cmd-down", OpenFolder, Some("FileList")),
        KeyBinding::new("cmd-up", Parent, Some("FilePane")),
        KeyBinding::new("cmd-[", Back, Some("FilePane")),
        KeyBinding::new("cmd-]", Forward, Some("FilePane")),
        KeyBinding::new("cmd-r", Refresh, Some("FilePane")),
        KeyBinding::new("cmd-shift-.", ToggleHidden, Some("FilePane")),
        KeyBinding::new("cmd-a", SelectAll, Some("FileList")),
        KeyBinding::new("tab", SwitchPane, Some("FilePane")),
        KeyBinding::new("shift-tab", SwitchPane, Some("FilePane")),
        KeyBinding::new("enter", SubmitPath, Some("FilePane > TextInput")),
    ]);
}

#[derive(Clone)]
pub enum Source {
    Local,
    Remote(Session),
}

impl Source {
    fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Remote(_) => "Remoto",
        }
    }

    async fn resolve_parent_components(
        &self,
        path: PathBuf,
        executor: &BackgroundExecutor,
    ) -> Result<PathBuf, String> {
        if !path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Ok(path);
        }
        // The filesystem decides how ".." traverses a symlink and whether the
        // preceding component exists.
        match self {
            Self::Local => executor
                .spawn(async move { path.canonicalize() })
                .await
                .map_err(|error| error.to_string()),
            Self::Remote(session) => session
                .realpath(path)
                .await
                .map_err(|error| error.to_string()),
        }
    }

    async fn list(&self, path: &Path, executor: &BackgroundExecutor) -> Result<Vec<Child>, String> {
        let start = Instant::now();
        let result = match self {
            Self::Local => {
                let path = path.to_owned();
                executor
                    .spawn(async move {
                        local::list_dir(&path).map(|entries| tree::prepare(&path, entries))
                    })
                    .await
                    .map_err(|error| error.to_string())
            }
            Self::Remote(session) => {
                let delay = dev::remote_delay();
                if !delay.is_zero() {
                    executor.timer(delay).await;
                }
                match session.list_dir(path.to_owned()).await {
                    Ok(entries) => {
                        let path = path.to_owned();
                        Ok(executor
                            .spawn(async move { tree::prepare(&path, entries) })
                            .await)
                    }
                    Err(Error::Sftp(message)) => Err(match message.trim() {
                        "Permission denied" => "Permiso denegado".into(),
                        "No such file" => "La ruta no existe".into(),
                        _ => message,
                    }),
                    Err(error) => Err(error.to_string()),
                }
            }
        };
        match &result {
            Ok(children) => dev::log(format_args!(
                "{} list: {:.2} ms, {} entries at {}",
                self.label(),
                start.elapsed().as_secs_f64() * 1000.,
                children.len(),
                path.display()
            )),
            Err(error) => dev::log(format_args!(
                "{} list error: {:.2} ms at {}: {error}",
                self.label(),
                start.elapsed().as_secs_f64() * 1000.,
                path.display()
            )),
        }
        result.map_err(|error| error.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

pub type ExpansionMemory = Rc<RefCell<HashMap<PathBuf, PathSet>>>;

#[derive(Default, Clone)]
pub struct PaneMemory {
    pub local: ExpansionMemory,
    pub remote: ExpansionMemory,
}

struct LocationRequest {
    revision: u64,
    _task: gpui::Task<()>,
}

pub struct Pane {
    source: Source,
    tree: Tree,
    history: History,
    memory: ExpansionMemory,
    input: Entity<TextInput>,
    path_error: Option<String>,
    focus: FocusHandle,
    list_focus: FocusHandle,
    other_focus: FocusHandle,
    sidebar: Rc<Cell<bool>>,
    scroll: UniformListScrollHandle,
    revision: u64,
    tree_revision: u64,
    location: Option<LocationRequest>,
    operations: Option<Operations>,
    finder: finder::PaneFinder,
    rename: Option<InlineRename>,
    menu: Option<Menu>,
    branch_generation: u64,
    branch_revisions: HashMap<Arc<Path>, u64>,
}

impl Pane {
    pub fn new(
        source: Source,
        root: PathBuf,
        memory: ExpansionMemory,
        sidebar: Rc<Cell<bool>>,
        list_focus: FocusHandle,
        other_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let expanded = memory.borrow().get(&root).cloned().unwrap_or_default();
        let label = match &source {
            Source::Local => "Ruta local",
            Source::Remote(_) => "Ruta remota",
        };
        Self {
            input: cx.new(|cx| TextInput::new(&root.to_string_lossy(), label, label, false, cx)),
            source,
            tree: Tree::new(root, false, expanded),
            history: History::default(),
            memory,
            path_error: None,
            focus: cx.focus_handle(),
            list_focus,
            other_focus,
            sidebar,
            scroll: UniformListScrollHandle::new(),
            revision: 0,
            tree_revision: 0,
            location: None,
            operations: None,
            finder: finder::PaneFinder::default(),
            rename: None,
            menu: None,
            branch_generation: 0,
            branch_revisions: HashMap::new(),
        }
    }

    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(&self.source, Source::Remote(_)) {
            for path in dev::expanded_paths() {
                self.tree.expanded.extend(
                    path.ancestors()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map(|p| self.tree.root.join(p).into()),
                );
            }
        }
        cx.on_focus_in(&self.focus, window, |this, _, cx| {
            this.scroll_to_cursor();
            cx.notify();
        })
        .detach();
        cx.on_focus_out(&self.focus, window, |_, _, _, cx| cx.notify())
            .detach();
        let root: Arc<Path> = self.tree.root.as_path().into();
        self.tree.begin(root.clone());
        self.load_branch(root, window, cx);
        self.report_rows(window);
        cx.notify();
    }

    fn load_branch(&self, path: Arc<Path>, window: &Window, cx: &mut Context<Self>) {
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        let revision = self.tree_revision;
        let branch = self.branch_revisions.get(&path).copied();
        cx.spawn_in(window, async move |this, cx| {
            let result = source.list(&path, &executor).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.tree_revision == revision
                    && this.branch_revisions.get(&path).copied() == branch
                {
                    this.tree.finish(path, result);
                    this.load_expanded(window, cx);
                    this.report_rows(window);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn load_expanded(&mut self, window: &Window, cx: &mut Context<Self>) {
        for path in self.tree.claim_loads() {
            self.load_branch(path, window, cx);
        }
    }

    fn remember(&self) {
        let mut memory = self.memory.borrow_mut();
        if self.tree.expanded.is_empty() {
            memory.remove(&self.tree.root);
        } else {
            memory.insert(self.tree.root.clone(), self.tree.expanded.clone());
        }
    }

    fn target(&self, direction: Navigation) -> Option<PathBuf> {
        self.history
            .target(&self.tree.root, direction)
            .map(Path::to_owned)
    }

    fn navigate(&mut self, direction: Navigation, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.target(direction) {
            self.open(path, direction, window, cx);
        }
    }

    fn open(
        &mut self,
        path: PathBuf,
        direction: Navigation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path == self.tree.root {
            // An explicit return to the current location supersedes a pending move.
            self.location.take();
            self.path_error = None;
            self.set_path_input(window, cx);
            self.list_focus.focus(window, cx);
            cx.notify();
            return;
        }
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        let revision = self.next_location();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let path = source.resolve_parent_components(path, &executor).await?;
                let children = source.list(&path, &executor).await?;
                Ok::<_, String>((path, children))
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.finish_location(revision) {
                    return;
                }
                match result {
                    Ok((path, children)) => {
                        if path == this.tree.root {
                            this.set_path_input(window, cx);
                            cx.notify();
                            return;
                        }
                        let previous = this.tree.root.clone();
                        this.remember();
                        this.history.commit(previous, direction);
                        this.tree_revision += 1;
                        this.branch_revisions.clear();
                        this.rename = None;
                        this.menu = None;
                        let expanded = this.memory.borrow().get(&path).cloned().unwrap_or_default();
                        this.tree = Tree::new(path, this.tree.show_hidden, expanded);
                        this.check_copy_selection();
                        this.tree
                            .finish(this.tree.root.as_path().into(), Ok(children));
                        this.load_expanded(window, cx);
                        this.scroll = UniformListScrollHandle::new();
                        let input_focused = this.input.read(cx).focus_handle(cx).is_focused(window);
                        this.set_path_input(window, cx);
                        if input_focused {
                            this.list_focus.focus(window, cx);
                        }
                        this.report_rows(window);
                    }
                    Err(error) => this.path_error = Some(format!("No se puede leer: {error}")),
                }
                cx.notify();
            });
        });
        self.location = Some(LocationRequest {
            revision,
            _task: task,
        });
        cx.notify();
    }

    fn next_location(&mut self) -> u64 {
        self.path_error = None;
        self.revision += 1;
        self.revision
    }

    fn finish_location(&mut self, revision: u64) -> bool {
        if self
            .location
            .as_ref()
            .is_none_or(|request| request.revision != revision)
        {
            return false;
        }
        self.location.take();
        true
    }

    fn set_path_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(&self.tree.root.to_string_lossy(), window, cx)
        });
    }

    fn submit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        if input.is_composing() {
            cx.propagate();
            return;
        }
        let text = input.value();
        if text.is_empty() {
            self.location.take();
            self.path_error = Some("Introduce una ruta.".into());
            cx.notify();
            return;
        }
        let path = if text == self.tree.root.to_string_lossy() {
            self.tree.root.clone()
        } else {
            self.tree.root.join(text)
        };
        self.open(path, Navigation::Open, window, cx);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.source.clone();
        let executor = cx.background_executor().clone();
        let expanded = self.tree.expanded.clone();
        let root: Arc<Path> = self.tree.root.as_path().into();
        let revision = self.next_location();
        let task = cx.spawn_in(window, async move |this, cx| {
            let mut queue = vec![root];
            let mut listings: Vec<Listing> = Vec::new();
            while let Some(path) = queue.pop() {
                let result = source.list(&path, &executor).await;
                if let Ok(children) = &result {
                    queue.extend(
                        children
                            .iter()
                            .filter(|child| {
                                child.entry.is_directory() && expanded.contains(&child.path)
                            })
                            .map(|child| child.path.clone()),
                    );
                }
                listings.push((path, result));
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if this.finish_location(revision) {
                    if let Err(error) = &listings[0].1 {
                        this.path_error = Some(format!("No se puede leer: {error}"));
                    } else {
                        this.tree_revision += 1;
                        this.branch_revisions.clear();
                        this.rename = None;
                        this.menu = None;
                        this.tree.refresh(listings);
                        this.invalidate_drag_cache();
                        this.remember();
                        this.load_expanded(window, cx);
                        this.scroll_to_cursor();
                        this.report_rows(window);
                    }
                    cx.notify();
                }
            });
        });
        self.location = Some(LocationRequest {
            revision,
            _task: task,
        });
        cx.notify();
    }

    fn report_rows(&self, window: &Window) {
        if dev::logging() {
            let rows = self.tree.rows.len();
            let count = self.tree.count;
            let label = self.source.label();
            window.on_next_frame(move |_, _| {
                dev::log(format_args!("{label} frame: {rows} rows, {count} items"))
            });
        }
    }

    fn scroll_to_cursor(&self) {
        if let Some(index) = self.tree.cursor_index() {
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }

    fn toggle_hidden(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tree.toggle_hidden();
        self.check_copy_selection();
        self.load_expanded(window, cx);
        self.scroll_to_cursor();
        self.report_rows(window);
        cx.notify();
    }

    fn set_expanded(
        &mut self,
        path: Arc<Path>,
        expanded: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.tree.set_expanded(path, expanded);
        self.check_copy_selection();
        self.remember();
        self.load_expanded(window, cx);
        self.scroll_to_cursor();
        self.report_rows(window);
        cx.notify();
    }

    fn step(&mut self, down: bool, mode: Selection, cx: &mut Context<Self>) {
        self.tree.step(down, mode);
        self.check_copy_selection();
        self.scroll_to_cursor();
        cx.notify();
    }

    fn horizontal(&mut self, right: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tree.cursor_index() else {
            return;
        };
        let row = &self.tree.rows[index];
        if row.entry().is_some_and(|entry| entry.is_directory()) && row.expanded != right {
            self.set_expanded(row.path.clone(), right, window, cx);
        } else if !right {
            self.tree.select_parent(index);
            self.check_copy_selection();
            self.scroll_to_cursor();
            cx.notify();
        }
    }

    fn open_folder(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let row = &self.tree.rows[index];
        if row.entry().is_some_and(|entry| entry.is_directory()) {
            self.open(row.path.to_path_buf(), Navigation::Open, window, cx);
        }
    }

    fn toolbar(&self, theme: Theme, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                icon_button(
                    "back",
                    "Atrás",
                    Icon::Back,
                    self.target(Navigation::Back).is_some(),
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.list_focus.focus(window, cx);
                    this.navigate(Navigation::Back, window, cx);
                })),
            )
            .child(
                icon_button(
                    "forward",
                    "Adelante",
                    Icon::Forward,
                    self.target(Navigation::Forward).is_some(),
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.list_focus.focus(window, cx);
                    this.navigate(Navigation::Forward, window, cx);
                })),
            )
            .child(
                icon_button(
                    "up",
                    "Subir",
                    Icon::Up,
                    self.target(Navigation::Up).is_some(),
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.list_focus.focus(window, cx);
                    this.navigate(Navigation::Up, window, cx);
                })),
            )
            .child(div().flex_1().min_w_0().child(self.input.clone()))
            .child(
                icon_button(
                    "copy-other",
                    "Copiar al otro panel (F5)",
                    if self.source.side() == operations::Side::Local {
                        Icon::Transfer
                    } else {
                        Icon::TransferBack
                    },
                    !self.tree.selected.is_empty(),
                    theme,
                )
                .tab_index(0)
                .tab_stop(!self.tree.selected.is_empty())
                .focus(move |style| style.bg(theme.selection))
                .tooltip(|_, cx| cx.new(|_| Tooltip("Copiar al otro panel · F5")).into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.list_focus.focus(window, cx);
                    this.copy_to_other(window, cx);
                })),
            )
            .child(
                icon_button("refresh", "Actualizar", Icon::Refresh, true, theme).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.list_focus.focus(window, cx);
                        this.refresh(window, cx);
                    }),
                ),
            )
            .child(
                icon_button(
                    "hidden",
                    if self.tree.show_hidden {
                        "Ocultar archivos ocultos"
                    } else {
                        "Mostrar archivos ocultos"
                    },
                    Icon::Hidden,
                    true,
                    theme,
                )
                .when(self.tree.show_hidden, |button| {
                    button.bg(theme.text_selection)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.list_focus.focus(window, cx);
                    this.toggle_hidden(window, cx);
                })),
            )
    }

    fn row(&self, index: usize, style: &RowStyle, cx: &Context<Self>) -> Stateful<Div> {
        let RowStyle {
            theme,
            columns,
            active,
            now,
            ref selected_paths,
        } = *style;
        let row = &self.tree.rows[index];
        let offset = px(row.depth.min(9) as f32 * 14.);
        let base = row_layout().text_color(theme.text);
        let Some(entry) = row.entry() else {
            let (text, color) = match &row.state {
                RowState::Loading => ("Cargando…".into(), theme.secondary_text),
                RowState::Error(error) => (format!("No se puede leer: {error}"), theme.danger),
                RowState::Entry(_) => unreachable!(),
            };
            let inset = if columns.size { 18. } else { 4. };
            return base.id(ElementId::named_usize("status", index)).child(
                div()
                    .min_w_0()
                    .pl(offset + px(inset))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(color)
                    .child(text),
            );
        };

        let expanded = row.expanded;
        let expandable = entry.is_directory();
        let selected = self.tree.selected.contains(&row.path);
        let path = row.path.clone();
        let name = entry.name.to_string_lossy().into_owned();
        let icon = match entry.kind {
            EntryKind::Directory => Icon::Folder,
            EntryKind::Symlink => Icon::Link,
            _ => Icon::File,
        };
        let disclosure = div().w(px(18.)).h_full().flex_none().flex().items_center();
        let disclosure = if expandable {
            disclosure
                .id("disclose")
                .role(Role::Button)
                .aria_label(if expanded { "Contraer" } else { "Expandir" })
                .aria_expanded(expanded)
                .cursor_pointer()
                .child(if expanded { Icon::Down } else { Icon::Forward }.draw(theme.secondary_text))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.list_focus.focus(window, cx);
                    if event.click_count() == 1 {
                        this.set_expanded(path.clone(), !expanded, window, cx);
                    }
                }))
                .into_any_element()
        } else {
            disclosure.into_any_element()
        };
        let editing = self
            .rename
            .as_ref()
            .filter(|rename| rename.path == row.path.as_ref());
        let drag = if editing.is_none() {
            self.drag(&row.path, selected_paths.clone(), &name, icon)
        } else {
            None
        };
        let element = base
            .id(ElementId::Path(row.path.clone()))
            .role(Role::TreeItem)
            .aria_label(name.clone())
            .aria_level(row.depth + 1)
            .aria_selected(selected)
            .when(expandable, |row| row.aria_expanded(expanded))
            .when(selected, |row| {
                if active {
                    row.bg(theme.text_selection)
                } else {
                    row.bg(theme.selection)
                }
            })
            .when(!selected, |row| {
                row.hover(move |style| style.bg(theme.hover))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .pl(offset)
                    .child(disclosure)
                    .child(icon.draw(theme.secondary_text))
                    .child(
                        div()
                            .ml_1()
                            .min_w_0()
                            .flex_1()
                            .when_some(editing, |cell, edit| {
                                cell.key_context("FileRename").child(edit.input.clone())
                            })
                            .when(editing.is_none(), |cell| cell.truncate().child(name)),
                    ),
            )
            .when(editing.is_none(), |element| {
                element.children(columns.metadata(entry, now, theme))
            })
            .when_some(editing.and_then(|edit| edit.error), |element, error| {
                element.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.danger)
                        .truncate()
                        .child(error),
                )
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(Some(index), event.position, window, cx);
                }),
            )
            .when(editing.is_none(), |element| {
                element.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.list_focus.focus(window, cx);
                    let modifiers = event.modifiers();
                    let mode = if modifiers.shift {
                        Selection::Range
                    } else if modifiers.platform {
                        Selection::Toggle
                    } else {
                        Selection::Single
                    };
                    this.tree.select(index, mode);
                    this.check_copy_selection();
                    if event.click_count() == 2 && !modifiers.modified() {
                        this.open_folder(index, window, cx);
                    }
                    cx.notify();
                }))
            })
            .when_some(drag, |element, drag| {
                self.wire_finder_drag(element, drag, cx)
            });
        if expandable {
            self.drop_zone(element, Some(row.path.clone()), theme, cx)
        } else {
            element.on_drop(|_: &operations::Drag, _, _| {})
        }
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.check_copy_selection();
        let theme = Theme::for_window(window);
        let active = self.focus.contains_focused(window, cx);
        let width = window.viewport_size().width
            - if self.sidebar.get() {
                px(crate::sidebar::WIDTH)
            } else {
                px(0.)
            };
        let columns = Columns::for_width((width - px(1.)) / 2.);
        let entity = cx.entity();
        let style = RowStyle {
            theme,
            columns,
            active,
            now: SystemTime::now(),
            selected_paths: self.selected_paths(),
        };
        let list = uniform_list("rows", self.tree.rows.len(), move |range, _, cx| {
            entity.update(cx, |this, cx| {
                range.map(|index| this.row(index, &style, cx)).collect()
            })
        })
        .flex_1()
        .min_h_0()
        .w_full()
        .track_scroll(&self.scroll);

        let count = self.tree.count;
        let selected = self.tree.selected.len();
        let empty = self.tree.rows.is_empty();
        div()
            .id("files")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("FilePane")
            .on_action(
                cx.listener(|this, _: &SwitchPane, window, cx| this.other_focus.focus(window, cx)),
            )
            .on_action(cx.listener(|this, _: &SubmitPath, window, cx| this.submit_path(window, cx)))
            .on_action(
                cx.listener(|this, _: &Parent, window, cx| {
                    this.navigate(Navigation::Up, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &Back, window, cx| {
                    this.navigate(Navigation::Back, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Forward, window, cx| {
                this.navigate(Navigation::Forward, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleHidden, window, cx| this.toggle_hidden(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CommitRename, window, cx| {
                cx.stop_propagation();
                this.commit_rename(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CancelRename, window, cx| {
                cx.stop_propagation();
                this.cancel_rename(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DismissFileMenu, window, cx| {
                if this.menu.is_some() {
                    cx.stop_propagation();
                    this.dismiss_menu(cx);
                    this.list_focus.focus(window, cx);
                }
            }))
            .child(self.drop_zone(self.toolbar(theme, cx).id("pane-toolbar"), None, theme, cx))
            .when_some(self.path_error.clone(), |pane, error| {
                pane.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(12.))
                        .text_color(theme.danger)
                        .bg(theme.danger_bg)
                        .child(error),
                )
            })
            .child(self.drop_zone(columns.header(theme).id("column-headers"), None, theme, cx))
            .child(
                self.drop_zone(
                    div()
                        .id("file-tree")
                        .role(Role::Tree)
                        .aria_label(self.source.label())
                        .track_focus(&self.list_focus)
                        .key_context("FileList")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .relative()
                        .overflow_hidden()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                if !this.rename.as_ref().is_some_and(|edit| {
                                    edit.input.focus_handle(cx).is_focused(window)
                                }) {
                                    this.list_focus.focus(window, cx);
                                }
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                this.open_menu(None, event.position, window, cx);
                            }),
                        )
                        .on_action(cx.listener(|this, _: &FinderCopy, window, cx| {
                            this.finder_copy(window, cx)
                        }))
                        .on_action(cx.listener(|this, _: &FinderPaste, window, cx| {
                            this.finder_paste(window, cx)
                        }))
                        .on_action(cx.listener(|this, _: &CopyOther, window, cx| {
                            this.copy_to_other(window, cx)
                        }))
                        .on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                            this.new_folder(this.tree.root.clone(), window, cx)
                        }))
                        .on_action(cx.listener(|this, _: &StartRename, window, cx| {
                            this.start_selected_rename(window, cx)
                        }))
                        .on_action(
                            cx.listener(|this, _: &DeleteFiles, _, cx| this.delete_selected(cx)),
                        )
                        .on_action(cx.listener(|this, _: &MoveUp, _, cx| {
                            this.step(false, Selection::Single, cx)
                        }))
                        .on_action(cx.listener(|this, _: &MoveDown, _, cx| {
                            this.step(true, Selection::Single, cx)
                        }))
                        .on_action(cx.listener(|this, _: &ExtendUp, _, cx| {
                            this.step(false, Selection::Range, cx)
                        }))
                        .on_action(cx.listener(|this, _: &ExtendDown, _, cx| {
                            this.step(true, Selection::Range, cx)
                        }))
                        .on_action(cx.listener(|this, _: &Expand, window, cx| {
                            this.horizontal(true, window, cx)
                        }))
                        .on_action(cx.listener(|this, _: &Collapse, window, cx| {
                            this.horizontal(false, window, cx)
                        }))
                        .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                            this.tree.select_all();
                            this.check_copy_selection();
                            this.scroll_to_cursor();
                            cx.notify();
                        }))
                        .on_action(cx.listener(|this, _: &OpenFolder, window, cx| {
                            if let Some(index) = this.tree.cursor_index() {
                                this.open_folder(index, window, cx);
                            }
                        }))
                        .child(list)
                        .when(empty, |list| {
                            list.child(
                                div()
                                    .absolute()
                                    .top(px(8.))
                                    .left(px(8.))
                                    .text_size(px(12.))
                                    .text_color(theme.secondary_text)
                                    .child("Carpeta vacía"),
                            )
                        }),
                    None,
                    theme,
                    cx,
                ),
            )
            .children(
                self.menu
                    .as_ref()
                    .map(|menu| menu.render(selected, theme, cx)),
            )
            .child(
                div()
                    .h(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .border_t_1()
                    .border_color(if active { theme.accent } else { theme.border })
                    .text_size(px(12.))
                    .text_color(theme.secondary_text)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.source.label()),
                    )
                    .child(div().truncate().child(match (count, selected) {
                        (1, 0) => "1 elemento".into(),
                        (_, 0) => format!("{count} elementos"),
                        (1, 1) => "1 elemento · 1 seleccionado".into(),
                        (_, 1) => format!("{count} elementos · 1 seleccionado"),
                        _ => format!("{count} elementos · {selected} seleccionados"),
                    }))
                    .when(self.location.is_some(), |footer| {
                        footer.child("· Cargando…")
                    }),
            )
    }
}

pub fn icon_button(
    id: &'static str,
    label: &'static str,
    icon: Icon,
    enabled: bool,
    theme: Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .h(px(28.))
        .w(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .child(icon.draw(if enabled {
            theme.text
        } else {
            theme.control_border
        }))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(theme.hover))
        })
}

fn row_layout() -> Div {
    div()
        .flex()
        .items_center()
        .w_full()
        .h(px(25.))
        .px_2()
        .gap_2()
        .overflow_hidden()
}

#[derive(Clone, Copy)]
struct Columns {
    size: bool,
    modified: bool,
    permissions: bool,
}

struct RowStyle {
    theme: Theme,
    columns: Columns,
    active: bool,
    now: SystemTime,
    selected_paths: Arc<[PathBuf]>,
}

impl Columns {
    fn for_width(width: Pixels) -> Self {
        Self {
            size: width >= px(280.),
            modified: width >= px(378.),
            permissions: width >= px(488.),
        }
    }

    fn header(self, theme: Theme) -> Div {
        row_layout()
            .bg(theme.sidebar)
            .border_b_1()
            .border_color(theme.border)
            .flex_none()
            .text_color(theme.secondary_text)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(px(38.))
                    .truncate()
                    .child("Nombre"),
            )
            .when(self.size, |row| row.child(Self::size_cell("Tamaño")))
            .when(self.modified, |row| {
                row.child(Self::date_cell("Modificado"))
            })
            .when(self.permissions, |row| {
                row.child(Self::mode_cell("Permisos"))
            })
    }

    fn metadata(
        self,
        entry: &vscp_core::Entry,
        now: SystemTime,
        theme: Theme,
    ) -> impl Iterator<Item = Div> {
        static TABULAR: LazyLock<FontFeatures> =
            LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])));
        [
            self.size.then(|| {
                Self::size_cell(file_metadata::size(entry)).font_features(TABULAR.clone())
            }),
            self.modified
                .then(|| Self::date_cell(file_metadata::modified(entry.mtime, now))),
            self.permissions
                .then(|| Self::mode_cell(file_metadata::permissions(entry)).font_family("Menlo")),
        ]
        .into_iter()
        .flatten()
        .map(move |cell| cell.text_color(theme.secondary_text).text_size(px(12.)))
    }

    fn size_cell(text: impl Into<gpui::SharedString>) -> Div {
        div()
            .w(px(65.))
            .flex_none()
            .text_right()
            .truncate()
            .child(text.into())
    }

    fn date_cell(text: impl Into<gpui::SharedString>) -> Div {
        div().w(px(122.)).flex_none().truncate().child(text.into())
    }

    fn mode_cell(text: impl Into<gpui::SharedString>) -> Div {
        div().w(px(79.)).flex_none().truncate().child(text.into())
    }
}
