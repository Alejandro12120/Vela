//! Motor de políticas unificado, introspección para el agente y simulador.

use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use crate::{
    command::{self, CommandWarning},
    config::{Decision, HeadlessAskBehavior, McpConfig, ServerAvailability, ServerMcpPolicy},
    jit::JitGrants,
    path::{self, PathAccess},
    presets::{self, PermissionProfile},
};

/// Todas las acciones MCP disponibles en v1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpAction {
    ListServers,
    DescribePermissions {
        server_id: String,
    },
    GetUiContext {
        server_id: String,
    },
    ConnectOnDemand {
        server_id: String,
    },
    FsList {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
    },
    FsStat {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
    },
    FsRead {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
    },
    FsWrite {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
        size: Option<u64>,
    },
    FsPatch {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
        size: Option<u64>,
    },
    FsMkdir {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
    },
    FsMove {
        server_id: String,
        from: String,
        from_canonical_path: Option<String>,
        to: String,
        to_canonical_path: Option<String>,
    },
    FsDelete {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
    },
    FsChmod {
        server_id: String,
        path: String,
        canonical_path: Option<String>,
    },
    TransferUpload {
        server_id: String,
        local_path: String,
        local_canonical_path: Option<String>,
        remote_path: String,
        remote_canonical_path: Option<String>,
        size: Option<u64>,
    },
    TransferDownload {
        server_id: String,
        local_path: String,
        local_canonical_path: Option<String>,
        remote_path: String,
        remote_canonical_path: Option<String>,
        size: Option<u64>,
    },
    ExecCommand {
        server_id: String,
        command: String,
    },
}

impl McpAction {
    pub fn server_id(&self) -> Option<&str> {
        match self {
            Self::ListServers => None,
            Self::DescribePermissions { server_id }
            | Self::GetUiContext { server_id }
            | Self::ConnectOnDemand { server_id }
            | Self::FsList { server_id, .. }
            | Self::FsStat { server_id, .. }
            | Self::FsRead { server_id, .. }
            | Self::FsWrite { server_id, .. }
            | Self::FsPatch { server_id, .. }
            | Self::FsMkdir { server_id, .. }
            | Self::FsMove { server_id, .. }
            | Self::FsDelete { server_id, .. }
            | Self::FsChmod { server_id, .. }
            | Self::TransferUpload { server_id, .. }
            | Self::TransferDownload { server_id, .. }
            | Self::ExecCommand { server_id, .. } => Some(server_id),
        }
    }

    /// Nombre estable de la operación, usado también en las concesiones JIT.
    pub fn operation_name(&self) -> &'static str {
        match self {
            Self::ListServers => "list_servers",
            Self::DescribePermissions { .. } => "describe_permissions",
            Self::GetUiContext { .. } => "get_ui_context",
            Self::ConnectOnDemand { .. } => "connect_on_demand",
            Self::FsList { .. } => "fs_list",
            Self::FsStat { .. } => "fs_stat",
            Self::FsRead { .. } => "fs_read",
            Self::FsWrite { .. } => "fs_write",
            Self::FsPatch { .. } => "fs_patch",
            Self::FsMkdir { .. } => "fs_mkdir",
            Self::FsMove { .. } => "fs_move",
            Self::FsDelete { .. } => "fs_delete",
            Self::FsChmod { .. } => "fs_chmod",
            Self::TransferUpload { .. } => "transfer_upload",
            Self::TransferDownload { .. } => "transfer_download",
            Self::ExecCommand { .. } => "exec_command",
        }
    }

    fn path_targets(&self) -> Vec<PathTarget<'_>> {
        match self {
            Self::FsList {
                path,
                canonical_path,
                ..
            }
            | Self::FsStat {
                path,
                canonical_path,
                ..
            }
            | Self::FsRead {
                path,
                canonical_path,
                ..
            }
            | Self::FsWrite {
                path,
                canonical_path,
                ..
            }
            | Self::FsPatch {
                path,
                canonical_path,
                ..
            }
            | Self::FsMkdir {
                path,
                canonical_path,
                ..
            }
            | Self::FsDelete {
                path,
                canonical_path,
                ..
            }
            | Self::FsChmod {
                path,
                canonical_path,
                ..
            } => vec![PathTarget {
                requested: path,
                canonical: canonical_path.as_deref(),
                local: false,
            }],
            Self::FsMove {
                from,
                from_canonical_path,
                to,
                to_canonical_path,
                ..
            } => vec![
                PathTarget {
                    requested: from,
                    canonical: from_canonical_path.as_deref(),
                    local: false,
                },
                PathTarget {
                    requested: to,
                    canonical: to_canonical_path.as_deref(),
                    local: false,
                },
            ],
            Self::TransferUpload {
                local_path,
                local_canonical_path,
                remote_path,
                remote_canonical_path,
                ..
            } => vec![
                PathTarget {
                    requested: local_path,
                    canonical: local_canonical_path.as_deref(),
                    local: true,
                },
                PathTarget {
                    requested: remote_path,
                    canonical: remote_canonical_path.as_deref(),
                    local: false,
                },
            ],
            Self::TransferDownload {
                local_path,
                local_canonical_path,
                remote_path,
                remote_canonical_path,
                ..
            } => vec![
                PathTarget {
                    requested: remote_path,
                    canonical: remote_canonical_path.as_deref(),
                    local: false,
                },
                PathTarget {
                    requested: local_path,
                    canonical: local_canonical_path.as_deref(),
                    local: true,
                },
            ],
            _ => Vec::new(),
        }
    }

    fn touches_edits(&self) -> bool {
        matches!(
            self,
            Self::FsWrite { .. }
                | Self::FsPatch { .. }
                | Self::FsDelete { .. }
                | Self::FsMove { .. }
                | Self::TransferUpload { .. }
        )
    }
}

struct PathTarget<'a> {
    requested: &'a str,
    canonical: Option<&'a str>,
    local: bool,
}

/// Normaliza el ámbito de una concesión efímera igual que se normalizan las rutas.
fn normalized_scope(value: &str, home: Option<&std::path::Path>) -> Option<String> {
    let expanded = path::expand_tilde(value, home)?;
    path::normalize_lexical(&expanded).ok()
}

/// Una concesión sobre una ruta no puede aprovechar un symlink que apunta a
/// otra: si se conoce la ruta canónica, el permiso debe coincidir con ambas.
fn scoped_allow_for_target(
    jit: &JitGrants,
    server_id: &str,
    operation: &str,
    target: &PathTarget<'_>,
    ctx: &EvalContext,
) -> bool {
    let Some(requested) = normalized_scope(target.requested, ctx.home_dir.as_deref()) else {
        return false;
    };
    let granted_scope = |scope: &str| {
        normalized_scope(scope, ctx.home_dir.as_deref()).as_deref() == Some(requested.as_str())
    };
    if !jit.any_scoped_allow(server_id, operation, ctx.now, granted_scope) {
        return false;
    }
    if let Some(canonical) = target.canonical {
        let Some(canonical) = normalized_scope(canonical, ctx.home_dir.as_deref()) else {
            return false;
        };
        let granted_canonical = |scope: &str| {
            normalized_scope(scope, ctx.home_dir.as_deref()).as_deref() == Some(canonical.as_str())
        };
        if !jit.any_scoped_allow(server_id, operation, ctx.now, granted_canonical) {
            return false;
        }
    }
    true
}

