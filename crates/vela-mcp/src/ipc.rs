//! Transporte local entre el puente MCP y un proceso Vela del mismo UID.

use std::{
    collections::HashMap,
    fs::{self, DirBuilder, File},
    future::Future,
    io,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncWrite, BufReader},
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::{AbortHandle, Id, JoinSet},
    time::timeout,
};

use crate::{
    ndjson,
    protocol::{
        CallTasks, ClientInfo, MAX_CALLS, Progress, ProgressReporter, RpcError, TextContent,
        ToolHandler, ToolResponse, VERSION, spawn_call,
    },
    tools::ToolCall,
};

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CLIENTS: usize = 32;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Hello {
        client_name: String,
        client_version: String,
        pid: u32,
    },
    CallTool {
        request_id: u64,
        name: String,
        arguments: Value,
        want_progress: bool,
    },
    Cancel {
        request_id: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Welcome {
        server_version: String,
    },
    Progress {
        request_id: u64,
        #[serde(flatten)]
        progress: Progress,
    },
    ToolResponse {
        request_id: u64,
        content: Vec<TextContent>,
        is_error: bool,
    },
    Error {
        request_id: u64,
        #[serde(flatten)]
        error: RpcError,
    },
}

impl ServerMessage {
    fn response(request_id: u64, result: Result<ToolResponse, RpcError>) -> Self {
        match result {
            Ok(ToolResponse { content, is_error }) => Self::ToolResponse {
                request_id,
                content,
                is_error,
            },
            Err(error) => Self::Error { request_id, error },
        }
    }
}

fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

/// Resuelve la ruta explícita, VELA_MCP_SOCK y la ruta compartida por defecto.
pub fn socket_path(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| {
            std::env::var_os("VELA_MCP_SOCK")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/vela-{}/mcp.sock", uid())))
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn private_parent(path: &Path, create: bool) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.file_name().is_none() || path.as_os_str().len() >= 104 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ruta de socket Unix no válida.",
        ));
    }
    let parent = path.parent().expect("absolute socket path");
    if create {
        match DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o7777 != 0o700 {
        return Err(permission_denied(
            "El directorio del socket MCP debe ser privado (0700).",
        ));
    }
    Ok(parent.to_path_buf())
}

fn socket_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid() {
        return Err(permission_denied(
            "La ruta MCP debe ser un socket Unix del usuario.",
        ));
    }
    Ok(metadata)
}

fn verify_peer(stream: &UnixStream) -> io::Result<()> {
    if stream.peer_cred()?.uid() == uid() {
        Ok(())
    } else {
        Err(permission_denied("El UID del peer MCP no coincide."))
    }
}

fn in_use() -> io::Error {
    io::Error::new(
        io::ErrorKind::AddrInUse,
        "Ya hay un servidor MCP escuchando.",
    )
}

fn invalid_message() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Mensaje IPC inesperado.")
}

async fn receive<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
    frame: &mut Vec<u8>,
) -> io::Result<Option<T>> {
    if !ndjson::read(reader, frame).await? {
        return Ok(None);
    }
    let result = serde_json::from_slice(frame);
    frame.clear();
    Ok(Some(result?))
}

async fn send(writer: &mut (impl AsyncWrite + Unpin), message: &impl Serialize) -> io::Result<()> {
    timeout(IO_TIMEOUT, ndjson::write(writer, message)).await?
}

/// Se conserva el inode para no eliminar un socket que haya reemplazado al propio.
struct SocketFile {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketFile {
    fn new(path: PathBuf, metadata: fs::Metadata) -> Self {
        Self {
            path,
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }

    fn matches(&self) -> bool {
        socket_metadata(&self.path)
            .is_ok_and(|meta| meta.dev() == self.dev && meta.ino() == self.ino)
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        if self.matches() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Listener IPC privado. La ruta se libera al terminar `run` o al descartar el listener.
pub struct IpcServer {
    // Desenlazar mientras el listener propio todavía sigue vivo.
    socket: SocketFile,
    listener: UnixListener,
}

impl IpcServer {
    pub async fn bind(path: PathBuf) -> io::Result<Self> {
        let parent = private_parent(&path, true)?;
        // Serializa bind, chmod y retirada de un socket huérfano entre instancias.
        // Usar el directorio evita introducir un segundo fichero persistente.
        let directory = File::open(parent)?;
        directory.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => in_use(),
            std::fs::TryLockError::Error(error) => error,
        })?;
        match socket_metadata(&path) {
            Ok(metadata) => match timeout(IO_TIMEOUT, UnixStream::connect(&path)).await {
                Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    let current = socket_metadata(&path)?;
                    if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
                        return Err(in_use());
                    }
                    fs::remove_file(&path)?;
                }
                Ok(Err(error)) => return Err(error),
                Ok(Ok(_)) | Err(_) => return Err(in_use()),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        let socket = SocketFile::new(path.clone(), socket_metadata(&path)?);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { socket, listener })
    }

