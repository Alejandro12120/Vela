//! Modelos de Ajustes MCP independientes de GPUI.

use std::{collections::HashMap, io, path::Path};

use vela_core::Auth;
use vela_mcp::{
    Decision, McpConfig, PermissionProfile, ServerAvailability,
    config::{ExecMode, RollbackConfig},
    harness::{
        self, ChangeReport, EntryState, HarnessEnv, HarnessError, HarnessId, HarnessStatus,
        InstallOptions, VelaCommand,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    Add,
    Remove,
    Update,
    Replace,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "Añadir",
            Self::Remove => "Quitar",
            Self::Update => "Actualizar ruta",
            Self::Replace => "Reemplazar",
        }
    }

    pub fn run(
        self,
        id: HarnessId,
        env: &HarnessEnv,
        command: &VelaCommand,
        dry_run: bool,
    ) -> Result<ChangeReport, HarnessError> {
        let options = InstallOptions {
            dry_run,
            force: self == Self::Replace,
        };
        match self {
            Self::Remove => harness::uninstall(id, env, command, &options),
            _ => harness::install(id, env, command, &options),
        }
    }
}

pub struct HarnessRow<'a> {
    pub label: &'static str,
    pub tone: Tone,
    pub action: Option<Change>,
    pub path: Option<&'a Path>,
    pub detail: Option<String>,
    pub snippet: Option<&'a str>,
}

pub fn harness_row(status: &HarnessStatus) -> HarnessRow<'_> {
    use Change::*;
    use Tone::*;
    let (label, tone, action, path, detail, snippet) = match status {
        HarnessStatus::NotDetected => ("No encontrado", Neutral, None, None, None, None),
        HarnessStatus::Detected {
            config_path, entry, ..
        } => {
            let (label, tone, action, detail) = match entry {
                EntryState::Installed => ("Configurado", Success, Remove, None),
                EntryState::NotInstalled => ("Detectado", Neutral, Add, None),
                EntryState::Outdated { found } => (
                    "Desactualizado",
                    Warning,
                    Update,
                    Some(format!("Ejecutable anterior: {found}")),
                ),
                EntryState::Conflict { found } => (
                    "Ocupado",
                    Warning,
                    Replace,
                    Some(format!("Ocupada por: {found}")),
                ),
            };
            (
                label,
                tone,
                Some(action),
                Some(config_path.as_path()),
                detail,
                None,
            )
        }
        HarnessStatus::ManualRequired {
            config_path,
            reason,
            snippet,
        } => (
            "Manual",
            Warning,
            None,
            Some(config_path.as_path()),
            Some(reason.clone()),
            Some(snippet.as_str()),
        ),
        HarnessStatus::Malformed { config_path, error } => (
            "Archivo inválido",
            Danger,
            None,
            Some(config_path.as_path()),
            Some(sanitize(error)),
            None,
        ),
        HarnessStatus::Unreadable { config_path, .. } => (
            "Sin acceso",
            Danger,
            None,
            Some(config_path.as_path()),
            None,
            None,
        ),
    };
    HarnessRow {
        label,
        tone,
        action,
        path,
        detail,
        snippet,
    }
}

fn sanitize(error: &str) -> String {
    error
        .chars()
        .filter(|ch| !ch.is_control())
        .take(220)
        .collect()
}

pub struct HarnessScan {
    pub env: HarnessEnv,
    pub command: VelaCommand,
    pub statuses: Vec<(HarnessId, HarnessStatus)>,
    pub development_program: bool,
}

impl HarnessScan {
    pub fn load(env: HarnessEnv) -> io::Result<Self> {
        let command = VelaCommand::current()?;
        let statuses = harness::detect_all(&env, &command);
        let development_program = Self::development_program(command.program());
        Ok(Self {
            env,
            command,
            statuses,
            development_program,
        })
    }