fn profile_decision(profile: &PermissionProfile, action: &McpAction) -> (Decision, String, String) {
    match profile.decision_for(action.operation_name()) {
        Some(base) => (
            base,
            action.operation_name().to_owned(),
            match base {
                Decision::Allow => "El perfil permite esta operación.".to_owned(),
                Decision::Ask => "Esta operación requiere aprobación.".to_owned(),
                Decision::Deny => "El perfil deniega esta operación.".to_owned(),
            },
        ),
        None => (
            Decision::Deny,
            "unsupported_action".to_owned(),
            "Esta acción no está disponible en v1.".to_owned(),
        ),
    }
}

/// Estado del entorno que afecta a una decisión.
#[derive(Clone, Debug)]
pub struct EvalContext {
    pub server_connected: bool,
    pub vela_gui_open: bool,
    pub open_in_edits_dirty: bool,
    pub home_dir: Option<PathBuf>,
    pub vela_config_dir: Option<PathBuf>,
    pub now: SystemTime,
}

impl Default for EvalContext {
    fn default() -> Self {
        Self {
            server_connected: true,
            vela_gui_open: true,
            open_in_edits_dirty: false,
            home_dir: std::env::var_os("HOME").map(PathBuf::from),
            vela_config_dir: None,
            now: SystemTime::now(),
        }
    }
}

/// Avisos no bloqueantes que se muestran junto a una decisión.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PolicyWarning {
    OutOfScopePath { path: String, reason: String },
    EditsConflict,
    Command(CommandWarning),
    FileExceedsRollbackLimit { size: u64, max_bytes: u64 },
}

/// Decisión devuelta por el motor de políticas.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyDecision {
    pub outcome: Decision,
    pub matched_rule: String,
    pub reason: String,
    pub warnings: Vec<PolicyWarning>,
    pub requires_launch_vela: bool,
}

impl PolicyDecision {
    fn new(outcome: Decision, matched_rule: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            outcome,
            matched_rule: matched_rule.into(),
            reason: reason.into(),
            warnings: Vec::new(),
            requires_launch_vela: false,
        }
    }
}

/// Identidad real de un servidor, que puede ocultarse al agente.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerIdentity {
    pub host: String,
    pub port: u16,
    pub user: String,
}

/// Información de un servidor que se expone al agente MCP.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentServerInfo {
    pub id: String,
    pub alias: Option<String>,
    pub notes: Option<String>,
    pub availability: ServerAvailability,
    pub connected: bool,
    pub effective_profile: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
}

/// Motor de políticas: combina configuración, perfiles, ámbitos de rutas y JIT.
#[derive(Clone, Debug)]
pub struct PolicyEngine {
    config: McpConfig,
    jit: JitGrants,
}

impl PolicyEngine {
    pub fn new(config: McpConfig) -> Self {
        Self {
            config,
            jit: JitGrants::new(),
        }
    }

