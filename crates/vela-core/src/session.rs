use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use openssh_sftp_client::Sftp;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    runtime::Builder,
    sync::oneshot,
    task::{AbortHandle, JoinSet},
    time::timeout,
};

use crate::{
    ConnectionSpec, Entry, Error, ExecOptions, ExecOutput,
    agent::{self, AgentError, AgentKey},
    askpass, sftp, ssh, transfer,
};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
type Operation = Box<dyn FnOnce(Arc<Sftp>) -> Job + Send>;

/// Cancels one connection attempt, including the `ssh` child it has already started.
#[derive(Clone)]
pub struct ConnectCancel {
    tx: async_channel::Sender<()>,
    rx: async_channel::Receiver<()>,
}

impl Default for ConnectCancel {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectCancel {
    pub fn new() -> Self {
        let (tx, rx) = async_channel::bounded(1);
        Self { tx, rx }
    }

    pub fn cancel(&self) {
        self.tx.close();
    }

    pub fn is_cancelled(&self) -> bool {
        self.tx.is_closed()
    }

    pub(crate) fn receiver(&self) -> async_channel::Receiver<()> {
        self.rx.clone()
    }
}

/// Owns the worker used by all sessions opened through this client and its clones.
#[derive(Clone)]
pub struct Client {
    app: PathBuf,
    jobs: async_channel::Sender<Job>,
    requests: async_channel::Receiver<askpass::Request>,
    send_request: async_channel::Sender<askpass::Request>,
    stopping: async_channel::Receiver<()>,
    worker_done: async_channel::Receiver<()>,
}

impl Client {
    /// Start the worker. The executable must dispatch its `askpass` subcommand
    /// to [askpass::client_main].
    pub fn new(app: PathBuf) -> io::Result<Self> {
        if !app.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Askpass executable path must be absolute",
            ));
        }
        let (jobs, incoming) = async_channel::bounded(16);
        let (send_request, requests) = async_channel::bounded(16);
        let (keep_open, stopping) = async_channel::bounded(1);
        let (done, worker_done) = async_channel::bounded(1);
        thread::Builder::new().name("vela-network".into()).spawn({
            let stopping = stopping.clone();
            move || {
                worker(incoming, stopping);
                drop((keep_open, done));
            }
        })?;
        Ok(Self {
            app,
            jobs,
            requests,
            send_request,
            stopping,
            worker_done,
        })
    }

    /// Receive the next OpenSSH prompt while a connection is in progress.
    pub async fn next_prompt(&self) -> Option<askpass::Request> {
        self.requests.recv().await.ok()
    }

    /// Stop new jobs, cancel pending connections, and wait for all SSH children.
    pub async fn shutdown(&self) {
        self.jobs.close();
        self.stopping.close();
        self.requests.close();
        while self.requests.try_recv().is_ok() {}
        let _ = self.worker_done.recv().await;
    }

    pub async fn effective_identities(&self, spec: ConnectionSpec) -> Result<Vec<PathBuf>, Error> {
        let (send, reply) = oneshot::channel();
        let app = self.app.clone();
        let requests = self.send_request.clone();
        let stopping = self.stopping.clone();
        self.submit(async move {
            let result = tokio::select! {
                _ = stopping.recv() => Err(Error::Cancelled),
                result = async {
                    let askpass = askpass::Server::start(app, requests, spec.destination())?;
                    ssh::identities(&spec, &askpass).await
                } => result,
            };
            let _ = send.send(result);
        })
        .await?;
        reply.await.map_err(closed)?
    }

    /// Lists the public keys of the SSH agent at `socket` on the worker, so a
    /// slow Bitwarden unlock prompt never blocks the caller's thread.
    pub async fn list_agent_keys(&self, socket: PathBuf) -> Result<Vec<AgentKey>, AgentError> {
        let (send, reply) = oneshot::channel();
        let stopping = self.stopping.clone();
        self.submit(async move {
            let result = tokio::select! {
                _ = stopping.recv() => Err(AgentError::Unavailable),
                result = agent::list_identities(&socket) => result,
            };
            let _ = send.send(result);
        })
        .await
        .map_err(|_| AgentError::Unavailable)?;
        reply.await.map_err(|_| AgentError::Unavailable)?
    }

    pub async fn connect(&self, spec: ConnectionSpec) -> Result<Session, Error> {
        self.connect_with_timeout(spec, ssh::CONNECT_TIMEOUT).await
    }

    #[doc(hidden)]
    pub async fn connect_with_timeout(
        &self,
        spec: ConnectionSpec,
        readiness_timeout: Duration,
    ) -> Result<Session, Error> {
        self.connect_inner(spec, readiness_timeout, None).await
    }

    pub async fn connect_with_cancel(
        &self,
        spec: ConnectionSpec,
        cancel: ConnectCancel,
    ) -> Result<Session, Error> {
        self.connect_with_timeout_and_cancel(spec, ssh::CONNECT_TIMEOUT, cancel)
            .await
    }

    #[doc(hidden)]
    pub async fn connect_with_timeout_and_cancel(
        &self,
        spec: ConnectionSpec,
        readiness_timeout: Duration,
        cancel: ConnectCancel,
    ) -> Result<Session, Error> {
        self.connect_inner(spec, readiness_timeout, Some(cancel))
            .await
    }

    async fn connect_inner(
        &self,
        spec: ConnectionSpec,
        readiness_timeout: Duration,
        cancel: Option<ConnectCancel>,
    ) -> Result<Session, Error> {
        let (send, reply) = oneshot::channel();
        let app = self.app.clone();
        let requests = self.send_request.clone();
        let stopping = self.stopping.clone();
        let cancel = cancel.map(|cancel| cancel.receiver());
        self.submit(async move {
            // A connection without an explicit handle keeps its receiver open
            // until the end of the job, so only `stopping` can interrupt it.
            let (keep_open, unused_cancel) = async_channel::bounded(1);
            let cancel = cancel.unwrap_or(unused_cancel);
            let result = connect(spec, app, requests, readiness_timeout, &stopping, &cancel).await;
            drop(keep_open);
            match result {
                Ok((master, channel, process)) => {
                    let (send_command, commands) = async_channel::bounded(16);
                    let terminal =
                        Arc::new((master.spec.clone(), master.askpass.directory().to_owned()));
                    let _ = send.send(Ok(Session {
                        commands: send_command,
                        terminal,
                    }));
                    serve(master, channel, process, commands, stopping).await;
                }
                Err(err) => {
                    let _ = send.send(Err(err));
                }
            }
        })
        .await?;
        reply.await.map_err(closed)?
    }

    async fn submit(&self, job: impl Future<Output = ()> + Send + 'static) -> Result<(), Error> {
        self.jobs.send(Box::pin(job)).await.map_err(closed)
    }
}

