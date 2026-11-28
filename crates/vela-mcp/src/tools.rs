//! Catálogo de herramientas v1 y despacho tipado al ejecutor de M2.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::{executor::*, protocol::RpcError};

fn schema(required: &[(&str, Value)], optional: &[(&str, Value)]) -> Value {
    let properties: Map<_, _> = required
        .iter()
        .chain(optional)
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    json!({
        "type": "object", "properties": properties, "additionalProperties": false,
        "required": required.iter().map(|(name, _)| name).collect::<Vec<_>>(),
    })
}

fn string(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn optional(mut property: Value) -> Value {
    property["type"] = json!([property["type"], "null"]);
    property
}

fn integer(description: &str, max: u64) -> Value {
    json!({"type": "integer", "description": description, "minimum": 0, "maximum": max})
}

fn dispatch_result<T: Serialize>(
    outcome: Result<ToolOutcome<T>, ExecutorError>,
) -> Result<ToolOutcome<Value>, ExecutorError> {
    Ok(match outcome? {
        ToolOutcome::Executed(value) => {
            ToolOutcome::Executed(serde_json::to_value(value).map_err(std::io::Error::other)?)
        }
        ToolOutcome::Denied(decision) => ToolOutcome::Denied(decision),
        ToolOutcome::NeedsApproval { decision, preview } => {
            ToolOutcome::NeedsApproval { decision, preview }
        }
    })
}

macro_rules! invoke {
    ($result:expr, sync) => {
        dispatch_result($result)
    };
    ($result:expr, async) => {
        dispatch_result($result.await)
    };
}

macro_rules! tools {
    ($(
        $variant:ident($input:ty) => $name:ident $kind:ident, $description:literal,
        { $($required:ident: $req:expr),* }, { $($optional:ident: $opt:expr),* };
    )*) => {
        /// Llamada v1 con argumentos validados por el transporte.
        #[derive(Clone, Debug)]
        pub enum ToolCall {
            $($variant($input),)*
        }

        impl ToolCall {
            pub fn parse(name: &str, arguments: Value) -> Result<Self, RpcError> {
                let fields = arguments.as_object().ok_or_else(RpcError::invalid_params)?;
                if name == "fs_patch"
                    && let Some(edits) = fields.get("edits").and_then(Value::as_array)
                    && edits.iter().filter_map(Value::as_object).any(|edit| {
                        edit.keys().any(|key| key != "old_text" && key != "new_text")
                    })
                {
                    return Err(RpcError::invalid_params());
                }
                match name {
                    $(stringify!($name) => {
                        let keys: &[&str] = &[$(stringify!($required),)* $(stringify!($optional),)*];
                        if fields.keys().any(|key| !keys.contains(&key.as_str())) {
                            return Err(RpcError::invalid_params());
                        }
                        serde_json::from_value(arguments)
                            .map(Self::$variant)
                            .map_err(|_| RpcError::invalid_params())
                    })*
                    _ => Err(RpcError::new(-32602, "Herramienta desconocida.")),
                }
            }

            pub fn name(&self) -> &'static str {
                match self { $(Self::$variant(_) => stringify!($name),)* }
            }

            pub fn arguments(&self) -> Value {
                match self { $(Self::$variant(input) => json!(input),)* }
            }

            pub async fn execute<B: McpBackend>(
                self, executor: &Executor<B>, mode: ApprovalMode,
            ) -> Result<ToolOutcome<Value>, ExecutorError> {
                match self {
                    $(Self::$variant(input) => invoke!(executor.$name(input, mode), $kind),)*
                }
            }
        }

        /// Definiciones MCP para `tools/list`.
        pub fn definitions() -> Vec<Value> {
            vec![$(json!({
                "name": stringify!($name), "description": $description,
                "inputSchema": schema(
                    &[$((stringify!($required), $req)),*],
                    &[$((stringify!($optional), $opt)),*],
                ),
            })),*]
        }
    };
}