    pub fn path(&self) -> &Path {
        &self.socket.path
    }

    /// Acepta clientes concurrentes hasta que se resuelve `shutdown`.
    /// Cierra cada conversación y cancela sus llamadas antes de regresar.
    pub async fn run(
        self,
        handler: impl ToolHandler,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        let (_, resets) = watch::channel(());
        self.run_with_client_reset(handler, shutdown, resets).await
    }

    /// Drops current clients on reset while retaining the listener and socket.
    pub async fn run_with_client_reset(
        self,
        handler: impl ToolHandler,
        shutdown: impl Future<Output = ()>,
        mut resets: watch::Receiver<()>,
    ) -> io::Result<()> {
        let handler = Arc::new(handler);
        let mut can_reset = true;
        let (mut stop, mut stopping) = watch::channel(());
        let mut clients = JoinSet::new();
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break Ok(()),
                reset = resets.changed(), if can_reset => {
                    can_reset = reset.is_ok();
                    if can_reset {
                        let (next_stop, next_stopping) = watch::channel(());
                        drop(std::mem::replace(&mut stop, next_stop));
                        stopping = next_stopping;
                        while clients.join_next().await.is_some() {}
                    }
                }
                _ = clients.join_next(), if !clients.is_empty() => {}
                accepted = self.listener.accept(), if clients.len() < MAX_CLIENTS => {
                    let (stream, _) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => break Err(error),
                    };
                    if verify_peer(&stream).is_ok() {
                        let handler = handler.clone();
                        let mut stopping = stopping.clone();
                        clients.spawn(async move {
                            let _ = serve_client(stream, handler, &mut stopping).await;
                        });
                    }
                }
            }
        };
        drop(stop);
        while clients.join_next().await.is_some() {}
        result
    }
}

async fn serve_client(
    stream: UnixStream,
    handler: Arc<impl ToolHandler>,
    stopping: &mut watch::Receiver<()>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut frame = Vec::new();
    let hello = tokio::select! {
        _ = stopping.changed() => return Ok(()),
        hello = timeout(IO_TIMEOUT, receive(&mut reader, &mut frame)) => hello??,
    };
    let client = match hello {
        Some(ClientMessage::Hello {
            client_name,
            client_version,
            ..
        }) => ClientInfo {
            name: client_name,
            version: client_version,
        },
        None => return Ok(()),
        _ => return Err(invalid_message()),
    };
    send(
        &mut writer,
        &ServerMessage::Welcome {
            server_version: VERSION.into(),
        },
    )
    .await?;

    let _connected = handler.client_connected(&client);
    let mut tasks = CallTasks::new();
    let mut pending: HashMap<Id, (u64, AbortHandle)> = HashMap::new();
    let (progress_tx, mut progress_rx) = mpsc::channel(64);
    let result = async {
        loop {
            tokio::select! {
                biased;
                _ = stopping.changed() => return Ok(()),
                Some((task, progress)) = progress_rx.recv() => {
                    if let Some(&(request_id, _)) = pending.get(&task) {
                        send(&mut writer, &ServerMessage::Progress { request_id, progress }).await?;
                    }
                }
                Some(result) = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    let (task, result) = match result {
                        Ok((task, result)) => (task, result),
                        Err(error) => (error.id(), Err(RpcError::internal())),
                    };
                    if let Some((request_id, _)) = pending.remove(&task) {
                        match send(&mut writer, &ServerMessage::response(request_id, result)).await {
                            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                                let result = ToolResponse::error("La respuesta supera el límite de tamaño.");
                                send(&mut writer, &ServerMessage::response(request_id, Ok(result))).await?;
                            }
                            result => result?,
                        }
                    }
                }
                message = receive(&mut reader, &mut frame) => match message? {
                    None => return Ok(()),
                    Some(ClientMessage::Cancel { request_id }) => {
                        if let Some(task) = pending.iter().find_map(|(task, (id, _))| (*id == request_id).then_some(*task))
                            && let Some((_, handle)) = pending.remove(&task)
                        {
                            handle.abort();
                        }
                    }
                    Some(ClientMessage::CallTool { request_id, name, arguments, want_progress }) => {
                        if pending.values().any(|(id, _)| *id == request_id) {
                            return Err(invalid_message());
                        }
                        let tool = ToolCall::parse(&name, arguments).and_then(|tool| {
                            if tasks.len() >= MAX_CALLS { Err(RpcError::busy()) } else { Ok(tool) }
                        });
                        match tool {
                            Ok(tool) => {
                                let progress = want_progress.then(|| progress_tx.clone());
                                let task = spawn_call(&mut tasks, handler.clone(), client.clone(), tool, progress);
                                pending.insert(task.id(), (request_id, task));
                            }
                            Err(error) => send(&mut writer, &ServerMessage::Error { request_id, error }).await?,
                        }
                    }
                    _ => return Err(invalid_message()),
                },
            }
        }
    }.await;
    tasks.shutdown().await;
    result
}

