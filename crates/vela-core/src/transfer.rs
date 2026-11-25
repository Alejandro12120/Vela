//! Recursive file transfers and the session worker's transfer messages.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use openssh_sftp_client::{Sftp, UnixTimeStamp, error::SftpErrorKind, metadata::MetaDataBuilder};
use tokio::fs;

use crate::{Entry, EntryKind, Error, local, sftp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub direction: Direction,
    pub sources: Vec<PathBuf>,
    pub destination: PathBuf,
}

pub type JobId = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Queued,
    Preparing,
    Running,
    Completed,
    CompletedWithErrors,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Totals {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum Event {
    Status(Status),
    Planned(Totals),
    SizeLimit(u64),
    File {
        index: usize,
        path: PathBuf,
    },
    Progress {
        bytes: u64,
    },
    Item {
        source: PathBuf,
        destination: Option<PathBuf>,
        result: ItemResult,
    },
    Conflict(Conflict),
    Finished(Summary),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ItemResult {
    Copied,
    Directory,
    Skipped(String),
    Failed(String),
    Incomplete(String),
}

#[derive(Debug)]
pub struct Summary {
    pub status: Status,
    pub totals: Totals,
    pub copied: usize,
    pub skipped: usize,
    pub errors: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Choice {
    Overwrite,
    Skip,
    Rename,
    Cancel,
}

#[derive(Clone, Copy, Debug)]
pub struct Answer {
    pub choice: Choice,
    pub apply_to_all: bool,
}

#[derive(Debug)]
pub struct Conflict {
    pub destination: PathBuf,
    pub source: Entry,
    pub existing: Entry,
    reply: async_channel::Sender<Answer>,
}

impl Conflict {
    pub fn new(
        destination: PathBuf,
        source: Entry,
        existing: Entry,
    ) -> (Self, async_channel::Receiver<Answer>) {
        let (reply, answer) = async_channel::bounded(1);
        (
            Self {
                destination,
                source,
                existing,
                reply,
            },
            answer,
        )
    }

    pub fn answer(self, answer: Answer) {
        let _ = self.reply.try_send(answer);
    }
}

#[derive(Clone)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    signal: async_channel::Sender<()>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.signal.close();
    }

    fn check(&self) -> Result<(), Error> {
        sftp::check_cancel(&self.flag)
    }
}

pub struct Job {
    pub id: JobId,
    pub events: async_channel::Receiver<Event>,
    pub cancel: Cancel,
}

pub(crate) struct Work {
    spec: Spec,
    staging_limit: Option<u64>,
    events: async_channel::Sender<Event>,
    cancellation: async_channel::Receiver<()>,
    pub(crate) cancel: Cancel,
}

impl Work {
    pub(crate) fn new(spec: Spec, staging_limit: Option<u64>) -> (Job, Self) {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let (events, receiver) = async_channel::unbounded();
        let (signal, cancellation) = async_channel::bounded(1);
        let cancel = Cancel {
            flag: Arc::new(AtomicBool::new(false)),
            signal,
        };
        let work = Self {
            spec,
            staging_limit,
            events,
            cancellation,
            cancel: cancel.clone(),
        };
        let job = Job {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            events: receiver,
            cancel,
        };
        work.emit(Event::Status(Status::Queued));
        (job, work)
    }

    fn emit(&self, event: Event) {
        let _ = self.events.try_send(event);
    }

    pub(crate) fn cancel_queued(self) {
        self.finish(Status::Cancelled, Totals::default(), 0, 0, 0);
    }

    fn finish(&self, status: Status, totals: Totals, copied: usize, skipped: usize, errors: usize) {
        self.emit(Event::Status(status));
        self.emit(Event::Finished(Summary {
            status,
            totals,
            copied,
            skipped,
            errors,
        }));
    }

    pub(crate) async fn run(self, sftp: &Sftp) {
        if self.cancel.check().is_err() {
            self.cancel_queued();
            return;
        }
        if self.spec.sources.is_empty() {
            self.finish(Status::Error, Totals::default(), 0, 0, 1);
            return;
        }

        self.emit(Event::Status(Status::Preparing));
        let mut errors = 0;
        let mut skipped = 0;
        let mut copied = 0;
        let mut totals = Totals::default();
        let mut plan = Vec::new();
        let mut roots = self.spec.sources.clone();
        roots.sort();
        roots.dedup();
        let mut covered = Vec::<PathBuf>::new();
        for source in roots {
            if self.cancel.check().is_err() {
                break;
            }
            if covered.iter().any(|path| source.starts_with(path)) {
                continue;
            }
            let Some(name) = source.file_name() else {
                errors += 1;
                self.emit(Event::Item {
                    source,
                    destination: None,
                    result: ItemResult::Failed("Ruta de origen inválida".into()),
                });
                continue;
            };
            let destination = self.spec.destination.join(name);
            let mut pending = vec![(source.clone(), destination)];
            while let Some((path, destination)) = pending.pop() {
                if self.cancel.check().is_err() {
                    break;
                }
                let result = match self.source(sftp, &path).await {
                    Ok(entry) if entry.kind == EntryKind::Directory => {
                        match self.children(sftp, &path).await {
                            Ok(children) => {
                                if path == source {
                                    covered.push(path.clone());
                                }
                                pending.extend(
                                    children
                                        .into_iter()
                                        .rev()
                                        .map(|name| (path.join(&name), destination.join(&name))),
                                );
                                plan.push((path, destination, entry));
                                continue;
                            }
                            Err(error) => ItemResult::Failed(error.to_string()),
                        }
                    }
                    Ok(entry) if entry.kind == EntryKind::File => {
                        totals.files += 1;
                        totals.bytes = totals.bytes.saturating_add(entry.size.unwrap_or(0));
                        plan.push((path, destination, entry));
                        continue;
                    }
                    Ok(entry) => {
                        let reason = match entry.symlink_target_kind {
                            Some(EntryKind::Directory) => "Omitido: enlace a carpeta",
                            None if entry.kind == EntryKind::Symlink => "Omitido: enlace roto",
                            _ => "Omitido: tipo de archivo no compatible",
                        };
                        ItemResult::Skipped(reason.into())
                    }
                    Err(error) => ItemResult::Failed(error.to_string()),
                };
                match result {
                    ItemResult::Skipped(_) => skipped += 1,
                    _ => errors += 1,
                }
                self.emit(Event::Item {
                    source: path,
                    destination: None,
                    result,
                });
            }
        }
        self.emit(Event::Planned(totals));
        if self.cancel.check().is_err() {
            self.finish(Status::Cancelled, totals, copied, skipped, errors);
            return;
        }
        if let Some(limit) = self.staging_limit
            && check_size(totals.bytes, limit).is_err()
        {
            self.emit(Event::SizeLimit(totals.bytes));
            self.finish(Status::Error, totals, 0, skipped, errors + 1);
            return;
        }
        if !plan.is_empty()
            && let Err(error) = self.prepare_destination(sftp).await
        {
            if self.cancel.check().is_err() {
                self.finish(Status::Cancelled, totals, copied, skipped, errors);
            } else {
                self.emit(Event::Item {
                    source: self.spec.destination.clone(),
                    destination: None,
                    result: ItemResult::Failed(error.to_string()),
                });
                self.finish(Status::Error, totals, copied, skipped, errors + 1);
            }
            return;
        }
        self.emit(Event::Status(Status::Running));
        let mut directories = HashMap::<PathBuf, Option<PathBuf>>::new();
        let mut policy = None;
        let mut bytes = 0u64;
        let mut index = 0;
        let mut directory_times = Vec::new();
        for (source, planned, entry) in plan {
            if self.cancel.check().is_err() {
                break;
            }
            let directory = entry.kind == EntryKind::Directory;
            let mtime = entry.mtime;
            if !directory {
                index += 1;
                self.emit(Event::File {
                    index,
                    path: source.clone(),
                });
            }
            let parent = planned.parent().unwrap_or(Path::new(""));
            let target = match directories.get(parent) {
                Some(Some(parent)) => {
                    Some(parent.join(planned.file_name().expect("planned child")))
                }
                Some(None) => None,
                None => Some(planned.clone()),
            };
            let result = match target {
                Some(target) => {
                    self.copy(sftp, &source, target, entry, &mut policy, &mut bytes)
                        .await
                }
                None => Ok((
                    None,
                    ItemResult::Skipped("Omitido: carpeta de destino no disponible".into()),
                )),
            };
            let (destination, result) = match result {
                Ok(item) => item,
                Err(Error::Cancelled) => break,
                Err(Error::SizeLimit(bytes)) => {
                    self.emit(Event::SizeLimit(bytes));
                    self.finish(Status::Error, totals, copied, skipped, errors + 1);
                    return;
                }
                Err(error) => (None, ItemResult::Failed(error.to_string())),
            };
            match &result {
                ItemResult::Copied => copied += 1,
                ItemResult::Directory => {}
                ItemResult::Skipped(_) => skipped += 1,
                ItemResult::Failed(_) | ItemResult::Incomplete(_) => errors += 1,
            }
            if self.staging_limit.is_some()
                && result == ItemResult::Directory
                && let (Some(path), Some(mtime)) = (&destination, mtime)
            {
                directory_times.push((path.clone(), mtime));
            }
            if directory {
                directories.insert(
                    planned,
                    destination
                        .clone()
                        .filter(|_| result == ItemResult::Directory),
                );
            }
            self.emit(Event::Item {
                source,
                destination,
                result,
            });
        }
        for (directory, mtime) in directory_times.into_iter().rev() {
            if self.cancel.check().is_err() {
                break;
            }
            let result = fs::File::open(&directory).await;
            let result = match result {
                Ok(file) => file.into_std().await.set_modified(mtime),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                errors += 1;
                self.emit(Event::Item {
                    source: directory,
                    destination: None,
                    result: ItemResult::Failed(error.to_string()),
                });
            }
        }
        let status = if self.cancel.check().is_err() {
            Status::Cancelled
        } else if errors == 0 {
            Status::Completed
        } else {
            Status::CompletedWithErrors
        };
        self.finish(status, totals, copied, skipped, errors);
    }

    async fn prepare_destination(&self, sftp: &Sftp) -> Result<(), Error> {
        let destination = &self.spec.destination;
        if self.spec.direction == Direction::Download {
            return Ok(fs::create_dir_all(destination).await?);
        }

        let mut fs = sftp.fs();
        let mut missing = Vec::new();
        let mut current = destination.as_path();
        loop {
            self.cancel.check()?;
            match fs.metadata(current).await {
                Ok(info) if info.file_type().is_some_and(|kind| kind.is_dir()) => break,
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "El destino no es una carpeta",
                    )
                    .into());
                }
                Err(openssh_sftp_client::Error::SftpError(SftpErrorKind::NoSuchFile, _)) => {
                    let Some(parent) = current.parent() else {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "La carpeta de destino no existe",
                        )
                        .into());
                    };
                    missing.push(current);
                    current = if parent.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        parent
                    };
                }
                Err(error) => return Err(error.into()),
            }
        }
        for path in missing.into_iter().rev() {
            self.cancel.check()?;
            fs.create_dir(path).await?;
        }
        Ok(())
    }

    async fn source(&self, sftp: &Sftp, path: &Path) -> Result<Entry, Error> {
        match self.spec.direction {
            Direction::Upload => {
                let info = fs::symlink_metadata(path).await?;
                if info.is_symlink() {
                    match fs::metadata(path).await {
                        Ok(target) if target.is_dir() => {
                            let mut entry = local_entry(path, &info);
                            entry.symlink_target_kind = Some(EntryKind::Directory);
                            Ok(entry)
                        }
                        Ok(target) => Ok(local_entry(path, &target)),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            Ok(local_entry(path, &info))
                        }
                        Err(error) => Err(error.into()),
                    }
                } else {
                    Ok(local_entry(path, &info))
                }
            }
            Direction::Download => {
                let info = sftp.fs().symlink_metadata(path).await?;
                let name = path.file_name().unwrap_or(path.as_os_str()).to_owned();
                let mut entry = sftp::entry(name.clone(), info);
                if entry.kind == EntryKind::Symlink {
                    match sftp.fs().metadata(path).await {
                        Ok(target) => {
                            let target = sftp::entry(name, target);
                            if target.kind == EntryKind::Directory {
                                entry.symlink_target_kind = Some(EntryKind::Directory);
                            } else {
                                entry = target;
                            }
                        }
                        Err(openssh_sftp_client::Error::SftpError(
                            SftpErrorKind::NoSuchFile,
                            _,
                        )) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(entry)
            }
        }
    }

    async fn children(&self, sftp: &Sftp, path: &Path) -> Result<Vec<OsString>, Error> {
        let mut names = Vec::new();
        match self.spec.direction {
            Direction::Upload => {
                let mut dir = fs::read_dir(path).await?;
                while let Some(item) = dir.next_entry().await? {
                    self.cancel.check()?;
                    names.push(item.file_name());
                }
            }
            Direction::Download => {
                names.extend(
                    sftp::read_dir(sftp, path)
                        .await?
                        .into_iter()
                        .map(|entry| entry.name),
                );
            }
        }
        names.sort();
        Ok(names)
    }

    async fn existing(&self, sftp: &Sftp, path: &Path) -> Result<Option<Entry>, Error> {
        match self.spec.direction {
            Direction::Download => match fs::symlink_metadata(path).await {
                Ok(info) => Ok(Some(local_entry(path, &info))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            },
            Direction::Upload => remote_existing(sftp, path).await,
        }
    }

    async fn copy(
        &self,
        sftp: &Sftp,
        source: &Path,
        mut target: PathBuf,
        entry: Entry,
        policy: &mut Option<Choice>,
        bytes: &mut u64,
    ) -> Result<(Option<PathBuf>, ItemResult), Error> {
        let directory = entry.kind == EntryKind::Directory;
        let mut overwrite = None;
        let mtime = entry.mtime;
        let mode = entry.mode;
        if let Some(existing) = self.existing(sftp, &target).await? {
            if self.staging_limit.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Dos elementos tienen el mismo nombre en la caché local",
                )
                .into());
            }
            if directory && existing.kind == EntryKind::Directory {
                return Ok((Some(target), ItemResult::Directory));
            }
            let kind = existing.kind;
            let existing_mode = existing.mode;
            let choice = if let Some(choice) = *policy {
                choice
            } else {
                let (conflict, answer) = Conflict::new(target.clone(), entry, existing);
                self.emit(Event::Conflict(conflict));
                let answer = tokio::select! {
                    _ = self.cancellation.recv() => return Err(Error::Cancelled),
                    answer = answer.recv() => answer,
                };
                let answer = answer.unwrap_or(Answer {
                    choice: Choice::Cancel,
                    apply_to_all: false,
                });
                if answer.apply_to_all {
                    *policy = Some(answer.choice);
                }
                answer.choice
            };
            match choice {
                Choice::Cancel => {
                    self.cancel.cancel();
                    return Err(Error::Cancelled);
                }
                Choice::Skip => {
                    return Ok((None, ItemResult::Skipped("Omitido: ya existe".into())));
                }
                Choice::Rename => {
                    let name = target.file_name().expect("planned name").to_owned();
                    for index in 1.. {
                        self.cancel.check()?;
                        target.set_file_name(numbered_name(&name, index));
                        if self.existing(sftp, &target).await?.is_none() {
                            break;
                        }
                    }
                }
                Choice::Overwrite if !directory && kind == EntryKind::File => {
                    overwrite = Some(existing_mode)
                }
                Choice::Overwrite => {
                    return Err(io::Error::other(
                        "No se puede sobrescribir un elemento de otro tipo",
                    )
                    .into());
                }
            }
        }
        self.cancel.check()?;
        if directory {
            match self.spec.direction {
                Direction::Upload => sftp.fs().create_dir(&target).await?,
                Direction::Download if self.staging_limit.is_some() => {
                    fs::DirBuilder::new().mode(0o700).create(&target).await?;
                }
                Direction::Download => fs::create_dir(&target).await?,
            }
            return Ok((Some(target), ItemResult::Directory));
        }

        let start = *bytes;
        let progress = Mutex::new((0, Instant::now()));
        let report = |current| {
            let mut progress = progress.lock().expect("transfer progress");
            progress.0 = current;
            if progress.1.elapsed() >= Duration::from_millis(50) {
                progress.1 = Instant::now();
                self.emit(Event::Progress {
                    bytes: start.saturating_add(current),
                });
            }
        };
        let result = match self.spec.direction {
            Direction::Upload => {
                self.upload(
                    sftp,
                    source,
                    &target,
                    overwrite.is_some(),
                    mode,
                    mtime,
                    &report,
                )
                .await
            }
            Direction::Download => {
                self.download(
                    sftp, source, &target, overwrite, mode, mtime, &report, start,
                )
                .await
            }
        };
        *bytes = start.saturating_add(progress.into_inner().expect("transfer progress").0);
        self.emit(Event::Progress { bytes: *bytes });
        result.map(|result| (Some(target), result))
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload(
        &self,
        sftp: &Sftp,
        source: &Path,
        target: &Path,
        overwrite: bool,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<ItemResult, Error> {
        let mut source = fs::File::open(source).await?;
        let mut metadata = MetaDataBuilder::new();
        if let Some(mtime) = mtime {
            let time = UnixTimeStamp::new(mtime).map_err(io::Error::other)?;
            metadata.time(time, time);
        }
        if let Some(mode) = mode.filter(|_| !overwrite) {
            metadata.permissions(((mode & 0o777) as u16).into());
        }
        let part = if overwrite {
            target.to_path_buf()
        } else {
            partial_path(target, true)
        };
        self.cancel.check()?;
        let mut output = sftp
            .options()
            .write(true)
            .create(overwrite)
            .truncate(overwrite)
            .create_new(!overwrite)
            .open(&part)
            .await?;
        let result = async {
            sftp::write_file(&mut source, &mut output, progress, &self.cancel.flag).await?;
            self.cancel.check()?;
            Ok::<_, Error>(())
        }
        .await;
        let closed = output.close().await;
        let result = async {
            result?;
            closed?;
            self.cancel.check()?;
            if mode.is_some() && !overwrite || mtime.is_some() {
                sftp.fs().set_metadata(&part, metadata.create()).await?;
            }
            self.cancel.check()?;
            if !overwrite {
                ensure_remote_free(sftp, target).await?;
                sftp.fs().rename(&part, target).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        match result {
            Ok(()) => Ok(ItemResult::Copied),
            Err(error) if overwrite => Ok(ItemResult::Incomplete(format!(
                "Incompleto en destino: {error}"
            ))),
            Err(error) => {
                sftp.fs().remove_file(&part).await.map_err(|cleanup| {
                    io::Error::other(format!(
                        "{error}. No se pudo quitar {}: {cleanup}",
                        part.display()
                    ))
                })?;
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn download(
        &self,
        sftp: &Sftp,
        source: &Path,
        target: &Path,
        overwrite: Option<Option<u32>>,
        mode: Option<u32>,
        mtime: Option<SystemTime>,
        progress: &(dyn Fn(u64) + Send + Sync),
        start: u64,
    ) -> Result<ItemResult, Error> {
        let source = sftp.open(source).await?;
        let part = partial_path(target, false);
        self.cancel.check()?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&part)
            .await?;
        let mut partial = LocalPartial(Some(part));
        sftp::read_file(
            source,
            &mut output,
            progress,
            &self.cancel.flag,
            self.staging_limit.map(|limit| (start, limit)),
        )
        .await?;
        let output = output.into_std().await;
        if let Some(mode) = overwrite.unwrap_or(mode.map(|mode| mode & 0o777)) {
            output.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        if let Some(mtime) = mtime {
            output.set_modified(mtime)?;
        }
        drop(output);
        self.cancel.check()?;
        let part = partial.0.as_ref().expect("owned partial download");
        if overwrite.is_some() {
            std::fs::rename(part, target)?;
        } else {
            local::rename(part, target)?;
        }
        partial.0 = None;
        Ok(ItemResult::Copied)
    }
}

pub(crate) fn check_size(bytes: u64, limit: u64) -> Result<(), Error> {
    if bytes > limit {
        Err(Error::SizeLimit(bytes))
    } else {
        Ok(())
    }
}

struct LocalPartial(Option<PathBuf>);

impl Drop for LocalPartial {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn local_entry(path: &Path, info: &std::fs::Metadata) -> Entry {
    let kind = if info.is_file() {
        EntryKind::File
    } else if info.is_dir() {
        EntryKind::Directory
    } else if info.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    };
    Entry {
        name: path.file_name().unwrap_or(path.as_os_str()).into(),
        kind,
        symlink_target_kind: None,
        size: Some(info.len()),
        mtime: info.modified().ok(),
        mode: Some(info.mode() & 0o7777),
    }
}

pub(crate) async fn remote_existing(sftp: &Sftp, path: &Path) -> Result<Option<Entry>, Error> {
    match sftp.fs().symlink_metadata(path).await {
        Ok(info) => Ok(Some(sftp::entry(
            path.file_name().unwrap_or(path.as_os_str()).into(),
            info,
        ))),
        Err(openssh_sftp_client::Error::SftpError(SftpErrorKind::NoSuchFile, _)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) async fn ensure_remote_free(sftp: &Sftp, target: &Path) -> Result<(), Error> {
    if remote_existing(sftp, target).await?.is_some() {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Ya existe un elemento con ese nombre",
        )
        .into())
    } else {
        Ok(())
    }
}

pub(crate) async fn remove_remote_tree(sftp: &Sftp, path: PathBuf) -> Result<(), Error> {
    if path.file_name().is_none() || path.components().any(|part| part == Component::ParentDir) {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "Ruta inválida para eliminar").into(),
        );
    }
    let normalized: PathBuf = path.components().collect();
    let mut pending = vec![(normalized, false)];
    while let Some((path, visited)) = pending.pop() {
        let mut fs = sftp.fs();
        if visited {
            fs.remove_dir(&path).await?;
        } else if fs
            .symlink_metadata(&path)
            .await?
            .file_type()
            .is_some_and(|kind| kind.is_dir())
        {
            let children = sftp::read_dir(sftp, &path).await?;
            pending.extend(
                std::iter::once((path.clone(), true)).chain(
                    children
                        .into_iter()
                        .map(|entry| (path.join(entry.name), false)),
                ),
            );
        } else {
            fs.remove_file(&path).await?;
        }
    }
    Ok(())
}

fn partial_path(target: &Path, hidden: bool) -> PathBuf {
    let mut name = OsString::from(if hidden { "." } else { "" });
    name.push(target.file_name().expect("planned name"));
    name.push(".vela-part");
    target.with_file_name(name)
}

/// Preserve the last extension, treating a leading dot as part of the basename.
pub fn numbered_name(name: &OsStr, index: usize) -> OsString {
    let path = Path::new(name);
    let mut numbered = path.file_stem().unwrap_or(name).to_owned();
    numbered.push(format!(" ({index})"));
    if let Some(extension) = path.extension() {
        numbered.push(".");
        numbered.push(extension);
    }
    numbered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_size_guard_accepts_the_limit() {
        assert!(check_size(2_000_000_000, 2_000_000_000).is_ok());
        assert!(matches!(
            check_size(2_000_000_001, 2_000_000_000),
            Err(Error::SizeLimit(2_000_000_001))
        ));
    }

    #[test]
    fn numbers_names_without_losing_the_extension() {
        for (input, expected) in [
            ("nombre.txt", "nombre (2).txt"),
            ("archivo.tar.gz", "archivo.tar (2).gz"),
            ("README", "README (2)"),
            (".env", ".env (2)"),
            (".env.local", ".env (2).local"),
        ] {
            assert_eq!(numbered_name(OsStr::new(input), 2), expected);
        }
    }
}
