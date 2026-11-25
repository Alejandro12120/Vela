//! Esquema de `mcp.toml` y persistencia privada y atómica.

use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    io::Write,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::presets::PermissionProfile;

/// Versión del formato `mcp.toml` que entiende esta versión de Vela.
pub const FILE_VERSION: u32 = 1;
pub const CONFIG_FILE_NAME: &str = "mcp.toml";

/// Qué hacer cuando una aprobación se solicita sin interfaz gráfica.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessAskBehavior {
    /// No aprobar nada sin interfaz: fallo cerrado.
    #[default]
    Deny,
    /// Abrir Vela para que la persona resuelva la aprobación.
    OpenVela,
}

/// Resultado de una decisión de política.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    #[default]
    Ask,
    Deny,
}

/// Disponibilidad de un servidor para el agente MCP.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerAvailability {
    #[default]
    Disabled,
    ConnectedOnly,
    OnDemandAsk,
    Headless,
}

/// Política de ejecución de comandos remotos.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecMode {
    #[default]
    Disabled,
    Allowlist,
    AskAll,
    AllowExceptDenylist,
}

/// Límites de la copia de seguridad previa a una escritura remota.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RollbackConfig {
    pub enabled: bool,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub retention_hours: u64,
}

impl Default for RollbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_bytes: 25 * 1024 * 1024,
            max_total_bytes: 100 * 1024 * 1024,
            retention_hours: 24,
        }
    }
}

/// Política MCP de un servidor, indexada por su `server_id`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServerMcpPolicy {
    #[serde(default)]
    pub availability: ServerAvailability,
    #[serde(default = "default_profile_id")]
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// `None` hereda el ajuste global `hide_server_identity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_server_identity: Option<bool>,
    #[serde(default)]
    pub allowed_remote_paths: Vec<String>,
    #[serde(default)]
    pub denied_remote_paths: Vec<String>,
    /// Vacía por defecto: ninguna ruta local se comparte hasta configurarla.
    #[serde(default)]
    pub allowed_local_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_mode: Option<ExecMode>,
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub denied_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sudo: Option<Decision>,
}

fn default_profile_id() -> String {
    "readonly".to_owned()
}

impl Default for ServerMcpPolicy {
    fn default() -> Self {
        Self {
            availability: ServerAvailability::Disabled,
            profile_id: default_profile_id(),
            alias: None,
            notes: None,
            hide_server_identity: None,
            allowed_remote_paths: Vec::new(),
            denied_remote_paths: Vec::new(),
            allowed_local_paths: Vec::new(),
            exec_mode: None,
            allowed_commands: Vec::new(),
            denied_commands: Vec::new(),
            sudo: None,
        }
    }
}

/// Configuración global del sistema MCP de Vela.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub headless_ask: HeadlessAskBehavior,
    #[serde(default = "default_hide_server_identity")]
    pub hide_server_identity: bool,
    #[serde(default)]
    pub share_ui_context: bool,
    #[serde(default = "default_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
    #[serde(default)]
    pub rollback: RollbackConfig,
    /// Perfiles creados o editados por la persona; se resuelven antes que los integrados.
    #[serde(default)]
    pub profiles: Vec<PermissionProfile>,
    #[serde(default)]
    pub servers: BTreeMap<String, ServerMcpPolicy>,
}

fn default_version() -> u32 {
    FILE_VERSION
}

fn default_hide_server_identity() -> bool {
    true
}

fn default_approval_timeout_secs() -> u64 {
    180
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            version: FILE_VERSION,
            enabled: false,
            headless_ask: HeadlessAskBehavior::Deny,
            hide_server_identity: true,
            share_ui_context: false,
            approval_timeout_secs: 180,
            rollback: RollbackConfig::default(),
            profiles: Vec::new(),
            servers: BTreeMap::new(),
        }
    }
}

impl McpConfig {
    /// Normaliza los valores cargados o editados antes de usarlos.
    ///
    /// El tiempo de aprobación se limita a 30–600 segundos y los perfiles con
    /// identificadores vacíos o repetidos se descartan (el último gana).
    pub fn validate(&mut self) {
        self.version = FILE_VERSION;
        self.approval_timeout_secs = self.approval_timeout_secs.clamp(30, 600);
        self.profiles
            .retain(|profile| !profile.id.trim().is_empty());
        let mut seen = HashSet::new();
        self.profiles
            .retain(|profile| seen.insert(profile.id.clone()));
    }
}

/// Serializa la configuración a JSON para la capa de transporte MCP.
pub fn to_json(config: &McpConfig) -> Result<String, serde_json::Error> {
    serde_json::to_string(config)
}

/// Carga `mcp.toml` desde `dir`. Un archivo inexistente equivale a la configuración por defecto.
pub fn load(dir: &Path) -> io::Result<McpConfig> {
    let text = match fs::read_to_string(dir.join(CONFIG_FILE_NAME)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(McpConfig::default()),
        Err(err) => return Err(err),
    };
    let mut config: McpConfig =
        toml::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    config.validate();
    Ok(config)
}

