use std::{
    ffi::OsString,
    io,
    path::Component,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};

use futures_lite::StreamExt;
use openssh_sftp_client::{Sftp, SftpOptions, error::SftpErrorKind, metadata::MetaData};
use tokio::{
    fs,
    io::{self as tokio_io, AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    process::Command,
    task::JoinSet,
    time::timeout,
};

use crate::{
    Error,
    ssh::{self, ConnectionSpec, Master, Process},
};

const CHUNK_SIZE: usize = 64 * 1024;
const BULK_CHUNK_SIZE: usize = 3 * CHUNK_SIZE;
pub(crate) const METADATA_CONCURRENCY: usize = 32;
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
        // Flush lone requests promptly and allow concurrent requests to share
        // the outgoing buffer.
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
        openssh_sftp_client::Error::SftpError(SftpErrorKind::Failure, message)
            if !message.get().0.is_empty() =>
        {
            Error::Sftp(message.get().0.to_owned())
        }
        other => other.into(),
    }
}

pub(crate) async fn read_dir(sftp: &Sftp, path: &Path) -> Result<Vec<Entry>, Error> {
    let mut fs = sftp.fs();
    let dir = match fs.open_dir(path).await {
        Ok(dir) => dir,
        Err(error) => {
            if ambiguous_path_error(&error)
                && fs
                    .metadata(path)
                    .await
                    .is_ok_and(|info| info.file_type().is_some_and(|kind| !kind.is_dir()))
            {
                return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
            }
            return Err(listing_error(error));
        }
    };
    let mut reader = std::pin::pin!(dir.read_dir());
    let mut entries = Vec::new();
    while let Some(item) = reader.next().await {
        let item = item.map_err(listing_error)?;
        let name = item.filename();
        if name != Path::new(".") && name != Path::new("..") {
            if !matches!(name.components().next(), Some(Component::Normal(_)))
                || name.file_name() != Some(name.as_os_str())
            {
                return Err(Error::Sftp("Invalid filename in the remote listing".into()));
            }
            entries.push(entry(name.as_os_str().to_owned(), item.metadata()));
        }
    }
    Ok(entries)
}

fn ambiguous_path_error(error: &openssh_sftp_client::Error) -> bool {
    matches!(
        error,
        openssh_sftp_client::Error::SftpError(
            SftpErrorKind::Failure | SftpErrorKind::NoSuchFile,
            _,
        )
    )
}

pub(crate) async fn remove_dir(sftp: &Sftp, path: &Path) -> Result<(), Error> {
    let mut fs = sftp.fs();
    let Err(error) = fs.remove_dir(path).await else {
        return Ok(());
    };
    // SFTP v3 has no DirectoryNotEmpty status. Check the failed RMDIR's
    // target without sending any unlink requests.
    if ambiguous_path_error(&error) {
        if let Ok(dir) = fs.open_dir(path).await {
            let mut entries = std::pin::pin!(dir.read_dir());
            while let Some(Ok(entry)) = entries.next().await {
                if !matches!(entry.filename().to_str(), Some("." | "..")) {
                    return Err(io::Error::from(io::ErrorKind::DirectoryNotEmpty).into());
                }
            }
        } else if fs
            .symlink_metadata(path)
            .await
            .is_ok_and(|info| info.file_type().is_some_and(|kind| !kind.is_dir()))
        {
            return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
        }
    }
    Err(error.into())
}

pub(crate) async fn rename_exclusive(
    sftp: &Sftp,
    spec: &ConnectionSpec,
    control_dir: &Path,
    from: PathBuf,
    to: PathBuf,
) -> Result<(), Error> {
    crate::transfer::ensure_remote_free(sftp, &to).await?;
    // The library picks the overwriting POSIX extension whenever advertised.
    // Open a channel that negotiates plain SFTP v3 for this operation only.
    let result = if sftp.support_posix_rename() {
        rename_v3(spec, control_dir, &from, &to).await
    } else {
        sftp.fs().rename(&from, &to).await.map_err(Error::from)
    };
    if matches!(result, Err(Error::Sftp(_)))
        && matches!(
            crate::transfer::remote_existing(sftp, &to).await,
            Ok(Some(_))
        )
    {
        // A competing writer can claim the destination between LSTAT and RENAME.
        return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
    }
    result
}

