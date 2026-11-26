//! Servidor MCP JSON-RPC 2.0 sobre stdio.

use std::{collections::HashMap, future::Future, io, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
    task::{AbortHandle, Id, JoinSet},
    time::{Instant, MissedTickBehavior, interval_at},
};

use crate::{
    ExecutorError, PolicyDecision, ToolOutcome, ndjson,
    tools::{self, ToolCall},
};

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const MAX_CALLS: usize = 32;
const PROGRESS_INTERVAL: Duration = Duration::from_secs(2);
const SUPPORTED_VERSIONS: [&str; 2] = ["2025-03-26", "2024-11-05"];

/// Identidad declarada por el cliente durante la inicialización de MCP.
/// Se conserva para auditoría y aprobaciones; la autenticación IPC usa el UID.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// Progreso de una llamada, por ejemplo mientras se muestra una aprobación.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Progress {
    pub progress: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub message: String,
}

pub(crate) type ProgressTx = mpsc::Sender<(Id, Progress)>;

/// Emisor acotado de progreso. Descarta actualizaciones si ya hay demasiadas
/// en cola o si el cliente no solicitó progreso.
#[derive(Clone, Default)]
pub struct ProgressReporter(Option<(Id, ProgressTx)>);

impl ProgressReporter {
    pub(crate) fn current(enabled: bool, tx: ProgressTx) -> Self {
        Self(enabled.then(|| (tokio::task::id(), tx)))
    }

    pub fn requested(&self) -> bool {
        self.0.is_some()
    }

    pub fn report(&self, progress: Progress) {
        if let Some((id, tx)) = &self.0 {
            let _ = tx.try_send((*id, progress));
        }
    }
}

#[derive(Clone)]
pub struct CallContext {
    pub client: ClientInfo,
    pub progress: ProgressReporter,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params() -> Self {
        Self::new(-32602, "Parámetros no válidos.")
    }

    pub(crate) fn busy() -> Self {
        Self::new(-32000, "Hay demasiadas llamadas pendientes.")
    }

