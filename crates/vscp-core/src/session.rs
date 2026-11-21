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
    time::Duration,
};

use openssh_sftp_client::Sftp;
use tokio::{
    runtime::Builder,
    sync::oneshot,
    task::{AbortHandle, JoinSet},
    time::timeout,
};

use crate::{ConnectionSpec, Entry, Error, askpass, sftp, ssh, transfer};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
type Operation = Box<dyn FnOnce(Arc<Sftp>) -> Job + Send>;

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
        thread::Builder::new().name("vscp-network".into()).spawn({
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

    pub async fn connect(&self, spec: ConnectionSpec) -> Result<Session, Error> {
        self.connect_with_timeout(spec, ssh::CONNECT_TIMEOUT).await
    }

    #[doc(hidden)]
    pub async fn connect_with_timeout(
        &self,
        spec: ConnectionSpec,
        readiness_timeout: Duration,
    ) -> Result<Session, Error> {
        let (send, reply) = oneshot::channel();
        let app = self.app.clone();
        let requests = self.send_request.clone();
        let stopping = self.stopping.clone();
        self.submit(async move {
            match connect(spec, app, requests, readiness_timeout, &stopping).await {
                Ok((master, channel, process)) => {
                    let (send_command, commands) = async_channel::bounded(16);
                    let _ = send.send(Ok(Session {
                        commands: send_command,
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
        "The vSCP SSH worker or session has stopped",
    )
    .into()
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
) -> Result<(ssh::Master, Sftp, ssh::Process), Error> {
    let server = askpass::Server::start(app, requests, spec.destination())?;
    let mut master = ssh::Master::connect(spec, server, readiness_timeout, stopping).await?;
    let channel = tokio::select! {
        _ = stopping.recv() => Err(Error::Cancelled),
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
}

impl Session {
    pub async fn closed(&self) {
        self.commands.closed().await;
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

    pub async fn list_dir(&self, path: PathBuf) -> Result<Vec<Entry>, Error> {
        self.run(move |sftp| async move { sftp::list_dir(&sftp, &path).await })
            .await
    }

    pub async fn stat(&self, path: PathBuf) -> Result<Entry, Error> {
        self.run(move |sftp| async move { sftp::stat(&sftp, &path).await })
            .await
    }

    pub async fn mkdir(&self, path: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move { Ok(sftp.fs().create_dir(path).await?) })
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