async fn rename_v3(
    spec: &ConnectionSpec,
    control_dir: &Path,
    from: &Path,
    to: &Path,
) -> Result<(), Error> {
    let mut process = Process::spawn(
        Command::new("/usr/bin/ssh")
            .arg("-s")
            .args(spec.exec_command(control_dir, "sftp"))
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped()),
    )?;
    let stdin = process.child.stdin.take().expect("piped stdin");
    let stdout = process.child.stdout.take().expect("piped stdout");
    let (reader, mut writer) = tokio_io::simplex(4096);
    let relay = async {
        relay_without_extensions(stdout, &mut writer).await?;
        writer.shutdown().await
    };
    let rename = async {
        let client = Sftp::new(stdin, reader, SftpOptions::default()).await?;
        let result = client.fs().rename(from, to).await;
        drop(client);
        result.map_err(Error::from)
    };
    timeout(ssh::CONNECT_TIMEOUT, async {
        tokio::pin!(rename);
        tokio::select! {
            biased;
            result = &mut rename => result,
            result = relay => {
                result?;
                rename.await
            }
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SFTP rename timed out"))?
}

async fn relay_without_extensions(
    mut server: impl AsyncRead + Unpin,
    mut client: impl AsyncWrite + Unpin,
) -> io::Result<()> {
    const VERSION: u8 = 2;
    let packet_len = server.read_u32().await? as usize;
    if !(5..=4096).contains(&packet_len) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut version = [0; 5];
    server.read_exact(&mut version).await?;
    if version[0] != VERSION {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut extensions = [0; 4091];
    server.read_exact(&mut extensions[..packet_len - 5]).await?;
    client.write_u32(5).await?;
    client.write_all(&version).await?;
    tokio_io::copy(&mut server, &mut client).await?;
    Ok(())
}

pub(crate) async fn list_dir(sftp: &Sftp, path: &Path) -> Result<Vec<Entry>, Error> {
    let mut entries = read_dir(sftp, path).await?;
    // READDIR supplies link metadata. Follow just the links to determine whether
    // they can be expanded, keeping broken or inaccessible links in the result.
    let mut requests = JoinSet::new();
    for index in 0..entries.len() {
        let entry = &entries[index];
        if entry.kind != EntryKind::Symlink {
            continue;
        }
        if requests.len() == METADATA_CONCURRENCY {
            read_link_result(&mut requests, &mut entries).await?;
        }
        let remote = path.join(&entries[index].name);
        let mut fs = sftp.fs();
        requests.spawn(async move {
            (
                index,
                fs.metadata(remote).await.ok().map(|meta| kind(&meta)),
            )
        });
    }
    while !requests.is_empty() {
        read_link_result(&mut requests, &mut entries).await?;
    }
    Ok(entries)
}

async fn read_link_result(
    requests: &mut JoinSet<(usize, Option<EntryKind>)>,
    entries: &mut [Entry],
) -> Result<(), Error> {
    if let Some(result) = requests.join_next().await {
        let (index, kind) = result.map_err(io::Error::other)?;
        entries[index].symlink_target_kind = kind;
    }
    Ok(())
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

pub(crate) async fn mkdir(sftp: &Sftp, path: &Path) -> Result<(), Error> {
    let mut fs = sftp.fs();
    let Err(error) = fs.create_dir(path).await else {
        return Ok(());
    };
    if ambiguous_path_error(&error) && fs.symlink_metadata(path).await.is_ok() {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
    }
    Err(error.into())
}

pub(crate) async fn create_file(sftp: &Sftp, path: &Path) -> Result<(), Error> {
    match sftp.options().write(true).create_new(true).open(path).await {
        Ok(file) => Ok(file.close().await?),
        Err(error) => {
            // SFTP v3 reports EEXIST as a generic Failure. LSTAT also detects
            // occupied names that are dangling symlinks.
            if matches!(
                error,
                openssh_sftp_client::Error::SftpError(SftpErrorKind::Failure, _)
            ) && sftp.fs().symlink_metadata(path).await.is_ok()
            {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists).into());
            }
            Err(error.into())
        }
    }
}

pub(crate) async fn chmod(sftp: &Sftp, path: &Path, mode: u32) -> Result<(), Error> {
    use openssh_sftp_client::metadata::Permissions;

    let mut fs = sftp.fs();
    fs.set_permissions(path, Permissions::from((mode & 0o7777) as u16))
        .await?;
    Ok(())
}

/// Lee hasta `max_bytes`, sin cargar el archivo completo en memoria. El segundo
/// valor indica que quedaban más bytes después del límite.
async fn is_dir(sftp: &Sftp, path: &Path) -> bool {
    sftp.fs()
        .metadata(path)
        .await
        .is_ok_and(|info| info.file_type().is_some_and(|kind| kind.is_dir()))
}

pub(crate) async fn read_bytes_limited(
    sftp: &Sftp,
    path: &Path,
    max_bytes: u64,
) -> Result<(Vec<u8>, bool), Error> {
    if is_dir(sftp, path).await {
        return Err(io::Error::from(io::ErrorKind::IsADirectory).into());
    }
    let source = sftp.open(path).await?;
    let mut source = source;
    let mut result = Vec::new();
    let mut buffer = Default::default();
    loop {
        let remaining = max_bytes.saturating_sub(result.len() as u64);
        if remaining == 0 {
            // Comprobar si queda algo para informar de truncamiento sin acumularlo.
            let probe = source.read(1, buffer).await?;
            let truncated = probe.is_some();
            let _ = source.close().await;
            return Ok((result, truncated));
        }
        let wanted = remaining.min(CHUNK_SIZE as u64) as u32;
        let Some(chunk) = source.read(wanted, buffer).await? else {
            let _ = source.close().await;
            return Ok((result, false));
        };
        result.extend_from_slice(&chunk);
        buffer = chunk;
        buffer.clear();
    }
}

/// Escribe todos los bytes y cierra el handle antes de devolver el control.
pub(crate) async fn write_bytes(sftp: &Sftp, path: &Path, bytes: Vec<u8>) -> Result<(), Error> {
    let opened = sftp
        .options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .await;
    let mut destination = match opened {
        Err(error) if ambiguous_path_error(&error) && is_dir(sftp, path).await => {
            return Err(io::Error::from(io::ErrorKind::IsADirectory).into());
        }
        other => other?,
    };
    let result = destination.write_all(&bytes).await;
    let closed = destination.close().await;
    result?;
    closed?;
    Ok(())
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
    let result = write_file(&mut source, &mut destination, progress, cancel, false).await;
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
    check_cancel(cancel)?;
    let mut done = 0u64;
    let mut spare = Default::default();
    let mut offset = source.offset();
    let mut chunk = source.read(CHUNK_SIZE as u32, Default::default()).await?;
    while let Some(mut current) = chunk {
        if current.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SFTP server returned an empty data packet",
            )
            .into());
        }
        // SFTP servers may return a short read before EOF. The client
        // advances by the request length, so resume at the last received byte.
        offset = offset.saturating_add(current.len() as u64);
        if source.offset() != offset {
            source.seek(io::SeekFrom::Start(offset)).await?;
        }
        let prefetch = current.len() >= CHUNK_SIZE
            && !cancel.load(Ordering::Relaxed)
            && budget.is_none_or(|(start, limit)| {
                start
                    .saturating_add(done)
                    .saturating_add(current.len() as u64)
                    <= limit
            });
        let write = async {
            for (index, part) in current.chunks(CHUNK_SIZE).enumerate() {
                if index != 0 {
                    check_cancel(cancel)?;
                }
                done = done.saturating_add(part.len() as u64);
                if let Some((start, limit)) = budget {
                    crate::transfer::check_size(start.saturating_add(done), limit)?;
                }
                destination.write_all(part).await?;
                progress(done);
            }
            Ok::<_, Error>(())
        };
        if !prefetch {
            write.await?;
            check_cancel(cancel)?;
            current.clear();
            chunk = source.read(CHUNK_SIZE as u32, current).await?;
        } else {
            let (written, next) = tokio::join!(write, source.read(BULK_CHUNK_SIZE as u32, spare));
            written?;
            check_cancel(cancel)?;
            chunk = next?;
            current.clear();
            spare = current;
        }
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
    staged: bool,
) -> Result<(), Error> {
    check_cancel(cancel)?;
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut spare = Vec::new();
    let mut done = 0;
    let mut len = source.read(&mut buffer).await?;
    // A staged upload can combine several acknowledged progress increments
    // in one packet. An in-place write keeps its original cancellation boundary.
    let bulk_size = if staged { BULK_CHUNK_SIZE } else { CHUNK_SIZE };
    while len != 0 {
        if staged {
            check_cancel(cancel)?;
        }
        if len < CHUNK_SIZE || cancel.load(Ordering::Relaxed) {
            destination.write_all(&buffer[..len]).await?;
            done += len as u64;
            progress(done);
            check_cancel(cancel)?;
            len = source.read(&mut buffer).await?;
        } else {
            if spare.len() != bulk_size {
                spare.resize(bulk_size, 0);
            }
            // Keep remote writes in order, including in-place edit uploads.
            let (written, next) = tokio::join!(
                destination.write_all(&buffer[..len]),
                source.read(&mut spare),
            );
            written?;
            for part in buffer[..len].chunks(CHUNK_SIZE) {
                done += part.len() as u64;
                progress(done);
                check_cancel(cancel)?;
            }
            len = next?;
            std::mem::swap(&mut buffer, &mut spare);
        }
    }
    Ok(())
}