fn closed<T>(_: T) -> Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "The Vela SSH worker or session has stopped",
    )
    .into()
}

/// Acumula como máximo `max` bytes y drena el resto hasta EOF para no bloquear
/// al proceso hijo ni consumir memoria sin límite.
async fn read_capped<R: AsyncRead + Unpin>(reader: &mut R, max: usize) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut chunk = vec![0; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(len) => {
                if bytes.len().saturating_add(len) > max {
                    let room = max.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&chunk[..room]);
                    truncated = true;
                } else {
                    bytes.extend_from_slice(&chunk[..len]);
                }
            }
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    (bytes, truncated)
}

/// Evita filtrar al agente la ruta interna del directorio de control.
fn sanitize_stderr(stderr: &str, control_dir: &std::path::Path) -> String {
    let directory = control_dir.to_string_lossy();
    if directory.is_empty() {
        return stderr.to_owned();
    }
    stderr.replace(directory.as_ref(), "<control-dir>")
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    #[tokio::test]
    async fn capped_reader_drains_without_accumulating() {
        let bytes = vec![7u8; 30_000];
        let mut reader = &bytes[..];
        let (kept, truncated) = read_capped(&mut reader, 100).await;
        assert_eq!(kept.len(), 100);
        assert!(truncated);
    }

    #[tokio::test]
    async fn capped_reader_reports_clean_eof() {
        let bytes = b"hola";
        let mut reader = &bytes[..];
        let (kept, truncated) = read_capped(&mut reader, 100).await;
        assert_eq!(kept, b"hola");
        assert!(!truncated);
    }

    #[test]
    fn stderr_hides_the_control_directory() {
        let text = "Control socket connect(/tmp/vela-1000/abc): failure";
        assert_eq!(
            sanitize_stderr(text, std::path::Path::new("/tmp/vela-1000")),
            "Control socket connect(<control-dir>/abc): failure"
        );
    }
}

