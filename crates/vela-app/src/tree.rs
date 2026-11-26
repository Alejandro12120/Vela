use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock},
};

use vela_core::Entry;

pub type PathSet = HashSet<Arc<Path>>;

pub type Child = Arc<ChildNode>;

pub struct ChildNode {
    pub path: Arc<Path>,
    pub entry: Entry,
}

enum Children {
    NotLoaded,
    Loading,
    Loaded(Vec<Child>),
    Error(Arc<str>),
}

pub enum RowState {
    Entry(Child),
    Loading(Arc<Path>),
    Error(Arc<Path>, Arc<str>),
}

pub type Listing = (Arc<Path>, Result<Vec<Child>, String>);

pub struct Row {
    pub depth: usize,
    pub expanded: bool,
    pub state: RowState,
}

impl Row {
    pub fn path(&self) -> &Arc<Path> {
        match &self.state {
            RowState::Entry(child) => &child.path,
            RowState::Loading(path) | RowState::Error(path, _) => path,
        }
    }

    pub fn entry(&self) -> Option<&Entry> {
        match &self.state {
            RowState::Entry(child) => Some(&child.entry),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Selection {
    Single,
    Toggle,
    Range,
}

pub struct Tree {
    pub root: PathBuf,
    pub rows: Vec<Row>,
    pub show_hidden: bool,
    pub expanded: PathSet,
    pub selected: PathSet,
    selected_paths: Arc<[PathBuf]>,
    cursor: Option<Arc<Path>>,
    cursor_index: Option<usize>,
    anchor: Option<Arc<Path>>,
    directories: HashMap<Arc<Path>, Children>,
    pub count: usize,
}

impl Tree {
    pub fn new(root: PathBuf, show_hidden: bool, expanded: PathSet) -> Self {
        Self {
            root,
            rows: Vec::new(),
            show_hidden,
            expanded,
            selected: PathSet::new(),
            selected_paths: Self::empty_paths(),
            cursor: None,
            cursor_index: None,
            anchor: None,
            directories: HashMap::new(),
            count: 0,
        }
    }

    pub fn begin(&mut self, path: Arc<Path>) {
        self.directories.insert(path, Children::Loading);
        self.rebuild();
    }

    pub fn finish(&mut self, path: Arc<Path>, result: Result<Vec<Child>, String>) {
        self.install(path, result);
        self.rebuild();
    }

    fn install(&mut self, path: Arc<Path>, result: Result<Vec<Child>, String>) {
        self.directories.insert(
            path,
            match result {
                Ok(entries) => Children::Loaded(entries),
                Err(error) => Children::Error(error.into()),
            },
        );
    }

    pub fn refresh(&mut self, listings: Vec<Listing>) {
        self.directories.clear();
        self.refresh_branches(listings);
    }

    pub fn has_branch(&self, path: &Path) -> bool {
        self.directories.contains_key(path)
    }

    pub fn refresh_branches(&mut self, listings: Vec<Listing>) {
        for (path, result) in listings {
            self.install(path, result);
        }
        // An expanded path can be hidden by a collapsed ancestor. Only discard it
        // when a fresh listing proves that one of the components is gone.
        let directories = &self.directories;
        let root = &self.root;
        let exists = |path: &Path| {
            path.ancestors()
                .take_while(|path| *path != root)
                .all(
                    |path| match path.parent().and_then(|parent| directories.get(parent)) {
                        Some(Children::Loaded(children)) => children
                            .iter()
                            .any(|child| child.path.as_ref() == path && child.entry.is_directory()),
                        _ => true,
                    },
                )
        };
        self.expanded.retain(|path| exists(path));
        let stale: Vec<_> = directories
            .keys()
            .filter(|path| !exists(path))
            .cloned()
            .collect();
        for path in stale {
            self.directories.remove(&path);
        }
        self.rebuild();
    }

    // Claim reachable loads together, so restoring many expanded folders needs
    // one flatten pass and never starts a second request for the same path.
    pub fn claim_loads(&mut self) -> Vec<Arc<Path>> {
        let pending: Vec<_> = self
            .rows
            .iter()
            .filter(|row| row.expanded && matches!(row.state, RowState::Entry(_)))
            .filter(|row| !self.directories.contains_key(row.path()))
            .map(|row| row.path().clone())
            .collect();
        for path in &pending {
            self.directories.insert(path.clone(), Children::Loading);
        }
        if !pending.is_empty() {
            self.rebuild();
        }
        pending
    }

    pub fn set_expanded(&mut self, path: Arc<Path>, expanded: bool) {
        if expanded {
            self.expanded.insert(path.clone());
            if matches!(self.directories.get(&path), Some(Children::Error(_))) {
                self.directories.remove(&path);
            }
        } else {
            self.expanded.remove(&path);
            let was_descendant = self
                .cursor
                .as_deref()
                .is_some_and(|cursor| cursor != path.as_ref() && cursor.starts_with(&path));
            if was_descendant {
                self.cursor = Some(path);
            }
        }
        self.rebuild();
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild();
    }

    pub fn cursor_index(&self) -> Option<usize> {
        self.cursor_index
    }

    pub fn select(&mut self, index: usize, mode: Selection) {
        let Some(row) = self.rows.get(index).filter(|row| row.entry().is_some()) else {
            return;
        };
        let path = row.path().clone();
        match mode {
            Selection::Single => {
                self.selected.clear();
                self.selected.insert(path.clone());
                self.anchor = Some(path.clone());
            }
            Selection::Toggle => {
                if !self.selected.remove(&path) {
                    self.selected.insert(path.clone());
                }
                self.anchor = Some(path.clone());
            }
            Selection::Range => {
                let anchor = self
                    .anchor
                    .clone()
                    .or_else(|| self.cursor.clone())
                    .unwrap_or_else(|| path.clone());
                let start = self
                    .rows
                    .iter()
                    .position(|row| row.path() == &anchor && row.entry().is_some())
                    .unwrap_or(index);
                self.selected = self.rows[start.min(index)..=start.max(index)]
                    .iter()
                    .filter(|row| row.entry().is_some())
                    .map(|row| row.path().clone())
                    .collect();
                self.anchor = Some(anchor);
            }
        }
        self.cursor = Some(path);
        self.cursor_index = Some(index);
        if self.selected.len() == 1 && self.selected.contains(row.path()) {
            self.selected_paths = Arc::from([row.path().to_path_buf()]);
        } else {
            self.update_selected_paths();
        }
    }

    pub fn step(&mut self, down: bool, mode: Selection) {
        let next = if down {
            let start = self.cursor_index.map_or(0, |cursor| cursor + 1);
            (start..self.rows.len()).find(|&i| self.rows[i].entry().is_some())
        } else {
            let end = self.cursor_index.unwrap_or(self.rows.len());
            (0..end).rfind(|&i| self.rows[i].entry().is_some())
        };
        if let Some(index) = next {
            self.select(index, mode);
        }
    }

    pub fn select_parent(&mut self, index: usize) {
        let parent = self.rows[index].path().parent();
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| Some(row.path().as_ref()) == parent && row.entry().is_some())
        {
            self.select(index, Selection::Single);
        }
    }

    pub fn select_all(&mut self) {
        self.selected = self
            .rows
            .iter()
            .filter(|row| row.entry().is_some())
            .map(|row| row.path().clone())
            .collect();
        if self.cursor.is_none() {
            self.cursor_index = self.rows.iter().position(|row| row.entry().is_some());
            self.cursor = self
                .cursor_index
                .map(|index| self.rows[index].path().clone());
        }
        self.anchor = self.cursor.clone();
        self.update_selected_paths();
    }

    pub fn replace_selection(&mut self, selected: PathSet) {
        self.selected = selected;
        self.update_selected_paths();
    }

    pub fn selected_paths(&self) -> Arc<[PathBuf]> {
        self.selected_paths.clone()
    }

    fn empty_paths() -> Arc<[PathBuf]> {
        static EMPTY: LazyLock<Arc<[PathBuf]>> = LazyLock::new(|| Arc::from([]));
        EMPTY.clone()
    }

    fn update_selected_paths(&mut self) {
        self.selected_paths = if self.selected.is_empty() {
            Self::empty_paths()
        } else {
            self.rows
                .iter()
                .filter(|row| self.selected.contains(row.path()))
                .map(|row| row.path().to_path_buf())
                .collect()
        };
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        self.count = 0;
        flatten(
            &self.directories,
            &self.expanded,
            self.show_hidden,
            &self.root,
            0,
            &mut self.rows,
            &mut self.count,
        );
        self.cursor_index = self.cursor.as_ref().and_then(|path| {
            self.rows
                .iter()
                .position(|row| row.path() == path && row.entry().is_some())
        });
        if self.selected.is_empty() && self.cursor.is_none() && self.anchor.is_none() {
            return;
        }
        let limit = 8;
        let visible: Option<HashSet<&Path>> = (self.selected.len() > limit).then(|| {
            self.rows
                .iter()
                .filter(|row| row.entry().is_some())
                .map(|row| row.path().as_ref())
                .collect()
        });
        let visible = |path: &Arc<Path>| match &visible {
            Some(visible) => visible.contains(path.as_ref()),
            None => self
                .rows
                .iter()
                .any(|row| row.entry().is_some() && row.path() == path),
        };
        self.selected.retain(&visible);
        if self.cursor_index.is_none() {
            self.cursor = None;
        }
        if self.anchor.as_ref().is_some_and(|path| !visible(path)) {
            self.anchor = self.cursor.clone();
        }
        self.update_selected_paths();
    }
}

fn flatten(
    directories: &HashMap<Arc<Path>, Children>,
    expanded: &PathSet,
    show_hidden: bool,
    path: &Path,
    depth: usize,
    rows: &mut Vec<Row>,
    count: &mut usize,
) {
    let state = match directories.get(path).unwrap_or(&Children::NotLoaded) {
        Children::Loaded(children) => {
            for child in children {
                if !show_hidden && child.entry.name.as_encoded_bytes().starts_with(b".") {
                    continue;
                }
                let open = child.entry.is_directory() && expanded.contains(&child.path);
                rows.push(Row {
                    depth,
                    expanded: open,
                    state: RowState::Entry(child.clone()),
                });
                *count += 1;
                if open {
                    flatten(
                        directories,
                        expanded,
                        show_hidden,
                        &child.path,
                        depth + 1,
                        rows,
                        count,
                    );
                }
            }
            return;
        }
        Children::NotLoaded => return,
        Children::Loading => RowState::Loading(path.into()),
        Children::Error(error) => RowState::Error(path.into(), error.clone()),
    };
    rows.push(Row {
        depth,
        expanded: false,
        state,
    });
}

// Prepare the entries off the UI thread. Keep filesystem names intact in paths.
pub fn prepare(path: &Path, entries: Vec<Entry>) -> Vec<Child> {
    let mut named: Vec<_> = entries.into_iter()
        .filter(|entry| matches!(Path::new(&entry.name).components().next(), Some(Component::Normal(name)) if name == entry.name))
        .map(|entry| {
            let lowercase = (!entry.name.as_encoded_bytes().is_ascii())
                .then(|| entry.name.to_string_lossy().to_lowercase());
            (lowercase, entry)
        })
        .collect();
    named.sort_unstable_by(|(a, aa), (b, bb)| {
        bb.is_directory()
            .cmp(&aa.is_directory())
            .then_with(|| {
                let a = a
                    .as_deref()
                    .map_or(aa.name.as_encoded_bytes(), str::as_bytes);
                let b = b
                    .as_deref()
                    .map_or(bb.name.as_encoded_bytes(), str::as_bytes);
                natural_cmp(a, b)
            })
            .then_with(|| aa.name.cmp(&bb.name))
    });
    named
        .into_iter()
        .map(|(_, entry)| {
            Arc::new(ChildNode {
                path: path.join(&entry.name).into(),
                entry,
            })
        })
        .collect()
}

fn natural_cmp(mut a: &[u8], mut b: &[u8]) -> Ordering {
    while let (Some(&x), Some(&y)) = (a.first(), b.first()) {
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let (digits_a, rest_a) =
                a.split_at(a.iter().take_while(|b| b.is_ascii_digit()).count());
            let (digits_b, rest_b) =
                b.split_at(b.iter().take_while(|b| b.is_ascii_digit()).count());
            let significant = |digits: &[u8]| digits.iter().take_while(|&&b| b == b'0').count();
            let value_a = &digits_a[significant(digits_a)..];
            let value_b = &digits_b[significant(digits_b)..];
            (a, b) = (rest_a, rest_b);
            value_a
                .len()
                .cmp(&value_b.len())
                .then_with(|| value_a.cmp(value_b))
                .then_with(|| digits_a.len().cmp(&digits_b.len()))
        } else {
            (a, b) = (&a[1..], &b[1..]);
            x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

#[derive(Clone, Copy)]
pub enum Navigation {
    Open,
    Back,
    Forward,
    Up,
}

#[derive(Default)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
}

impl History {
    pub fn target<'a>(&'a self, current: &'a Path, direction: Navigation) -> Option<&'a Path> {
        match direction {
            Navigation::Back => self.back.last().map(PathBuf::as_path),
            Navigation::Forward => self.forward.last().map(PathBuf::as_path),
            Navigation::Up => current.parent(),
            Navigation::Open => None,
        }
    }

    pub fn commit(&mut self, previous: PathBuf, direction: Navigation) {
        match direction {
            Navigation::Back => {
                self.back.pop();
                self.forward.push(previous);
            }
            Navigation::Forward => {
                self.forward.pop();
                self.back.push(previous);
            }
            Navigation::Open | Navigation::Up => {
                self.back.push(previous);
                self.forward.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use vela_core::EntryKind::{self, Directory, File, Symlink};

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.into(),
            kind,
            symlink_target_kind: None,
            size: None,
            mtime: None,
            mode: None,
        }
    }

    fn path(relative: &str) -> Arc<Path> {
        Path::new("/root").join(relative).into()
    }

    fn listing(relative: &str, entries: Vec<Entry>) -> Listing {
        let path = path(relative);
        let children = prepare(&path, entries);
        (path, Ok(children))
    }

    fn tree(entries: Vec<Entry>) -> Tree {
        let mut tree = Tree::new("/root".into(), false, PathSet::new());
        let (path, result) = listing("", entries);
        tree.finish(path, result);
        tree
    }

    fn names(tree: &Tree) -> Vec<(&str, usize)> {
        tree.rows
            .iter()
            .filter_map(|row| Some((row.entry()?.name.to_str()?, row.depth)))
            .collect()
    }

    #[test]
    fn sorts_directories_and_links_before_natural_case_insensitive_names() {
        let mut link = entry("link", Symlink);
        link.symlink_target_kind = Some(Directory);
        let tree = tree(vec![
            entry("archivo10", File),
            entry("archivo2", File),
            entry("ArchivO1", File),
            link,
            entry("Zeta", Directory),
        ]);
        assert_eq!(
            names(&tree),
            vec![
                ("link", 0),
                ("Zeta", 0),
                ("ArchivO1", 0),
                ("archivo2", 0),
                ("archivo10", 0)
            ]
        );
    }

    #[test]
    fn flattens_loading_error_and_loaded_children_in_place() {
        let mut tree = Tree::new("/root".into(), false, PathSet::new());
        tree.begin(path(""));
        assert!(matches!(tree.rows[0].state, RowState::Loading(_)));
        let (root, result) = listing("", vec![entry("a", Directory), entry("z", File)]);
        tree.finish(root, result);
        tree.set_expanded(path("a"), true);
        assert_eq!(tree.claim_loads(), vec![path("a")]);
        assert!(tree.claim_loads().is_empty());
        assert!(matches!(tree.rows[1].state, RowState::Loading(_)));
        assert_eq!(tree.rows[1].depth, 1);
        assert_eq!(tree.count, 2);
        tree.finish(path("a"), Err("permiso denegado".into()));
        assert!(
            matches!(&tree.rows[1].state, RowState::Error(_, error) if error.as_ref() == "permiso denegado")
        );
        tree.set_expanded(path("a"), false);
        assert_eq!(names(&tree), vec![("a", 0), ("z", 0)]);
        tree.set_expanded(path("a"), true);
        assert_eq!(tree.claim_loads(), vec![path("a")]);
        let (folder, result) = listing("a", vec![entry("b", Directory), entry("f", File)]);
        tree.finish(folder, result);
        tree.set_expanded(path("a/b"), true);
        assert_eq!(tree.claim_loads(), vec![path("a/b")]);
        let (folder, result) = listing("a/b", vec![entry("g", File)]);
        tree.finish(folder, result);
        assert_eq!(
            names(&tree),
            vec![("a", 0), ("b", 1), ("g", 2), ("f", 1), ("z", 0)]
        );
    }

    #[test]
    fn selects_single_toggle_range_keyboard_and_all_without_status_rows() {
        let mut tree = tree(vec![
            entry("a", Directory),
            entry("b", File),
            entry("c", File),
            entry("d", File),
        ]);
        tree.set_expanded(path("a"), true);
        tree.claim_loads();
        tree.select(2, Selection::Single);
        assert_eq!(tree.selected, PathSet::from([path("b")]));
        tree.select(4, Selection::Toggle);
        assert_eq!(tree.selected, PathSet::from([path("b"), path("d")]));
        tree.select(4, Selection::Toggle);
        assert_eq!(tree.selected, PathSet::from([path("b")]));
        tree.select(0, Selection::Range);
        assert_eq!(
            tree.selected,
            PathSet::from([path("a"), path("b"), path("c"), path("d")])
        );
        tree.step(true, Selection::Range);
        assert_eq!(
            tree.selected,
            PathSet::from([path("b"), path("c"), path("d")])
        );
        tree.step(true, Selection::Single);
        assert_eq!(tree.selected, PathSet::from([path("c")]));
        tree.select_all();
        assert_eq!(tree.selected.len(), 4);
    }

    #[test]
    fn collapse_and_hidden_filter_clear_descendants_and_restore_cached_expansion() {
        let mut tree = tree(vec![entry("a", Directory), entry(".secret", File)]);
        let (a, result) = listing("a", vec![entry("b", Directory)]);
        let (b, nested) = listing("a/b", vec![entry("c", File)]);
        tree.set_expanded(a.clone(), true);
        tree.finish(a.clone(), result);
        tree.set_expanded(b.clone(), true);
        tree.finish(b, nested);
        tree.select(2, Selection::Single);
        tree.select_parent(2);
        assert_eq!(tree.selected, PathSet::from([path("a/b")]));
        tree.set_expanded(a.clone(), false);
        assert!(tree.selected.is_empty());
        assert_eq!(tree.cursor_index(), Some(0));
        tree.set_expanded(a, true);
        assert_eq!(names(&tree), vec![("a", 0), ("b", 1), ("c", 2)]);
        tree.toggle_hidden();
        tree.select(3, Selection::Single);
        assert!(tree.selected.contains(&path(".secret")));
        tree.toggle_hidden();
        assert!(tree.selected.is_empty());
    }

    #[test]
    fn refresh_preserves_surviving_paths_and_prunes_removed_subtrees() {
        let mut tree = tree(vec![entry("a", Directory), entry("removed", Directory)]);
        tree.set_expanded(path("a"), true);
        tree.set_expanded(path("removed"), true);
        tree.set_expanded(path("removed/nested"), true);
        let (dir, contents) = listing("a", vec![entry("c", File), entry("d", File)]);
        tree.finish(dir, contents);
        tree.select_all();
        tree.refresh(vec![
            listing("", vec![entry("a", Directory)]),
            listing("a", vec![entry("c", File), entry("e", File)]),
        ]);
        assert_eq!(tree.expanded, PathSet::from([path("a")]));
        assert_eq!(tree.selected, PathSet::from([path("a"), path("a/c")]));
        assert_eq!(names(&tree), vec![("a", 0), ("c", 1), ("e", 1)]);
    }

    #[test]
    fn history_commits_successful_moves_and_discards_forward_on_a_new_branch() {
        let mut history = History::default();
        let a = Path::new("/root");
        let b = Path::new("/root/child");
        assert_eq!(history.target(a, Navigation::Back), None);
        assert_eq!(history.target(b, Navigation::Up), Some(a));
        history.commit(a.into(), Navigation::Open);
        assert_eq!(history.target(b, Navigation::Back), Some(a));
        history.commit(b.into(), Navigation::Back);
        assert_eq!(history.target(a, Navigation::Forward), Some(b));
        history.commit(a.into(), Navigation::Forward);
        history.commit(b.into(), Navigation::Back);
        history.commit(a.into(), Navigation::Up);
        assert_eq!(history.target(Path::new("/"), Navigation::Forward), None);
        assert_eq!(history.target(Path::new("/"), Navigation::Up), None);
    }

    #[test]
    fn flattens_ten_thousand_entries_under_five_ms() {
        let mut entries: Vec<_> = (0..10_000)
            .map(|i| entry(&format!("archivo{i}"), File))
            .collect();
        entries.push(entry("a", Directory));
        let mut tree = tree(entries);
        tree.set_expanded(path("a"), true);
        tree.set_expanded(path("a/b"), true);
        tree.set_expanded(path("a/b/c"), true);
        for (dir, result) in [
            listing("a", vec![entry("b", Directory)]),
            listing("a/b", vec![entry("c", Directory)]),
            listing("a/b/c", vec![entry("child", File)]),
        ] {
            tree.finish(dir, result);
        }
        // Reuse the row allocation as during normal browsing. Take the median
        // to avoid failing due to an unrelated scheduler pause.
        let mut samples = [std::time::Duration::ZERO; 9];
        for sample in &mut samples {
            let start = Instant::now();
            tree.rebuild();
            *sample = start.elapsed();
            assert_eq!(tree.rows.len(), 10_004);
        }
        samples.sort_unstable();
        eprintln!(
            "10,004-row flatten median: {:.3} ms (min {:.3}, max {:.3})",
            samples[4].as_secs_f64() * 1000.0,
            samples[0].as_secs_f64() * 1000.0,
            samples[8].as_secs_f64() * 1000.0
        );
        assert!(samples[4].as_millis() < 5, "{samples:?}");
    }
}