    pub fn with_jit(config: McpConfig, jit: JitGrants) -> Self {
        Self { config, jit }
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut McpConfig {
        &mut self.config
    }

    pub fn jit_mut(&mut self) -> &mut JitGrants {
        &mut self.jit
    }

    /// Ámbitos remotos efectivos de un servidor: perfil activo (incluida una
    /// elevación JIT), reglas del servidor y denylist crítica. Se usan también
    /// al recorrer el árbol para no listar ni atravesar rutas denegadas.
    pub fn effective_remote_path_scopes(
        &self,
        server_id: &str,
        now: SystemTime,
    ) -> (Vec<String>, Vec<String>) {
        let policy = self
            .config
            .servers
            .get(server_id)
            .cloned()
            .unwrap_or_default();
        let profile_id = self
            .jit
            .active_profile_override(server_id, now)
            .unwrap_or(&policy.profile_id)
            .to_owned();
        let profile = presets::resolve_profile(&self.config, &profile_id).unwrap_or_default();
        let denied = path_scopes(&profile, &policy, true);
        let allowed = path_scopes(&profile, &policy, false);
        (allowed.remote, denied.remote)
    }

    /// Evalúa la acción sin los datos canónicos de una aprobación previa.
    pub fn evaluate(&self, action: &McpAction, ctx: &EvalContext) -> PolicyDecision {
        self.finish_evaluation(self.evaluate_base(action, ctx), ctx, None)
    }

    pub(crate) fn evaluate_with_preview(
        &self,
        action: &McpAction,
        ctx: &EvalContext,
        preview: &crate::executor::ApprovalPreview,
    ) -> PolicyDecision {
        debug_assert!(action.server_id().unwrap_or("") == preview.server_id);
        self.finish_evaluation(self.evaluate_base(action, ctx), ctx, Some(preview))
    }

    pub(crate) fn evaluate_base(&self, action: &McpAction, ctx: &EvalContext) -> PolicyDecision {
        if !self.config.enabled {
            return PolicyDecision::new(
                Decision::Deny,
                "mcp_disabled",
                "El sistema MCP está desactivado.",
            );
        }

        let server_id = action.server_id();
        let policy = server_id
            .and_then(|id| self.config.servers.get(id))
            .cloned()
            .unwrap_or_default();

        if matches!(action, McpAction::GetUiContext { .. }) {
            if !self.config.share_ui_context {
                return PolicyDecision::new(
                    Decision::Deny,
                    "ui_context_disabled",
                    "El contexto de la interfaz no está compartido.",
                );
            }
            if policy.availability == ServerAvailability::Disabled {
                return PolicyDecision::new(
                    Decision::Deny,
                    "server_disabled",
                    "Este servidor no está disponible para el agente.",
                );
            }
            return PolicyDecision::new(
                Decision::Allow,
                "ui_context_allowed",
                "El contexto de la interfaz está disponible.",
            );
        }

        if action.touches_edits() && ctx.open_in_edits_dirty {
            let mut decision = PolicyDecision::new(
                Decision::Deny,
                "edits_conflict",
                "Hay ediciones abiertas sin sincronizar.",
            );
            decision.warnings.push(PolicyWarning::EditsConflict);
            return decision;
        }

        // Las acciones globales (ListServers) no tienen política de servidor:
        // solo quedan sujetas al interruptor general `enabled`.
        // `DescribePermissions` solo lee metadatos locales: no abre SSH, así que
        // no depende de que el servidor esté conectado.
        let has_server = server_id.is_some();
        let is_metadata = matches!(action, McpAction::DescribePermissions { .. });
        if has_server && policy.availability == ServerAvailability::Disabled {
            return PolicyDecision::new(
                Decision::Deny,
                "server_disabled",
                "Este servidor no está disponible para el agente.",
            );
        }
        if has_server
            && !is_metadata
            && policy.availability == ServerAvailability::ConnectedOnly
            && !ctx.server_connected
        {
            return PolicyDecision::new(
                Decision::Deny,
                "server_disconnected",
                "El servidor no está conectado.",
            );
        }
        let availability_ask = has_server
            && !is_metadata
            && policy.availability == ServerAvailability::OnDemandAsk
            && !ctx.server_connected;

        let mut decision = Decision::Allow;
        let mut matched_rule: String;
        let mut reason: String;
        let mut warnings = Vec::new();

        let profile_id = self
            .jit
            .active_profile_override(server_id.unwrap_or(""), ctx.now)
            .unwrap_or(&policy.profile_id)
            .to_owned();
        let profile = match presets::resolve_profile(&self.config, &profile_id) {
            Some(profile) => profile,
            None => {
                return PolicyDecision::new(
                    Decision::Deny,
                    "unknown_profile",
                    format!("El perfil '{profile_id}' no existe."),
                );
            }
        };

        match action {
            McpAction::ListServers | McpAction::DescribePermissions { .. } => {
                matched_rule = "info_allowed".to_owned();
                reason = "La información se puede compartir con el agente.".to_owned();
            }
            McpAction::ConnectOnDemand { .. } => {
                matched_rule = "connect_on_demand".to_owned();
                reason = "La conexión bajo demanda está permitida.".to_owned();
            }
            McpAction::ExecCommand { command, .. } => {
                let mode = policy.exec_mode.unwrap_or(profile.exec_mode);
                let sudo = match policy.sudo.unwrap_or(profile.sudo) {
                    Decision::Allow if !ctx.vela_gui_open => Decision::Ask,
                    decision => decision,
                };
                let mut allowed = profile.allowed_commands.clone();
                allowed.extend(policy.allowed_commands.iter().cloned());
                let mut denied = profile.denied_commands.clone();
                denied.extend(policy.denied_commands.iter().cloned());
                let check = command::evaluate_command(command, mode, &allowed, &denied, sudo);
                decision = check.outcome;
                matched_rule = check.matched_rule;
                reason = check.reason;
                warnings.extend(check.warnings.into_iter().map(PolicyWarning::Command));
            }
            McpAction::FsWrite { size, .. } | McpAction::FsPatch { size, .. } => {
                (decision, matched_rule, reason) = profile_decision(&profile, action);
                if self.config.rollback.enabled
                    && (*size).is_some_and(|size| size > self.config.rollback.max_file_bytes)
                {
                    warnings.push(PolicyWarning::FileExceedsRollbackLimit {
                        size: (*size).unwrap(),
                        max_bytes: self.config.rollback.max_file_bytes,
                    });
                }
            }
            _ => {
                (decision, matched_rule, reason) = profile_decision(&profile, action);
            }
        }

        // Una conexión bajo demanda nunca puede convertir una autorización del
        // perfil en un acceso directo: siempre requiere aprobación previa.
        if availability_ask && decision == Decision::Allow {
            decision = Decision::Ask;
            matched_rule = "on_demand_ask".to_owned();
            reason = "El servidor se conectará bajo demanda tras aprobación.".to_owned();
        }

        if decision != Decision::Deny {
            let denied_paths = path_scopes(&profile, &policy, true);
            let allowed_paths = path_scopes(&profile, &policy, false);
            for target in action.path_targets() {
                let access = if target.local {
                    path::check_local_path(
                        target.requested,
                        target.canonical,
                        ctx.home_dir.as_deref(),
                        ctx.vela_config_dir.as_deref(),
                        &allowed_paths.local,
                    )
                } else {
                    path::check_remote_path(
                        target.requested,
                        target.canonical,
                        &allowed_paths.remote,
                        &denied_paths.remote,
                    )
                };
                match access {
                    PathAccess::Denied { matched_rule: rule } => {
                        decision = Decision::Deny;
                        matched_rule = rule;
                        reason = "Una de las rutas está denegada por la denylist.".to_owned();
                        break;
                    }
                    PathAccess::OutsideScope { reason: why } => {
                        let scoped = scoped_allow_for_target(
                            &self.jit,
                            server_id.unwrap_or(""),
                            action.operation_name(),
                            &target,
                            ctx,
                        );
                        if !scoped {
                            warnings.push(PolicyWarning::OutOfScopePath {
                                path: target.requested.to_owned(),
                                reason: why,
                            });
                            if decision == Decision::Allow {
                                decision = Decision::Ask;
                                matched_rule = "out_of_scope_path".to_owned();
                                reason = "La ruta está fuera del ámbito permitido.".to_owned();
                            }
                        }
                    }
                    PathAccess::Allowed { .. } => {}
                }
            }
        }

        // Las concesiones efímeras solo pueden convertir un `Ask` en `Allow`:
        // nunca sortean un `Deny`, un conflicto de ediciones ni la aprobación
        // exigida por una conexión bajo demanda.
        if decision == Decision::Ask && !availability_ask && !ctx.open_in_edits_dirty {
            let server = server_id.unwrap_or("");
            let operation = action.operation_name();
            let granted = match action {
                McpAction::ExecCommand { command, .. } => {
                    self.jit
                        .has_scoped_allow(server, operation, command.trim(), ctx.now)
                }
                _ => {
                    let targets = action.path_targets();
                    !targets.is_empty()
                        && targets.iter().all(|target| {
                            scoped_allow_for_target(&self.jit, server, operation, target, ctx)
                        })
                }
            };
            if granted {
                decision = Decision::Allow;
                matched_rule = "jit_scoped_allow".to_owned();
                reason = "Una concesión efímera permite esta operación.".to_owned();
            }
        }

        PolicyDecision {
            outcome: decision,
            matched_rule,
            reason,
            warnings,
            requires_launch_vela: false,
        }
    }

    // La vista previa canónica conserva los parámetros de la aprobación, como
    // el cwd de un comando o la eliminación recursiva.
    pub(crate) fn finish_evaluation(
        &self,
        mut result: PolicyDecision,
        ctx: &EvalContext,
        preview: Option<&crate::executor::ApprovalPreview>,
    ) -> PolicyDecision {
        if result.outcome == Decision::Ask
            && (ctx.vela_gui_open
                || !result
                    .warnings
                    .contains(&PolicyWarning::Command(CommandWarning::ContainsSudo)))
            && let Some(preview) = preview
            && let Some(policy) = self.config.servers.get(&preview.server_id)
            && (ctx.server_connected || policy.availability != ServerAvailability::OnDemandAsk)
            && let Some(rule) = preview.scoped_rule()
        {
            if policy.scoped_rules.contains(&rule) {
                result.outcome = Decision::Allow;
                result.matched_rule = "saved_scoped_allow".to_owned();
                result.reason = "Una regla guardada permite esta operación.".to_owned();
            } else if self.jit.has_scoped_allow(
                &preview.server_id,
                &rule.operation,
                &rule.scope,
                ctx.now,
            ) {
                result.outcome = Decision::Allow;
                result.matched_rule = "jit_scoped_allow".to_owned();
                result.reason = "Una concesión efímera permite esta operación.".to_owned();
            }
        }
        if result.outcome == Decision::Ask && !ctx.vela_gui_open {
            match self.config.headless_ask {
                HeadlessAskBehavior::Deny => {
                    result.outcome = Decision::Deny;
                    result.matched_rule = "headless_fail_closed".to_owned();
                    result.reason =
                        "Se requiere aprobación pero no hay interfaz disponible.".to_owned();
                }
                HeadlessAskBehavior::OpenVela => {
                    result.requires_launch_vela = true;
                }
            }
        }
        result
    }

    /// Describe un servidor para el agente, ocultando su identidad si procede.
    pub fn describe_server_for_agent(
        &self,
        server_id: &str,
        identity: &ServerIdentity,
        connected: bool,
        now: SystemTime,
    ) -> Option<AgentServerInfo> {
        let policy = self.config.servers.get(server_id)?;
        if !self.config.enabled || policy.availability == ServerAvailability::Disabled {
            return None;
        }
        let effective_profile = self
            .jit
            .active_profile_override(server_id, now)
            .unwrap_or(&policy.profile_id)
            .to_owned();
        let hide = policy.hides_identity(&self.config);
        Some(AgentServerInfo {
            id: server_id.to_owned(),
            alias: policy.alias.clone(),
            notes: policy.notes.clone(),
            availability: policy.availability,
            connected,
            effective_profile,
            host: (!hide).then(|| identity.host.clone()),
            port: (!hide).then_some(identity.port),
            user: (!hide).then(|| identity.user.clone()),
        })
    }

    /// Simula una entrada sencilla del panel de Ajustes, por ejemplo
    /// `fs_read /etc/nginx/.env` o `exec sudo nginx -t`.
    ///
    /// El simulador fuerza un servidor conectado con interfaz abierta para que
    /// muestre la decisión del perfil y no el bloqueo global de arranque.
    pub fn simulate(&self, profile_or_server: &str, input: &str) -> PolicyDecision {
        let context = EvalContext {
            home_dir: std::env::var_os("HOME").map(PathBuf::from),
            ..EvalContext::default()
        };
        let is_server = self.config.servers.contains_key(profile_or_server);
        self.simulate_target(profile_or_server, is_server, input, &context)
    }

    /// Previsualiza reglas sin conexiones ni consultas al sistema de archivos.
    pub fn simulate_target(
        &self,
        target: &str,
        is_server: bool,
        input: &str,
        context: &EvalContext,
    ) -> PolicyDecision {
        let input = input.trim();
        let (operation, arguments) = match input.split_once(char::is_whitespace) {
            Some((operation, arguments)) => (operation, arguments.trim()),
            None => (input, ""),
        };
        let mut config = self.config.clone();
        config.enabled = true;
        let server_id = if is_server {
            let policy = config.servers.entry(target.to_owned()).or_default();
            // Ajustes debe poder previsualizar las reglas de un servidor
            // desactivado antes de activarlo.
            if policy.availability == ServerAvailability::Disabled {
                policy.availability = ServerAvailability::ConnectedOnly;
            }
            target.to_owned()
        } else {
            let id = "__sim__".to_owned();
            config.servers.insert(
                id.clone(),
                ServerMcpPolicy {
                    availability: ServerAvailability::ConnectedOnly,
                    profile_id: target.to_owned(),
                    ..ServerMcpPolicy::default()
                },
            );
            id
        };
        let action = match parse_simulated_action(&server_id, operation, arguments) {
            Some(action) => action,
            None => {
                return PolicyDecision::new(
                    Decision::Deny,
                    "unknown_simulated_input",
                    "No reconozco esa entrada del simulador.",
                );
            }
        };
        let ctx = EvalContext {
            server_connected: true,
            vela_gui_open: true,
            open_in_edits_dirty: false,
            ..context.clone()
        };
        PolicyEngine::new(config).evaluate(&action, &ctx)
    }
}

#[derive(Default)]
struct PathScopes {
    remote: Vec<String>,
    local: Vec<String>,
}

fn path_scopes(profile: &PermissionProfile, policy: &ServerMcpPolicy, denied: bool) -> PathScopes {
    if denied {
        PathScopes {
            remote: profile
                .denied_paths()
                .into_iter()
                .chain(policy.denied_remote_paths.iter().cloned())
                .collect(),
            local: Vec::new(),
        }
    } else {
        PathScopes {
            remote: profile
                .default_allowed_remote_paths
                .iter()
                .cloned()
                .chain(policy.allowed_remote_paths.iter().cloned())
                .collect(),
            local: policy.allowed_local_paths.clone(),
        }
    }
}

fn parse_simulated_action(server_id: &str, operation: &str, arguments: &str) -> Option<McpAction> {
    let id = server_id.to_owned();
    if arguments.is_empty() && (operation.starts_with("fs_") || operation.starts_with("exec")) {
        return None;
    }
    let path = || arguments.to_owned();
    match operation {
        "servers" | "list_servers" => Some(McpAction::ListServers),
        "permissions" | "describe_permissions" => {
            Some(McpAction::DescribePermissions { server_id: id })
        }
        "ui_context" | "get_ui_context" => Some(McpAction::GetUiContext { server_id: id }),
        "connect" | "connect_on_demand" => Some(McpAction::ConnectOnDemand { server_id: id }),
        "fs_list" => Some(McpAction::FsList {
            server_id: id,
            path: path(),
            canonical_path: None,
        }),
        "fs_stat" => Some(McpAction::FsStat {
            server_id: id,
            path: path(),
            canonical_path: None,
        }),
        "fs_read" => Some(McpAction::FsRead {
            server_id: id,
            path: path(),
            canonical_path: None,
        }),
        "fs_write" => Some(McpAction::FsWrite {
            server_id: id,
            path: path(),
            canonical_path: None,
            size: None,
        }),
        "fs_patch" => Some(McpAction::FsPatch {
            server_id: id,
            path: path(),
            canonical_path: None,
            size: None,
        }),
        "fs_mkdir" => Some(McpAction::FsMkdir {
            server_id: id,
            path: path(),
            canonical_path: None,
        }),
        "fs_delete" => Some(McpAction::FsDelete {
            server_id: id,
            path: path(),
            canonical_path: None,
        }),
        "fs_chmod" => Some(McpAction::FsChmod {
            server_id: id,
            path: path(),
            canonical_path: None,
        }),
        "fs_move" => {
            let mut parts = arguments.split_whitespace();
            Some(McpAction::FsMove {
                server_id: id,
                from: parts.next()?.to_owned(),
                from_canonical_path: None,
                to: parts.next()?.to_owned(),
                to_canonical_path: None,
            })
        }
        "transfer_upload" | "transfer_download" => {
            let mut parts = arguments.split_whitespace();
            let local_path = parts.next()?.to_owned();
            let remote_path = parts.next()?.to_owned();
            if operation == "transfer_upload" {
                Some(McpAction::TransferUpload {
                    server_id: id,
                    local_path,
                    local_canonical_path: None,
                    remote_path,
                    remote_canonical_path: None,
                    size: None,
                })
            } else {
                Some(McpAction::TransferDownload {
                    server_id: id,
                    local_path,
                    local_canonical_path: None,
                    remote_path,
                    remote_canonical_path: None,
                    size: None,
                })
            }
        }
        "exec" | "exec_command" => Some(McpAction::ExecCommand {
            server_id: id,
            command: arguments.to_owned(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecMode;
    use std::time::{Duration, UNIX_EPOCH};

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn engine(availability: ServerAvailability, profile_id: &str) -> PolicyEngine {
        let mut config = McpConfig {
            enabled: true,
            ..McpConfig::default()
        };
        config.servers.insert(
            "srv".to_owned(),
            ServerMcpPolicy {
                availability,
                profile_id: profile_id.to_owned(),
                alias: Some("Producción".to_owned()),
                notes: Some("servidor de pruebas".to_owned()),
                allowed_remote_paths: vec!["/srv/api/**".to_owned()],
                allowed_local_paths: vec!["~/proyectos/**".to_owned()],
                ..ServerMcpPolicy::default()
            },
        );
        PolicyEngine::new(config)
    }

    fn read_action() -> McpAction {
        McpAction::FsRead {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
        }
    }

    fn ctx() -> EvalContext {
        EvalContext {
            server_connected: true,
            vela_gui_open: true,
            open_in_edits_dirty: false,
            home_dir: Some(PathBuf::from("/home/alice")),
            vela_config_dir: Some(PathBuf::from("/home/alice/.config/vela")),
            now: at(100),
        }
    }

    #[test]
    fn disabled_mcp_denies_everything() {
        let engine = PolicyEngine::new(McpConfig::default());
        let decision = engine.evaluate(&read_action(), &ctx());
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "mcp_disabled");
    }

    #[test]
    fn listing_servers_is_allowed_without_a_server_policy() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let decision = policy_engine.evaluate(&McpAction::ListServers, &ctx());
        assert_eq!(decision.outcome, Decision::Allow);
        assert_eq!(decision.matched_rule, "info_allowed");
    }

    #[test]
    fn availability_disabled_denies() {
        let engine = engine(ServerAvailability::Disabled, "readonly");
        let decision = engine.evaluate(&read_action(), &ctx());
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "server_disabled");
    }

    #[test]
    fn connected_only_denies_when_disconnected() {
        let engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let mut context = ctx();
        context.server_connected = false;
        let decision = engine.evaluate(&read_action(), &context);
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "server_disconnected");
    }

    #[test]
    fn connected_only_allows_when_connected() {
        let engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let decision = engine.evaluate(&read_action(), &ctx());
        assert_eq!(decision.outcome, Decision::Allow);
    }

    #[test]
    fn on_demand_ask_asks_when_disconnected() {
        let engine = engine(ServerAvailability::OnDemandAsk, "readonly");
        let mut context = ctx();
        context.server_connected = false;
        let decision = engine.evaluate(&read_action(), &context);
        assert_eq!(decision.outcome, Decision::Ask);
        assert_eq!(decision.matched_rule, "on_demand_ask");
        let mut context = ctx();
        context.server_connected = true;
        assert_eq!(
            engine.evaluate(&read_action(), &context).outcome,
            Decision::Allow
        );
    }

    #[test]
    fn headless_availability_works_without_connection() {
        let engine = engine(ServerAvailability::Headless, "readonly");
        let mut context = ctx();
        context.server_connected = false;
        assert_eq!(
            engine.evaluate(&read_action(), &context).outcome,
            Decision::Allow
        );
    }

    #[test]
    fn unknown_server_is_denied() {
        let engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let action = McpAction::FsRead {
            server_id: "desconocido".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
        };
        assert_eq!(engine.evaluate(&action, &ctx()).outcome, Decision::Deny);
    }

    #[test]
    fn edits_conflict_denies_write_patch_delete_and_move() {
        let engine = engine(ServerAvailability::ConnectedOnly, "development");
        let mut context = ctx();
        context.open_in_edits_dirty = true;
        let actions = [
            McpAction::FsWrite {
                server_id: "srv".to_owned(),
                path: "/srv/api/app.js".to_owned(),
                canonical_path: None,
                size: Some(10),
            },
            McpAction::FsPatch {
                server_id: "srv".to_owned(),
                path: "/srv/api/app.js".to_owned(),
                canonical_path: None,
                size: None,
            },
            McpAction::FsDelete {
                server_id: "srv".to_owned(),
                path: "/srv/api/app.js".to_owned(),
                canonical_path: None,
            },
            McpAction::FsMove {
                server_id: "srv".to_owned(),
                from: "/srv/api/a".to_owned(),
                from_canonical_path: None,
                to: "/srv/api/b".to_owned(),
                to_canonical_path: None,
            },
        ];
        for action in actions {
            let decision = engine.evaluate(&action, &context);
            assert_eq!(decision.outcome, Decision::Deny);
            assert_eq!(decision.matched_rule, "edits_conflict");
            assert!(decision.warnings.contains(&PolicyWarning::EditsConflict));
        }
    }

    #[test]
    fn headless_deny_fails_closed() {
        let engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let action = McpAction::FsWrite {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
            size: Some(12),
        };
        let mut context = ctx();
        context.vela_gui_open = false;
        let decision = engine.evaluate(&action, &context);
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "headless_fail_closed");
        assert!(!decision.requires_launch_vela);
    }

