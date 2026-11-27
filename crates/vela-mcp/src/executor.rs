//! Ejecutor de las herramientas MCP v1 sobre sesiones `vela-core`.
//!
//! Todas las herramientas siguen el mismo orden: resolver rutas canónicas
//! reales, consultar Ediciones, evaluar la política con ese contexto y solo
//! entonces escribir o ejecutar. Una respuesta `Ask` devuelve una vista previa
//! y no produce efectos; `ApprovedOnce` vuelve a evaluar la política, de modo
//! que una denegación o un conflicto de ediciones sigue bloqueando la llamada.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use vela_core::{ExecOptions, ExecOutput, Session};

use crate::{
    McpAction, McpConfig, PolicyDecision, PolicyEngine, PolicyWarning, ServerIdentity,
    config::Decision,
    patch::{self, PatchError, TextEdit},
    path::{self, PathAccess, glob_match, normalize_lexical},
    rollback::RollbackStore,
};

/// Tope duro de lectura para `fs_read` (512 KiB).
pub const MAX_READ_BYTES: u64 = 512 * 1024;
/// Tope propio de lectura para parchear, independiente del límite de rollback.
/// Un archivo mayor no puede leerse, pero sí puede recibir un aviso de rollback.
pub const MAX_PATCH_BYTES: u64 = 32 * 1024 * 1024;
/// Líneas devueltas por página si el agente no indica otra cosa.
pub const DEFAULT_MAX_LINES: usize = 500;
/// Límite superior de líneas por página y de profundidad de `fs_list`.
pub const MAX_LINES: usize = 5000;
pub const MAX_DEPTH: u8 = 5;

/// Fallos previos a la evaluación de política o durante la ejecución.
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("servidor desconocido: {0}")]
    UnknownServer(String),
    #[error("el servidor {0} no tiene una sesión conectada")]
    NoSession(String),
    #[error("ruta no válida: {0}")]
    InvalidPath(String),
    #[error("ruta no encontrada: {0}")]
    NotFound(String),
    #[error("el archivo es binario: {0}")]
    BinaryFile(String),
    #[error("el archivo supera el límite de lectura para parcheo: {0}")]
    FileTooLarge(String),
    #[error("edición no válida: {0}")]
    Patch(#[from] PatchError),
    #[error("no existe una copia de seguridad con identificador {0}")]
    SnapshotNotFound(String),
    #[error("error de E/S: {0}")]
    Io(#[from] io::Error),
    #[error("SSH/SFTP: {0}")]
    Core(#[from] vela_core::Error),
}

/// Estado de aprobación de una llamada ya revisada por la persona.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalMode {
    /// Una decisión `Ask` se devuelve sin ejecutar.
    Normal,
    /// La persona aprobó la llamada: se reevalúa la política y solo se ejecuta
    /// si el resultado es `Allow` o `Ask`, nunca `Deny` ni con `edits_conflict`.
    ApprovedOnce,
}

/// Vista previa que se muestra a la persona antes de autorizar la acción.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApprovalPreview {
    pub operation: String,
    pub server_id: String,
    pub paths: Vec<String>,
    pub command: Option<String>,
    pub unified_diff: Option<String>,
    pub warnings: Vec<String>,
    /// Motivo enviado por el agente, conservado tal cual.
    #[serde(default)]
    pub agent_reason: Option<String>,
    /// Motivo de la política, distinto del motivo del agente.
    pub reason: String,
    pub matched_rule: String,
}

/// Resultado común de cualquier herramienta.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolOutcome<T> {
    Executed(T),
    NeedsApproval {
        decision: PolicyDecision,
        /// Se boxea para no inflar el tamaño del enum con la vista previa.
        preview: Box<ApprovalPreview>,
    },
    Denied(PolicyDecision),
}

impl<T> ToolOutcome<T> {
    pub fn executed(self) -> Option<T> {
        match self {
            Self::Executed(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Denied(_))
    }
}

/// Datos que el ejecutor necesita del proceso anfitrión (interfaz, ajustes...).
pub trait McpBackend {
    /// Servidores registrados y su identidad real.
    fn list_servers(&self) -> Vec<(String, ServerIdentity)>;
    /// Sesión activa de un servidor, si está conectado.
    fn session(&self, server_id: &str) -> Option<&Session>;
    /// ¿Hay ediciones locales sin subir para esa ruta canónica?
    fn edits_dirty(&self, server_id: &str, canonical_path: &str) -> bool;
    /// Contexto de la interfaz: `(server_id, ruta remota actual)`.
    fn ui_context(&self) -> Option<(String, String)>;
    /// ¿Hay interfaz gráfica abierta para resolver aprobaciones? En modo
    /// headless la política falla cerrada; M3 podrá devolver `false`.
    fn vela_gui_open(&self) -> bool {
        true
    }
    /// Directorio de configuración de Vela (siempre bloqueado para el agente).
    fn vela_config_dir(&self) -> Option<PathBuf>;
    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Ejecutor de herramientas. `policy` y `rollback` se comparten con la interfaz.
pub struct Executor<B: McpBackend> {
    pub backend: B,
    pub policy: PolicyEngine,
    pub rollback: Arc<RollbackStore>,
}

impl<B: McpBackend> Executor<B> {
    pub fn new(backend: B, config: McpConfig, rollback: Arc<RollbackStore>) -> Self {
        Self::with_policy(backend, PolicyEngine::new(config), rollback)
    }

    pub fn with_policy(backend: B, policy: PolicyEngine, rollback: Arc<RollbackStore>) -> Self {
        Self {
            backend,
            policy,
            rollback,
        }
    }

