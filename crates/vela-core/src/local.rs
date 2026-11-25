//! Read local directory entries without following symbolic links for their own metadata.

use std::{
    fs::{self, FileType},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
};

#[cfg(not(target_os = "macos"))]
use std::{
    ffi::{OsStr, OsString},
    fs::{OpenOptions, Permissions},
    io::Write,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::PermissionsExt,
    },
    path::PathBuf,
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

/// Whether the extension alone marks the file as text. Performs no I/O.
pub fn has_text_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            TEXT_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Whether a file is text that an editor should open. Known extensions answer
/// without reading. Other files are text when their first 8 KiB hold no NUL
/// byte and are valid UTF-8.
pub fn is_text(path: &Path) -> io::Result<bool> {
    if has_text_extension(path) {
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
pub fn trash(path: &Path) -> io::Result<Option<PathBuf>> {
    trash_in(path, &home_trash_dir()?, unsafe { libc::getuid() })
}

#[cfg(not(target_os = "macos"))]
fn home_trash_dir() -> io::Result<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_DATA_HOME")
        && !base.is_empty()
    {
        let base = PathBuf::from(base);
        if base.is_absolute() {
            return Ok(base.join("Trash"));
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME no definido"))?;
    Ok(PathBuf::from(home).join(".local/share/Trash"))
}

#[cfg(not(target_os = "macos"))]
struct TrashLocation {
    root: PathBuf,
    relative_to: Option<PathBuf>,
}

#[cfg(not(target_os = "macos"))]
fn trash_in(path: &Path, home_trash: &Path, uid: u32) -> io::Result<Option<PathBuf>> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Se necesita una ruta absoluta",
        ));
    }
    let dev = fs::symlink_metadata(path)?.dev();

    let home_existed = home_trash.is_dir();
    fs::create_dir_all(home_trash.join("files"))?;
    fs::create_dir_all(home_trash.join("info"))?;
    if !home_existed {
        fs::set_permissions(home_trash, Permissions::from_mode(0o700))?;
    }

    let home = || TrashLocation {
        root: home_trash.to_owned(),
        relative_to: None,
    };
    // Like KIO, a disk other than the home one keeps its own trash only when
    // it is a block device. Memory and virtual filesystems such as tmpfs, and
    // a trash that cannot be prepared, fall back to the home trash, which
    // file managers always list.
    let location = if fs::metadata(home_trash)?.dev() == dev {
        home()
    } else if mount_source_is_block_device(dev) {
        volume_trash(&mount_topdir(path, dev)?, uid).unwrap_or_else(|_| home())
    } else {
        home()
    };

    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Se necesita un nombre"))?
        .to_owned();
    let files = location.root.join("files");
    let info = location.root.join("info");

    let stored_path = match &location.relative_to {
        None => path.as_os_str().as_bytes().to_vec(),
        Some(base) => path
            .strip_prefix(base)
            .map(|relative| relative.as_os_str().as_bytes().to_vec())
            .unwrap_or_else(|_| path.as_os_str().as_bytes().to_vec()),
    };
    let contents = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(&stored_path),
        deletion_date()?
    );

    let mut number = 1u32;
    let (chosen, info_path, mut info_file) = loop {
        let candidate = if number == 1 {
            name.clone()
        } else {
            numbered_name(&name, number)
        };
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        let info_path = info.join(info_name);
        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                number = number
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("Demasiados nombres en la Papelera"))?;
                continue;
            }
            Err(error) => return Err(error),
        };
        match fs::symlink_metadata(files.join(&candidate)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                drop(file);
                let _ = fs::remove_file(&info_path);
                number = number
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("Demasiados nombres en la Papelera"))?;
                continue;
            }
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(&info_path);
                return Err(error);
            }
        }
        break (candidate, info_path, file);
    };

    let written = (|| -> io::Result<()> {
        info_file.write_all(contents.as_bytes())?;
        info_file.flush()?;
        info_file.sync_all()
    })();
    if let Err(error) = written {
        drop(info_file);
        let _ = fs::remove_file(&info_path);
        return Err(error);
    }
    drop(info_file);

    let destination = files.join(&chosen);
    match fs::rename(path, &destination) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            if let Err(error) = copy_tree(path, &destination) {
                let _ = remove_tree(&destination);
                let _ = fs::remove_file(&info_path);
                return Err(error);
            }
            // The copy is safe in the trash, so a failed removal keeps both.
            remove_tree(path)?;
        }
        Err(error) => {
            let _ = fs::remove_file(&info_path);
            return Err(error);
        }
    }
    Ok(Some(destination))
}