    #[test]
    fn headless_open_vela_keeps_ask_and_requests_launch() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        policy_engine.config_mut().headless_ask = HeadlessAskBehavior::OpenVela;
        let action = McpAction::FsWrite {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
            size: None,
        };
        let mut context = ctx();
        context.vela_gui_open = false;
        let decision = policy_engine.evaluate(&action, &context);
        assert_eq!(decision.outcome, Decision::Ask);
        assert!(decision.requires_launch_vela);
        context.vela_gui_open = true;
        let decision = policy_engine.evaluate(&action, &context);
        assert!(!decision.requires_launch_vela);
    }

    #[test]
    fn ui_context_requires_sharing_and_enabled_server() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let action = McpAction::GetUiContext {
            server_id: "srv".to_owned(),
        };
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
        policy_engine.config_mut().share_ui_context = true;
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Allow
        );
        policy_engine
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .availability = ServerAvailability::Disabled;
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
    }

    #[test]
    fn hide_server_identity_global_and_per_server_override() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let identity = ServerIdentity {
            host: "ejemplo.com".to_owned(),
            port: 22,
            user: "root".to_owned(),
        };
        let info = policy_engine
            .describe_server_for_agent("srv", &identity, true, at(100))
            .unwrap();
        assert_eq!(info.id, "srv");
        assert_eq!(info.alias.as_deref(), Some("Producción"));
        assert_eq!(info.notes.as_deref(), Some("servidor de pruebas"));
        assert_eq!(info.effective_profile, "readonly");
        assert_eq!(info.host, None);
        assert_eq!(info.port, None);
        assert_eq!(info.user, None);

        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        policy_engine
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .hide_server_identity = Some(false);
        let info = policy_engine
            .describe_server_for_agent("srv", &identity, false, at(100))
            .unwrap();
        assert_eq!(info.host.as_deref(), Some("ejemplo.com"));
        assert_eq!(info.port, Some(22));
        assert_eq!(info.user.as_deref(), Some("root"));
        assert!(
            policy_engine
                .describe_server_for_agent("otro", &identity, false, at(100))
                .is_none()
        );
    }

    #[test]
    fn jit_profile_override_changes_effective_profile_and_description() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        policy_engine
            .jit_mut()
            .grant_profile("srv", "development", at(300));
        let decision = policy_engine.evaluate(&read_action(), &ctx());
        assert_eq!(decision.outcome, Decision::Allow);
        let identity = ServerIdentity {
            host: "h".to_owned(),
            port: 22,
            user: "u".to_owned(),
        };
        let info = policy_engine
            .describe_server_for_agent("srv", &identity, true, at(100))
            .unwrap();
        assert_eq!(info.effective_profile, "development");
        let mut later = ctx();
        later.now = at(300);
        let info = policy_engine
            .describe_server_for_agent("srv", &identity, true, later.now)
            .unwrap();
        assert_eq!(info.effective_profile, "readonly");
    }

    #[test]
    fn fs_move_validates_both_paths() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "development");
        let make_move = |to: &str| McpAction::FsMove {
            server_id: "srv".to_owned(),
            from: "/srv/api/a".to_owned(),
            from_canonical_path: None,
            to: to.to_owned(),
            to_canonical_path: None,
        };
        assert_eq!(
            policy_engine
                .evaluate(&make_move("/srv/api/b"), &ctx())
                .outcome,
            Decision::Allow
        );
        let outside = policy_engine.evaluate(&make_move("/tmp/b"), &ctx());
        assert_eq!(outside.outcome, Decision::Ask);
        assert!(
            outside
                .warnings
                .iter()
                .any(|warning| matches!(warning, PolicyWarning::OutOfScopePath { .. }))
        );
        let denied = McpAction::FsMove {
            server_id: "srv".to_owned(),
            from: "/srv/api/a".to_owned(),
            from_canonical_path: None,
            to: "/srv/api/.env".to_owned(),
            to_canonical_path: None,
        };
        assert_eq!(
            policy_engine.evaluate(&denied, &ctx()).outcome,
            Decision::Deny
        );
    }

    #[test]
    fn transfers_validate_local_and_remote_paths() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "development");
        let upload = McpAction::TransferUpload {
            server_id: "srv".to_owned(),
            local_path: "~/proyectos/app.js".to_owned(),
            local_canonical_path: None,
            remote_path: "/srv/api/app.js".to_owned(),
            remote_canonical_path: None,
            size: Some(100),
        };
        assert_eq!(
            policy_engine.evaluate(&upload, &ctx()).outcome,
            Decision::Ask
        );
        let bad_local = McpAction::TransferDownload {
            server_id: "srv".to_owned(),
            local_path: "~/.ssh/id_rsa".to_owned(),
            local_canonical_path: None,
            remote_path: "/srv/api/app.js".to_owned(),
            remote_canonical_path: None,
            size: None,
        };
        assert_eq!(
            policy_engine.evaluate(&bad_local, &ctx()).outcome,
            Decision::Deny
        );
        let bad_remote = McpAction::TransferUpload {
            server_id: "srv".to_owned(),
            local_path: "~/proyectos/app.js".to_owned(),
            local_canonical_path: None,
            remote_path: "/etc/shadow".to_owned(),
            remote_canonical_path: None,
            size: None,
        };
        assert_eq!(
            policy_engine.evaluate(&bad_remote, &ctx()).outcome,
            Decision::Deny
        );
    }

    #[test]
    fn base_deny_is_not_escalated_to_ask_by_out_of_scope_path() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let action = McpAction::FsDelete {
            server_id: "srv".to_owned(),
            path: "/tmp/app.js".to_owned(),
            canonical_path: None,
        };
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
    }

    #[test]
    fn scoped_allow_keeps_out_of_scope_path_allowed() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "fs_read", "/tmp/app.log", at(900));
        let action = McpAction::FsRead {
            server_id: "srv".to_owned(),
            path: "/tmp/app.log".to_owned(),
            canonical_path: None,
        };
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Allow
        );
    }

    #[test]
    fn canonical_path_escape_is_denied_by_the_engine() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let action = McpAction::FsRead {
            server_id: "srv".to_owned(),
            path: "/srv/api/x".to_owned(),
            canonical_path: Some("/etc/shadow".to_owned()),
        };
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
    }

    #[test]
    fn exec_respects_mode_and_sudo() {
        let diagnostics = engine(ServerAvailability::ConnectedOnly, "diagnostics");
        let exec = |command: &str| McpAction::ExecCommand {
            server_id: "srv".to_owned(),
            command: command.to_owned(),
        };
        assert_eq!(
            diagnostics
                .evaluate(&exec("systemctl status api"), &ctx())
                .outcome,
            Decision::Allow
        );
        let denied_sudo = diagnostics.evaluate(&exec("sudo nginx -t"), &ctx());
        assert_eq!(denied_sudo.outcome, Decision::Deny);
        assert!(
            denied_sudo
                .warnings
                .contains(&PolicyWarning::Command(CommandWarning::ContainsSudo))
        );

        let mut development = engine(ServerAvailability::ConnectedOnly, "development");
        development
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .sudo = Some(Decision::Ask);
        let asked = development.evaluate(&exec("sudo nginx -t"), &ctx());
        assert_eq!(asked.outcome, Decision::Ask);
        assert_eq!(asked.matched_rule, "sudo_requires_approval");
    }

    #[test]
    fn exec_mode_override_wins_over_profile() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        let action = McpAction::ExecCommand {
            server_id: "srv".to_owned(),
            command: "uname -a".to_owned(),
        };
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
        policy_engine
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .exec_mode = Some(ExecMode::AllowExceptDenylist);
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Allow
        );
    }

    #[test]
    fn simulator_parses_paths_and_commands() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        assert_eq!(
            policy_engine
                .simulate("readonly", "fs_read /etc/nginx/.env")
                .outcome,
            Decision::Deny
        );
        let patch = policy_engine.simulate("readonly", "fs_patch /srv/api/app.js");
        assert_eq!(patch.outcome, Decision::Ask);
        assert_eq!(
            policy_engine
                .simulate("diagnostics", "exec systemctl status api")
                .outcome,
            Decision::Allow
        );
        let sudo = policy_engine.simulate("development", "exec sudo nginx -t");
        assert_eq!(sudo.outcome, Decision::Ask);
        assert!(
            sudo.warnings
                .contains(&PolicyWarning::Command(CommandWarning::ContainsSudo))
        );
        assert_eq!(
            policy_engine
                .simulate("readonly", "fs_move /srv/api/a /tmp/b")
                .outcome,
            Decision::Ask
        );
        assert_eq!(
            policy_engine.simulate("readonly", "chorrada rara").outcome,
            Decision::Deny
        );
    }

    #[test]
    fn simulator_uses_configured_server_when_id_matches() {
        let policy_engine = engine(ServerAvailability::ConnectedOnly, "diagnostics");
        let decision = policy_engine.simulate("srv", "exec systemctl status api");
        assert_eq!(decision.outcome, Decision::Allow);
    }

    #[test]
    fn scoped_allow_does_not_follow_symlinks_to_another_path() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "fs_read", "/srv/api/tmp.log", at(900));
        let action = McpAction::FsRead {
            server_id: "srv".to_owned(),
            path: "/srv/api/tmp.log".to_owned(),
            canonical_path: Some("/var/backups/db.sql".to_owned()),
        };
        let decision = policy_engine.evaluate(&action, &ctx());
        assert_eq!(decision.outcome, Decision::Ask);
        assert_eq!(decision.matched_rule, "out_of_scope_path");
        assert!(
            decision
                .warnings
                .iter()
                .any(|warning| matches!(warning, PolicyWarning::OutOfScopePath { .. }))
        );

        // Tampoco basta conceder solo la ruta real: la solicitada no coincide.
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        policy_engine.jit_mut().grant_scoped_allow(
            "srv",
            "fs_read",
            "/var/backups/db.sql",
            at(900),
        );
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Ask
        );
    }

    #[test]
    fn transfer_upload_denies_overwriting_a_dirty_edit() {
        let engine = engine(ServerAvailability::ConnectedOnly, "production_protected");
        let action = McpAction::TransferUpload {
            server_id: "srv".into(),
            local_path: "/home/alice/proyectos/app.js".into(),
            local_canonical_path: None,
            remote_path: "/srv/api/app.js".into(),
            remote_canonical_path: None,
            size: Some(10),
        };
        let mut ctx = ctx();
        assert_eq!(engine.evaluate(&action, &ctx).outcome, Decision::Ask);
        ctx.open_in_edits_dirty = true;
        let decision = engine.evaluate(&action, &ctx);
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "edits_conflict");
        assert!(decision.warnings.contains(&PolicyWarning::EditsConflict));
    }

    #[test]
    fn saved_rule_resolves_headless_ask_but_never_overrides_denials() {
        use crate::executor::ApprovalPreview;

        let mut engine = engine(ServerAvailability::Headless, "production_protected");
        engine.config_mut().headless_ask = HeadlessAskBehavior::Deny;
        let mut ctx = ctx();
        ctx.vela_gui_open = false;
        let write = |path: &str| McpAction::FsWrite {
            server_id: "srv".into(),
            path: path.into(),
            canonical_path: Some(path.into()),
            size: Some(10),
        };
        let preview = |path: &str| ApprovalPreview {
            operation: "fs_write".into(),
            server_id: "srv".into(),
            paths: vec![path.into()],
            ..Default::default()
        };
        let path = "/srv/api/app.js";
        let evaluate = |engine: &PolicyEngine, context: &EvalContext, path| {
            engine.evaluate_with_preview(&write(path), context, &preview(path))
        };
        assert_eq!(
            evaluate(&engine, &ctx, path).matched_rule,
            "headless_fail_closed"
        );
        engine
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .scoped_rules = [path, "/etc/shadow"]
            .map(|path| preview(path).scoped_rule().unwrap())
            .into();

        let decision = evaluate(&engine, &ctx, path);
        assert_eq!(decision.outcome, Decision::Allow);
        assert_eq!(decision.matched_rule, "saved_scoped_allow");
        assert!(!decision.requires_launch_vela);
        assert_eq!(
            evaluate(&engine, &ctx, "/srv/api/other.js").matched_rule,
            "headless_fail_closed"
        );
        let critical = evaluate(&engine, &ctx, "/etc/shadow");
        assert_eq!(critical.outcome, Decision::Deny);
        assert!(critical.matched_rule.contains("/etc/shadow"));

        ctx.open_in_edits_dirty = true;
        let decision = evaluate(&engine, &ctx, path);
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "edits_conflict");
        ctx.open_in_edits_dirty = false;
        let mut profile = presets::production_protected_profile();
        profile.fs_write = Decision::Deny;
        engine.config_mut().profiles.push(profile);
        let decision = evaluate(&engine, &ctx, path);
        assert_eq!(decision.outcome, Decision::Deny);
        assert_eq!(decision.matched_rule, "fs_write");
    }

    #[test]
    fn scoped_allow_promotes_in_scope_write_ask() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "production_protected");
        policy_engine.jit_mut().grant_scoped_allow(
            "srv",
            "fs_write",
            "/srv//api/./app.js",
            at(900),
        );
        let action = McpAction::FsWrite {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
            size: Some(10),
        };
        let decision = policy_engine.evaluate(&action, &ctx());
        assert_eq!(decision.outcome, Decision::Allow);
        assert_eq!(decision.matched_rule, "jit_scoped_allow");
        let mut later = ctx();
        later.now = at(900);
        assert_eq!(
            policy_engine.evaluate(&action, &later).outcome,
            Decision::Ask
        );
    }

    #[test]
    fn scoped_allow_promotes_exact_exec_command() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "development");
        let exec = |command: &str| McpAction::ExecCommand {
            server_id: "srv".to_owned(),
            command: command.to_owned(),
        };
        assert_eq!(
            policy_engine.evaluate(&exec("uname -a"), &ctx()).outcome,
            Decision::Ask
        );
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "exec_command", "uname -a", at(900));
        let decision = policy_engine.evaluate(&exec("uname -a"), &ctx());
        assert_eq!(decision.outcome, Decision::Allow);
        assert_eq!(decision.matched_rule, "jit_scoped_allow");
        assert_eq!(
            policy_engine.evaluate(&exec("uname -b"), &ctx()).outcome,
            Decision::Ask
        );
    }

    #[test]
    fn scoped_allow_never_bypasses_a_deny() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "development");
        policy_engine
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .sudo = Some(Decision::Deny);
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "fs_read", "/etc/shadow", at(900));
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "exec_command", "sudo nginx -t", at(900));
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "exec_command", "rm -rf /", at(900));
        policy_engine
            .jit_mut()
            .grant_scoped_allow("srv", "fs_write", "/srv/api/app.js", at(900));

        let shadow = McpAction::FsRead {
            server_id: "srv".to_owned(),
            path: "/etc/shadow".to_owned(),
            canonical_path: None,
        };
        assert_eq!(
            policy_engine.evaluate(&shadow, &ctx()).outcome,
            Decision::Deny
        );
        let sudo = McpAction::ExecCommand {
            server_id: "srv".to_owned(),
            command: "sudo nginx -t".to_owned(),
        };
        assert_eq!(
            policy_engine.evaluate(&sudo, &ctx()).outcome,
            Decision::Deny
        );
        let catastrophic = McpAction::ExecCommand {
            server_id: "srv".to_owned(),
            command: "rm -rf /".to_owned(),
        };
        assert_eq!(
            policy_engine.evaluate(&catastrophic, &ctx()).outcome,
            Decision::Deny
        );
        let mut conflict = ctx();
        conflict.open_in_edits_dirty = true;
        let write = McpAction::FsWrite {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
            size: Some(10),
        };
        assert_eq!(
            policy_engine.evaluate(&write, &conflict).outcome,
            Decision::Deny
        );

        let mut on_demand = engine(ServerAvailability::OnDemandAsk, "readonly");
        on_demand
            .jit_mut()
            .grant_scoped_allow("srv", "fs_read", "/tmp/app.log", at(900));
        let outside = McpAction::FsRead {
            server_id: "srv".to_owned(),
            path: "/tmp/app.log".to_owned(),
            canonical_path: None,
        };
        let mut disconnected = ctx();
        disconnected.server_connected = false;
        let decision = on_demand.evaluate(&outside, &disconnected);
        assert_eq!(decision.outcome, Decision::Ask);
        assert_eq!(decision.matched_rule, "on_demand_ask");
    }

    #[test]
    fn describe_server_for_agent_hides_disabled_servers() {
        let identity = ServerIdentity {
            host: "ejemplo.com".to_owned(),
            port: 22,
            user: "root".to_owned(),
        };
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "readonly");
        assert!(
            policy_engine
                .describe_server_for_agent("srv", &identity, true, at(100))
                .is_some()
        );
        policy_engine
            .config_mut()
            .servers
            .get_mut("srv")
            .unwrap()
            .availability = ServerAvailability::Disabled;
        assert!(
            policy_engine
                .describe_server_for_agent("srv", &identity, true, at(100))
                .is_none()
        );

        let mut disabled_globally = engine(ServerAvailability::ConnectedOnly, "readonly");
        disabled_globally.config_mut().enabled = false;
        assert!(
            disabled_globally
                .describe_server_for_agent("srv", &identity, true, at(100))
                .is_none()
        );
    }

    #[test]
    fn describe_permissions_does_not_require_a_connection() {
        let action = McpAction::DescribePermissions {
            server_id: "srv".to_owned(),
        };
        for availability in [
            ServerAvailability::ConnectedOnly,
            ServerAvailability::OnDemandAsk,
        ] {
            let policy_engine = engine(availability, "readonly");
            let mut context = ctx();
            context.server_connected = false;
            let decision = policy_engine.evaluate(&action, &context);
            assert_eq!(decision.outcome, Decision::Allow, "{availability:?}");
            assert_eq!(decision.matched_rule, "info_allowed");
        }
        let disabled = engine(ServerAvailability::Disabled, "readonly");
        assert_eq!(disabled.evaluate(&action, &ctx()).outcome, Decision::Deny);
        let mut globally_disabled = engine(ServerAvailability::ConnectedOnly, "readonly");
        globally_disabled.config_mut().enabled = false;
        assert_eq!(
            globally_disabled.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
    }

    #[test]
    fn rollback_warns_only_for_large_files_when_enabled() {
        let mut policy_engine = engine(ServerAvailability::ConnectedOnly, "development");
        policy_engine.config_mut().rollback.max_file_bytes = 100;
        let write = |size| McpAction::FsWrite {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
            size: Some(size),
        };
        let big = policy_engine.evaluate(&write(101), &ctx());
        assert_eq!(big.outcome, Decision::Allow);
        assert!(
            big.warnings
                .contains(&PolicyWarning::FileExceedsRollbackLimit {
                    size: 101,
                    max_bytes: 100,
                })
        );
        assert!(
            !policy_engine
                .evaluate(&write(100), &ctx())
                .warnings
                .iter()
                .any(|warning| matches!(warning, PolicyWarning::FileExceedsRollbackLimit { .. }))
        );

        policy_engine.config_mut().rollback.enabled = false;
        assert!(
            !policy_engine
                .evaluate(&write(101), &ctx())
                .warnings
                .iter()
                .any(|warning| matches!(warning, PolicyWarning::FileExceedsRollbackLimit { .. }))
        );

        policy_engine.config_mut().rollback.enabled = true;
        let patch = McpAction::FsPatch {
            server_id: "srv".to_owned(),
            path: "/srv/api/app.js".to_owned(),
            canonical_path: None,
            size: Some(101),
        };
        assert!(policy_engine.evaluate(&patch, &ctx()).warnings.contains(
            &PolicyWarning::FileExceedsRollbackLimit {
                size: 101,
                max_bytes: 100,
            }
        ));
    }

    #[test]
    fn simulator_previews_rules_of_a_disabled_server() {
        let policy_engine = engine(ServerAvailability::Disabled, "development");
        let action = McpAction::ExecCommand {
            server_id: "srv".to_owned(),
            command: "uname -a".to_owned(),
        };
        assert_eq!(
            policy_engine.evaluate(&action, &ctx()).outcome,
            Decision::Deny
        );
        assert_eq!(
            policy_engine.simulate("srv", "exec uname -a").outcome,
            Decision::Ask
        );
        assert_eq!(
            policy_engine
                .simulate("development", "exec uname -a")
                .outcome,
            Decision::Ask
        );
    }
}