    pub(crate) fn context(&self, server_id: &str, dirty: bool) -> crate::EvalContext {
        crate::EvalContext {
            server_connected: self.backend.session(server_id).is_some(),
            vela_gui_open: self.backend.vela_gui_open(),
            open_in_edits_dirty: dirty,
            home_dir: self.backend.home_dir(),
            vela_config_dir: self.backend.vela_config_dir(),
            now: self.backend.now(),
        }
    }

    fn dirty_for(&self, server_id: &str, paths: &[String]) -> bool {
        paths
            .iter()
            .any(|path| self.backend.edits_dirty(server_id, path))
    }

    fn session(&self, server_id: &str) -> Result<&Session, ExecutorError> {
        self.backend
            .session(server_id)
            .ok_or_else(|| ExecutorError::NoSession(server_id.to_owned()))
    }

    fn gate<T>(
        &self,
        action: &McpAction,
        ctx: &crate::EvalContext,
        mode: ApprovalMode,
        preview: ApprovalPreview,
    ) -> Result<Option<ToolOutcome<T>>, ExecutorError> {
        let decision = self.policy.evaluate(action, ctx);
        self.gate_decision(decision, mode, preview)
    }

    fn gate_decision<T>(
        &self,
        decision: PolicyDecision,
        mode: ApprovalMode,
        mut preview: ApprovalPreview,
    ) -> Result<Option<ToolOutcome<T>>, ExecutorError> {
        let edits_conflict = decision
            .warnings
            .iter()
            .any(|warning| matches!(warning, PolicyWarning::EditsConflict));
        preview.warnings.extend(
            decision
                .warnings
                .iter()
                .map(|warning| format!("{warning:?}")),
        );
        preview.reason = decision.reason.clone();
        preview.matched_rule = decision.matched_rule.clone();
        if decision.outcome == Decision::Deny || edits_conflict {
            return Ok(Some(ToolOutcome::Denied(decision)));
        }
        if decision.outcome == Decision::Ask && mode == ApprovalMode::Normal {
            return Ok(Some(ToolOutcome::NeedsApproval {
                decision,
                preview: Box::new(preview),
            }));
        }
        Ok(None)
    }

    fn preview(operation: &str, server_id: &str, paths: &[String]) -> ApprovalPreview {
        ApprovalPreview {
            operation: operation.to_owned(),
            server_id: server_id.to_owned(),
            paths: paths.to_vec(),
            ..ApprovalPreview::default()
        }
    }

    fn expand_local(&self, value: &str) -> Result<PathBuf, ExecutorError> {
        if value.as_bytes().contains(&0) {
            return Err(ExecutorError::InvalidPath(value.to_owned()));
        }
        if value == "~" || value.starts_with("~/") {
            let home = self
                .backend
                .home_dir()
                .ok_or_else(|| ExecutorError::InvalidPath(value.to_owned()))?;
            let rest = value.strip_prefix("~/").unwrap_or("");
            return Ok(home.join(rest));
        }
        Ok(PathBuf::from(value))
    }

    /// Ruta canónica real del servidor. Si el destino no existe todavía se
    /// resuelve el padre y se añade el nombre normalizado sin seguir symlinks.
    async fn canonical_remote(
        &self,
        session: &Session,
        value: &str,
        must_exist: bool,
    ) -> Result<String, ExecutorError> {
        let normalized = normalize_lexical(value)
            .map_err(|err| ExecutorError::InvalidPath(format!("{value}: {err}")))?;
        if let Ok(path) = session.realpath(PathBuf::from(&normalized)).await {
            return Ok(path.to_string_lossy().into_owned());
        }
        if must_exist {
            return Err(ExecutorError::NotFound(normalized));
        }
        let requested = Path::new(&normalized);
        let parent = requested
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("/"));
        let parent = normalize_lexical(&parent.to_string_lossy())
            .map_err(|err| ExecutorError::InvalidPath(format!("{err}")))?;
        let parent = session
            .realpath(PathBuf::from(&parent))
            .await
            .map_err(|_| ExecutorError::NotFound(parent.clone()))?;
        let name = requested
            .file_name()
            .ok_or_else(|| ExecutorError::InvalidPath(normalized.clone()))?;
        let name = name.to_string_lossy();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(ExecutorError::InvalidPath(normalized));
        }
        Ok(parent.join(name.as_ref()).to_string_lossy().into_owned())
    }