/// Copy a file, folder or symlink across disks, keeping permissions and
/// never following symlinks.
#[cfg(not(target_os = "macos"))]
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if kind.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        // Set last, so a read-only folder still receives its children.
        fs::set_permissions(to, meta.permissions())
    } else if kind.is_file() {
        fs::copy(from, to).map(drop)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "No se puede mover este tipo de elemento a la Papelera",
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn remove_tree(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether the filesystem with device number `dev` is mounted from a block
/// device (`/dev/...`), read from `/proc/self/mountinfo`. Unknown devices,
/// such as tmpfs, overlays or btrfs subvolumes that are not mount points,
/// count as not being one.
#[cfg(not(target_os = "macos"))]
fn mount_source_is_block_device(dev: u64) -> bool {
    fs::read_to_string("/proc/self/mountinfo")
        .is_ok_and(|mountinfo| mountinfo_source_is_block_device(&mountinfo, dev))
}

#[cfg(not(target_os = "macos"))]
fn mountinfo_source_is_block_device(mountinfo: &str, dev: u64) -> bool {
    // glibc's encoding of `major:minor` in `st_dev`.
    let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0fff);
    let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0xff);
    let wanted = format!("{major}:{minor}");
    // Later lines win: they are mounted over earlier ones.
    mountinfo
        .lines()
        .filter(|line| line.split(' ').nth(2) == Some(wanted.as_str()))
        .filter_map(|line| {
            let (_, after) = line.split_once(" - ")?;
            after.split(' ').nth(1)
        })
        .next_back()
        .is_some_and(|source| source.starts_with("/dev/"))
}

#[cfg(not(target_os = "macos"))]
fn mount_topdir(path: &Path, dev: u64) -> io::Result<PathBuf> {
    let mut top = path.parent().unwrap_or(path).to_owned();
    while let Some(parent) = top.parent() {
        if fs::metadata(parent)?.dev() != dev {
            break;
        }
        top = parent.to_owned();
    }
    Ok(top)
}

#[cfg(not(target_os = "macos"))]
fn volume_trash(topdir: &Path, uid: u32) -> io::Result<TrashLocation> {
    let dot_trash = topdir.join(".Trash");
    if let Ok(meta) = fs::symlink_metadata(&dot_trash)
        && meta.is_dir()
        && meta.mode() & 0o1000 != 0
    {
        let user_dir = dot_trash.join(uid.to_string());
        if prepare_trash_root(&user_dir) {
            return Ok(TrashLocation {
                root: user_dir,
                relative_to: Some(topdir.to_owned()),
            });
        }
    }

    let user_dir = topdir.join(format!(".Trash-{uid}"));
    match fs::symlink_metadata(&user_dir) {
        Ok(meta) if meta.is_dir() && meta.uid() == uid => {}
        Ok(_) => return Err(trash_unavailable()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&user_dir)?;
            fs::set_permissions(&user_dir, Permissions::from_mode(0o700))?;
        }
        Err(error) => return Err(error),
    }
    fs::create_dir_all(user_dir.join("files"))?;
    fs::create_dir_all(user_dir.join("info"))?;
    Ok(TrashLocation {
        root: user_dir,
        relative_to: Some(topdir.to_owned()),
    })
}

#[cfg(not(target_os = "macos"))]
fn prepare_trash_root(root: &Path) -> bool {
    match fs::create_dir(root) {
        Ok(()) => {
            if fs::set_permissions(root, Permissions::from_mode(0o700)).is_err() {
                return false;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(root).is_ok_and(|meta| meta.is_dir()) {
                return false;
            }
        }
        Err(_) => return false,
    }
    fs::create_dir_all(root.join("files")).is_ok() && fs::create_dir_all(root.join("info")).is_ok()
}

#[cfg(not(target_os = "macos"))]
fn trash_unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "No se puede mover a la Papelera en este disco",
    )
}

