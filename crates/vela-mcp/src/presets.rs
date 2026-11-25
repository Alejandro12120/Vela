//! Perfiles de permisos de fábrica y resolución de perfiles efectivos.

use serde::{Deserialize, Serialize};

use crate::config::{Decision, ExecMode, McpConfig};

/// Denylist de seguridad que ningún perfil ni servidor puede relajar.
pub const CRITICAL_DENIED_PATHS: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/*id_rsa*",
    "**/*id_ed25519*",
    "/etc/shadow",
    "/etc/gshadow",
    "/root/.ssh/**",
    "**/.ssh/**",
];

/// Devuelve la denylist crítica como cadenas independientes.
pub fn critical_denied_paths() -> Vec<String> {
    CRITICAL_DENIED_PATHS
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect()
}

/// Un perfil de permisos, ya sea integrado o definido por la persona.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PermissionProfile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
    pub fs_list: Decision,
    pub fs_stat: Decision,
    pub fs_read: Decision,
    pub fs_write: Decision,
    pub fs_patch: Decision,
    pub fs_mkdir: Decision,
    pub fs_move: Decision,
    pub fs_delete: Decision,
    pub fs_chmod: Decision,
    pub transfer_upload: Decision,
    pub transfer_download: Decision,
    pub exec_mode: ExecMode,
    pub sudo: Decision,
    pub default_allowed_remote_paths: Vec<String>,
    pub default_denied_remote_paths: Vec<String>,
    pub allowed_commands: Vec<String>,
    pub denied_commands: Vec<String>,
}

impl Default for PermissionProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            builtin: false,
            fs_list: Decision::Ask,
            fs_stat: Decision::Ask,
            fs_read: Decision::Ask,
            fs_write: Decision::Ask,
            fs_patch: Decision::Ask,
            fs_mkdir: Decision::Ask,
            fs_move: Decision::Ask,
            fs_delete: Decision::Ask,
            fs_chmod: Decision::Ask,
            transfer_upload: Decision::Ask,
            transfer_download: Decision::Ask,
            exec_mode: ExecMode::Disabled,
            sudo: Decision::Deny,
            default_allowed_remote_paths: Vec::new(),
            default_denied_remote_paths: critical_denied_paths(),
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
        }
    }
}

impl PermissionProfile {
    /// Decisión del perfil para una operación de archivo o transferencia.
    ///
    /// Los nombres son los mismos que usa el simulador (`fs_read`, `fs_write`, …).
    pub fn decision_for(&self, operation: &str) -> Option<Decision> {
        Some(match operation {
            "fs_list" => self.fs_list,
            "fs_stat" => self.fs_stat,
            "fs_read" => self.fs_read,
            "fs_write" => self.fs_write,
            "fs_patch" => self.fs_patch,
            "fs_mkdir" => self.fs_mkdir,
            "fs_move" => self.fs_move,
            "fs_delete" => self.fs_delete,
            "fs_chmod" => self.fs_chmod,
            "transfer_upload" => self.transfer_upload,
            "transfer_download" => self.transfer_download,
            _ => return None,
        })
    }

    /// Combina la denylist crítica con las reglas propias del perfil.
    pub fn denied_paths(&self) -> Vec<String> {
        let mut denied = critical_denied_paths();
        for pattern in &self.default_denied_remote_paths {
            if !denied.contains(pattern) {
                denied.push(pattern.clone());
            }
        }
        denied
    }
}

fn base(id: &str, name: &str, description: &str) -> PermissionProfile {
    PermissionProfile {
        id: id.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
        builtin: true,
        ..PermissionProfile::default()
    }
}

/// Los cuatro perfiles de fábrica, en el orden en que se muestran en Ajustes.
pub fn builtin_profiles() -> Vec<PermissionProfile> {
    vec![
        readonly_profile(),
        diagnostics_profile(),
        development_profile(),
        production_protected_profile(),
    ]
}

/// Busca un perfil integrado por identificador.
pub fn builtin_profile(id: &str) -> Option<PermissionProfile> {
    builtin_profiles()
        .into_iter()
        .find(|profile| profile.id == id)
}