struct ClientCall {
    reply: oneshot::Sender<Result<ToolResponse, RpcError>>,
    progress: ProgressReporter,
    _permit: OwnedSemaphorePermit,
}

enum ClientCommand {
    Call {
        id: u64,
        tool: ToolCall,
        call: ClientCall,
    },
    Cancel(u64),
}

struct CancelOnDrop<'a> {
    id: u64,
    tx: &'a mpsc::UnboundedSender<ClientCommand>,
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.tx.send(ClientCommand::Cancel(self.id));
        }
    }
}

/// Conexión multiplexada. Los clones comparten el socket y la identidad MCP.
#[derive(Clone)]
pub struct IpcClient {
    tx: mpsc::UnboundedSender<ClientCommand>,
    ids: Arc<AtomicU64>,
    slots: Arc<Semaphore>,
}

impl IpcClient {
    pub async fn connect(path: &Path, client: &ClientInfo) -> io::Result<Self> {
        private_parent(path, false)?;
        if socket_metadata(path)?.mode() & 0o7777 != 0o600 {
            return Err(permission_denied("El socket MCP debe tener permisos 0600."));
        }
        timeout(IO_TIMEOUT, async {
            let stream = UnixStream::connect(path).await?;
            verify_peer(&stream)?;
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            send(
                &mut writer,
                &ClientMessage::Hello {
                    client_name: client.name.clone(),
                    client_version: client.version.clone(),
                    pid: std::process::id(),
                },
            )
            .await?;
            let mut frame = Vec::new();
            if !matches!(
                receive::<ServerMessage>(&mut reader, &mut frame).await?,
                Some(ServerMessage::Welcome { .. })
            ) {
                return Err(invalid_message());
            }
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                let _ = bridge(reader, writer, rx).await;
            });
            Ok(Self {
                tx,
                ids: Arc::new(AtomicU64::new(1)),
                slots: Arc::new(Semaphore::new(MAX_CALLS)),
            })
        })
        .await?
    }

    /// Al descartar el futuro, envía `Cancel` para esa llamada.
    pub async fn call(
        &self,
        tool: ToolCall,
        progress: ProgressReporter,
    ) -> Result<ToolResponse, RpcError> {
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| RpcError::busy())?;
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let (reply, response) = oneshot::channel();
        let call = ClientCall {
            reply,
            progress,
            _permit: permit,
        };
        if self
            .tx
            .send(ClientCommand::Call { id, tool, call })
            .is_err()
        {
            return Ok(disconnected());
        }
        let mut cancel = CancelOnDrop {
            id,
            tx: &self.tx,
            armed: true,
        };
        let result = response.await.unwrap_or_else(|_| Ok(disconnected()));
        cancel.armed = false;
        result
    }
}

fn disconnected() -> ToolResponse {
    ToolResponse::error("Se perdió la conexión IPC con Vela.")
}