fn worker(jobs: async_channel::Receiver<Job>, stopping: async_channel::Receiver<()>) {
    let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
        return;
    };
    runtime.block_on(async {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = stopping.recv() => {
                    while jobs.try_recv().is_ok() {}
                    break;
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
                job = jobs.recv() => match job {
                    Ok(job) => { tasks.spawn(job); }
                    Err(_) => break,
                },
            }
        }
        while tasks.join_next().await.is_some() {}
    });
}

async fn connect(
    spec: ConnectionSpec,
    app: PathBuf,
    requests: async_channel::Sender<askpass::Request>,
    readiness_timeout: Duration,
    stopping: &async_channel::Receiver<()>,
    cancel: &async_channel::Receiver<()>,
) -> Result<(ssh::Master, Sftp, ssh::Process), Error> {
    if cancel.is_closed() {
        return Err(Error::Cancelled);
    }
    let server = askpass::Server::start(app, requests, spec.destination())?;
    let mut master =
        ssh::Master::connect(spec, server, readiness_timeout, stopping, cancel).await?;
    let channel = tokio::select! {
        biased;
        _ = stopping.recv() => Err(Error::Cancelled),
        _ = cancel.recv() => Err(Error::Cancelled),
        result = sftp::open(&master) => result,
    };
    match channel {
        Ok((channel, process)) => Ok((master, channel, process)),
        Err(err) => {
            let _ = master.disconnect().await;
            Err(err)
        }
    }
}

enum Command {
    Operation(Operation, Option<Arc<AtomicBool>>),
    Transfer(transfer::Work),
    Disconnect(oneshot::Sender<Result<(), Error>>),
}

async fn serve(
    mut master: ssh::Master,
    sftp: Sftp,
    mut process: ssh::Process,
    commands: async_channel::Receiver<Command>,
    stopping: async_channel::Receiver<()>,
) {
    let sftp = Arc::new(sftp);
    let mut tasks = JoinSet::new();
    let mut active = HashMap::<_, (AbortHandle, Option<Arc<AtomicBool>>)>::new();
    let mut transfers = VecDeque::<transfer::Work>::new();
    let mut transfer_active = None::<(tokio::task::Id, transfer::Cancel)>;
    let disconnect = loop {
        if transfer_active.is_none()
            && let Some(work) = transfers.pop_front()
        {
            let cancel = work.cancel.clone();
            let channel = sftp.clone();
            let task = tasks.spawn(async move { work.run(&channel).await });
            transfer_active = Some((task.id(), cancel));
        }
        tokio::select! {
            _ = stopping.recv() => break None,
            _ = process.child.wait() => break None,
            result = commands.recv() => match result {
                Ok(Command::Operation(operation, cancel)) => {
                    let task = tasks.spawn(operation(sftp.clone()));
                    active.insert(task.id(), (task, cancel));
                }
                Ok(Command::Transfer(work)) => transfers.push_back(work),
                Ok(Command::Disconnect(reply)) => break Some(reply),
                Err(_) => break None,
            },
            Some(result) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                let id = result.map_or_else(|err| err.id(), |(id, ())| id);
                active.remove(&id);
                if transfer_active.as_ref().is_some_and(|(running, _)| *running == id) {
                    transfer_active = None;
                }
            }
        }
    };
    commands.close();
    while let Ok(command) = commands.try_recv() {
        if let Command::Transfer(work) = command {
            transfers.push_back(work);
        }
    }
    for work in transfers {
        work.cancel_queued();
    }
    if let Some((_, cancel)) = transfer_active {
        cancel.cancel();
    }
    for (task, cancel) in active.values() {
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Relaxed);
        } else {
            task.abort();
        }
    }
    // Let transfers remove partial downloads. A stalled peer must not hang shutdown.
    if timeout(Duration::from_secs(2), async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.shutdown().await;
    }
    let close_sftp = sftp::close(Arc::into_inner(sftp), process).await;
    let close_master = master.disconnect().await;
    if let Some(reply) = disconnect {
        let _ = reply.send(close_sftp.and(close_master).map_err(Error::Io));
    }
}