#[cfg(not(target_os = "macos"))]
fn numbered_name(original: &OsStr, number: u32) -> OsString {
    let bytes = original.as_bytes();
    let split = bytes
        .iter()
        .rposition(|&byte| byte == b'.')
        .filter(|&dot| dot > 0);
    let mut result = match split {
        Some(dot) => bytes[..dot].to_vec(),
        None => bytes.to_vec(),
    };
    result.push(b'.');
    result.extend_from_slice(number.to_string().as_bytes());
    if let Some(dot) = split {
        result.push(b'.');
        result.extend_from_slice(&bytes[dot + 1..]);
    }
    OsString::from_vec(result)
}

#[cfg(not(target_os = "macos"))]
fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    const KEEP: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~/!$&'()*+,;=:@";
    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if KEEP.contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

#[cfg(not(target_os = "macos"))]
fn deletion_date() -> io::Result<String> {
    let mut seconds: libc::time_t = 0;
    if unsafe { libc::time(&mut seconds) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut broken_down = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&seconds, broken_down.as_mut_ptr()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    let time = unsafe { broken_down.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    ))
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn sends_a_test_file_to_trash() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory
        .path()
        .join(format!("Vela Trash test {}", std::process::id()));
    fs::write(&file, b"Vela test").unwrap();
    let trashed = trash(&file).unwrap().expect("macOS Trash path");
    eprintln!("Trashed test item: {}", trashed.display());
    let result = fs::read(&trashed);
    fs::remove_file(&trashed).unwrap();
    assert!(!file.exists());
    assert_eq!(result.unwrap(), b"Vela test");
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

#[cfg(all(test, not(target_os = "macos")))]
mod trash_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    fn assert_date(value: &str) {
        assert_eq!(value.len(), 19, "{value}");
        for (position, byte) in value.bytes().enumerate() {
            match position {
                4 | 7 => assert_eq!(byte, b'-'),
                10 => assert_eq!(byte, b'T'),
                13 | 16 => assert_eq!(byte, b':'),
                _ => assert!(byte.is_ascii_digit(), "{value}"),
            }
        }
    }

    #[test]
    fn moves_a_file_and_writes_trashinfo() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");
        let file = dir.path().join("notas.txt");
        fs::write(&file, b"contenido").unwrap();

        let trashed = trash_in(&file, &home, uid()).unwrap().unwrap();

        assert_eq!(trashed, home.join("files/notas.txt"));
        assert!(!file.exists());
        assert_eq!(fs::read(&trashed).unwrap(), b"contenido");
        let info = fs::read_to_string(home.join("info/notas.txt.trashinfo")).unwrap();
        let lines: Vec<&str> = info.lines().collect();
        assert_eq!(lines.len(), 3, "{info:?}");
        assert_eq!(lines[0], "[Trash Info]");
        assert_eq!(
            lines[1],
            format!("Path={}", percent_encode(file.as_os_str().as_bytes()))
        );
        assert_date(lines[2].strip_prefix("DeletionDate=").unwrap());
    }

    #[test]
    fn moves_a_directory_with_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");
        let source = dir.path().join("carpeta");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hijo.txt"), b"dentro").unwrap();

        let trashed = trash_in(&source, &home, uid()).unwrap().unwrap();

        assert_eq!(trashed, home.join("files/carpeta"));
        assert!(!source.exists());
        assert_eq!(fs::read(trashed.join("hijo.txt")).unwrap(), b"dentro");
    }

    #[test]
    fn numbered_names_keep_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");
        let mut trashed = Vec::new();
        for _ in 0..3 {
            let file = dir.path().join("notas.txt");
            fs::write(&file, b"x").unwrap();
            trashed.push(trash_in(&file, &home, uid()).unwrap().unwrap());
        }
        let names: Vec<_> = trashed
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["notas.txt", "notas.2.txt", "notas.3.txt"]);
        assert!(home.join("info/notas.3.txt.trashinfo").is_file());

        for name in ["name", "name"] {
            let file = dir.path().join(name);
            fs::write(&file, b"x").unwrap();
            trashed.push(trash_in(&file, &home, uid()).unwrap().unwrap());
        }
        assert_eq!(trashed[4].file_name().unwrap(), "name.2");
        assert!(home.join("info/name.2.trashinfo").is_file());
    }

    #[test]
    fn encodes_the_original_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");
        let file = dir.path().join("espacio ñ%#.txt");
        fs::write(&file, b"x").unwrap();

        trash_in(&file, &home, uid()).unwrap().unwrap();

        let info = fs::read_to_string(home.join("info/espacio ñ%#.txt.trashinfo")).unwrap();
        let expected = format!(
            "Path={}/espacio%20%C3%B1%25%23.txt",
            percent_encode(dir.path().as_os_str().as_bytes())
        );
        assert!(info.contains(&expected), "{info:?}");
    }

    #[test]
    fn trashes_a_symlink_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");
        let target = dir.path().join("target");
        fs::write(&target, b"seguir").unwrap();
        let link = dir.path().join("link");
        symlink("target", &link).unwrap();

        let trashed = trash_in(&link, &home, uid()).unwrap().unwrap();

        assert_eq!(fs::read_link(&trashed).unwrap(), Path::new("target"));
        assert!(!link.exists());
        assert_eq!(fs::read(&target).unwrap(), b"seguir");
    }

    #[test]
    fn rejects_relative_and_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");

        let error = trash_in(Path::new("relativo.txt"), &home, uid()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let missing = dir.path().join("fantasma.txt");
        let error = trash_in(&missing, &home, uid()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!home.join("info/fantasma.txt.trashinfo").exists());
    }

    #[test]
    fn a_failed_move_leaves_no_info_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("trash");
        fs::create_dir_all(home.join("files")).unwrap();
        fs::create_dir_all(home.join("info")).unwrap();
        let file = dir.path().join("notas.txt");
        fs::write(&file, b"x").unwrap();

        fs::set_permissions(home.join("files"), Permissions::from_mode(0o500)).unwrap();
        let result = trash_in(&file, &home, uid());
        fs::set_permissions(home.join("files"), Permissions::from_mode(0o700)).unwrap();

        assert!(result.is_err());
        assert_eq!(fs::read_dir(home.join("info")).unwrap().count(), 0);
        assert!(file.exists());
    }

    #[test]
    fn only_block_devices_keep_their_own_trash() {
        let mountinfo = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
30 22 0:35 / /tmp rw,nosuid,nodev shared:5 - tmpfs tmpfs rw
31 22 8:17 / /run/media/usb rw,relatime shared:9 - vfat /dev/sdb1 rw
40 30 8:17 / /tmp/usb rw,relatime shared:9 - vfat /dev/sdb1 rw
41 22 0:50 / /mnt/over rw - overlay overlay rw
";
        assert!(mountinfo_source_is_block_device(mountinfo, (259 << 8) | 2));
        assert!(mountinfo_source_is_block_device(mountinfo, (8 << 8) | 17));
        assert!(!mountinfo_source_is_block_device(mountinfo, 35));
        assert!(!mountinfo_source_is_block_device(mountinfo, 50));
        assert!(!mountinfo_source_is_block_device(mountinfo, 99));
    }

    /// `/dev/shm` and the temporary directory are usually separate memory
    /// disks, so the item goes to the home trash by copying.
    #[test]
    fn a_memory_disk_falls_back_to_the_home_trash() {
        let shm = Path::new("/dev/shm");
        let dir = tempfile::tempdir().unwrap();
        if !shm.is_dir()
            || fs::metadata(shm).unwrap().dev() == fs::metadata(dir.path()).unwrap().dev()
        {
            eprintln!("skipped: /dev/shm is missing or on the same disk");
            return;
        }
        let home = dir.path().join("trash");
        let outside = tempfile::tempdir_in(shm).unwrap();
        let source = outside.path().join("carpeta");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("hijo.txt"), b"dentro").unwrap();
        fs::set_permissions(source.join("hijo.txt"), Permissions::from_mode(0o640)).unwrap();
        symlink("hijo.txt", source.join("enlace")).unwrap();

        let trashed = trash_in(&source, &home, uid()).unwrap().unwrap();

        assert_eq!(trashed, home.join("files/carpeta"));
        assert!(!source.exists());
        assert_eq!(fs::read(trashed.join("hijo.txt")).unwrap(), b"dentro");
        let mode = fs::metadata(trashed.join("hijo.txt")).unwrap().mode();
        assert_eq!(mode & 0o777, 0o640);
        assert_eq!(
            fs::read_link(trashed.join("enlace")).unwrap(),
            Path::new("hijo.txt")
        );
        let info = fs::read_to_string(home.join("info/carpeta.trashinfo")).unwrap();
        assert!(
            info.contains(&format!(
                "Path={}",
                percent_encode(source.as_os_str().as_bytes())
            )),
            "{info:?}"
        );
    }
}
