use std::{
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
    thread,
};

use openssh_sftp_client::Sftp;
use tokio::{runtime::Builder, sync::oneshot, task::JoinSet};

use crate::{ConnectionSpec, Entry, Error, askpass, sftp, ssh};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
type Operation = Box<dyn FnOnce(Arc<Sftp>) -> Job + Send>;

/// Owns the worker used by all sessions opened through this client and its clones.
#[derive(Clone)]
pub struct Client {
    app: PathBuf,
    jobs: async_channel::Sender<Job>,
    requests: async_channel::Receiver<askpass::Request>,
    send_request: async_channel::Sender<askpass::Request>,
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
        thread::Builder::new()
            .name("vscp-network".into())
            .spawn(move || worker(incoming))?;
        Ok(Self {
            app,
            jobs,
            requests,
            send_request,
        })
    }

    /// Receive the next OpenSSH prompt while a connection is in progress.
    pub async fn next_prompt(&self) -> Option<askpass::Request> {
        self.requests.recv().await.ok()
    }

    pub async fn effective_identities(&self, spec: ConnectionSpec) -> Result<Vec<PathBuf>, Error> {
        let (send, reply) = oneshot::channel();
        let app = self.app.clone();
        let requests = self.send_request.clone();
        self.submit(async move {
            let result = async {
                let askpass = askpass::Server::start(app, requests)?;
                ssh::identities(&spec, &askpass).await
            }
            .await;
            let _ = send.send(result);
        })
        .await?;
        reply.await.map_err(closed)?
    }

    pub async fn connect(&self, spec: ConnectionSpec) -> Result<Session, Error> {
        let (send, reply) = oneshot::channel();
        let app = self.app.clone();
        let requests = self.send_request.clone();
        self.submit(async move {
            match connect(spec, app, requests).await {
                Ok((master, channel, process)) => {
                    let (send_command, commands) = async_channel::bounded(16);
                    let _ = send.send(Ok(Session {
                        commands: send_command,
                    }));
                    serve(master, channel, process, commands).await;
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

fn worker(jobs: async_channel::Receiver<Job>) {
    let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
        return;
    };
    runtime.block_on(async {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                job = jobs.recv() => match job {
                    Ok(job) => { tasks.spawn(job); }
                    Err(_) => break,
                },
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
        while tasks.join_next().await.is_some() {}
    });
}

async fn connect(
    spec: ConnectionSpec,
    app: PathBuf,
    requests: async_channel::Sender<askpass::Request>,
) -> Result<(ssh::Master, Sftp, ssh::Process), Error> {
    let server = askpass::Server::start(app, requests)?;
    let mut master = ssh::Master::connect(spec, server).await?;
    match sftp::open(&master).await {
        Ok((channel, process)) => Ok((master, channel, process)),
        Err(err) => {
            let _ = master.disconnect().await;
            Err(err)
        }
    }
}

enum Command {
    Operation(Operation),
    Disconnect(oneshot::Sender<Result<(), Error>>),
}

async fn serve(
    mut master: ssh::Master,
    sftp: Sftp,
    process: ssh::Process,
    commands: async_channel::Receiver<Command>,
) {
    let sftp = Arc::new(sftp);
    let disconnect = loop {
        match commands.recv().await {
            Ok(Command::Operation(operation)) => operation(Arc::clone(&sftp)).await,
            Ok(Command::Disconnect(reply)) => break Some(reply),
            Err(_) => break None,
        }
    };
    commands.close();
    let sftp = Arc::try_unwrap(sftp).expect("SFTP operations have completed");
    let close_sftp = sftp::close(sftp, process).await;
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
    pub async fn disconnect(&self) -> Result<(), Error> {
        let (send, reply) = oneshot::channel();
        self.commands
            .send(Command::Disconnect(send))
            .await
            .map_err(closed)?;
        reply.await.map_err(closed)?
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
        self.run(move |sftp| async move { Ok(sftp.fs().rename(from, to).await?) })
            .await
    }

    pub async fn remove_file(&self, path: PathBuf) -> Result<(), Error> {
        self.run(move |sftp| async move { Ok(sftp.fs().remove_file(path).await?) })
            .await
    }

    /// Progress callbacks run on the worker thread and should return promptly.
    pub async fn download(
        &self,
        remote: PathBuf,
        local: PathBuf,
        progress: impl Fn(u64) + Send + Sync + 'static,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), Error> {
        self.run(move |sftp| async move {
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
        self.run(move |sftp| async move {
            sftp::upload(&sftp, &local, &remote, &progress, &cancel).await
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
        let (send, reply) = oneshot::channel();
        let operation = Box::new(move |sftp| -> Job {
            Box::pin(async move {
                if !send.is_closed() {
                    let _ = send.send(operation(sftp).await);
                }
            })
        });
        self.commands
            .send(Command::Operation(operation))
            .await
            .map_err(closed)?;
        reply.await.map_err(closed)?
    }
}