    pub fn summary(&self) -> String {
        let detected = self
            .statuses
            .iter()
            .filter(|(_, status)| *status != HarnessStatus::NotDetected)
            .count();
        let installed: Vec<_> = self
            .statuses
            .iter()
            .filter(|(_, status)| {
                matches!(
                    status,
                    HarnessStatus::Detected {
                        entry: EntryState::Installed,
                        ..
                    }
                )
            })
            .map(|(id, _)| id.display_name())
            .collect();
        let names = if installed.is_empty() {
            String::new()
        } else {
            format!(" ({})", installed.join(", "))
        };
        format!(
            "{detected} de {} detectados · {} configurados{names}",
            HarnessId::ALL.len(),
            installed.len()
        )
    }

    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.env.home).map_or_else(
            |_| path.display().to_string(),
            |suffix| Path::new("~").join(suffix).display().to_string(),
        )
    }

    fn development_program(path: &Path) -> bool {
        path.starts_with("/tmp")
            || path.starts_with("/private/tmp")
            || path.components().any(|part| part.as_os_str() == "target")
            || std::env::var_os("CARGO_TARGET_DIR").is_some_and(|dir| path.starts_with(dir))
            || path.parent().is_some_and(|bin| {
                bin.file_name()
                    .is_some_and(|name| name == "debug" || name == "release")
                    && (bin.join(".fingerprint").is_dir() || bin.join(".cargo-lock").is_file())
            })
    }
}

#[derive(Default)]
pub struct PendingChanges(HashMap<HarnessId, Change>);

impl PendingChanges {
    pub fn get(&self, id: HarnessId) -> Option<Change> {
        self.0.get(&id).copied()
    }

    pub fn toggle(&mut self, id: HarnessId, action: Change) {
        if self.0.remove(&id).is_none() {
            self.0.insert(id, action);
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn can_apply(&self, busy: bool) -> bool {
        !busy && !self.0.is_empty()
    }

    pub fn take(&mut self) -> Vec<(HarnessId, Change)> {
        HarnessId::ALL
            .into_iter()
            .filter_map(|id| self.0.remove(&id).map(|action| (id, action)))
            .collect()
    }
}

pub fn harness_result(
    result: Result<ChangeReport, HarnessError>,
    scan: &HarnessScan,
) -> (String, Tone) {
    use harness::Action;
    match result {
        Ok(report) => {
            let action = match report.action {
                Action::Created => "Añadido",
                Action::Updated => "Actualizado",
                Action::Removed => "Quitado",
                Action::AlreadyUpToDate => "Ya configurado",
                Action::NothingToRemove => "Ya estaba quitado",
            };
            let text = report.backup.map_or_else(
                || action.to_owned(),
                |path| format!("{action} · Copia: {}", scan.display_path(&path)),
            );
            (text, Tone::Success)
        }
        Err(error) => (harness_error(&error), Tone::Danger),
    }
}

pub fn harness_error(error: &HarnessError) -> String {
    if matches!(error, HarnessError::ConcurrentModification { .. }) {
        "El archivo cambió durante la operación. Vuelve a detectar y reintenta.".into()
    } else {
        sanitize(&error.to_string())
    }
}

pub fn valid_notes(notes: &str) -> bool {
    !notes.chars().any(char::is_control)
}

pub fn availability_allowed(availability: ServerAvailability, auth: &Auth) -> bool {
    availability != ServerAvailability::Headless || matches!(auth, Auth::Agent { .. })
}

pub fn decision_label(decision: Decision) -> &'static str {
    match decision {
        Decision::Allow => "Allow",
        Decision::Ask => "Ask",
        Decision::Deny => "Deny",
    }
}

pub fn exec_label(mode: ExecMode) -> &'static str {
    match mode {
        ExecMode::Disabled => "Denegar",
        ExecMode::Allowlist => "Lista de permitidos",
        ExecMode::AskAll => "Preguntar siempre",
        ExecMode::AllowExceptDenylist => "Permitir salvo prohibidos",
    }
}

pub fn profile_summary(profile: &PermissionProfile) -> String {
    format!(
        "Lectura {} · Escritura {} · Borrado {} · SSH {}",
        decision_label(profile.fs_read),
        decision_label(profile.fs_write),
        decision_label(profile.fs_delete),
        exec_label(profile.exec_mode)
    )
}

pub fn profile_usage(config: &McpConfig, id: &str) -> usize {
    config
        .servers
        .values()
        .filter(|policy| policy.profile_id == id)
        .count()
}