tools! {
    ListServers(ListServersInput) => list_servers sync, "Lista los servidores disponibles para el agente.",
    {}, {};
    DescribePermissions(DescribePermissionsInput) => describe_permissions sync, "Consulta las operaciones y el ámbito de permisos de un servidor.",
    { server_id: server_id() }, {};
    GetUiContext(GetUiContextInput) => get_ui_context sync, "Devuelve el servidor y la ruta activos cuando Vela comparte ese contexto.",
    {}, { server_id: optional(string("ID del servidor que se espera encontrar activo.")) };
    FsList(FsListInput) => fs_list async, "Lista un directorio remoto dentro del ámbito permitido.",
    { server_id: server_id(), path: remote_path() },
    { depth: optional(integer("Profundidad (1 por defecto, máximo efectivo 5).", u8::MAX.into())), pattern: optional(string("Glob para filtrar entradas.")) };
    FsStat(FsStatInput) => fs_stat async, "Consulta los metadatos de un archivo o directorio remoto.",
    { server_id: server_id(), path: remote_path() }, {};
    FsRead(FsReadInput) => fs_read async, "Lee una página de un archivo remoto de texto (hasta 512 KiB).",
    { server_id: server_id(), path: remote_path() },
    { start_line: optional(integer("Primera línea solicitada (0-based: la primera línea es 0).", usize::MAX as u64)), max_lines: optional(integer("Líneas solicitadas (500 por defecto, máximo efectivo 5000).", usize::MAX as u64)) };
    FsWrite(FsWriteInput) => fs_write async, "Crea o reemplaza el contenido de un archivo de texto remoto.",
    { server_id: server_id(), path: remote_path(), content: string("Contenido UTF-8 completo.") },
    { mode: optional(file_mode()), reason: approval_reason() };
    FsPatch(FsPatchInput) => fs_patch async, "Aplica sustituciones precisas a un archivo de texto remoto.",
    { server_id: server_id(), path: remote_path(), edits: edit_schema() },
    { reason: approval_reason() };
    FsMkdir(FsMkdirInput) => fs_mkdir async, "Crea un directorio remoto.",
    { server_id: server_id(), path: remote_path() }, { reason: approval_reason() };
    FsMove(FsMoveInput) => fs_move async, "Mueve o renombra una ruta remota.",
    { server_id: server_id(), from: string("Ruta remota de origen."), to: string("Ruta remota de destino.") }, { reason: approval_reason() };
    FsDelete(FsDeleteInput) => fs_delete async, "Elimina un archivo o directorio remoto.",
    { server_id: server_id(), path: remote_path() },
    { recursive: json!({"type": "boolean", "default": false, "description": "Permite borrar un árbol de directorios."}), reason: approval_reason() };
    FsChmod(FsChmodInput) => fs_chmod async, "Cambia los permisos de una ruta remota.",
    { server_id: server_id(), path: remote_path(), mode: file_mode() }, { reason: approval_reason() };
    TransferUpload(TransferUploadInput) => transfer_upload async, "Sube un archivo local a una ruta remota permitida.",
    { server_id: server_id(), local_path: string("Ruta local de origen."), remote_path: string("Ruta remota de destino.") }, { reason: approval_reason() };
    TransferDownload(TransferDownloadInput) => transfer_download async, "Descarga un archivo remoto a una ruta local permitida.",
    { server_id: server_id(), remote_path: string("Ruta remota de origen."), local_path: string("Ruta local de destino.") }, { reason: approval_reason() };
    ExecCommand(ExecCommandInput) => exec_command async, "Ejecuta un comando remoto no interactivo sujeto a la política del servidor.",
    { server_id: server_id(), command: string("Comando en sintaxis de shell POSIX.") },
    { cwd: optional(string("Directorio de trabajo remoto.")), timeout_secs: optional(integer("Tiempo en segundos (30 por defecto, rango efectivo 1–300).", u64::MAX)), reason: approval_reason() };
}

fn server_id() -> Value {
    string("ID devuelto por list_servers.")
}

fn remote_path() -> Value {
    string("Ruta remota absoluta.")
}

fn file_mode() -> Value {
    integer(
        "Modo POSIX en decimal (por ejemplo 420 para 0644).",
        u32::MAX.into(),
    )
}

fn approval_reason() -> Value {
    optional(string("Motivo para mostrar en la aprobación."))
}

fn edit_schema() -> Value {
    json!({"type": "array", "description": "Sustituciones aplicadas en orden.", "items": schema(
        &[("old_text", string("Texto que debe aparecer exactamente una vez.")),
          ("new_text", string("Texto de reemplazo."))], &[]
    )})
}

impl ToolCall {
    /// Servidor que necesita una sesión SSH para esta llamada. Los metadatos
    /// locales nunca conectan por SSH ni abren la interfaz.
    pub fn session_server(&self) -> Option<&str> {
        Some(match self {
            Self::ListServers(_) | Self::DescribePermissions(_) | Self::GetUiContext(_) => {
                return None;
            }
            Self::FsList(input) => &input.server_id,
            Self::FsStat(input) => &input.server_id,
            Self::FsRead(input) => &input.server_id,
            Self::FsWrite(input) => &input.server_id,
            Self::FsPatch(input) => &input.server_id,
            Self::FsMkdir(input) => &input.server_id,
            Self::FsMove(input) => &input.server_id,
            Self::FsDelete(input) => &input.server_id,
            Self::FsChmod(input) => &input.server_id,
            Self::TransferUpload(input) => &input.server_id,
            Self::TransferDownload(input) => &input.server_id,
            Self::ExecCommand(input) => &input.server_id,
        })
    }
}