/// An active SSH master and SFTP channel. Dropping the last handle disconnects.
#[derive(Clone)]
pub struct Session {
    commands: async_channel::Sender<Command>,
    /// Connection spec and the directory that holds the master's control socket.
    terminal: Arc<(ConnectionSpec, PathBuf)>,
}

impl Session {
    pub async fn closed(&self) {
        self.commands.closed().await;
    }

    /// Arguments for `ssh` that open a terminal on this session's master
    /// connection, without authenticating again.
    pub fn terminal_command(&self, remote_command: &str) -> Vec<std::ffi::OsString> {
        self.terminal
            .0
            .terminal_command(&self.terminal.1, remote_command)
    }

    pub fn same_connection(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }

    pub async fn disconnect(&self) -> Result<(), Error> {
        let (send, reply) = oneshot::channel();
        self.commands
            .send(Command::Disconnect(send))
            .await
            .map_err(closed)?;
        reply.await.map_err(closed)?
    }

    pub async fn home_dir(&self) -> Result<PathBuf, Error> {
        self.realpath(".".into()).await
    }

    pub async fn realpath(&self, path: PathBuf) -> Result<PathBuf, Error> {
        self.run(move |sftp| async move { Ok(sftp.fs().canonicalize(path).await?) })
            .await
    }

    /// Ejecuta un comando no interactivo reutilizando el `ControlPath` de la
    /// sesión. Nunca abre un TTY, nunca reenvía el agente y nunca espera una
    /// contraseña. La salida se acota por stream y un vencimiento mata al hijo.
    pub async fn exec(&self, command: String, options: ExecOptions) -> Result<ExecOutput, Error> {
        let spec = self.terminal.0.clone();
        let control_dir = self.terminal.1.clone();
        let budget = options.timeout.min(ssh::MAX_EXEC_TIMEOUT);
        let limit = options.max_output_bytes;
        let remote = ssh::exec_remote_command(&command, options.cwd.as_deref());
        let mut child = ssh::exec_process(&spec, &control_dir, &remote).spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let out = tokio::spawn(async move { read_capped(&mut stdout, limit).await });
        let err = tokio::spawn(async move { read_capped(&mut stderr, limit).await });

        let started = Instant::now();
        let (exit_code, timed_out) = match timeout(budget, child.wait()).await {
            Ok(status) => (status?.code(), false),
            Err(_) => {
                let _ = child.start_kill();
                let _ = timeout(ssh::EXIT_TIMEOUT, child.wait()).await;
                (None, true)
            }
        };
        let duration = started.elapsed();
        let (stdout, stdout_truncated) = timeout(ssh::EXIT_TIMEOUT, out)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let (stderr, stderr_truncated) = timeout(ssh::EXIT_TIMEOUT, err)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();

        Ok(ExecOutput {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: sanitize_stderr(&String::from_utf8_lossy(&stderr), &control_dir),
            stdout_truncated,
            stderr_truncated,
            timed_out,
            duration,
        })
    }

    pub async fn list_dir(&self, path: PathBuf) -> Result<Vec<Entry>, Error> {
        self.run(move |sftp| async move { sftp::list_dir(&sftp, &path).await })
            .await
    }

    pub async fn stat(&self, path: PathBuf) -> Result<Entry, Error> {
        self.run(move |sftp| async move { sftp::stat(&sftp, &path).await })
            .await
    }

    /// Cambia los permisos de una ruta remota (bits `0o7777`).
    pub async fn chmod(&self, path: PathBuf, mode: u32) -> Result<(), Error> {
        self.run(move |sftp| async move { sftp::chmod(&sftp, &path, mode).await })
            .await
    }

    /// Lee como máximo `max_bytes`; el booleano indica que el archivo sigue.
    pub async fn read_bytes_limited(
        &self,
        path: PathBuf,
        max_bytes: u64,
    ) -> Result<(Vec<u8>, bool), Error> {
        self.run(move |sftp| async move { sftp::read_bytes_limited(&sftp, &path, max_bytes).await })
            .await
    }