    pub(crate) fn internal() -> Self {
        Self::new(-32603, "No se pudo completar la llamada.")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextContent {
    #[serde(rename = "type")]
    kind: TextKind,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum TextKind {
    Text,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResponse {
    pub content: Vec<TextContent>,
    pub is_error: bool,
}

impl ToolResponse {
    pub fn executed(value: Value) -> Self {
        Self::text(value.to_string(), false)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::text(message.into(), true)
    }

    pub fn denied(decision: &PolicyDecision) -> Self {
        Self::error(
            json!({
                "reason": decision.reason,
                "matched_rule": decision.matched_rule,
            })
            .to_string(),
        )
    }

    pub fn from_outcome(outcome: Result<ToolOutcome<Value>, ExecutorError>) -> Self {
        match outcome {
            Ok(ToolOutcome::Executed(value)) => Self::executed(value),
            Ok(ToolOutcome::Denied(decision) | ToolOutcome::NeedsApproval { decision, .. }) => {
                Self::denied(&decision)
            }
            Err(error) => Self::execution_error(&error),
        }
    }

    pub(crate) fn execution_error(error: &ExecutorError) -> Self {
        let message = match error {
            ExecutorError::UnknownServer(_) => "Servidor desconocido.",
            ExecutorError::NoSession(_) => "El servidor no tiene una sesión conectada.",
            ExecutorError::InvalidPath(_) => "Ruta no válida.",
            ExecutorError::NotFound(_) => "El recurso solicitado no está disponible.",
            ExecutorError::BinaryFile(_) => "El archivo no es texto UTF-8.",
            ExecutorError::FileTooLarge(_) => "El archivo supera el límite de tamaño.",
            ExecutorError::Patch(_) => "No se pudo aplicar la edición al archivo.",
            ExecutorError::SnapshotNotFound(_) => "La copia de seguridad no existe.",
            ExecutorError::Io(_) | ExecutorError::Core(_) => "Falló la operación de E/S o SSH.",
        };
        Self::error(message)
    }

    fn text(text: String, is_error: bool) -> Self {
        Self {
            content: vec![TextContent {
                kind: TextKind::Text,
                text,
            }],
            is_error,
        }
    }
}

/// Punto de integración de stdio e IPC. Cancelar una llamada descarta su
/// futuro; una aprobación debe mantener su inscripción ligada a ese futuro.
pub trait ToolHandler: Send + Sync + 'static {
    fn call(
        &self,
        context: CallContext,
        tool: ToolCall,
    ) -> impl Future<Output = Result<ToolResponse, RpcError>> + Send;
}

impl<F, Fut> ToolHandler for F
where
    F: Fn(CallContext, ToolCall) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<ToolResponse, RpcError>> + Send,
{
    async fn call(&self, context: CallContext, tool: ToolCall) -> Result<ToolResponse, RpcError> {
        self(context, tool).await
    }
}

pub(crate) type CallTasks = JoinSet<Result<ToolResponse, RpcError>>;

pub(crate) fn spawn_call(
    tasks: &mut CallTasks,
    handler: Arc<impl ToolHandler>,
    client: ClientInfo,
    tool: ToolCall,
    progress: Option<ProgressTx>,
) -> AbortHandle {
    tasks.spawn(async move {
        let progress = progress.map_or_else(ProgressReporter::default, |tx| {
            ProgressReporter::current(true, tx)
        });
        handler.call(CallContext { client, progress }, tool).await
    })
}

#[derive(Deserialize)]
struct Initialize {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
}

#[derive(Deserialize)]
struct CallParams {
    name: String,
    #[serde(default = "empty_arguments")]
    arguments: Value,
    #[serde(default, rename = "_meta")]
    meta: CallMeta,
}

#[derive(Default, Deserialize)]
struct CallMeta {
    #[serde(rename = "progressToken")]
    progress_token: Option<Value>,
}

fn empty_arguments() -> Value {
    json!({})
}

struct PendingCall {
    id: Value,
    batch: Option<u64>,
    task: AbortHandle,
    token: Option<Value>,
    cancelled: bool,
    last_progress: Option<u64>,
    next_progress: Instant,
}

impl PendingCall {
    fn cancel(&mut self) {
        self.task.abort();
        self.cancelled = true;
        self.token = None;
    }

    fn progress(&mut self, event: Progress) -> Option<Value> {
        let token = self.token.as_ref()?;
        let progress = self.last_progress.map_or(event.progress, |last| {
            event.progress.max(last.saturating_add(1))
        });
        self.last_progress = Some(progress);
        self.next_progress = Instant::now() + PROGRESS_INTERVAL;
        let mut params = json!({
            "progressToken": token, "progress": progress, "message": event.message,
        });
        if let Some(total) = event.total.filter(|&total| total >= progress) {
            params["total"] = json!(total);
        }
        Some(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}))
    }
}

fn valid_id(value: &Value) -> bool {
    value.is_string() || value.is_i64() || value.is_u64()
}

fn error_response(id: Value, error: RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

fn response(id: Value, result: Result<Value, RpcError>) -> Value {
    match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => error_response(id, error),
    }
}

#[derive(Default)]
struct Batch {
    pending: usize,
    replies: Vec<Value>,
}

#[derive(Default)]
struct StdioState {
    client: Option<ClientInfo>,
    pending: HashMap<Id, PendingCall>,
    tasks: CallTasks,
    batches: HashMap<u64, Batch>,
    next_batch: u64,
}

impl StdioState {
    fn receive(
        &mut self,
        frame: &[u8],
        handler: &Arc<impl ToolHandler>,
        progress: &ProgressTx,
    ) -> Option<Value> {
        let value = match serde_json::from_slice::<Value>(frame) {
            Ok(value) => value,
            Err(_) => {
                return Some(error_response(
                    Value::Null,
                    RpcError::new(-32700, "JSON no válido."),
                ));
            }
        };
        match value {
            Value::Array(items) if items.is_empty() || items.len() > MAX_CALLS => {
                Some(error_response(
                    Value::Null,
                    RpcError::new(-32600, "Lote vacío o demasiado grande."),
                ))
            }
            Value::Array(items) => {
                let batch = self.next_batch;
                self.next_batch += 1;
                self.batches.insert(batch, Batch::default());
                for request in items {
                    if let Some(reply) = self.request(request, Some(batch), handler, progress) {
                        self.batches
                            .get_mut(&batch)
                            .expect("active batch")
                            .replies
                            .push(reply);
                    }
                }
                self.finish_batch(batch)
            }
            request => self.request(request, None, handler, progress),
        }
    }

