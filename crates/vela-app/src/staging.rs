use std::{
    cell::Cell,
    fs::{self, DirBuilder, File},
    io,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

pub const BYTE_LIMIT: u64 = 2_000_000_000;
const PREFIX: &str = "handoff-";

pub fn root() -> io::Result<PathBuf> {
    if let Some(path) = crate::dev::staging_root() {
        return Ok(path);
    }
    std::env::home_dir()
        .map(|home| home.join("Library/Caches/Vela/staging"))
        .ok_or_else(|| io::Error::other("No se encuentra la carpeta de caché"))
}

pub struct Directory {
    temp: tempfile::TempDir,
    _lock: File,
    keep: Cell<bool>,
}

impl Directory {
    pub fn path(&self) -> &Path {
        self.temp.path()
    }

    pub fn keep(&self) {
        self.keep.set(true);
    }

    pub fn remove(&self) -> io::Result<()> {
        remove_directory(self.path())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        self.temp.disable_cleanup(self.keep.get());
    }
}

fn lock(directory: &Path, mode: libc::c_int) -> io::Result<File> {
    let file = File::open(directory)?;
    // The descriptor remains open for the lifetime of the advisory lock.
    if unsafe { libc::flock(file.as_raw_fd(), mode) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

pub fn create(root: &Path) -> io::Result<Directory> {
    DirBuilder::new().recursive(true).mode(0o700).create(root)?;
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(io::Error::other("La caché no es una carpeta"));
    }
    let _root_lock = lock(root, libc::LOCK_EX)?;
    let temp = tempfile::Builder::new()
        .prefix(PREFIX)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(root)?;
    let stage_lock = lock(temp.path(), libc::LOCK_EX)?;
    Ok(Directory {
        temp,
        _lock: stage_lock,
        keep: Cell::new(false),
    })
}

pub fn referenced(directory: &Path, paths: &[PathBuf]) -> bool {
    paths.iter().any(|path| path.starts_with(directory))
}

fn remove_directory(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub fn cleanup(root: &Path, paths: &[PathBuf]) -> io::Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(io::Error::other("La caché no es una carpeta")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let _root_lock = match lock(root, libc::LOCK_EX) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    let entries = match fs::read_dir(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    for entry in entries {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(PREFIX.as_bytes())
            && entry.file_type()?.is_dir()
            && !referenced(&entry.path(), paths)
        {
            let path = entry.path();
            match lock(&path, libc::LOCK_EX | libc::LOCK_NB) {
                Ok(_guard) => remove_directory(&path)?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::NotFound
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_unique_private_directories() {
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path().join("staging");
        let first = create(&root).unwrap();
        let second = create(&root).unwrap();
        assert_ne!(first.path(), second.path());
        for path in [&root, first.path(), second.path()] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn retains_only_referenced_staging_at_startup() {
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path();
        let retained = || {
            let directory = create(root).unwrap();
            directory.keep();
            directory.path().to_owned()
        };
        let referenced = retained();
        let expired = retained();
        let live = create(root).unwrap();
        let unrelated = root.join("unrelated");
        fs::create_dir(&unrelated).unwrap();
        cleanup(root, &[referenced.join("niño #.txt")]).unwrap();
        assert!(referenced.exists());
        assert!(!expired.exists());
        assert!(unrelated.exists());
        assert!(live.path().exists());
        cleanup(root, &[]).unwrap();
        assert!(!referenced.exists());
    }

    #[test]
    fn replacing_staging_removes_the_previous_copy() {
        let cache = tempfile::tempdir().unwrap();
        let mut current = create(cache.path()).unwrap();
        let previous = current.path().to_owned();
        current = create(cache.path()).unwrap();
        assert!(!previous.exists());
        assert!(current.path().exists());
    }
}
