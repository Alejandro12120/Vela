//! Read local directory entries without following symbolic links for their own metadata.

use std::{
    fs::{self, FileType},
    io::{self, Read},
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

const TEXT_EXTENSIONS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "txt", "md", "markdown", "rst", "log", "csv", "tsv", "json",
    "jsonc", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "xml", "html", "htm", "css",
    "scss", "js", "mjs", "cjs", "ts", "tsx", "jsx", "py", "rb", "pl", "php", "go", "rs", "c", "h",
    "cc", "cpp", "hpp", "java", "kt", "swift", "sql", "lua", "vim", "service", "socket", "timer",
];

const SNIFF_BYTES: usize = 8 * 1024;

/// Whether a file is text that an editor should open. Known extensions answer
/// without reading. Other files are text when their first 8 KiB hold no NUL
/// byte and are valid UTF-8.
pub fn is_text(path: &Path) -> io::Result<bool> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            TEXT_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    {
        return Ok(true);
    }
    let mut buffer = Vec::with_capacity(SNIFF_BYTES);
    fs::File::open(path)?
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut buffer)?;
    Ok(looks_like_text(&buffer))
}

fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return false;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        // The sample may end inside a multi-byte sequence: up to 3 bytes.
        Err(error) => error.error_len().is_none() && bytes.len() - error.valid_up_to() <= 3,
    }
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
    fn known_extensions_are_text_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        // Not even created: a known extension is not read.
        assert!(is_text(&dir.path().join("run.SH")).unwrap());
        assert!(is_text(&dir.path().join("a.service")).unwrap());
        assert!(is_text(&dir.path().join("notas.md")).unwrap());
    }

    #[test]
    fn other_files_are_classified_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        assert!(is_text(&write("Makefile", "all:\n\techo ñ\n".as_bytes())).unwrap());
        assert!(is_text(&write("vacío", b"")).unwrap());
        assert!(!is_text(&write("imagen.png", b"\x89PNG\r\n\x1a\n\0\0")).unwrap());
        assert!(!is_text(&write("binario", b"abc\0def")).unwrap());
        assert!(!is_text(&write("latin1", b"caf\xe9 au lait")).unwrap());
        assert!(is_text(&write("sin-extension", "hola ñandú".as_bytes())).unwrap());
        assert!(is_text(&dir.path().join("fantasma.txt")).unwrap());
        assert!(is_text(&dir.path().join("inexistente")).is_err());
    }

    #[test]
    fn a_cut_multibyte_sequence_at_the_sample_end_is_text() {
        let mut bytes = vec![b'a'; SNIFF_BYTES - 1];
        bytes.extend("ñ".as_bytes());
        // Only the first byte of "ñ" fits in the sample.
        assert!(looks_like_text(&bytes[..SNIFF_BYTES]));
        // Four stray continuation-leading bytes are not a cut sequence.
        assert!(!looks_like_text(&[b'a', 0xc3, 0xc3, 0xc3, 0xc3]));
        let emoji = "😀".as_bytes();
        assert!(looks_like_text(&emoji[..3]));
    }

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

/// Rename without replacing an existing item.
#[cfg(target_os = "macos")]
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // Both pointers remain valid and NUL-terminated for the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(error) => Err(error),
        Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
    }
}

/// Move the item to the system Trash and return its new path, when supplied by macOS.
#[cfg(target_os = "macos")]
pub fn trash(path: &Path) -> io::Result<Option<std::path::PathBuf>> {
    use objc2_foundation::{NSFileManager, NSURL};
    use std::{
        ffi::{CStr, CString, OsStr},
        os::unix::ffi::OsStrExt,
    };

    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Se necesita una ruta absoluta",
        ));
    }
    let is_directory = fs::symlink_metadata(path)?.is_dir();
    let path = CString::new(path.as_os_str().as_bytes())?;
    objc2::rc::autoreleasepool(|_| {
        // NSURL copies the NUL-terminated filesystem representation.
        let url = unsafe {
            NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                std::ptr::NonNull::from(path.as_c_str()).cast(),
                is_directory,
                None,
            )
        };
        let mut result = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut result))
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(result.map(|url| {
            // The string is owned by the URL and is copied before it goes out of scope.
            let bytes = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };
            Path::new(OsStr::from_bytes(bytes.to_bytes())).to_path_buf()
        }))
    })
}

#[cfg(not(target_os = "macos"))]
pub fn trash(_: &Path) -> io::Result<Option<std::path::PathBuf>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Papelera no disponible en este sistema",
    ))
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn sends_a_test_file_to_trash() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory
        .path()
        .join(format!("vSCP Trash test {}", std::process::id()));
    fs::write(&file, b"vSCP test").unwrap();
    let trashed = trash(&file).unwrap().expect("macOS Trash path");
    eprintln!("Trashed test item: {}", trashed.display());
    let result = fs::read(&trashed);
    fs::remove_file(&trashed).unwrap();
    assert!(!file.exists());
    assert_eq!(result.unwrap(), b"vSCP test");
}