/// Resuelve un perfil: los perfiles del usuario (incluidos los editados) ganan
/// sobre los integrados con el mismo identificador.
pub fn resolve_profile(config: &McpConfig, id: &str) -> Option<PermissionProfile> {
    config
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .cloned()
        .or_else(|| builtin_profile(id))
}

/// Perfiles visibles en la interfaz: primero los del usuario, luego los integrados.
pub fn all_profiles(config: &McpConfig) -> Vec<PermissionProfile> {
    let mut profiles = config.profiles.clone();
    for builtin in builtin_profiles() {
        if !profiles.iter().any(|profile| profile.id == builtin.id) {
            profiles.push(builtin);
        }
    }
    profiles
}

pub fn readonly_profile() -> PermissionProfile {
    PermissionProfile {
        fs_list: Decision::Allow,
        fs_stat: Decision::Allow,
        fs_read: Decision::Allow,
        fs_delete: Decision::Deny,
        transfer_download: Decision::Allow,
        ..base(
            "readonly",
            "Solo lectura",
            "Explorar y descargar sin modificar el servidor.",
        )
    }
}

pub fn diagnostics_profile() -> PermissionProfile {
    PermissionProfile {
        fs_list: Decision::Allow,
        fs_stat: Decision::Allow,
        fs_read: Decision::Allow,
        fs_delete: Decision::Deny,
        transfer_download: Decision::Allow,
        exec_mode: ExecMode::Allowlist,
        allowed_commands: vec![
            "ls *".to_owned(),
            "df *".to_owned(),
            "free *".to_owned(),
            "uptime".to_owned(),
            "ps *".to_owned(),
            "ss *".to_owned(),
            "systemctl status *".to_owned(),
            "journalctl *".to_owned(),
            "docker ps*".to_owned(),
            "docker logs *".to_owned(),
            "nginx -t".to_owned(),
            "uname *".to_owned(),
        ],
        ..base(
            "diagnostics",
            "Diagnóstico",
            "Inspeccionar el servidor con comandos seguros de lectura.",
        )
    }
}

pub fn development_profile() -> PermissionProfile {
    PermissionProfile {
        fs_list: Decision::Allow,
        fs_stat: Decision::Allow,
        fs_read: Decision::Allow,
        fs_write: Decision::Allow,
        fs_patch: Decision::Allow,
        fs_mkdir: Decision::Allow,
        fs_move: Decision::Allow,
        fs_delete: Decision::Ask,
        fs_chmod: Decision::Ask,
        transfer_upload: Decision::Ask,
        transfer_download: Decision::Ask,
        exec_mode: ExecMode::AskAll,
        sudo: Decision::Ask,
        ..base(
            "development",
            "Desarrollo",
            "Editar ficheros y ejecutar comandos con aprobación.",
        )
    }
}

