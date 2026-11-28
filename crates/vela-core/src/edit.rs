//! Private working copies and upload-on-save policy.

use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, DirBuilder, File, Metadata, OpenOptions, Permissions},
    io::{self, Read, Seek, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    thread,
    time::{Duration, Instant, SystemTime},
};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};

use crate::{Entry, EntryKind, Error, Session, config::Server};

pub const CONFIRM_SIZE: u64 = 100_000_000;
pub const DEBOUNCE: Duration = Duration::from_millis(300);
const STAT_INTERVAL: Duration = Duration::from_millis(50);
const INDEX: &str = "edits.toml";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Target {
    pub server_id: String,
    host: String,
    port: u16,
    user: String,
    pub remote_path: PathBuf,
}

impl Target {
    pub fn new(server: &Server, remote_path: PathBuf) -> Self {
        Self {
            server_id: server.id.clone(),
            host: server.host.clone(),
            port: server.port,
            user: server.user.clone(),
            remote_path,
        }
    }

    pub fn matches(&self, server: &Server) -> bool {
        self.server_id == server.id
            && self.host == server.host
            && self.port == server.port
            && self.user == server.user
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub mtime: SystemTime,
}

impl Stamp {
    pub(crate) fn remote(entry: &Entry) -> Option<Self> {
        if entry.kind != EntryKind::File {
            return None;
        }
        Some(Self {
            size: entry.size?,
            mtime: entry.mtime?,
        })
    }