    /// Escribe bytes y cierra el handle SFTP antes de devolver el control.
    pub async fn write_bytes(&self, path: PathBuf, bytes: Vec<u8>) -> Result<(), Error> {
        self.run(move |sftp| async move { sftp::write_bytes(&sftp, &path, bytes).await })
            .await
    }

    pub async fn mkdir(&self, path: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move { Ok(sftp.fs().create_dir(path).await?) })
            .await
    }

    /// Creates an empty file exclusively. An occupied name yields AlreadyExists.
    pub async fn create_file(&self, path: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move { sftp::create_file(&sftp, &path).await })
            .await
    }

    pub async fn rename(&self, from: PathBuf, to: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move {
            transfer::ensure_remote_free(&sftp, &to).await?;
            Ok(sftp.fs().rename(from, to).await?)
        })
        .await
    }

    pub async fn remove_file(&self, path: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move { Ok(sftp.fs().remove_file(path).await?) })
            .await
    }

    pub async fn remove_tree(&self, path: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move { transfer::remove_remote_tree(&sftp, path).await })
            .await
    }

    pub async fn enqueue_transfer(&self, spec: transfer::Spec) -> Result<transfer::Job, Error> {
        self.queue_transfer(spec, None).await
    }

    /// Download into a private staging directory with exact names and a byte limit.
    pub async fn enqueue_staging(
        &self,
        sources: Vec<PathBuf>,
        destination: PathBuf,
        limit: u64,
    ) -> Result<transfer::Job, Error> {
        self.queue_transfer(
            transfer::Spec {
                direction: transfer::Direction::Download,
                sources,
                destination,
            },
            Some(limit),
        )
        .await
    }

    async fn queue_transfer(
        &self,
        spec: transfer::Spec,
        staging_limit: Option<u64>,
    ) -> Result<transfer::Job, Error> {
        let (job, work) = transfer::Work::new(spec, staging_limit);
        self.commands
            .send(Command::Transfer(work))
            .await
            .map_err(closed)?;
        Ok(job)
    }

    /// Progress callbacks run on the worker thread and should return promptly.
    pub async fn download(
        &self,
        remote: PathBuf,
        local: PathBuf,
        progress: impl Fn(u64) + Send + Sync + 'static,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        self.run_cancellable(Some(cancel.clone()), move |sftp| async move {
            sftp::download(&sftp, &remote, &local, &progress, &cancel).await
        })
        .await
    }

    /// Progress callbacks run on the worker thread and should return promptly.
    pub async fn upload(
        &self,
        local: PathBuf,
        remote: PathBuf,
        progress: impl Fn(u64) + Send + Sync + 'static,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        self.run_cancellable(Some(cancel.clone()), move |sftp| async move {
            sftp::upload(&sftp, &local, &remote, &progress, &cancel).await
        })
        .await
    }

    pub(crate) async fn upload_edit(
        &self,
        path: PathBuf,
        canonical: PathBuf,
        snapshot: crate::edit::Snapshot,
        expected: crate::edit::Stamp,
    ) -> Result<crate::edit::Upload, Error> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.run_cancellable(Some(cancel.clone()), move |sftp| async move {
            crate::edit::upload(&sftp, &path, &canonical, snapshot, expected, &cancel).await
        })
        .await
    }

    async fn run<T, F>(
        &self,
        operation: impl FnOnce(Arc<Sftp>) -> F + Send + 'static,
    ) -> Result<T, Error>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, Error>> + Send + 'static,
    {
        self.run_cancellable(None, operation).await
    }

    async fn run_cancellable<T, F>(
        &self,
        cancel: Option<Arc<AtomicBool>>,
        operation: impl FnOnce(Arc<Sftp>) -> F + Send + 'static,
    ) -> Result<T, Error>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, Error>> + Send + 'static,
    {
        let (send, reply) = oneshot::channel();
        let operation = Box::new(move |sftp| -> Job {
            Box::pin(async move {
                if !send.is_closed() {
                    let _ = send.send(operation(sftp).await);
                }
            })
        });
        self.commands
            .send(Command::Operation(operation, cancel))
            .await
            .map_err(closed)?;
        reply.await.map_err(closed)?
    }
}