    /// Ruta local canónica, con el mismo criterio para destinos nuevos.
    fn canonical_local(
        &self,
        value: &str,
        must_exist: bool,
    ) -> Result<(String, PathBuf), ExecutorError> {
        let expanded = self.expand_local(value)?;
        if expanded.exists() {
            let canonical = std::fs::canonicalize(&expanded)?;
            return Ok((canonical.to_string_lossy().into_owned(), expanded));
        }
        if must_exist {
            return Err(ExecutorError::NotFound(value.to_owned()));
        }
        let parent = expanded
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("/"));
        let parent =
            std::fs::canonicalize(parent).map_err(|_| ExecutorError::NotFound(value.to_owned()))?;
        let name = expanded
            .file_name()
            .ok_or_else(|| ExecutorError::InvalidPath(value.to_owned()))?;
        Ok((parent.join(name).to_string_lossy().into_owned(), expanded))
    }

    fn transfer_context(
        &self,
        server_id: &str,
        dirty: bool,
        canonical_path: &str,
    ) -> crate::EvalContext {
        let mut context = self.context(server_id, dirty);
        // Protect the configured spelling and its real location when Vela's
        // configuration directory itself was reached through a symlink.
        if let Some(dir) = context
            .vela_config_dir
            .as_ref()
            .and_then(|dir| dir.canonicalize().ok())
            && Path::new(canonical_path).starts_with(&dir)
        {
            context.vela_config_dir = Some(dir);
        }
        context
    }

    fn text(bytes: &[u8], path: &str) -> Result<String, ExecutorError> {
        if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
            return Err(ExecutorError::BinaryFile(path.to_owned()));
        }
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    // 1. list_servers ------------------------------------------------------

    pub fn list_servers(
        &self,
        _input: ListServersInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<Vec<AgentServerSummary>>, ExecutorError> {
        let ctx = self.context("", false);
        let preview = Self::preview("list_servers", "", &[]);
        if let Some(outcome) = self.gate(&McpAction::ListServers, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let now = self.backend.now();
        let mut servers = Vec::new();
        for (id, identity) in self.backend.list_servers() {
            let connected = self.backend.session(&id).is_some();
            if let Some(info) = self
                .policy
                .describe_server_for_agent(&id, &identity, connected, now)
            {
                servers.push(info.into());
            }
        }
        Ok(ToolOutcome::Executed(servers))
    }

    // 2. describe_permissions ----------------------------------------------

    pub fn describe_permissions(
        &self,
        input: DescribePermissionsInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<PermissionsInfo>, ExecutorError> {
        let action = McpAction::DescribePermissions {
            server_id: input.server_id.clone(),
        };
        let ctx = self.context(&input.server_id, false);
        let preview = Self::preview(
            "describe_permissions",
            &input.server_id,
            std::slice::from_ref(&input.server_id),
        );
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let config = self.policy.config();
        let policy = config
            .servers
            .get(&input.server_id)
            .cloned()
            .unwrap_or_default();
        let identity = self
            .backend
            .list_servers()
            .into_iter()
            .find(|(id, _)| id == &input.server_id)
            .map(|(_, identity)| identity)
            .unwrap_or(ServerIdentity {
                host: String::new(),
                port: 0,
                user: String::new(),
            });
        let now = self.backend.now();
        let info = self
            .policy
            .describe_server_for_agent(
                &input.server_id,
                &identity,
                self.backend.session(&input.server_id).is_some(),
                now,
            )
            .ok_or_else(|| ExecutorError::UnknownServer(input.server_id.clone()))?;
        let profile = crate::presets::resolve_profile(config, &info.effective_profile)
            .ok_or_else(|| ExecutorError::UnknownServer(info.effective_profile.clone()))?;
        let mut operations = BTreeMap::new();
        for operation in [
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
        ] {
            if let Some(decision) = profile.decision_for(operation) {
                operations.insert(operation.to_owned(), decision);
            }
        }
        let effective_exec = policy.exec_mode.unwrap_or(profile.exec_mode);
        let effective_sudo = policy.sudo.unwrap_or(profile.sudo);
        // Listas efectivas: perfil + servidor, con la denylist crítica ya
        // incluida por `PermissionProfile::denied_paths()`.
        let denied_remote_paths = merge_unique(profile.denied_paths(), &policy.denied_remote_paths);
        let allowed_remote_paths = merge_unique(
            profile.default_allowed_remote_paths.clone(),
            &policy.allowed_remote_paths,
        );
        let allowed_commands =
            merge_unique(profile.allowed_commands.clone(), &policy.allowed_commands);
        let denied_commands =
            merge_unique(profile.denied_commands.clone(), &policy.denied_commands);
        Ok(ToolOutcome::Executed(PermissionsInfo {
            server_id: info.id,
            alias: info.alias,
            notes: info.notes,
            availability: format!("{:?}", info.availability).to_lowercase(),
            connected: info.connected,
            profile_id: info.effective_profile,
            operations,
            allowed_remote_paths,
            denied_remote_paths,
            allowed_local_paths: policy.allowed_local_paths,
            allowed_commands,
            denied_commands,
            exec_mode: format!("{effective_exec:?}").to_lowercase(),
            sudo: format!("{effective_sudo:?}").to_lowercase(),
        }))
    }

    // 3. get_ui_context -----------------------------------------------------

    pub fn get_ui_context(
        &self,
        input: GetUiContextInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<UiContext>, ExecutorError> {
        // La ruta abierta pertenece al servidor activo, no al que el agente
        // diga: se comprueba primero la identidad y después la política de ese
        // servidor real.
        let (active_server, current_remote_path) = self
            .backend
            .ui_context()
            .ok_or_else(|| ExecutorError::NotFound("contexto de interfaz".into()))?;
        if let Some(requested) = &input.server_id
            && requested != &active_server
        {
            return Ok(ToolOutcome::Denied(PolicyDecision {
                outcome: Decision::Deny,
                matched_rule: "ui_context_server_mismatch".into(),
                reason: "El contexto de la interfaz pertenece a otro servidor.".into(),
                warnings: Vec::new(),
                requires_launch_vela: false,
            }));
        }
        let action = McpAction::GetUiContext {
            server_id: active_server.clone(),
        };
        let ctx = self.context(&active_server, false);
        let preview = Self::preview("get_ui_context", &active_server, &[]);
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        Ok(ToolOutcome::Executed(UiContext {
            server_id: active_server,
            current_remote_path,
        }))
    }

    // 4. fs_list ------------------------------------------------------------

    pub async fn fs_list(
        &self,
        input: FsListInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<FsListOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, true).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsList {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let preview = Self::preview(
            "fs_list",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let depth = input.depth.unwrap_or(1).clamp(1, MAX_DEPTH);
        let root = PathBuf::from(&canonical);
        let (allowed, denied) = self
            .policy
            .effective_remote_path_scopes(&input.server_id, ctx.now);
        let mut entries = Vec::new();
        collect_entries(
            session,
            &root,
            &root,
            depth,
            &mut entries,
            &allowed,
            &denied,
        )
        .await?;
        if let Some(pattern) = &input.pattern {
            entries.retain(|entry| {
                let full = root.join(&entry.path).to_string_lossy().into_owned();
                glob_match(pattern, &full) || glob_match(pattern, &entry.path)
            });
        }
        Ok(ToolOutcome::Executed(FsListOutput { entries, depth }))
    }

    // 5. fs_stat ------------------------------------------------------------

    pub async fn fs_stat(
        &self,
        input: FsStatInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<FsStatOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, true).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsStat {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let preview = Self::preview(
            "fs_stat",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let entry = session.stat(PathBuf::from(&canonical)).await?;
        Ok(ToolOutcome::Executed(FsStatOutput {
            path: canonical,
            kind: entry_kind(&entry),
            is_directory: entry.is_directory(),
            size: entry.size,
            mode: entry.mode,
            mtime_secs: entry.mtime.and_then(system_secs),
        }))
    }

    // 6. fs_read ------------------------------------------------------------

    pub async fn fs_read(
        &self,
        input: FsReadInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<FsReadOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, true).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsRead {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let preview = Self::preview(
            "fs_read",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let (bytes, file_truncated) = session
            .read_bytes_limited(PathBuf::from(&canonical), MAX_READ_BYTES)
            .await?;
        let text = Self::text(&bytes, &input.path)?;
        let all_lines: Vec<&str> = text.lines().collect();
        let total_lines = all_lines.len();
        let start_line = input.start_line.unwrap_or(0).min(total_lines);
        let max_lines = input
            .max_lines
            .unwrap_or(DEFAULT_MAX_LINES)
            .clamp(1, MAX_LINES);
        let end_line = start_line.saturating_add(max_lines).min(total_lines);
        let content = all_lines[start_line..end_line].join("\n");
        Ok(ToolOutcome::Executed(FsReadOutput {
            content,
            total_lines,
            start_line,
            returned_lines: end_line - start_line,
            truncated: file_truncated || end_line < total_lines,
        }))
    }

    // 7. fs_write -----------------------------------------------------------

    pub async fn fs_write(
        &self,
        input: FsWriteInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<FileWriteOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, false).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsWrite {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
            size: Some(input.content.len() as u64),
        };
        let ctx = self.context(&input.server_id, dirty);
        let decision = self.policy.evaluate(&action, &ctx);
        if decision.outcome == Decision::Deny {
            return Ok(ToolOutcome::Denied(decision));
        }
        // Solo se lee el original para construir el diff de aprobación; la
        // política ya ha decidido antes de cualquier acceso al contenido.
        let original = session
            .stat(PathBuf::from(&canonical))
            .await
            .ok()
            .map(|_| ());
        let previous = match original {
            Some(()) => {
                let limit = self.rollback.config().max_file_bytes;
                Some(
                    session
                        .read_bytes_limited(PathBuf::from(&canonical), limit)
                        .await?,
                )
            }
            None => None,
        };
        let diff = match &previous {
            Some((bytes, _)) => {
                patch::unified_diff(&String::from_utf8_lossy(bytes), &input.content)
            }
            None => patch::unified_diff("", &input.content),
        };
        let mut preview = Self::preview(
            "fs_write",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        preview.agent_reason = input.reason.clone();
        preview.unified_diff = Some(diff.clone());
        if let Some(outcome) = self.gate_decision(decision, mode, preview)? {
            return Ok(outcome);
        }
        let backup_id = match previous {
            Some((bytes, false)) => self
                .rollback
                .record_snapshot(
                    input.server_id.clone(),
                    canonical.clone(),
                    bytes,
                    previous_mode(session, &canonical).await,
                    self.backend.now(),
                )?
                .map(|entry| entry.id),
            _ => None,
        };
        let path = PathBuf::from(&canonical);
        session
            .write_bytes(path.clone(), input.content.clone().into_bytes())
            .await?;
        if let Some(mode) = input.mode {
            session.chmod(path, mode).await?;
        }
        Ok(ToolOutcome::Executed(FileWriteOutput {
            path: canonical,
            backup_id,
            unified_diff: diff,
            bytes_written: input.content.len() as u64,
        }))
    }

    // 8. fs_patch -----------------------------------------------------------

    pub async fn fs_patch(
        &self,
        input: FsPatchInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<FileWriteOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, true).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let ctx = self.context(&input.server_id, dirty);
        // La política va ANTES de leer cualquier byte del archivo: si está
        // denegado, debe devolver siempre `Denied`, sin filtrar con
        // `PatchError::NotFound` ni con `BinaryFile` pistas sobre su contenido.
        let action = McpAction::FsPatch {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
            size: None,
        };
        let first = self.policy.evaluate(&action, &ctx);
        let edits_conflict = first
            .warnings
            .iter()
            .any(|warning| matches!(warning, PolicyWarning::EditsConflict));
        if first.outcome == Decision::Deny || edits_conflict {
            return Ok(ToolOutcome::Denied(first));
        }
        // Tope propio de parcheo: no se usa `rollback.max_file_bytes`, porque
        // un archivo más grande que el límite de copia debe poder parchearse y
        // avisar con `FileExceedsRollbackLimit`.
        let limit = MAX_PATCH_BYTES.max(self.rollback.config().max_file_bytes);
        let (original_bytes, truncated) = session
            .read_bytes_limited(PathBuf::from(&canonical), limit)
            .await?;
        if truncated {
            return Err(ExecutorError::FileTooLarge(canonical.clone()));
        }
        let original = Self::text(&original_bytes, &input.path)?;
        let result = patch::apply_edits(&original, &input.edits)?;
        // Se reevalúa con el tamaño real para adjuntar el aviso de rollback.
        let action = McpAction::FsPatch {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
            size: Some(result.updated.len() as u64),
        };
        let decision = self.policy.evaluate(&action, &ctx);
        let mut preview = Self::preview(
            "fs_patch",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        preview.agent_reason = input.reason.clone();
        preview.unified_diff = Some(result.unified_diff.clone());
        if let Some(outcome) = self.gate_decision(decision, mode, preview)? {
            return Ok(outcome);
        }
        let backup_id = self
            .rollback
            .record_snapshot(
                input.server_id.clone(),
                canonical.clone(),
                original_bytes,
                previous_mode(session, &canonical).await,
                self.backend.now(),
            )?
            .map(|entry| entry.id);
        session
            .write_bytes(
                PathBuf::from(&canonical),
                result.updated.clone().into_bytes(),
            )
            .await?;
        Ok(ToolOutcome::Executed(FileWriteOutput {
            path: canonical,
            backup_id,
            unified_diff: result.unified_diff,
            bytes_written: result.updated.len() as u64,
        }))
    }

    // 9. fs_mkdir -----------------------------------------------------------

    pub async fn fs_mkdir(
        &self,
        input: FsMkdirInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<PathOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, false).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsMkdir {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let mut preview = Self::preview(
            "fs_mkdir",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        preview.agent_reason = input.reason.clone();
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        session.mkdir(PathBuf::from(&canonical)).await?;
        Ok(ToolOutcome::Executed(PathOutput { path: canonical }))
    }

    // 10. fs_move -----------------------------------------------------------

    pub async fn fs_move(
        &self,
        input: FsMoveInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<MoveOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let from = self.canonical_remote(session, &input.from, true).await?;
        let to = self.canonical_remote(session, &input.to, false).await?;
        let dirty = self.dirty_for(&input.server_id, &[from.clone(), to.clone()]);
        let action = McpAction::FsMove {
            server_id: input.server_id.clone(),
            from: input.from.clone(),
            from_canonical_path: Some(from.clone()),
            to: input.to.clone(),
            to_canonical_path: Some(to.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let mut preview = Self::preview("fs_move", &input.server_id, &[from.clone(), to.clone()]);
        preview.agent_reason = input.reason.clone();
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        session
            .rename(PathBuf::from(&from), PathBuf::from(&to))
            .await?;
        Ok(ToolOutcome::Executed(MoveOutput { from, to }))
    }

    // 11. fs_delete ---------------------------------------------------------

    pub async fn fs_delete(
        &self,
        input: FsDeleteInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<DeleteOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, true).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsDelete {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let mut preview = Self::preview(
            "fs_delete",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        preview.agent_reason = input.reason.clone();
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let path = PathBuf::from(&canonical);
        let mut backup_id = None;
        if !input.recursive {
            if let Ok(entry) = session.stat(path.clone()).await
                && entry.kind == vela_core::EntryKind::File
            {
                let limit = self.rollback.config().max_file_bytes;
                let readable = session.read_bytes_limited(path.clone(), limit).await?;
                if !readable.1 {
                    backup_id = self
                        .rollback
                        .record_snapshot(
                            input.server_id.clone(),
                            canonical.clone(),
                            readable.0,
                            entry.mode,
                            self.backend.now(),
                        )?
                        .map(|backup| backup.id);
                }
            }
            session.remove_file(path).await?;
        } else {
            session.remove_tree(path).await?;
        }
        Ok(ToolOutcome::Executed(DeleteOutput {
            path: canonical,
            backup_id,
            recursive: input.recursive,
        }))
    }

    // 12. fs_chmod ----------------------------------------------------------

    pub async fn fs_chmod(
        &self,
        input: FsChmodInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<ChmodOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let canonical = self.canonical_remote(session, &input.path, true).await?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&canonical));
        let action = McpAction::FsChmod {
            server_id: input.server_id.clone(),
            path: input.path.clone(),
            canonical_path: Some(canonical.clone()),
        };
        let ctx = self.context(&input.server_id, dirty);
        let mut preview = Self::preview(
            "fs_chmod",
            &input.server_id,
            std::slice::from_ref(&canonical),
        );
        preview.agent_reason = input.reason.clone();
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        session.chmod(PathBuf::from(&canonical), input.mode).await?;
        Ok(ToolOutcome::Executed(ChmodOutput {
            path: canonical,
            mode: input.mode & 0o7777,
        }))
    }

    // 13. transfer_upload ---------------------------------------------------

    pub async fn transfer_upload(
        &self,
        input: TransferUploadInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<TransferOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let (local_canonical, local) = self.canonical_local(&input.local_path, true)?;
        let remote = self
            .canonical_remote(session, &input.remote_path, false)
            .await?;
        let size = std::fs::metadata(&local)?.len();
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&remote));
        let action = McpAction::TransferUpload {
            server_id: input.server_id.clone(),
            local_path: input.local_path.clone(),
            local_canonical_path: Some(local_canonical.clone()),
            remote_path: input.remote_path.clone(),
            remote_canonical_path: Some(remote.clone()),
            size: Some(size),
        };
        let ctx = self.transfer_context(&input.server_id, dirty, &local_canonical);
        let mut preview = Self::preview(
            "transfer_upload",
            &input.server_id,
            &[local_canonical.clone(), remote.clone()],
        );
        preview.agent_reason = input.reason.clone();
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        session
            .upload(local, PathBuf::from(&remote), |_| {}, cancel)
            .await?;
        Ok(ToolOutcome::Executed(TransferOutput {
            local_path: local_canonical,
            remote_path: remote,
            bytes: size,
        }))
    }

    // 14. transfer_download -------------------------------------------------

    pub async fn transfer_download(
        &self,
        input: TransferDownloadInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<TransferOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let remote = self
            .canonical_remote(session, &input.remote_path, true)
            .await?;
        let (local_canonical, local) = self.canonical_local(&input.local_path, false)?;
        let dirty = self.dirty_for(&input.server_id, std::slice::from_ref(&remote));
        let action = McpAction::TransferDownload {
            server_id: input.server_id.clone(),
            local_path: input.local_path.clone(),
            local_canonical_path: Some(local_canonical.clone()),
            remote_path: input.remote_path.clone(),
            remote_canonical_path: Some(remote.clone()),
            size: None,
        };
        let ctx = self.transfer_context(&input.server_id, dirty, &local_canonical);
        let mut preview = Self::preview(
            "transfer_download",
            &input.server_id,
            &[remote.clone(), local_canonical.clone()],
        );
        preview.agent_reason = input.reason.clone();
        if let Some(outcome) = self.gate(&action, &ctx, mode, preview)? {
            return Ok(outcome);
        }
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        session
            .download(PathBuf::from(&remote), local, |_| {}, cancel)
            .await?;
        let bytes = std::fs::metadata(&local_canonical)?.len();
        Ok(ToolOutcome::Executed(TransferOutput {
            local_path: local_canonical,
            remote_path: remote,
            bytes,
        }))
    }

    // 15. exec_command ------------------------------------------------------

    pub async fn exec_command(
        &self,
        input: ExecCommandInput,
        mode: ApprovalMode,
    ) -> Result<ToolOutcome<ExecOutput>, ExecutorError> {
        let session = self.session(&input.server_id)?;
        let ctx = self.context(&input.server_id, false);
        let action = McpAction::ExecCommand {
            server_id: input.server_id.clone(),
            command: input.command.clone(),
        };
        let mut decision = self.policy.evaluate(&action, &ctx);
        // Un comando denegado nunca puede rebajarse a `Ask` por un cwd fuera
        // del ámbito, ni siquiera con aprobación previa.
        if decision.outcome == Decision::Deny {
            return Ok(ToolOutcome::Denied(decision));
        }
        let mut preview = Self::preview("exec_command", &input.server_id, &[]);
        preview.agent_reason = input.reason.clone();
        let canonical_cwd = match &input.cwd {
            Some(cwd) => Some(self.canonical_remote(session, cwd, true).await?),
            None => None,
        };
        preview.command = Some(match &canonical_cwd {
            Some(cwd) => vela_core::exec_remote_command(&input.command, Some(cwd)),
            None => input.command.clone(),
        });
        if let (Some(cwd), Some(canonical)) = (&input.cwd, &canonical_cwd) {
            // El directorio de trabajo se valida igual que cualquier otra ruta remota.
            let path_action = McpAction::FsList {
                server_id: input.server_id.clone(),
                path: cwd.clone(),
                canonical_path: Some(canonical.clone()),
            };
            let path_decision = self.policy.evaluate(&path_action, &ctx);
            preview.paths.push(canonical.clone());
            if path_decision.outcome == Decision::Deny {
                return Ok(ToolOutcome::Denied(path_decision));
            }
            // Solo un `Allow` puede elevarse a `Ask`: un `Ask` previo conserva
            // su regla y su motivo; el `Deny` ya se ha devuelto arriba.
            if path_decision.outcome == Decision::Ask && decision.outcome == Decision::Allow {
                let mut warnings = decision.warnings.clone();
                warnings.extend(path_decision.warnings.clone());
                decision = PolicyDecision {
                    outcome: Decision::Ask,
                    matched_rule: path_decision.matched_rule.clone(),
                    reason: format!("Directorio de trabajo: {}", path_decision.reason),
                    warnings,
                    requires_launch_vela: decision.requires_launch_vela
                        || path_decision.requires_launch_vela,
                };
            } else {
                decision.warnings.extend(path_decision.warnings);
                decision.requires_launch_vela |= path_decision.requires_launch_vela;
            }
        }
        if let Some(outcome) = self.gate_decision(decision, mode, preview)? {
            return Ok(outcome);
        }
        let options = ExecOptions {
            // Se ejecuta en la ruta canónica validada, no en la cadena cruda.
            cwd: canonical_cwd,
            timeout: Duration::from_secs(input.timeout_secs.unwrap_or(30).clamp(1, 300)),
            max_output_bytes: 64 * 1024,
        };
        let output = session.exec(input.command.clone(), options).await?;
        Ok(ToolOutcome::Executed(output))
    }
}

async fn collect_entries(
    session: &Session,
    root: &Path,
    directory: &Path,
    depth: u8,
    output: &mut Vec<FsEntry>,
    allowed: &[String],
    denied: &[String],
) -> Result<(), ExecutorError> {
    let mut listed = session.list_dir(directory.to_path_buf()).await?;
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in listed {
        let full = directory.join(&entry.name);
        // Ni se listan ni se recorren rutas de la denylist crítica, del perfil
        // o del servidor (por ejemplo `.ssh` o `.env`).
        let access = path::check_remote_path(&full.to_string_lossy(), None, allowed, denied);
        if matches!(access, PathAccess::Denied { .. }) {
            continue;
        }
        let can_descend = matches!(access, PathAccess::Allowed { .. });
        let relative = full
            .strip_prefix(root)
            .unwrap_or(&full)
            .to_string_lossy()
            .into_owned();
        output.push(FsEntry {
            path: relative,
            name: entry.name.to_string_lossy().into_owned(),
            kind: entry_kind(&entry),
            size: entry.size,
            mode: entry.mode,
            mtime_secs: entry.mtime.and_then(system_secs),
        });
        // Solo se desciende por directorios reales: `entry.is_directory()` es
        // cierto para symlinks a directorios y permitiría salir del ámbito o
        // entrar en un ciclo.
        if depth > 1 && can_descend && entry.kind == vela_core::EntryKind::Directory {
            Box::pin(collect_entries(
                session,
                root,
                &full,
                depth - 1,
                output,
                allowed,
                denied,
            ))
            .await?;
        }
    }
    Ok(())
}

async fn previous_mode(session: &Session, path: &str) -> Option<u32> {
    session
        .stat(PathBuf::from(path))
        .await
        .ok()
        .and_then(|entry| entry.mode)
}

/// Fusiona dos listas de reglas conservando el orden y evitando duplicados.
fn merge_unique(base: Vec<String>, extra: &[String]) -> Vec<String> {
    let mut merged = base;
    for value in extra {
        if !merged.contains(value) {
            merged.push(value.clone());
        }
    }
    merged
}

fn system_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn entry_kind(entry: &vela_core::Entry) -> String {
    format!("{:?}", entry.kind).to_lowercase()
}

// Entradas ------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListServersInput {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DescribePermissionsInput {
    pub server_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GetUiContextInput {
    /// Si se indica, debe coincidir con el servidor activo de la interfaz.
    #[serde(default)]
    pub server_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsListInput {
    pub server_id: String,
    pub path: String,
    #[serde(default)]
    pub depth: Option<u8>,
    #[serde(default)]
    pub pattern: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsStatInput {
    pub server_id: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsReadInput {
    pub server_id: String,
    pub path: String,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsWriteInput {
    pub server_id: String,
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub mode: Option<u32>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsPatchInput {
    pub server_id: String,
    pub path: String,
    pub edits: Vec<TextEdit>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsMkdirInput {
    pub server_id: String,
    pub path: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsMoveInput {
    pub server_id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsDeleteInput {
    pub server_id: String,
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsChmodInput {
    pub server_id: String,
    pub path: String,
    pub mode: u32,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferUploadInput {
    pub server_id: String,
    pub local_path: String,
    pub remote_path: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferDownloadInput {
    pub server_id: String,
    pub remote_path: String,
    pub local_path: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecCommandInput {
    pub server_id: String,
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub reason: Option<String>,
}

// Salidas -------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentServerSummary {
    pub id: String,
    pub alias: Option<String>,
    pub notes: Option<String>,
    pub availability: String,
    pub connected: bool,
    pub effective_profile: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
}

impl From<crate::AgentServerInfo> for AgentServerSummary {
    fn from(info: crate::AgentServerInfo) -> Self {
        Self {
            id: info.id,
            alias: info.alias,
            notes: info.notes,
            availability: format!("{:?}", info.availability).to_lowercase(),
            connected: info.connected,
            effective_profile: info.effective_profile,
            host: info.host,
            port: info.port,
            user: info.user,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PermissionsInfo {
    pub server_id: String,
    pub alias: Option<String>,
    pub notes: Option<String>,
    pub availability: String,
    pub connected: bool,
    pub profile_id: String,
    pub operations: BTreeMap<String, Decision>,
    pub allowed_remote_paths: Vec<String>,
    pub denied_remote_paths: Vec<String>,
    pub allowed_local_paths: Vec<String>,
    pub allowed_commands: Vec<String>,
    pub denied_commands: Vec<String>,
    pub exec_mode: String,
    pub sudo: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiContext {
    pub server_id: String,
    pub current_remote_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsEntry {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub mtime_secs: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsListOutput {
    pub entries: Vec<FsEntry>,
    pub depth: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsStatOutput {
    pub path: String,
    pub kind: String,
    pub is_directory: bool,
    pub size: Option<u64>,
    pub mode: Option<u32>,
    pub mtime_secs: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FsReadOutput {
    pub content: String,
    pub total_lines: usize,
    pub start_line: usize,
    pub returned_lines: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileWriteOutput {
    pub path: String,
    pub backup_id: Option<String>,
    pub unified_diff: String,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PathOutput {
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MoveOutput {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeleteOutput {
    pub path: String,
    pub backup_id: Option<String>,
    pub recursive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChmodOutput {
    pub path: String,
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransferOutput {
    pub local_path: String,
    pub remote_path: String,
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct TestBackend {
        ui: Option<(String, String)>,
        gui_open: bool,
        dirty: bool,
    }

    impl McpBackend for TestBackend {
        fn list_servers(&self) -> Vec<(String, ServerIdentity)> {
            Vec::new()
        }
        fn session(&self, _: &str) -> Option<&Session> {
            None
        }
        fn edits_dirty(&self, _: &str, _: &str) -> bool {
            self.dirty
        }
        fn ui_context(&self) -> Option<(String, String)> {
            self.ui.clone()
        }
        fn vela_gui_open(&self) -> bool {
            self.gui_open
        }
        fn vela_config_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    fn plain_backend() -> TestBackend {
        TestBackend {
            ui: None,
            gui_open: true,
            dirty: false,
        }
    }

    fn executor_with(
        backend: TestBackend,
        config: McpConfig,
    ) -> (tempfile::TempDir, Executor<TestBackend>) {
        let dir = tempfile::tempdir().unwrap();
        let rollback =
            Arc::new(RollbackStore::new(dir.path().join("rollback"), Default::default()).unwrap());
        (dir, Executor::new(backend, config, rollback))
    }

    #[test]
    fn inputs_and_outputs_round_trip_as_json() {
        let input = FsPatchInput {
            server_id: "srv".into(),
            path: "/srv/api/app.js".into(),
            edits: vec![TextEdit {
                old_text: "a".into(),
                new_text: "b".into(),
            }],
            reason: None,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert_eq!(serde_json::from_str::<FsPatchInput>(&json).unwrap(), input);
        let outcome: ToolOutcome<PathOutput> =
            serde_json::from_str(r#"{"status":"executed","path":"/tmp/x"}"#).unwrap();
        assert_eq!(
            outcome.executed(),
            Some(PathOutput {
                path: "/tmp/x".into()
            })
        );
    }

    #[test]
    fn approved_once_still_denies_edits_conflict() {
        let mut config = McpConfig {
            enabled: true,
            ..McpConfig::default()
        };
        config.servers.insert(
            "srv".into(),
            crate::ServerMcpPolicy {
                availability: crate::ServerAvailability::Headless,
                profile_id: "development".into(),
                ..Default::default()
            },
        );
        let (_dir, executor) = executor_with(plain_backend(), config);
        let mut ctx = executor.context("srv", true);
        ctx.server_connected = true;
        let action = McpAction::FsWrite {
            server_id: "srv".into(),
            path: "/srv/api/a".into(),
            canonical_path: Some("/srv/api/a".into()),
            size: Some(4),
        };
        let decision = executor.policy.evaluate(&action, &ctx);
        let outcome: ToolOutcome<PathOutput> = executor
            .gate_decision(
                decision,
                ApprovalMode::ApprovedOnce,
                ApprovalPreview::default(),
            )
            .unwrap()
            .unwrap();
        assert!(outcome.is_denied());
    }

    #[test]
    fn get_ui_context_only_exposes_the_active_server_and_its_policy() {
        let mut config = McpConfig {
            enabled: true,
            share_ui_context: true,
            ..McpConfig::default()
        };
        config.servers.insert(
            "activo".into(),
            crate::ServerMcpPolicy {
                availability: crate::ServerAvailability::Disabled,
                profile_id: "readonly".into(),
                ..Default::default()
            },
        );
        config.servers.insert(
            "otro".into(),
            crate::ServerMcpPolicy {
                availability: crate::ServerAvailability::Headless,
                profile_id: "readonly".into(),
                ..Default::default()
            },
        );
        let backend = TestBackend {
            ui: Some(("activo".into(), "/srv/activo".into())),
            ..plain_backend()
        };
        let (_dir, executor) = executor_with(backend, config);

        // Pedir un servidor distinto no revela la ruta del activo.
        let outcome = executor
            .get_ui_context(
                GetUiContextInput {
                    server_id: Some("otro".into()),
                },
                ApprovalMode::Normal,
            )
            .unwrap();
        assert!(outcome.is_denied());
        assert!(outcome.executed().is_none());

        // Sin identificador se evalúa al servidor realmente activo: Disabled.
        let outcome = executor
            .get_ui_context(GetUiContextInput { server_id: None }, ApprovalMode::Normal)
            .unwrap();
        assert!(outcome.is_denied());
    }

    #[test]
    fn vela_gui_open_false_fails_closed_on_ask() {
        let mut config = McpConfig {
            enabled: true,
            ..McpConfig::default()
        };
        config.servers.insert(
            "srv".into(),
            crate::ServerMcpPolicy {
                availability: crate::ServerAvailability::Headless,
                profile_id: "development".into(),
                allowed_remote_paths: vec!["/srv/api/**".into()],
                ..Default::default()
            },
        );
        let backend = TestBackend {
            gui_open: false,
            ..plain_backend()
        };
        let (_dir, executor) = executor_with(backend, config);
        let ctx = executor.context("srv", false);
        assert!(!ctx.vela_gui_open);
        let action = McpAction::FsRead {
            server_id: "srv".into(),
            path: "/tmp/fuera".into(),
            canonical_path: Some("/tmp/fuera".into()),
        };
        let decision = executor.policy.evaluate(&action, &ctx);
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "headless_fail_closed");
    }

    #[test]
    fn approval_preview_keeps_the_agent_reason() {
        let decision = PolicyDecision {
            outcome: Decision::Ask,
            matched_rule: "regla".into(),
            reason: "motivo de la política".into(),
            warnings: Vec::new(),
            requires_launch_vela: false,
        };
        let preview = ApprovalPreview {
            agent_reason: Some("motivo del agente".into()),
            ..ApprovalPreview::default()
        };
        let (_dir, executor) = executor_with(plain_backend(), McpConfig::default());
        let outcome: ToolOutcome<PathOutput> = executor
            .gate_decision(decision, ApprovalMode::Normal, preview)
            .unwrap()
            .unwrap();
        match outcome {
            ToolOutcome::NeedsApproval { preview, .. } => {
                assert_eq!(preview.agent_reason.as_deref(), Some("motivo del agente"));
                assert_eq!(preview.reason, "motivo de la política");
            }
            other => panic!("se esperaba NeedsApproval: {other:?}"),
        }
    }
}