/// Guarda `mcp.toml` de forma atómica. Se niega a reemplazar un archivo existente
/// que no se pueda interpretar, para no destruir la configuración de la persona.
pub fn save(dir: &Path, config: &McpConfig) -> io::Result<()> {
    load(dir)?;
    let mut config = config.clone();
    config.validate();
    let text = toml::to_string_pretty(&config).map_err(io::Error::other)?;
    save_private(dir, CONFIG_FILE_NAME, &text)
}

#[cfg(unix)]
fn save_private(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join(name))?;
    Ok(())
}

#[cfg(not(unix))]
fn save_private(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn defaults_are_fail_closed_and_private() {
        let config = McpConfig::default();
        assert_eq!(config.version, 1);
        assert!(!config.enabled);
        assert_eq!(config.headless_ask, HeadlessAskBehavior::Deny);
        assert!(config.hide_server_identity);
        assert!(!config.share_ui_context);
        assert_eq!(config.approval_timeout_secs, 180);
        assert!(config.rollback.enabled);
        assert_eq!(config.rollback.max_file_bytes, 25 * 1024 * 1024);
        assert_eq!(config.rollback.max_total_bytes, 100 * 1024 * 1024);
        assert_eq!(config.rollback.retention_hours, 24);
        assert!(config.servers.is_empty());
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn server_policy_defaults_are_restrictive() {
        let policy = ServerMcpPolicy::default();
        assert_eq!(policy.availability, ServerAvailability::Disabled);
        assert_eq!(policy.profile_id, "readonly");
        assert!(policy.allowed_remote_paths.is_empty());
        assert!(policy.denied_remote_paths.is_empty());
        assert!(policy.allowed_local_paths.is_empty());
        assert_eq!(policy.exec_mode, None);
        assert_eq!(policy.sudo, None);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = temp_dir();
        let config = load(dir.path()).unwrap();
        assert_eq!(config, McpConfig::default());
    }

    #[test]
    fn round_trip_preserves_all_sections() {
        let dir = temp_dir();
        let mut config = McpConfig {
            enabled: true,
            headless_ask: HeadlessAskBehavior::OpenVela,
            hide_server_identity: false,
            share_ui_context: true,
            approval_timeout_secs: 90,
            ..McpConfig::default()
        };
        config.servers.insert(
            "prod".to_owned(),
            ServerMcpPolicy {
                availability: ServerAvailability::ConnectedOnly,
                profile_id: "production_protected".to_owned(),
                alias: Some("Producción".to_owned()),
                notes: Some("solo lectura".to_owned()),
                hide_server_identity: Some(true),
                allowed_remote_paths: vec!["/srv/api/**".to_owned()],
                denied_remote_paths: vec!["/srv/api/secrets".to_owned()],
                allowed_local_paths: vec!["~/proyectos/**".to_owned()],
                exec_mode: Some(ExecMode::Allowlist),
                allowed_commands: vec!["ls *".to_owned()],
                denied_commands: vec!["rm *".to_owned()],
                sudo: Some(Decision::Deny),
            },
        );
        save(dir.path(), &config).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn config_serializes_to_json_for_inspection() {
        let json = super::to_json(&McpConfig::default()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["enabled"], false);
        assert_eq!(value["hide_server_identity"], true);
    }

    #[test]
    fn validate_clamps_approval_timeout() {
        let mut config = McpConfig {
            approval_timeout_secs: 1,
            ..McpConfig::default()
        };
        config.validate();
        assert_eq!(config.approval_timeout_secs, 30);
        config.approval_timeout_secs = 10_000;
        config.validate();
        assert_eq!(config.approval_timeout_secs, 600);
    }

    #[test]
    fn load_clamps_timeout_from_file() {
        let dir = temp_dir();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "approval_timeout_secs = 5000\n",
        )
        .unwrap();
        let config = load(dir.path()).unwrap();
        assert_eq!(config.approval_timeout_secs, 600);
    }

    #[test]
    #[cfg(unix)]
    fn saved_file_has_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        save(dir.path(), &McpConfig::default()).unwrap();
        let mode = fs::metadata(dir.path().join(CONFIG_FILE_NAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn save_refuses_to_overwrite_corrupt_file() {
        let dir = temp_dir();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "esto no es toml = = =").unwrap();
        let before = fs::read(&path).unwrap();
        let error = save(dir.path(), &McpConfig::default()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn load_rejects_corrupt_file() {
        let dir = temp_dir();
        fs::write(dir.path().join(CONFIG_FILE_NAME), "[mcp\n").unwrap();
        let error = load(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn validate_drops_empty_and_duplicate_profile_ids() {
        let mut config = McpConfig {
            profiles: vec![
                PermissionProfile {
                    id: String::new(),
                    ..PermissionProfile::default()
                },
                crate::presets::builtin_profile("readonly").unwrap(),
                crate::presets::builtin_profile("readonly").unwrap(),
            ],
            ..McpConfig::default()
        };
        config.validate();
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].id, "readonly");
        assert!(config.servers.is_empty());
    }
}
