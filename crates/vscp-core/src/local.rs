//! Read local directory entries without following symbolic links for their own metadata.

use std::{
    fs::{self, FileType},
    io,
    os::unix::fs::MetadataExt,
    path::Path,
};

use crate::{Entry, EntryKind};

/// Read a directory, skipping individual entries that disappeared or cannot be read.
pub fn list_dir(path: &Path) -> io::Result<Vec<Entry>> {
    Ok(fs::read_dir(path)?
        .filter_map(|item| stat(&item.ok()?.path()).ok())
        .collect())
}

pub fn stat(path: &Path) -> io::Result<Entry> {
    let metadata = fs::symlink_metadata(path)?;
    let kind = entry_kind(metadata.file_type());
    let symlink_target_kind = if kind == EntryKind::Symlink {
        fs::metadata(path).ok().map(|m| entry_kind(m.file_type()))
    } else {
        None
    };
    Ok(Entry {
        name: path.file_name().unwrap_or(path.as_os_str()).to_owned(),
        kind,
        symlink_target_kind,
        size: Some(metadata.len()),
        mtime: metadata.modified().ok(),
        mode: Some(metadata.mode()),
    })
}

fn entry_kind(kind: FileType) -> EntryKind {
    if kind.is_dir() {
        EntryKind::Directory
    } else if kind.is_file() {
        EntryKind::File
    } else if kind.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn lists_metadata_and_symbolic_links() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fs::write(path.join("espa cio # ñ"), b"abc").unwrap();
        fs::create_dir(path.join("folder")).unwrap();
        symlink("folder", path.join("link")).unwrap();
        symlink("missing", path.join("broken")).unwrap();
        let entries = list_dir(path).unwrap();
        assert_eq!(entries.len(), 4);
        let find = |name: &str| entries.iter().find(|entry| entry.name == name).unwrap();
        let file = find("espa cio # ñ");
        assert_eq!(file.kind, EntryKind::File);
        assert_eq!(file.size, Some(3));
        assert!(file.mode.is_some() && file.mtime.is_some());
        assert_eq!(find("folder").kind, EntryKind::Directory);
        let link = find("link");
        assert_eq!(link.kind, EntryKind::Symlink);
        assert_eq!(link.symlink_target_kind, Some(EntryKind::Directory));
        assert_eq!(find("broken").kind, EntryKind::Symlink);
        assert_eq!(find("broken").symlink_target_kind, None);
        assert_eq!(
            stat(&path.join("link")).unwrap().symlink_target_kind,
            link.symlink_target_kind
        );
        assert!(list_dir(&path.join("broken")).is_err());
    }
}
