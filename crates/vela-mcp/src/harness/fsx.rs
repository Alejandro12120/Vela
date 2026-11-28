use std::{
    ffi::OsString,
    fs::{DirBuilder, File, Metadata, OpenOptions, Permissions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

pub(super) fn resolve(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(target) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Refuse dangling links rather than replacing them with regular files.
            match path.symlink_metadata() {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(path.to_owned()),
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    device: u64,
    inode: u64,
    mode: u32,
}

impl Stamp {
    fn new(metadata: Metadata) -> io::Result<Self> {
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.permissions().mode() & 0o7777,
        })
    }
}

pub(super) struct Snapshot {
    pub bytes: Vec<u8>,
    stamp: Stamp,
    digest: [u8; 32],
}

pub(super) enum ReadResult {
    Stable(Option<Snapshot>),
    Changed,
}

pub(super) fn read(path: &Path) -> io::Result<ReadResult> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ReadResult::Stable(None));
        }
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(ReadResult::Changed),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a regular file.",
        ));
    }
    let stamp = Stamp::new(metadata)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if stamp != Stamp::new(file.metadata()?)? || stamp.len != bytes.len() as u64 {
        return Ok(ReadResult::Changed);
    }
    let digest = Sha256::digest(&bytes).into();
    Ok(ReadResult::Stable(Some(Snapshot {
        bytes,
        stamp,
        digest,
    })))
}

pub(super) enum WriteResult {
    Written { backup: Option<PathBuf> },
    Changed,
}

pub(super) fn write(
    path: &Path,
    target: &Path,
    original: Option<&Snapshot>,
    updated: &[u8],
    still_selected: impl FnOnce() -> bool,
) -> io::Result<WriteResult> {
    let parent = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700).create(parent)?;
    let directory = File::open(parent)?;
    // Serialize Vela writers, including those addressing a symlink to this directory.
    directory.lock()?;

    let updated = stage(
        target,
        updated,
        original.map_or(0o600, |snapshot| snapshot.stamp.mode),
    )?;
    let backup = original
        .map(|snapshot| stage(target, &snapshot.bytes, 0o600))
        .transpose()?;
    if !still_selected() || resolve(path)? != target {
        return Ok(WriteResult::Changed);
    }
    let unchanged = match (read(target)?, original) {
        (ReadResult::Stable(None), None) => true,
        (ReadResult::Stable(Some(current)), Some(original)) => {
            current.stamp == original.stamp && current.digest == original.digest
        }
        _ => false,
    };
    if !unchanged {
        return Ok(WriteResult::Changed);
    }

    let backup = if let Some(backup) = backup {
        let path = backup_path(target);
        backup.persist(&path)?;
        updated.persist(target)?;
        Some(path)
    } else {
        // Publish a new file only if the destination is still absent.
        match updated.persist_noclobber(target) {
            Ok(_) => (),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                return Ok(WriteResult::Changed);
            }
            Err(error) => return Err(error.error),
        }
        None
    };
    directory.sync_all()?;
    Ok(WriteResult::Written { backup })
}

fn stage(target: &Path, bytes: &[u8], mode: u32) -> io::Result<NamedTempFile> {
    let prefix = format!(
        ".{}.vela-tmp-{}-",
        target
            .file_name()
            .ok_or(io::ErrorKind::InvalidInput)?
            .to_string_lossy(),
        std::process::id()
    );
    let mut temp = tempfile::Builder::new()
        .prefix(&prefix)
        .tempfile_in(target.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    temp.write_all(bytes)?;
    temp.as_file()
        .set_permissions(Permissions::from_mode(mode))?;
    temp.as_file().sync_all()?;
    Ok(temp)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".vela.bak");
    name.into()
}