pub fn production_protected_profile() -> PermissionProfile {
    PermissionProfile {
        fs_list: Decision::Allow,
        fs_stat: Decision::Allow,
        fs_read: Decision::Allow,
        exec_mode: ExecMode::AskAll,
        ..base(
            "production_protected",
            "Producción protegido",
            "Lectura libre; cualquier cambio requiere aprobación explícita.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_builtin_profiles_with_expected_ids() {
        let profiles = builtin_profiles();
        assert_eq!(profiles.len(), 4);
        assert_eq!(
            profiles.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec![
                "readonly",
                "diagnostics",
                "development",
                "production_protected"
            ]
        );
        assert!(profiles.iter().all(|profile| profile.builtin));
    }

    #[test]
    fn readonly_allows_reads_and_download_but_denies_delete() {
        let profile = readonly_profile();
        assert_eq!(profile.fs_list, Decision::Allow);
        assert_eq!(profile.fs_stat, Decision::Allow);
        assert_eq!(profile.fs_read, Decision::Allow);
        assert_eq!(profile.fs_delete, Decision::Deny);
        assert_eq!(profile.fs_write, Decision::Ask);
        assert_eq!(profile.fs_patch, Decision::Ask);
        assert_eq!(profile.fs_mkdir, Decision::Ask);
        assert_eq!(profile.fs_move, Decision::Ask);
        assert_eq!(profile.fs_chmod, Decision::Ask);
        assert_eq!(profile.transfer_upload, Decision::Ask);
        assert_eq!(profile.transfer_download, Decision::Allow);
        assert_eq!(profile.exec_mode, ExecMode::Disabled);
        assert_eq!(profile.sudo, Decision::Deny);
    }

    #[test]
    fn diagnostics_shares_readonly_filesystem_decisions() {
        let readonly = readonly_profile();
        let diagnostics = diagnostics_profile();
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
            assert_eq!(
                diagnostics.decision_for(operation),
                readonly.decision_for(operation),
                "differ on {operation}"
            );
        }
        assert_eq!(diagnostics.exec_mode, ExecMode::Allowlist);
        assert_eq!(diagnostics.sudo, Decision::Deny);
    }

    #[test]
    fn diagnostics_allowlist_has_no_file_reading_commands() {
        let commands = diagnostics_profile().allowed_commands;
        assert!(
            commands
                .iter()
                .any(|command| command == "systemctl status *")
        );
        assert!(commands.iter().any(|command| command == "journalctl *"));
        assert!(commands.iter().any(|command| command == "docker ps*"));
        assert!(commands.iter().any(|command| command == "nginx -t"));
        for forbidden in ["cat", "head", "tail"] {
            assert!(
                commands
                    .iter()
                    .all(|command| command.split_whitespace().next() != Some(forbidden)),
                "{forbidden} must not be allowed"
            );
        }
    }

    #[test]
    fn development_permits_editing_and_asks_for_elevation() {
        let profile = development_profile();
        for operation in ["fs_write", "fs_patch", "fs_mkdir", "fs_move"] {
            assert_eq!(profile.decision_for(operation), Some(Decision::Allow));
        }
        for operation in [
            "fs_delete",
            "fs_chmod",
            "transfer_upload",
            "transfer_download",
        ] {
            assert_eq!(profile.decision_for(operation), Some(Decision::Ask));
        }
        assert_eq!(profile.exec_mode, ExecMode::AskAll);
        assert_eq!(profile.sudo, Decision::Ask);
    }

    #[test]
    fn production_protected_asks_for_every_write() {
        let profile = production_protected_profile();
        assert_eq!(profile.fs_read, Decision::Allow);
        for operation in [
            "fs_write",
            "fs_patch",
            "fs_mkdir",
            "fs_move",
            "fs_delete",
            "fs_chmod",
            "transfer_upload",
            "transfer_download",
        ] {
            assert_eq!(profile.decision_for(operation), Some(Decision::Ask));
        }
        assert_eq!(profile.exec_mode, ExecMode::AskAll);
        assert_eq!(profile.sudo, Decision::Deny);
    }

    #[test]
    fn every_profile_includes_the_critical_denylist() {
        for profile in builtin_profiles() {
            for pattern in CRITICAL_DENIED_PATHS {
                assert!(
                    profile
                        .default_denied_remote_paths
                        .contains(&(*pattern).to_owned()),
                    "{} is missing {}",
                    profile.id,
                    pattern
                );
            }
            for pattern in CRITICAL_DENIED_PATHS {
                assert!(
                    profile.denied_paths().contains(&(*pattern).to_owned()),
                    "{} is missing {} in effective denylist",
                    profile.id,
                    pattern
                );
            }
        }
    }

    #[test]
    fn resolve_profile_prefers_user_profiles_then_builtins() {
        let mut config = McpConfig::default();
        assert_eq!(resolve_profile(&config, "readonly").unwrap().id, "readonly");
        let mut custom = PermissionProfile {
            id: "readonly".to_owned(),
            name: "Mío".to_owned(),
            ..PermissionProfile::default()
        };
        custom.fs_write = Decision::Allow;
        config.profiles.push(custom);
        let resolved = resolve_profile(&config, "readonly").unwrap();
        assert_eq!(resolved.name, "Mío");
        assert_eq!(resolved.fs_write, Decision::Allow);
        assert!(resolve_profile(&config, "inexistente").is_none());
        assert_eq!(all_profiles(&config).len(), 4);
    }
}
