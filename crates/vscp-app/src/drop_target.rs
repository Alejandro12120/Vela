use std::{
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The pane root. The whole list is outlined.
    Root,
    /// The folder row and, when expanded, all its visible descendants.
    Folder {
        row: usize,
        block: RangeInclusive<usize>,
        expanded: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropTarget {
    pub folder: PathBuf,
    pub target: Target,
}

pub struct RowInfo<'a> {
    pub path: &'a Path,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// `hovered` is the row under the pointer, or None below the rows.
/// `same_pane` distinguishes a move from an opposite-pane copy.
pub fn resolve(
    rows: &[RowInfo<'_>],
    root: &Path,
    hovered: Option<usize>,
    dragged: &[PathBuf],
    same_pane: bool,
) -> Option<DropTarget> {
    let folder = hovered.and_then(|index| {
        let row = &rows[index];
        if row.is_dir {
            Some(index)
        } else {
            rows[..index]
                .iter()
                .rposition(|parent| parent.depth < row.depth)
        }
    });
    let target = match folder {
        Some(index) => DropTarget {
            folder: rows[index].path.to_owned(),
            target: folder_target(rows, index),
        },
        None => DropTarget {
            folder: root.to_owned(),
            target: Target::Root,
        },
    };
    if same_pane
        && (dragged.iter().any(|path| target.folder.starts_with(path))
            || dragged
                .iter()
                .all(|path| path.parent() == Some(&target.folder)))
    {
        return None;
    }
    Some(target)
}

pub fn folder_target(rows: &[RowInfo<'_>], row: usize) -> Target {
    let RowInfo {
        depth, expanded, ..
    } = rows[row];
    let last = if expanded {
        row + rows[row + 1..]
            .iter()
            .take_while(|child| child.depth > depth)
            .count()
    } else {
        row
    };
    Target::Folder {
        row,
        block: row..=last,
        expanded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_at(hovered: Option<usize>, dragged: &[&str], same_pane: bool) -> Option<DropTarget> {
        let rows = [
            ("/root/closed", 0, true, false),
            ("/root/open", 0, true, true),
            ("/root/open/a", 1, false, false),
            ("/root/open/b", 1, false, false),
            ("/root/open/c", 1, false, false),
            ("/root/outer", 0, true, true),
            ("/root/outer/inner", 1, true, true),
            ("/root/outer/inner/a", 2, false, false),
            ("/root/file", 0, false, false),
        ]
        .map(|(path, depth, is_dir, expanded)| RowInfo {
            path: Path::new(path),
            depth,
            is_dir,
            expanded,
        });
        let dragged: Vec<_> = dragged.iter().map(PathBuf::from).collect();
        resolve(&rows, Path::new("/root"), hovered, &dragged, same_pane)
    }

    fn folder(path: &str, row: usize, last: usize, expanded: bool) -> Option<DropTarget> {
        Some(DropTarget {
            folder: path.into(),
            target: Target::Folder {
                row,
                block: row..=last,
                expanded,
            },
        })
    }

    fn root() -> Option<DropTarget> {
        Some(DropTarget {
            folder: "/root".into(),
            target: Target::Root,
        })
    }

    #[test]
    fn collapsed_folder() {
        assert_eq!(
            resolve_at(Some(0), &["/root/file"], true),
            folder("/root/closed", 0, 0, false)
        );
    }

    #[test]
    fn expanded_folder_spans_three_children() {
        assert_eq!(
            resolve_at(Some(1), &["/root/file"], true),
            folder("/root/open", 1, 4, true)
        );
    }

    #[test]
    fn file_uses_expanded_parent_block() {
        assert_eq!(
            resolve_at(Some(3), &["/root/file"], true),
            folder("/root/open", 1, 4, true)
        );
    }

    #[test]
    fn grandchild_uses_direct_parent() {
        assert_eq!(
            resolve_at(Some(7), &["/root/file"], true),
            folder("/root/outer/inner", 6, 7, true)
        );
    }

    #[test]
    fn root_file_targets_root() {
        assert_eq!(resolve_at(Some(8), &["/root/open/a"], true), root());
    }

    #[test]
    fn empty_space_targets_root() {
        assert_eq!(resolve_at(None, &["/root/open/a"], true), root());
    }

    #[test]
    fn rejects_folder_itself() {
        assert_eq!(resolve_at(Some(5), &["/root/outer"], true), None);
    }

    #[test]
    fn rejects_own_descendant() {
        assert_eq!(resolve_at(Some(6), &["/root/outer"], true), None);
    }

    #[test]
    fn rejects_same_parent_no_op() {
        assert_eq!(
            resolve_at(Some(1), &["/root/open/a", "/root/open/c"], true),
            None
        );
    }

    #[test]
    fn accepts_selection_when_any_item_would_move() {
        assert_eq!(
            resolve_at(Some(1), &["/root/open/a", "/root/file"], true),
            folder("/root/open", 1, 4, true)
        );
    }

    #[test]
    fn opposite_pane_ignores_matching_paths() {
        assert_eq!(resolve_at(Some(8), &["/root/file"], false), root());
        assert_eq!(
            resolve_at(Some(6), &["/root/outer"], false),
            folder("/root/outer/inner", 6, 7, true)
        );
    }
}