async fn bridge(
    mut reader: impl AsyncBufRead + Unpin,
    mut writer: OwnedWriteHalf,
    mut commands: mpsc::UnboundedReceiver<ClientCommand>,
) -> io::Result<()> {
    let mut frame = Vec::new();
    let mut pending: HashMap<u64, ClientCall> = HashMap::new();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None => return Ok(()),
                Some(ClientCommand::Call { id, tool, call }) => {
                    if call.reply.is_closed() {
                        continue;
                    }
                    let message = ClientMessage::CallTool {
                        request_id: id, name: tool.name().into(), arguments: tool.arguments(),
                        want_progress: call.progress.requested(),
                    };
                    match send(&mut writer, &message).await {
                        Ok(()) => { pending.insert(id, call); }
                        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                            let _ = call.reply.send(Err(RpcError::invalid_params()));
                        }
                        Err(error) => return Err(error),
                    }
                }
                Some(ClientCommand::Cancel(request_id)) => {
                    if pending.remove(&request_id).is_some() {
                        send(&mut writer, &ClientMessage::Cancel { request_id }).await?;
                    }
                }
            },
            response = receive(&mut reader, &mut frame) => match response? {
                None => return Ok(()),
                Some(ServerMessage::Progress { request_id, progress }) => {
                    if let Some(call) = pending.get(&request_id) {
                        call.progress.report(progress);
                    }
                }
                Some(ServerMessage::ToolResponse { request_id, content, is_error }) => {
                    if let Some(call) = pending.remove(&request_id) {
                        let _ = call.reply.send(Ok(ToolResponse { content, is_error }));
                    }
                }
                Some(ServerMessage::Error { request_id, error }) => {
                    if let Some(call) = pending.remove(&request_id) {
                        let _ = call.reply.send(Err(error));
                    }
                }
                _ => return Err(invalid_message()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use serde_json::json;

    use super::*;
    use crate::protocol::CallContext;

    fn private_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ipc-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn list() -> ToolCall {
        ToolCall::parse("list_servers", json!({})).unwrap()
    }

    #[tokio::test]
    async fn recovers_stale_socket_and_refuses_to_replace_live_same_uid_listener() {
        let dir = private_dir();
        let path = dir.path().join("mcp.sock");
        drop(UnixListener::bind(&path).unwrap());
        let server = IpcServer::bind(path.clone()).await.unwrap();
        assert_eq!(
            fs::symlink_metadata(server.path()).unwrap().mode() & 0o7777,
            0o600
        );
        let error = IpcServer::bind(path.clone()).await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);

        let (stop, stopping) = oneshot::channel::<()>();
        let task = tokio::spawn(server.run(
            |context: CallContext, _| async move {
                Ok(ToolResponse::executed(json!(context.client.name)))
            },
            async {
                let _ = stopping.await;
            },
        ));
        let client = IpcClient::connect(
            &path,
            &ClientInfo {
                name: "peer".into(),
                version: "7".into(),
            },
        )
        .await
        .unwrap();
        let result = client
            .call(list(), ProgressReporter::default())
            .await
            .unwrap();
        assert_eq!(result.content[0].text, "\"peer\"");
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn refuses_public_parents_and_non_socket_entries() {
        let dir = private_dir();
        let path = dir.path().join("mcp.sock");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let error = IpcServer::bind(path.clone()).await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, b"keep").unwrap();
        let error = IpcServer::bind(path.clone()).await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(&path).unwrap();
        let server = IpcServer::bind(path.clone()).await.unwrap();
        drop(server);
        assert!(!path.exists());
    }

    struct PendingGuard(Option<oneshot::Sender<()>>);

    impl Drop for PendingGuard {
        fn drop(&mut self) {
            if let Some(dropped) = self.0.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[tokio::test]
    async fn streams_progress_and_cancels_one_call_without_blocking_other_clients() {
        let dir = private_dir();
        let path = dir.path().join("mcp.sock");
        let server = IpcServer::bind(path.clone()).await.unwrap();
        let (stop, stopping) = oneshot::channel::<()>();
        let (drop_tx, dropped) = oneshot::channel();
        let drop_tx = std::sync::Mutex::new(Some(drop_tx));
        let task = tokio::spawn(server.run(
            move |context: CallContext, tool: ToolCall| {
                let waiting = context.client.name == "wait" && tool.name() == "list_servers";
                let guard = PendingGuard(if waiting {
                    drop_tx.lock().unwrap().take()
                } else {
                    None
                });
                async move {
                    if waiting {
                        assert!(context.progress.requested());
                        context.progress.report(Progress {
                            progress: 1,
                            total: Some(3),
                            message: "wait".into(),
                        });
                        context.progress.report(Progress {
                            progress: 2,
                            total: Some(3),
                            message: "still waiting".into(),
                        });
                        pending::<()>().await;
                    }
                    drop(guard);
                    Ok(ToolResponse::executed(
                        json!({"version": context.client.version}),
                    ))
                }
            },
            async {
                let _ = stopping.await;
            },
        ));

        let first = IpcClient::connect(
            &path,
            &ClientInfo {
                name: "wait".into(),
                version: "1".into(),
            },
        )
        .await
        .unwrap();
        let second = IpcClient::connect(
            &path,
            &ClientInfo {
                name: "other".into(),
                version: "2".into(),
            },
        )
        .await
        .unwrap();
        let (progress, mut events) = mpsc::channel(2);
        let request = tokio::spawn({
            let first = first.clone();
            async move {
                first
                    .call(list(), ProgressReporter::current(true, progress))
                    .await
            }
        });
        for step in [1, 2] {
            let (_, progress) = timeout(IO_TIMEOUT, events.recv()).await.unwrap().unwrap();
            assert_eq!(progress.progress, step);
            assert_eq!(progress.total, Some(3));
        }
        let response = timeout(IO_TIMEOUT, second.call(list(), ProgressReporter::default()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.content[0].text, "{\"version\":\"2\"}");
        let other_tool = ToolCall::parse("get_ui_context", json!({})).unwrap();
        let response = timeout(
            IO_TIMEOUT,
            first.call(other_tool, ProgressReporter::default()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.content[0].text, "{\"version\":\"1\"}");

        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        timeout(IO_TIMEOUT, dropped).await.unwrap().unwrap();
        stop.send(()).unwrap();
        task.await.unwrap().unwrap();
    }
}