    fn local(meta: &Metadata) -> io::Result<Self> {
        Ok(Self {
            size: meta.len(),
            mtime: meta.modified()?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Baseline {
    pub remote: Stamp,
    pub local: Stamp,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum State {
    #[serde(rename = "sincronizado")]
    Synced,
    #[serde(rename = "pendiente")]
    Pending,
    #[serde(rename = "conflicto")]
    Conflict,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Edit {
    pub target: Target,
    pub local_path: PathBuf,
    canonical_remote: PathBuf,
    pub baseline: Baseline,
    pub state: State,
}

impl Edit {
    pub fn canonical_remote_path(&self) -> &Path {
        &self.canonical_remote
    }

    /// Also detects a local save before the directory watch has processed it.
    pub fn has_local_changes(&self) -> bool {
        self.state != State::Synced
            || fs::metadata(&self.local_path)
                .and_then(|metadata| Stamp::local(&metadata))
                .is_ok_and(|stamp| stamp != self.baseline.local)
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Records {
    edits: Vec<Edit>,
}

pub struct Index {
    dir: PathBuf,
    root: PathBuf,
    records: Records,
    _lock: File,
}

fn private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other("The cache path isn't a folder"));
    }
    fs::set_permissions(path, Permissions::from_mode(0o700))
}

fn valid_component(id: &str) -> bool {
    matches!(Path::new(id).components().next(), Some(Component::Normal(name)) if name == id)
        && id != ".lock"
}

pub fn default_root(app_name: &str) -> io::Result<PathBuf> {
    std::env::home_dir()
        .map(|home| home.join("Library/Caches").join(app_name).join("edit"))
        .ok_or_else(|| io::Error::other("Cache folder not found"))
}

fn lock_root(root: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(".lock"))?;
    lock.try_lock().map_err(io::Error::from)?;
    Ok(lock)
}

impl Records {
    fn load(dir: &Path) -> io::Result<Self> {
        match fs::read_to_string(dir.join(INDEX)) {
            Ok(text) => {
                toml::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    fn save(&self, dir: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        crate::config::save_private(dir, INDEX, &text)
    }

    fn directories(&self, root: &Path) -> io::Result<HashSet<&Path>> {
        let mut targets = HashSet::new();
        let mut directories = HashSet::new();
        for edit in &self.edits {
            if edit.relative_path(root).is_none()
                || !targets.insert(&edit.target)
                || !directories.insert(edit.local_path.parent().unwrap())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid or duplicate edit path",
                ));
            }
        }
        Ok(directories)
    }
}

impl Edit {
    fn relative_path(&self, root: &Path) -> Option<&Path> {
        let relative = self.local_path.strip_prefix(root).ok()?;
        let path = relative.strip_prefix(&self.target.server_id).ok()?;
        let mut parts = path.components();
        (valid_component(&self.target.server_id)
            && self.target.remote_path.is_absolute()
            && self.canonical_remote.is_absolute()
            && matches!(parts.next(), Some(Component::Normal(id)) if id.as_encoded_bytes().starts_with(b"edit-"))
            && matches!(parts.next(), Some(Component::Normal(_)))
            && parts.next().is_none()
            && self.local_path.file_name() == self.target.remote_path.file_name())
            .then_some(relative)
    }
}

/// Rebase working copies without opening an Index, which would clean up files
/// before they can be matched with their original records.
pub(crate) fn migrate_index(
    legacy: &Path,
    target: &Path,
    root: &Path,
    legacy_cache: Option<&Path>,
) -> io::Result<bool> {
    let legacy_inline = legacy.join("edit-cache");
    let target_inline = target.join("edit-cache");
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let old_roots: Vec<_> = [
        Some(legacy_inline.as_path()),
        legacy_cache,
        Some(target_inline.as_path()),
    ]
    .into_iter()
    .flatten()
    .map(|path| (path, canonical(path)))
    .collect();
    let mut current = Records::load(target)?;
    let imported = Records::load(legacy)?;
    let current_root = canonical(root);
    if imported.edits.is_empty()
        && current
            .edits
            .iter()
            .all(|edit| edit.relative_path(&current_root).is_some())
    {
        return Ok(false);
    }

    private_dir(root)?;
    let root = root.canonicalize()?;
    let mut locked = HashSet::from([root.clone()]);
    let _target_lock = lock_root(&root)?;
    let mut old_locks = Vec::new();
    for (_, path) in &old_roots {
        if path.is_dir() && locked.insert(path.clone()) {
            old_locks.push(lock_root(path)?);
        }
    }

    // Re-read after taking the same locks used by live edit indexes.
    current = Records::load(target)?;
    let imported = Records::load(legacy)?;
    let mut merged = Records::default();
    for mut edit in current.edits.into_iter().chain(imported.edits) {
        if edit.relative_path(&root).is_none() {
            let (original_root, source_root, relative) = old_roots
                .iter()
                .find_map(|(original, canonical)| {
                    edit.relative_path(original)
                        .or_else(|| edit.relative_path(canonical))
                        .map(|relative| (*original, canonical, relative))
                })
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Invalid legacy edit path")
                })?;
            let mut source = source_root.join(relative);
            if original_root == legacy_inline && !source.exists() {
                // Recover files copied to the Vela configuration directory by
                // the previous startup migration.
                source = canonical(&target_inline).join(relative);
            }
            let destination = root.join(relative);
            let destination_dir = destination.parent().unwrap();
            private_dir(destination_dir.parent().unwrap())?;
            private_dir(destination_dir)?;
            for directory in [source.parent().unwrap(), destination_dir] {
                if directory.canonicalize()? != directory {
                    return Err(io::Error::other("Edit cache contains a linked directory"));
                }
            }
            if source != destination {
                if destination.exists() {
                    if read_content(&source, io::sink())?.1
                        != read_content(&destination, io::sink())?.1
                    {
                        return Err(io::Error::other("Working copies conflict during migration"));
                    }
                } else {
                    crate::config::copy_file(&source, &destination)?;
                }
            }
            edit.local_path = destination;
        }
        if let Some(existing) = merged
            .edits
            .iter()
            .find(|existing| existing.target == edit.target)
        {
            if existing.local_path != edit.local_path || existing.baseline != edit.baseline {
                return Err(io::Error::other("Edit records conflict during migration"));
            }
        } else {
            merged.edits.push(edit);
        }
    }
    merged.directories(&root)?;
    merged.save(target)?;
    Ok(true)
}

impl Index {
    pub fn load(dir: PathBuf, root: PathBuf) -> io::Result<Self> {
        private_dir(&root)?;
        let root = root.canonicalize()?;
        let lock = lock_root(&root)?;
        let records = Records::load(&dir)?;
        let directories = records.directories(&root)?;
        for server in fs::read_dir(&root)? {
            let server = server?;
            if !server.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(server.path())? {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && entry.file_name().as_encoded_bytes().starts_with(b"edit-")
                    && !directories.contains(entry.path().as_path())
                {
                    fs::remove_dir_all(entry.path())?;
                }
            }
        }
        Ok(Self {
            dir,
            root,
            records,
            _lock: lock,
        })
    }

    pub fn edits(&self) -> &[Edit] {
        &self.records.edits
    }

    pub fn get(&self, target: &Target) -> Option<&Edit> {
        self.edits().iter().find(|edit| &edit.target == target)
    }

    pub fn directory(&self, target: &Target) -> io::Result<TempDir> {
        if !valid_component(&target.server_id) {
            return Err(io::Error::other("Invalid server ID"));
        }
        let parent = self.root.join(&target.server_id);
        private_dir(&parent)?;
        tempfile::Builder::new()
            .prefix("edit-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(parent)
    }

    fn save(&self) -> io::Result<()> {
        self.records.save(&self.dir)
    }

    pub fn put(&mut self, edit: Edit) -> io::Result<()> {
        if let Some(index) = self
            .records
            .edits
            .iter()
            .position(|row| row.target == edit.target)
        {
            let previous = std::mem::replace(&mut self.records.edits[index], edit);
            if let Err(error) = self.save() {
                self.records.edits[index] = previous;
                return Err(error);
            }
        } else {
            self.records.edits.push(edit);
            if let Err(error) = self.save() {
                self.records.edits.pop();
                return Err(error);
            }
        }
        Ok(())
    }

    /// Keep an edit with newer bytes even if its last persisted state is synced.
    pub fn remove(&mut self, target: &Target, discard: bool) -> io::Result<bool> {
        let Some(i) = self
            .records
            .edits
            .iter()
            .position(|row| &row.target == target)
        else {
            return Ok(false);
        };
        let row = &self.records.edits[i];
        if !discard
            && (row.state != State::Synced
                || !matches!(content(&row.local_path), Ok((_, hash)) if hash == row.baseline.sha256))
        {
            return Ok(false);
        }
        let parent = row.local_path.parent().unwrap();
        match parent.canonicalize() {
            Ok(path) if path == parent => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(io::Error::other("The local copy's path has changed")),
        }
        // Remove the bytes before dropping the record. A write failure leaves
        // the record available for retry and startup recovery.
        match fs::remove_dir_all(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let removed = self.records.edits.remove(i);
        if let Err(error) = self.save() {
            self.records.edits.insert(i, removed);
            return Err(error);
        }
        Ok(true)
    }

    pub fn cleanup_synced(&mut self) -> io::Result<()> {
        let targets: Vec<_> = self
            .edits()
            .iter()
            .filter(|edit| edit.state == State::Synced)
            .map(|edit| edit.target.clone())
            .collect();
        for target in targets {
            if !self.remove(&target, false)? {
                let mut row = self.get(&target).unwrap().clone();
                row.state = State::Pending;
                self.put(row)?;
            }
        }
        Ok(())
    }
}

pub struct DirectoryWatch {
    watcher: RecommendedWatcher,
}

impl DirectoryWatch {
    pub fn new() -> notify::Result<(Self, async_channel::Receiver<notify::Result<Event>>)> {
        let (send, events) = async_channel::unbounded();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = send.try_send(event);
        })?;
        Ok((Self { watcher }, events))
    }

    pub fn add(&mut self, file: &Path) -> notify::Result<()> {
        self.watcher
            .watch(file.parent().unwrap(), RecursiveMode::NonRecursive)
    }

    pub fn remove(&mut self, file: &Path) -> notify::Result<()> {
        self.watcher.unwatch(file.parent().unwrap())
    }

    pub fn affects(event: &Event, file: &Path) -> bool {
        event.need_rescan()
            || (!event.kind.is_access()
                && event
                    .paths
                    .iter()
                    .any(|path| file.starts_with(path) || file.parent() == path.parent()))
    }
}

/// The caller supplies the clock so debounce and in-flight coalescing are testable.
#[derive(Default)]
pub struct Schedule {
    due: Option<Instant>,
    active: bool,
}

impl Schedule {
    pub fn changed(&mut self, now: Instant) {
        self.due = Some(now + DEBOUNCE);
    }

    pub fn delay(&self, now: Instant) -> Option<Duration> {
        self.due
            .filter(|_| !self.active)
            .map(|due| due.saturating_duration_since(now))
    }

    pub fn begin(&mut self, now: Instant, immediate: bool) -> bool {
        if self.active || (!immediate && self.delay(now) != Some(Duration::ZERO)) {
            return false;
        }
        self.active = true;
        self.due = None;
        true
    }

    pub fn finish(&mut self) {
        self.active = false;
    }

    pub fn busy(&self) -> bool {
        self.active || self.due.is_some()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LocalVersion {
    stamp: Stamp,
    device: u64,
    inode: u64,
    ctime: (i64, i64),
}

fn version(meta: &Metadata) -> io::Result<LocalVersion> {
    if !meta.is_file() {
        return Err(io::Error::other("The local copy isn't a file"));
    }
    Ok(LocalVersion {
        stamp: Stamp::local(meta)?,
        device: meta.dev(),
        inode: meta.ino(),
        ctime: (meta.ctime(), meta.ctime_nsec()),
    })
}

fn read_content(path: &Path, mut copy: impl Write) -> io::Result<(LocalVersion, String)> {
    let expected = version(&fs::symlink_metadata(path)?)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if version(&file.metadata()?)? != expected {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        copy.write_all(&buffer[..n])?;
    }
    if version(&fs::symlink_metadata(path)?)? != expected {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Ok((expected, format!("{:x}", hash.finalize())))
}

fn content(path: &Path) -> io::Result<(Stamp, String)> {
    read_content(path, io::sink()).map(|(version, hash)| (version.stamp, hash))
}

pub struct Snapshot {
    file: File,
    local: Stamp,
    sha256: String,
}

impl Edit {
    /// Snapshot the bytes so later saves cannot change an in-flight upload.
    pub fn snapshot(&self) -> io::Result<Option<Snapshot>> {
        let path = &self.local_path;
        let metadata = fs::symlink_metadata(path)?;
        let first = version(&metadata)?;
        thread::sleep(STAT_INTERVAL);
        if version(&fs::symlink_metadata(path)?)? != first {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // Snapshot outside the watched directory. An unnamed file survives
        // only while the worker owns its descriptor.
        let mut output = tempfile::tempfile_in(path.parent().unwrap().parent().unwrap())?;
        let (latest, sha256) = read_content(path, &mut output)?;
        output.rewind()?;
        if first != latest {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if metadata.mode() & 0o777 != 0o600 {
            fs::set_permissions(path, Permissions::from_mode(0o600))?;
        }
        Ok((sha256 != self.baseline.sha256).then_some(Snapshot {
            file: output,
            local: latest.stamp,
            sha256,
        }))
    }
}

pub struct OpenRemote {
    canonical: PathBuf,
    pub stamp: Stamp,
}

impl OpenRemote {
    pub async fn inspect(session: &Session, target: &Target) -> Result<Self, Error> {
        let path = target.remote_path.clone();
        let stamp = Stamp::remote(&session.stat(path.clone()).await?).ok_or_else(|| {
            Error::Sftp("Editing requires a regular file with a modification time and size".into())
        })?;
        let canonical = session.realpath(path).await?;
        Ok(Self { canonical, stamp })
    }

    async fn download(&self, session: &Session, directory: &Path) -> Result<NamedTempFile, Error> {
        let staged = NamedTempFile::new_in(directory)?;
        session
            .download(
                self.canonical.clone(),
                staged.path().to_owned(),
                |_| {},
                Arc::new(AtomicBool::new(false)),
            )
            .await?;
        if Stamp::remote(&session.stat(self.canonical.clone()).await?) != Some(self.stamp)
            || staged.as_file().metadata()?.len() != self.stamp.size
        {
            return Err(Error::Sftp(
                "The remote file changed during download".into(),
            ));
        }
        Ok(staged)
    }

    pub async fn open(
        self,
        session: &Session,
        target: Target,
        directory: &Path,
    ) -> Result<Edit, Error> {
        let local_path = directory.join(
            target
                .remote_path
                .file_name()
                .ok_or_else(|| io::Error::other("The filename is missing"))?,
        );
        let staged = self.download(session, directory).await?;
        let (local, sha256) = content(staged.path())?;
        staged
            .persist_noclobber(&local_path)
            .map_err(|err| err.error)?;
        Ok(Edit {
            target,
            local_path,
            canonical_remote: self.canonical,
            baseline: Baseline {
                remote: self.stamp,
                local,
                sha256,
            },
            state: State::Synced,
        })
    }
}

pub enum Upload {
    Conflict(Option<Stamp>),
    Synced(Baseline),
}

impl Edit {
    pub async fn upload(
        &self,
        session: &Session,
        snapshot: Snapshot,
        expected: Stamp,
    ) -> Result<Upload, Error> {
        // Ownership of the snapshot stays with the worker until it closes the handle.
        session
            .upload_edit(
                self.target.remote_path.clone(),
                self.canonical_remote.clone(),
                snapshot,
                expected,
            )
            .await
    }

    pub async fn refresh(
        &self,
        session: &Session,
        remote: OpenRemote,
    ) -> Result<Option<Baseline>, Error> {
        if remote.stamp == self.baseline.remote && remote.canonical == self.canonical_remote {
            return Ok(None);
        }
        if remote.canonical != self.canonical_remote {
            return Err(Error::Sftp("The remote path has changed".into()));
        }
        let staged = remote
            .download(session, self.local_path.parent().unwrap())
            .await?;
        let (local, sha256) = content(staged.path())?;
        if content(&self.local_path)?.1 != self.baseline.sha256 {
            return Ok(None);
        }
        staged
            .persist(&self.local_path)
            .map_err(|error| error.error)?;
        Ok(Some(Baseline {
            remote: remote.stamp,
            local,
            sha256,
        }))
    }

    pub async fn save_remote_copy(&self, session: &Session) -> Result<PathBuf, Error> {
        let remote = OpenRemote::inspect(session, &self.target).await?;
        if remote.canonical != self.canonical_remote {
            return Err(Error::Sftp("The remote path has changed".into()));
        }
        let parent = self.local_path.parent().unwrap();
        let name = Path::new(self.local_path.file_name().unwrap());
        let mut copy = OsString::from(name.file_stem().unwrap());
        copy.push(" (remote)");
        if let Some(ext) = name.extension() {
            copy.push(".");
            copy.push(ext);
        }
        let mut staged = remote.download(session, parent).await?;
        for n in 1.. {
            let path = parent.join(if n == 1 {
                copy.clone()
            } else {
                crate::transfer::numbered_name(&copy, n)
            });
            match staged.persist_noclobber(&path) {
                Ok(_) => return Ok(path),
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                    staged = error.file
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        unreachable!()
    }
}

pub(crate) async fn upload(
    sftp: &openssh_sftp_client::Sftp,
    path: &Path,
    canonical: &Path,
    snapshot: Snapshot,
    expected: Stamp,
    cancel: &AtomicBool,
) -> Result<Upload, Error> {
    let mut source = tokio::fs::File::from_std(snapshot.file);
    let mut sftp_fs = sftp.fs();
    let stamp = crate::transfer::remote_existing(sftp, path)
        .await?
        .as_ref()
        .and_then(Stamp::remote);
    if stamp != Some(expected) || sftp_fs.canonicalize(path).await? != canonical {
        return Ok(Upload::Conflict(stamp));
    }
    crate::sftp::check_cancel(cancel)?;
    // Open the existing inode without truncating. Verify the handle as well
    // as the path before setting its length, then reuse the F1 stream writer.
    let mut destination = sftp
        .options()
        .read(true)
        .write(true)
        .open(canonical)
        .await?;
    let metadata = destination.metadata().await?;
    let actual = Stamp::remote(&crate::sftp::entry(OsString::new(), metadata));
    if actual != Some(expected) {
        destination.close().await?;
        return Ok(Upload::Conflict(actual));
    }
    crate::sftp::check_cancel(cancel)?;
    destination.set_len(0).await?;
    let result =
        crate::sftp::write_file(&mut source, &mut destination, &|_| {}, cancel, false).await;
    let written = destination.metadata().await;
    let closed = destination.close().await;
    result?;
    closed?;
    let written = Stamp::remote(&crate::sftp::entry(OsString::new(), written?));
    let remote = Stamp::remote(&crate::sftp::stat(sftp, canonical).await?)
        .filter(|remote| remote.size == snapshot.local.size && Some(*remote) == written)
        .ok_or_else(|| Error::Sftp("The remote file changed during upload".into()))?;
    Ok(Upload::Synced(Baseline {
        remote,
        local: snapshot.local,
        sha256: snapshot.sha256,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounces_and_separates_saves() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        schedule.changed(start);
        schedule.changed(start + Duration::from_millis(100));
        assert!(!schedule.begin(start + DEBOUNCE, false));
        assert!(schedule.begin(start + Duration::from_millis(400), false));
        schedule.finish();
        assert!(!schedule.begin(start + Duration::from_millis(500), false));

        schedule.changed(start + Duration::from_millis(850));
        assert!(schedule.begin(start + Duration::from_millis(1150), false));
        schedule.finish();
        assert!(!schedule.busy());
    }

    #[test]
    fn coalesces_saves_while_an_upload_is_active() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        assert!(schedule.begin(start, true));
        schedule.changed(start + Duration::from_millis(100));
        schedule.changed(start + Duration::from_millis(200));
        assert!(!schedule.begin(start + Duration::from_millis(600), true));
        assert_eq!(schedule.delay(start + Duration::from_millis(600)), None);
        schedule.finish();
        assert!(schedule.begin(start + Duration::from_millis(600), false));
        schedule.finish();
        assert!(!schedule.busy());
    }

    fn local_edit(index: &Index, file: &str, state: State) -> Edit {
        let target = Target::new(
            &Server {
                id: "test".into(),
                name: "Test".into(),
                host: "127.0.0.1".into(),
                port: 22,
                user: "tester".into(),
                auth: crate::Auth::Password,
                initial_remote_path: None,
                pinned: false,
            },
            Path::new("/remote").join(file),
        );
        let local_path = index.directory(&target).unwrap().keep().join(file);
        fs::write(&local_path, b"original").unwrap();
        let (local, sha256) = content(&local_path).unwrap();
        Edit {
            canonical_remote: target.remote_path.clone(),
            target,
            local_path,
            baseline: Baseline {
                remote: local,
                local,
                sha256,
            },
            state,
        }
    }

    #[test]
    fn migration_rebases_pending_inline_edits_and_keeps_target_edits() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("vscp");
        let target = temp.path().join("vela");
        let root = target.join("edit-cache");
        let mut old = Index::load(legacy.clone(), legacy.join("edit-cache")).unwrap();
        let pending = local_edit(&old, "pending.txt", State::Pending);
        old.put(pending.clone()).unwrap();
        fs::write(&pending.local_path, b"pending changes").unwrap();
        let mut current = Index::load(target.clone(), root.clone()).unwrap();
        let synced = local_edit(&current, "synced.txt", State::Synced);
        current.put(synced.clone()).unwrap();
        drop(current);

        assert!(crate::config::migrate_directory(&legacy, &target).is_err());
        assert!(
            legacy.exists(),
            "a live legacy index must keep its working copies"
        );
        drop(old);
        assert!(crate::config::migrate_directory(&legacy, &target).unwrap());
        assert!(!legacy.exists());

        let current = Index::load(target, root).unwrap();
        assert_eq!(current.edits()[0], synced);
        let migrated = current.get(&pending.target).unwrap();
        assert_eq!(migrated.baseline, pending.baseline);
        assert_eq!(migrated.state, State::Pending);
        assert!(migrated.snapshot().unwrap().is_some());
        assert_eq!(fs::read(&migrated.local_path).unwrap(), b"pending changes");
    }

    #[test]
    fn migration_repairs_records_from_an_already_moved_config() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("vscp");
        let target = temp.path().join("vela");
        let legacy_root = temp.path().join("old-cache");
        let root = temp.path().join("new-cache");
        let mut old = Index::load(legacy.clone(), legacy_root.clone()).unwrap();
        let conflict = local_edit(&old, "conflict.txt", State::Conflict);
        old.put(conflict.clone()).unwrap();
        drop(old);
        fs::rename(&legacy, &target).unwrap();

        assert!(migrate_index(&legacy, &target, &root, Some(&legacy_root)).unwrap());
        assert!(!migrate_index(&legacy, &target, &root, Some(&legacy_root)).unwrap());
        let current = Index::load(target, root).unwrap();
        let migrated = current.get(&conflict.target).unwrap();
        assert_eq!(migrated.state, State::Conflict);
        assert_eq!(migrated.baseline, conflict.baseline);
        assert_ne!(migrated.local_path, conflict.local_path);
        assert_eq!(fs::read(&migrated.local_path).unwrap(), b"original");
    }

    #[test]
    fn skips_metadata_changes_but_detects_bytes_with_same_size_and_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let index = Index::load(temp.path().join("config"), temp.path().join("cache")).unwrap();
        let edit = local_edit(&index, "hello.txt", State::Synced);
        let file = File::options().write(true).open(&edit.local_path).unwrap();
        file.set_modified(SystemTime::now()).unwrap();
        assert!(edit.snapshot().unwrap().is_none());
        fs::write(&edit.local_path, b"modified").unwrap();
        file.set_modified(edit.baseline.local.mtime).unwrap();
        assert!(edit.snapshot().unwrap().is_some());
    }

    #[test]
    fn index_round_trip_and_cleanup_preserve_unsent_copies() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("config");
        let root = temp.path().join("cache");
        let mut index = Index::load(dir.clone(), root.clone()).unwrap();
        let synced = local_edit(&index, "synced.txt", State::Synced);
        let unobserved = local_edit(&index, "changed.txt", State::Synced);
        let pending = local_edit(&index, "pending.txt", State::Pending);
        let conflict = local_edit(&index, "conflict.txt", State::Conflict);
        let orphan = local_edit(&index, "orphan.txt", State::Synced);
        for row in [&synced, &unobserved, &pending, &conflict] {
            index.put(row.clone()).unwrap();
        }
        fs::write(&unobserved.local_path, b"unsaved by watcher").unwrap();
        assert_eq!(fs::metadata(dir.join(INDEX)).unwrap().mode() & 0o777, 0o600);
        assert!(
            Index::load(dir.clone(), root.clone()).is_err(),
            "cache must have a single writer"
        );
        drop(index);
        let mut index = Index::load(dir, root).unwrap();
        assert_eq!(
            index.edits(),
            [
                synced.clone(),
                unobserved.clone(),
                pending.clone(),
                conflict.clone()
            ]
        );
        assert!(!orphan.local_path.exists());
        index.cleanup_synced().unwrap();
        assert!(!synced.local_path.exists());
        assert_eq!(index.get(&unobserved.target).unwrap().state, State::Pending);
        assert_eq!(
            fs::read(&unobserved.local_path).unwrap(),
            b"unsaved by watcher"
        );
        for row in [&pending, &conflict] {
            assert!(row.local_path.is_file());
            assert!(!index.remove(&row.target, false).unwrap());
        }
        assert!(index.remove(&conflict.target, true).unwrap());
        assert!(!conflict.local_path.exists());
    }

    #[test]
    fn remote_stamp_detects_size_time_and_kind_changes() {
        let mut entry = Entry {
            name: "hello.txt".into(),
            kind: EntryKind::File,
            symlink_target_kind: None,
            size: Some(12),
            mtime: Some(SystemTime::UNIX_EPOCH),
            mode: Some(0o640),
        };
        let original = Stamp::remote(&entry).unwrap();
        entry.size = Some(13);
        assert_ne!(Stamp::remote(&entry), Some(original));
        entry.size = Some(12);
        entry.mtime = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        assert_ne!(Stamp::remote(&entry), Some(original));
        entry.mtime = Some(SystemTime::UNIX_EPOCH);
        assert_eq!(Stamp::remote(&entry), Some(original));
        entry.kind = EntryKind::Symlink;
        assert_eq!(Stamp::remote(&entry), None);
    }
}
