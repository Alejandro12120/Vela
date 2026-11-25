use std::{
    ffi::OsString,
    io,
    path::Component,
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use futures_lite::StreamExt;
use openssh_sftp_client::{Sftp, SftpOptions, metadata::MetaData};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};

use crate::{
    Error,
    ssh::{self, Master, Process},
};

const CHUNK_SIZE: usize = 64 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug)]
pub struct Entry {
    pub name: OsString,
    pub kind: EntryKind,
    pub symlink_target_kind: Option<EntryKind>,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub mode: Option<u32>,
}

fn kind(meta: &MetaData) -> EntryKind {
    match meta.file_type() {
        Some(kind) if kind.is_file() => EntryKind::File,
        Some(kind) if kind.is_dir() => EntryKind::Directory,
        Some(kind) if kind.is_symlink() => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

pub(crate) fn entry(name: OsString, meta: MetaData) -> Entry {
    Entry {
        name,
        kind: kind(&meta),
        symlink_target_kind: None,
        size: meta.len(),
        mtime: meta.modified().map(|time| time.as_system_time()),
        mode: meta.permissions().map(|mode| mode.as_raw().bits()),
    }
}

impl Entry {
    pub fn is_directory(&self) -> bool {
        self.kind == EntryKind::Directory || self.symlink_target_kind == Some(EntryKind::Directory)
    }
}

pub(crate) async fn open(master: &Master) -> Result<(Sftp, Process), Error> {
    let mut process = Process::spawn(
        ssh::command(
            &master.spec,
            &master.askpass,
            &["-o", "ControlMaster=no", "-s"],
            &["sftp"],
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped()),
    )?;
    let stdin = process.child.stdin.take().expect("piped stdin");
    let stdout = process.child.stdout.take().expect("piped stdout");
    let err = match timeout(
        ssh::CONNECT_TIMEOUT,
        // File reads and writes are issued serially. Flush each promptly so the
        // client can receive its reply and issue the next request.
        Sftp::new(
            stdin,
            stdout,
            SftpOptions::default().flush_interval(Duration::ZERO),
        ),
    )
    .await
    {
        Ok(Ok(sftp)) => return Ok((sftp, process)),
        Ok(Err(err)) => err.to_string(),
        Err(_) => "Timed out starting the SFTP subsystem".into(),
    };
    process.stop().await?;
    let stderr = process.stderr().await;
    Err(Error::Sftp(format!("{err}\n{stderr}")))
}

pub(crate) async fn close(sftp: Option<Sftp>, mut process: Process) -> io::Result<()> {
    if let Some(sftp) = sftp {
        let _ = timeout(CLOSE_TIMEOUT, sftp.close()).await;
    }
    process.stop().await?;
    Ok(())
}

fn listing_error(error: openssh_sftp_client::Error) -> Error {
    match error {
        openssh_sftp_client::Error::SftpError(_, message) if !message.get().0.is_empty() => {
            Error::Sftp(message.get().0.to_owned())
        }
        other => other.into(),
    }
}

pub(crate) async fn read_dir(sftp: &Sftp, path: &Path) -> Result<Vec<Entry>, Error> {
    let dir = sftp.fs().open_dir(path).await.map_err(listing_error)?;
    let mut reader = std::pin::pin!(dir.read_dir());
    let mut entries = Vec::new();
    while let Some(item) = reader.next().await {
        let item = item.map_err(listing_error)?;
        let name = item.filename();
        if name != Path::new(".") && name != Path::new("..") {
            if !matches!(name.components().next(), Some(Component::Normal(_)))
                || name.file_name() != Some(name.as_os_str())
            {
                return Err(Error::Sftp(
                    "Nombre de archivo inválido en el listado remoto".into(),
                ));
            }
            entries.push(entry(name.as_os_str().to_owned(), item.metadata()));
        }
    }
    Ok(entries)
}

pub(crate) async fn list_dir(sftp: &Sftp, path: &Path) -> Result<Vec<Entry>, Error> {
    let mut entries = read_dir(sftp, path).await?;
    // READDIR supplies link metadata. Follow just the links to determine whether
    // they can be expanded, keeping broken or inaccessible links in the result.
    let mut fs = sftp.fs();
    for entry in entries
        .iter_mut()
        .filter(|entry| entry.kind == EntryKind::Symlink)
    {
        entry.symlink_target_kind = fs
            .metadata(path.join(&entry.name))
            .await
            .ok()
            .map(|meta| kind(&meta));
    }
    Ok(entries)
}

pub(crate) async fn stat(sftp: &Sftp, path: &Path) -> Result<Entry, Error> {
    let mut fs = sftp.fs();
    let mut entry = entry(
        path.file_name().unwrap_or(path.as_os_str()).to_owned(),
        fs.symlink_metadata(path).await?,
    );
    if entry.kind == EntryKind::Symlink {
        entry.symlink_target_kind = fs.metadata(path).await.ok().map(|meta| kind(&meta));
    }
    Ok(entry)
}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) async fn download(
    sftp: &Sftp,
    remote: &Path,
    local: &Path,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<(), Error> {
    check_cancel(cancel)?;
    let source = sftp.open(remote).await?;
    check_cancel(cancel)?;
    let mut destination = fs::File::create(local).await?;
    let result = read_file(source, &mut destination, progress, cancel, None).await;
    drop(destination);
    if result.is_err() {
        fs::remove_file(local).await?;
    }
    result
}

pub(crate) async fn upload(
    sftp: &Sftp,
    local: &Path,
    remote: &Path,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<(), Error> {
    check_cancel(cancel)?;
    let mut source = fs::File::open(local).await?;
    check_cancel(cancel)?;
    let mut destination = sftp
        .options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(remote)
        .await?;
    let result = write_file(&mut source, &mut destination, progress, cancel).await;
    let closed = destination.close().await;
    result?;
    Ok(closed?)
}

pub(crate) async fn read_file(
    mut source: openssh_sftp_client::file::File,
    destination: &mut fs::File,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
    budget: Option<(u64, u64)>,
) -> Result<(), Error> {
    let mut done = 0u64;
    let mut buffer = Default::default();
    loop {
        check_cancel(cancel)?;
        let Some(chunk) = source.read(CHUNK_SIZE as u32, buffer).await? else {
            break;
        };
        done = done.saturating_add(chunk.len() as u64);
        if let Some((start, limit)) = budget {
            crate::transfer::check_size(start.saturating_add(done), limit)?;
        }
        destination.write_all(&chunk).await?;
        progress(done);
        buffer = chunk;
        buffer.clear();
    }
    destination.flush().await?;
    source.close().await?;
    check_cancel(cancel)
}

pub(crate) async fn write_file(
    source: &mut fs::File,
    destination: &mut openssh_sftp_client::file::File,
    progress: &(dyn Fn(u64) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut done = 0;
    loop {
        check_cancel(cancel)?;
        let len = source.read(&mut buffer).await?;
        if len == 0 {
            return Ok(());
        }
        destination.write_all(&buffer[..len]).await?;
        done += len as u64;
        progress(done);
    }
}