    fn request(
        &mut self,
        value: Value,
        batch: Option<u64>,
        handler: &Arc<impl ToolHandler>,
        progress: &ProgressTx,
    ) -> Option<Value> {
        let (version, method, id, params) = (
            &value["jsonrpc"],
            value["method"].as_str(),
            value.get("id"),
            value.get("params"),
        );
        let Some(method) = method.filter(|_| version == "2.0" && id.is_none_or(valid_id)) else {
            return Some(error_response(
                Value::Null,
                RpcError::new(-32600, "Solicitud no válida."),
            ));
        };
        let Some(id) = id.cloned() else {
            if method == "notifications/cancelled"
                && let Some(id) = params
                    .and_then(|p| p.get("requestId"))
                    .filter(|id| valid_id(id))
                && let Some(call) = self.pending.values_mut().find(|call| call.id == *id)
            {
                call.cancel();
            }
            return None;
        };
        if self.pending.values().any(|call| call.id == id) {
            return Some(error_response(
                Value::Null,
                RpcError::new(-32600, "ID ya en uso."),
            ));
        }
        let params = params.cloned().unwrap_or_else(empty_arguments);
        if !params.is_object() {
            return Some(error_response(id, RpcError::invalid_params()));
        }
        let result = match method {
            "initialize" if batch.is_some() => {
                Err(RpcError::new(-32600, "Inicialización dentro de un lote."))
            }
            "initialize" => self.initialize(params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools::definitions()})),
            "tools/call" => match self.start_call(id.clone(), batch, params, handler, progress) {
                Ok(()) => return None,
                Err(error) => Err(error),
            },
            _ => Err(RpcError::new(-32601, "Método desconocido.")),
        };
        Some(response(id, result))
    }

    fn initialize(&mut self, params: Value) -> Result<Value, RpcError> {
        let input: Initialize =
            serde_json::from_value(params).map_err(|_| RpcError::invalid_params())?;
        if self.client.is_some() {
            return Err(RpcError::new(-32600, "MCP ya se ha inicializado."));
        }
        self.client = Some(input.client_info);
        let version = if SUPPORTED_VERSIONS.contains(&input.protocol_version.as_str()) {
            input.protocol_version.as_str()
        } else {
            SUPPORTED_VERSIONS[0]
        };
        Ok(json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "vela", "version": VERSION},
        }))
    }

    fn start_call(
        &mut self,
        id: Value,
        batch: Option<u64>,
        params: Value,
        handler: &Arc<impl ToolHandler>,
        progress: &ProgressTx,
    ) -> Result<(), RpcError> {
        if let Some(token) = params.pointer("/_meta/progressToken")
            && (!valid_id(token)
                || self
                    .pending
                    .values()
                    .any(|call| call.token.as_ref() == Some(token)))
        {
            return Err(RpcError::invalid_params());
        }
        let input: CallParams =
            serde_json::from_value(params).map_err(|_| RpcError::invalid_params())?;
        let token = input.meta.progress_token;
        let tool = ToolCall::parse(&input.name, input.arguments)?;
        let client = self.client.clone().ok_or_else(|| {
            RpcError::new(-32002, "Inicializa MCP antes de llamar a una herramienta.")
        })?;
        if self.tasks.len() >= MAX_CALLS {
            return Err(RpcError::busy());
        }
        let progress = token.as_ref().map(|_| progress.clone());
        let task = spawn_call(&mut self.tasks, handler.clone(), client, tool, progress);
        if let Some(batch) = batch {
            self.batches.get_mut(&batch).expect("active batch").pending += 1;
        }
        self.pending.insert(
            task.id(),
            PendingCall {
                id,
                batch,
                task,
                token,
                cancelled: false,
                last_progress: None,
                next_progress: Instant::now() + PROGRESS_INTERVAL,
            },
        );
        Ok(())
    }

    fn complete(&mut self, id: Id, result: Result<ToolResponse, RpcError>) -> Option<Value> {
        let call = self.pending.remove(&id)?;
        let reply = (!call.cancelled).then(|| response(call.id, result.map(|value| json!(value))));
        let Some(batch_id) = call.batch else {
            return reply;
        };
        let batch = self.batches.get_mut(&batch_id).expect("active batch");
        batch.pending -= 1;
        batch.replies.extend(reply);
        self.finish_batch(batch_id)
    }

    fn finish_batch(&mut self, id: u64) -> Option<Value> {
        if self.batches.get(&id)?.pending != 0 {
            return None;
        }
        let batch = self.batches.remove(&id)?;
        (!batch.replies.is_empty()).then(|| Value::Array(batch.replies))
    }
}