pub fn rollback_limits(
    file: &str,
    total: &str,
    hours: &str,
    enabled: bool,
) -> Option<RollbackConfig> {
    let bytes = |value: &str| {
        value
            .trim()
            .parse::<u64>()
            .ok()?
            .checked_mul(1024 * 1024)
            .filter(|&bytes| bytes <= i64::MAX as u64)
    };
    let max_file_bytes = bytes(file)?;
    let max_total_bytes = bytes(total)?;
    let retention_hours = hours.trim().parse::<u32>().ok()?.into();
    (max_file_bytes > 0 && max_file_bytes <= max_total_bytes && retention_hours > 0).then_some(
        RollbackConfig {
            enabled,
            max_file_bytes,
            max_total_bytes,
            retention_hours,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vela_mcp::{
        EvalContext, PolicyEngine, ServerMcpPolicy, path::valid_scope_pattern, presets,
    };

    #[test]
    fn each_harness_status_exposes_only_its_applicable_action() {
        let path = Path::new("/fixture/config");
        for (entry, label, action, tone, detail) in [
            (
                EntryState::Installed,
                "Configurado",
                Change::Remove,
                Tone::Success,
                None,
            ),
            (
                EntryState::NotInstalled,
                "Detectado",
                Change::Add,
                Tone::Neutral,
                None,
            ),
            (
                EntryState::Outdated {
                    found: "/old/vela".into(),
                },
                "Desactualizado",
                Change::Update,
                Tone::Warning,
                Some("Ejecutable anterior: /old/vela"),
            ),
            (
                EntryState::Conflict {
                    found: "url: example.test".into(),
                },
                "Ocupado",
                Change::Replace,
                Tone::Warning,
                Some("Ocupada por: url: example.test"),
            ),
        ] {
            let status = HarnessStatus::Detected {
                config_path: path.into(),
                config_exists: true,
                entry,
            };
            let row = harness_row(&status);
            assert_eq!(
                (
                    row.label,
                    row.action,
                    row.tone,
                    row.path,
                    row.detail.as_deref()
                ),
                (label, Some(action), tone, Some(path), detail)
            );
        }
        for (status, label, tone, detail, snippet) in [
            (
                HarnessStatus::NotDetected,
                "No encontrado",
                Tone::Neutral,
                None,
                None,
            ),
            (
                HarnessStatus::Malformed {
                    config_path: path.into(),
                    error: "Error\r\n".into(),
                },
                "Archivo inválido",
                Tone::Danger,
                Some("Error"),
                None,
            ),
            (
                HarnessStatus::Unreadable {
                    config_path: path.into(),
                    error: "Permiso".into(),
                },
                "Sin acceso",
                Tone::Danger,
                None,
                None,
            ),
            (
                HarnessStatus::ManualRequired {
                    config_path: path.into(),
                    reason: "Dos archivos".into(),
                    snippet: "snippet".into(),
                },
                "Manual",
                Tone::Warning,
                Some("Dos archivos"),
                Some("snippet"),
            ),
        ] {
            let row = harness_row(&status);
            assert_eq!(
                (
                    row.label,
                    row.tone,
                    row.detail.as_deref(),
                    row.snippet,
                    row.action
                ),
                (label, tone, detail, snippet, None)
            );
        }
    }

    #[test]
    fn pending_changes_toggle_and_apply_in_harness_order() {
        let mut pending = PendingChanges::default();
        assert!(!pending.can_apply(false));
        pending.toggle(HarnessId::Cursor, Change::Remove);
        pending.toggle(HarnessId::ClaudeCode, Change::Add);
        pending.toggle(HarnessId::Antigravity, Change::Update);
        pending.toggle(HarnessId::Antigravity, Change::Update);
        assert_eq!(pending.len(), 2);
        assert!(pending.can_apply(false));
        assert!(!pending.can_apply(true));
        assert_eq!(
            pending.take(),
            [
                (HarnessId::ClaudeCode, Change::Add),
                (HarnessId::Cursor, Change::Remove)
            ]
        );
        assert!(!pending.can_apply(false));
    }

    #[test]
    fn validates_patterns_notes_and_rollback_limits() {
        for pattern in ["/srv/**", "**/.secret", "/foo?.txt", "/a/with space"] {
            assert!(valid_scope_pattern(pattern, false));
        }
        for pattern in ["", "abc", "/../../x", "~/remote/**", "/null\0byte"] {
            assert!(!valid_scope_pattern(pattern, false));
        }
        assert!(valid_scope_pattern("~/local/**", true));
        assert!(valid_notes("Producción española · solo lectura"));
        assert!(!valid_notes("notas\0"));
        assert!(!valid_notes("a\nb"));
        assert!(rollback_limits("25", "100", "24", true).is_some());
        assert!(rollback_limits("100", "25", "24", true).is_none());
        assert!(rollback_limits("a", "25", "0", true).is_none());
    }

    #[test]
    fn availability_and_identity_inherit_conservative_defaults() {
        let mut config = McpConfig::default();
        let mut policy = ServerMcpPolicy::default();
        assert_eq!(policy.availability, ServerAvailability::Disabled);
        assert!(policy.hides_identity(&config));
        config.hide_server_identity = false;
        assert!(!policy.hides_identity(&config));
        policy.hide_server_identity = Some(true);
        assert!(policy.hides_identity(&config));
        policy.hide_server_identity = Some(false);
        config.hide_server_identity = true;
        assert!(!policy.hides_identity(&config));
        assert!(!availability_allowed(
            ServerAvailability::Headless,
            &Auth::Password
        ));
        assert!(availability_allowed(
            ServerAvailability::OnDemandAsk,
            &Auth::Password
        ));
        assert!(availability_allowed(
            ServerAvailability::Headless,
            &Auth::Agent {
                identity_pub: "/fixture/key.pub".into(),
                agent_socket: None,
            }
        ));
    }

    #[test]
    fn profile_summary_and_usage_are_derived_from_effective_values() {
        let mut profile = presets::readonly_profile();
        let original = profile_summary(&profile);
        profile.fs_read = Decision::Deny;
        assert_ne!(original, profile_summary(&profile));
        assert!(profile_summary(&profile).contains("Lectura Deny"));
        let mut config = McpConfig::default();
        assert_eq!(profile_usage(&config, &profile.id), 0);
        config
            .servers
            .insert("test".into(), ServerMcpPolicy::default());
        assert_eq!(profile_usage(&config, &profile.id), 1);
    }

    #[test]
    fn simulator_distinguishes_profiles_from_servers_and_prioritizes_denies() {
        let mut config = McpConfig::default();
        let mut profile = presets::readonly_profile();
        profile.default_allowed_remote_paths = vec!["/srv/**".into()];
        config.profiles.push(profile);
        config.servers.insert(
            "readonly".into(),
            ServerMcpPolicy {
                denied_remote_paths: vec!["/srv/private/**".into()],
                ..Default::default()
            },
        );
        let engine = PolicyEngine::new(config);
        for (server, target, input, outcome) in [
            (
                false,
                "readonly",
                "fs_read /srv/private/data",
                Decision::Allow,
            ),
            (
                true,
                "readonly",
                "fs_read /srv/private/data",
                Decision::Deny,
            ),
            (true, "readonly", "fs_read /srv/data", Decision::Allow),
            (
                false,
                "readonly",
                "fs_read /var/log/nginx.log",
                Decision::Ask,
            ),
            (false, "development", "exec uname -a", Decision::Ask),
            (false, "readonly", "fs_read /srv/.env", Decision::Deny),
            (false, "readonly", "fs_read", Decision::Deny),
        ] {
            let decision = engine.simulate_target(target, server, input, &EvalContext::default());
            assert_eq!(decision.outcome, outcome, "{input}: {decision:?}");
        }
        let invalid = engine.simulate_target("readonly", false, "exec", &EvalContext::default());
        assert_eq!(invalid.matched_rule, "unknown_simulated_input");
        let deny = engine.simulate_target(
            "readonly",
            true,
            "fs_read /srv/private/data",
            &EvalContext::default(),
        );
        assert_eq!(deny.matched_rule, "/srv/private/**");
    }
}