/// Sirve MCP hasta EOF. Las llamadas activas se cancelan antes de regresar.
pub async fn serve_stdio<R, W, H>(mut reader: R, mut writer: W, handler: H) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    H: ToolHandler,
{
    let handler = Arc::new(handler);
    let mut state = StdioState::default();
    let mut frame = Vec::new();
    let (progress_tx, mut progress_rx) = mpsc::channel(64);
    let mut ticker = interval_at(Instant::now() + PROGRESS_INTERVAL, PROGRESS_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let result = async {
        loop {
            tokio::select! {
                biased;
                Some((id, event)) = progress_rx.recv() => {
                    if let Some(event) = state.pending.get_mut(&id).and_then(|call| call.progress(event)) {
                        ndjson::write(&mut writer, &event).await?;
                    }
                }
                Some(result) = state.tasks.join_next_with_id(), if !state.tasks.is_empty() => {
                    let (id, result) = match result {
                        Ok((id, result)) => (id, result),
                        Err(error) => (error.id(), Err(RpcError::internal())),
                    };
                    if let Some(reply) = state.complete(id, result) {
                        match ndjson::write(&mut writer, &reply).await {
                            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                                fn truncated(reply: &Value) -> Value {
                                    let fallback = ToolResponse::error("La respuesta supera el límite de tamaño.");
                                    response(reply["id"].clone(), Ok(json!(fallback)))
                                }
                                let fallback = reply.as_array().map_or_else(
                                    || truncated(&reply),
                                    |replies| Value::Array(replies.iter().map(truncated).collect()),
                                );
                                ndjson::write(&mut writer, &fallback).await?;
                            }
                            result => result?,
                        }
                    }
                }
                _ = ticker.tick() => {
                    let now = Instant::now();
                    for call in state.pending.values_mut().filter(|call| call.next_progress <= now) {
                        if let Some(event) = call.progress(Progress {
                            progress: 1, total: None, message: "Esperando a Vela.".into(),
                        }) {
                            ndjson::write(&mut writer, &event).await?;
                        }
                    }
                }
                result = ndjson::read(&mut reader, &mut frame) => {
                    if !result? {
                        return writer.flush().await;
                    }
                    if let Some(reply) = state.receive(&frame, &handler, &progress_tx) {
                        ndjson::write(&mut writer, &reply).await?;
                    }
                    frame.clear();
                }
            }
        }
    }.await;
    state.tasks.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use tokio::{
        io::{AsyncBufReadExt, BufReader, DuplexStream, duplex},
        sync::oneshot,
        task::JoinHandle,
        time::timeout,
    };

    use super::*;

    async fn read_reply(reader: &mut BufReader<DuplexStream>) -> Value {
        let mut line = String::new();
        assert_ne!(
            timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        serde_json::from_str(&line).unwrap()
    }

    fn start(
        handler: impl ToolHandler,
    ) -> (
        DuplexStream,
        BufReader<DuplexStream>,
        JoinHandle<io::Result<()>>,
    ) {
        let (input, reader) = duplex(1024);
        let (writer, output) = duplex(1024);
        let server = tokio::spawn(serve_stdio(BufReader::new(reader), writer, handler));
        (input, BufReader::new(output), server)
    }

    fn initialize(version: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": version, "capabilities": {},
            "clientInfo": {"name": "client", "version": "11"},
        }})
    }

    fn call(id: u64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn list_servers(id: u64) -> Value {
        call(id, "tools/call", json!({"name": "list_servers"}))
    }

    #[tokio::test]
    async fn negotiates_version_preserves_identity_and_lists_exactly_fifteen_tools() {
        let (mut input, mut output, server) =
            start(|context: CallContext, tool: ToolCall| async move {
                assert_eq!(
                    context.client,
                    ClientInfo {
                        name: "client".into(),
                        version: "11".into()
                    }
                );
                assert_eq!(tool.name(), "list_servers");
                Ok(ToolResponse::executed(json!([])))
            });
        ndjson::write(&mut input, &initialize("2024-11-05"))
            .await
            .unwrap();
        assert_eq!(
            read_reply(&mut output).await["result"],
            json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "vela", "version": VERSION},
                "capabilities": {"tools": {"listChanged": false}},
            })
        );
        ndjson::write(
            &mut input,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await
        .unwrap();
        ndjson::write(&mut input, &call(2, "ping", json!({})))
            .await
            .unwrap();
        assert_eq!(
            read_reply(&mut output).await,
            response(json!(2), Ok(json!({})))
        );
        ndjson::write(&mut input, &call(3, "tools/list", json!({})))
            .await
            .unwrap();
        let reply = read_reply(&mut output).await;
        let defs = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = defs
            .iter()
            .map(|def| def["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_servers",
                "describe_permissions",
                "get_ui_context",
                "fs_list",
                "fs_stat",
                "fs_read",
                "fs_write",
                "fs_patch",
                "fs_mkdir",
                "fs_move",
                "fs_delete",
                "fs_chmod",
                "transfer_upload",
                "transfer_download",
                "exec_command"
            ]
        );
        assert!(
            defs.iter()
                .all(|def| def["inputSchema"]["type"] == "object")
        );
        assert_eq!(
            defs[7]["inputSchema"]["properties"]["edits"]["items"]["required"],
            json!(["old_text", "new_text"])
        );
        ndjson::write(&mut input, &list_servers(4)).await.unwrap();
        assert_eq!(
            read_reply(&mut output).await["result"],
            json!({
                "content": [{"type": "text", "text": "[]"}], "isError": false,
            })
        );
        drop(input);
        server.await.unwrap().unwrap();
        assert_eq!(output.read_line(&mut String::new()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn produces_standard_errors_and_handles_mixed_batches() {
        let (mut input, mut output, server) =
            start(|_, _| async { Ok(ToolResponse::executed(json!([]))) });
        input.write_all(b"{no json}\n").await.unwrap();
        assert_eq!(read_reply(&mut output).await["error"]["code"], -32700);
        ndjson::write(&mut input, &json!({"method": "ping", "id": 2}))
            .await
            .unwrap();
        assert_eq!(read_reply(&mut output).await["error"]["code"], -32600);
        ndjson::write(&mut input, &initialize("unsupported"))
            .await
            .unwrap();
        assert_eq!(
            read_reply(&mut output).await["result"]["protocolVersion"],
            "2025-03-26"
        );
        ndjson::write(&mut input, &json!([
            call(2, "missing", json!({})),
            call(3, "tools/call", json!({"name": "fs_read", "arguments": {"server_id": "srv"}})),
            list_servers(4),
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            42,
        ])).await.unwrap();
        let replies = read_reply(&mut output).await;
        let replies = replies.as_array().unwrap();
        assert_eq!(replies.len(), 4);
        assert_eq!(
            replies.iter().find(|r| r["id"] == 2).unwrap()["error"]["code"],
            -32601
        );
        assert_eq!(
            replies.iter().find(|r| r["id"] == 3).unwrap()["error"]["code"],
            -32602
        );
        assert_eq!(
            replies.iter().find(|r| r["id"] == 4).unwrap()["result"]["isError"],
            false
        );
        assert_eq!(
            replies.iter().find(|r| r["id"].is_null()).unwrap()["error"]["code"],
            -32600
        );
        drop(input);
        server.await.unwrap().unwrap();
    }

    struct OnDrop(Option<oneshot::Sender<()>>);

    impl Drop for OnDrop {
        fn drop(&mut self) {
            if let Some(send) = self.0.take() {
                let _ = send.send(());
            }
        }
    }

    #[tokio::test]
    async fn emits_progress_while_waiting_and_cancels_without_reply() {
        let (dropped, drop_seen) = oneshot::channel();
        let dropped = std::sync::Mutex::new(Some(dropped));
        let (mut input, mut output, server) = start(move |context: CallContext, _| {
            let dropped = OnDrop(dropped.lock().unwrap().take());
            async move {
                context.progress.report(Progress {
                    progress: 3,
                    total: Some(10),
                    message: "Aprobación".into(),
                });
                pending::<()>().await;
                drop(dropped);
                unreachable!()
            }
        });
        ndjson::write(&mut input, &initialize("2025-03-26"))
            .await
            .unwrap();
        read_reply(&mut output).await;
        ndjson::write(
            &mut input,
            &call(
                2,
                "tools/call",
                json!({
                    "name": "list_servers", "_meta": {"progressToken": "t"},
                }),
            ),
        )
        .await
        .unwrap();
        let first = read_reply(&mut output).await;
        assert_eq!(first["method"], "notifications/progress");
        assert_eq!(
            first["params"],
            json!({"progress": 3, "total": 10, "message": "Aprobación", "progressToken": "t"})
        );
        // Leave a partial request buffered across the automatic heartbeat.
        input
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":")
            .await
            .unwrap();
        let next = read_reply(&mut output).await;
        assert_eq!(next["method"], "notifications/progress");
        assert_eq!(next["params"]["progress"], 4);
        input
            .write_all(b"\"notifications/cancelled\",\"params\":{\"requestId\":2}}\n")
            .await
            .unwrap();
        timeout(Duration::from_secs(1), drop_seen)
            .await
            .unwrap()
            .unwrap();
        ndjson::write(&mut input, &call(3, "ping", json!({})))
            .await
            .unwrap();
        assert_eq!(read_reply(&mut output).await["id"], 3);
        drop(input);
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn eof_drops_pending_tool_futures() {
        let (drop_tx, dropped) = oneshot::channel();
        let drop_tx = std::sync::Mutex::new(Some(drop_tx));
        let (mut input, mut output, server) = start(move |context: CallContext, _| {
            let drop_tx = OnDrop(drop_tx.lock().unwrap().take());
            async move {
                context.progress.report(Progress {
                    progress: 1,
                    total: None,
                    message: "pending".into(),
                });
                pending::<()>().await;
                drop(drop_tx);
                unreachable!()
            }
        });
        ndjson::write(&mut input, &initialize("2025-03-26"))
            .await
            .unwrap();
        read_reply(&mut output).await;
        ndjson::write(
            &mut input,
            &call(
                2,
                "tools/call",
                json!({
                    "name": "list_servers", "_meta": {"progressToken": 3},
                }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            read_reply(&mut output).await["method"],
            "notifications/progress"
        );
        drop(input);
        server.await.unwrap().unwrap();
        dropped.await.unwrap();
        assert_eq!(output.read_line(&mut String::new()).await.unwrap(), 0);
    }

    #[test]
    fn execution_errors_hide_internal_paths_and_diagnostics() {
        let result = ToolResponse::from_outcome(Err(ExecutorError::Io(io::Error::other(
            "/tmp/vm-secret/socket credentials",
        ))));
        assert!(result.is_error);
        assert_eq!(result.content[0].text, "Falló la operación de E/S o SSH.");
    }
}
