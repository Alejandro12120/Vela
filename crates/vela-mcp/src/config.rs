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
#[serde(default)]
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

/// Excepción aprobada por la persona para una operación y su destino exacto.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopedRule {
    pub operation: String,
    pub scope: String,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped_rules: Vec<ScopedRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sudo: Option<Decision>,
}

impl ServerMcpPolicy {
    pub fn hides_identity(&self, config: &McpConfig) -> bool {
        self.hide_server_identity
            .unwrap_or(config.hide_server_identity)
    }
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
            scoped_rules: Vec::new(),
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
        // Recorremos al revés para que, en caso de identificadores repetidos,
        // gane la última definición y se conserve el orden de las supervivientes.
        let mut seen = HashSet::new();
        self.profiles.reverse();
        self.profiles
            .retain(|profile| seen.insert(profile.id.clone()));
        self.profiles.reverse();
    }
}

/// Serializa la configuración a JSON para la capa de transporte MCP.
pub fn to_json(config: &McpConfig) -> Result<String, serde_json::Error> {
    serde_json::to_string(config)
}

/// Carga `mcp.toml` desde `dir`. Un archivo inexistente equivale a la configuración por defecto.
pub fn load(dir: &Path) -> io::Result<McpConfig> {
    Ok(ConfigSnapshot::load(dir)?.config)
}

fn parse(text: &str) -> io::Result<McpConfig> {
    let mut config: McpConfig = toml::from_str(text).map_err(invalid_data)?;
    config.validate();
    Ok(config)
}

fn invalid_data(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn read(dir: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(dir.join(CONFIG_FILE_NAME)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Configuración y contenido original, usados para detectar ediciones externas.
#[derive(Clone, Debug)]
pub struct ConfigSnapshot {
    pub config: McpConfig,
    source: Option<String>,
}

impl ConfigSnapshot {
    pub fn load(dir: &Path) -> io::Result<Self> {
        let source = read(dir)?;
        let config = source
            .as_deref()
            .map(parse)
            .transpose()?
            .unwrap_or_default();
        Ok(Self { config, source })
    }

    pub fn save(&self, dir: &Path, config: &McpConfig) -> io::Result<Self> {
        use toml_edit::DocumentMut;

        let mut config = config.clone();
        config.validate();
        let encode = |config: &McpConfig| -> io::Result<DocumentMut> {
            toml::to_string_pretty(config)
                .map_err(invalid_data)?
                .parse()
                .map_err(invalid_data)
        };
        let mut document: DocumentMut = self
            .source
            .as_deref()
            .unwrap_or("")
            .parse()
            .map_err(invalid_data)?;
        let old = encode(&self.config)?;
        let new = encode(&config)?;
        patch_table(
            document.as_table_mut(),
            old.as_table(),
            new.as_table(),
            false,
        );
        let text = document.to_string();
        if parse(&text)? != config {
            return Err(invalid_data("No se pudo validar la edición de mcp.toml."));
        }

        fs::create_dir_all(dir)?;
        let directory = fs::File::open(dir)?;
        directory.lock()?;
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temp.write_all(text.as_bytes())?;
        temp.as_file().sync_all()?;
        if read(dir)? != self.source {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "La configuración cambió fuera de esta ventana.",
            ));
        }
        temp.persist(dir.join(CONFIG_FILE_NAME))?;
        directory.sync_all()?;
        Ok(Self {
            config,
            source: Some(text),
        })
    }
}

fn patch_table(
    target: &mut dyn toml_edit::TableLike,
    old: &dyn toml_edit::TableLike,
    new: &dyn toml_edit::TableLike,
    inlined: bool,
) {
    for (key, _) in old.iter().filter(|(key, _)| !new.contains_key(key)) {
        target.remove(key);
    }
    for (key, value) in new.iter() {
        let before = old.get(key);
        if before.is_some_and(|before| formatted_value(before) == formatted_value(value)) {
            continue;
        }
        match target.get_mut(key) {
            // Unlike profiles, scoped rules have no ID to merge array tables by.
            Some(current) if key != "scoped_rules" => patch_item(current, before, value, inlined),
            _ => {
                let mut value = value.clone();
                if inlined {
                    value.make_value();
                }
                target.insert(key, value);
            }
        }
    }
}

fn formatted_value(item: &toml_edit::Item) -> String {
    item.clone()
        .into_value()
        .map_or_else(|item| item.to_string(), |value| value.to_string())
}

fn patch_item(
    current: &mut toml_edit::Item,
    before: Option<&toml_edit::Item>,
    value: &toml_edit::Item,
    inlined: bool,
) {
    use toml_edit::Item;
    let inline_table = current.as_inline_table().is_some();

    if let Some((current, old, new)) = current
        .as_table_like_mut()
        .zip(
            before
                .and_then(Item::as_table_like)
                .zip(value.as_table_like()),
        )
        .map(|(current, (old, new))| (current, old, new))
    {
        patch_table(current, old, new, inline_table);
        return;
    }

    // Each profile keeps its existing table, position and comments.
    if let Some(current) = current.as_array_of_tables_mut()
        && let Some(old) = before.and_then(Item::as_array_of_tables)
    {
        let new = value.as_array_of_tables();
        let empty = toml_edit::ArrayOfTables::new();
        if new.is_some() || value.as_array().is_some_and(|array| array.is_empty()) {
            let new = new.unwrap_or(&empty);
            let id = |table: &toml_edit::Table| {
                table
                    .get("id")
                    .and_then(Item::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            for index in (0..current.len()).rev() {
                let profile_id = id(current.get(index).unwrap());
                if old.iter().any(|table| id(table) == profile_id)
                    && !new.iter().any(|table| id(table) == profile_id)
                {
                    current.remove(index);
                }
            }
            for profile in new.iter() {
                let profile_id = id(profile);
                let index = current
                    .iter()
                    .enumerate()
                    .filter(|(_, table)| id(table) == profile_id)
                    .map(|(i, _)| i)
                    .last();
                if let Some(index) = index {
                    if let Some(old) = old.iter().find(|table| id(table) == profile_id) {
                        patch_table(current.get_mut(index).unwrap(), old, profile, false);
                    }
                } else {
                    current.push(profile.clone());
                }
            }
            if !current.is_empty() {
                return;
            }
        }
    }
    let array_value = |item: &Item| {
        item.clone().into_value().ok().and_then(|value| {
            if let toml_edit::Value::Array(array) = value {
                Some(array)
            } else {
                None
            }
        })
    };
    if let Some(current) = current.as_array_mut()
        && let Some(next) = array_value(value)
    {
        if before.and_then(Item::as_array_of_tables).is_some()
            || value.as_array_of_tables().is_some()
        {
            patch_inline_profiles(current, before.and_then(Item::as_array_of_tables), &next);
        } else {
            patch_list(current, &next);
        }
        return;
    }
    let mut replacement = value.clone();
    if inlined {
        replacement.make_value();
    }
    if let (Some(old), Some(new)) = (current.as_value(), replacement.as_value_mut()) {
        *new.decor_mut() = old.decor().clone();
    }
    *current = replacement;
}

fn patch_list(current: &mut toml_edit::Array, next: &toml_edit::Array) {
    for (index, desired) in next.iter().enumerate() {
        let existing = current
            .iter()
            .skip(index)
            .position(|item| item.as_str() == desired.as_str());
        match existing {
            Some(0) => {}
            Some(offset) => {
                let value = current.remove(index + offset);
                current.insert_formatted(index, value);
            }
            None => current.insert(index, desired.clone()),
        }
    }
    while current.len() > next.len() {
        current.remove(current.len() - 1);
    }
}

fn patch_inline_profiles(
    current: &mut toml_edit::Array,
    before: Option<&toml_edit::ArrayOfTables>,
    next: &toml_edit::Array,
) {
    use toml_edit::{Item, TableLike, Value};
    fn id(value: &Value) -> Option<&str> {
        value.as_inline_table()?.get("id")?.as_str()
    }
    for index in (0..current.len()).rev() {
        let id = id(current.get(index).unwrap());
        if let Some(id) = id
            && before.is_some_and(|profiles| {
                profiles
                    .iter()
                    .any(|profile| profile.get("id").and_then(Item::as_str) == Some(id))
            })
            && !next.iter().any(|profile| {
                profile
                    .as_inline_table()
                    .and_then(|table| table.get("id"))
                    .and_then(Value::as_str)
                    == Some(id)
            })
        {
            current.remove(index);
        }
    }
    for profile in next.iter() {
        let profile_id = id(profile);
        let index = current
            .iter()
            .enumerate()
            .filter(|(_, item)| id(item) == profile_id)
            .map(|(index, _)| index)
            .last();
        if let Some(index) = index {
            let old = before.and_then(|profiles| {
                profiles
                    .iter()
                    .find(|table| table.get("id").and_then(Item::as_str) == profile_id)
            });
            let existing = current.get_mut(index).and_then(Value::as_inline_table_mut);
            if let (Some(existing), Some(old), Some(next)) =
                (existing, old, profile.as_inline_table())
            {
                patch_table(existing as &mut dyn TableLike, old, next, true);
            }
        } else {
            current.push(profile.clone());
        }
    }
}

/// Guarda la configuración conservando el documento existente.
pub fn save(dir: &Path, config: &McpConfig) -> io::Result<()> {
    ConfigSnapshot::load(dir)?.save(dir, config)?;
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
                scoped_rules: Vec::new(),
                sudo: Some(Decision::Deny),
            },
        );
        save(dir.path(), &config).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn scoped_rules_can_be_appended_and_removed_from_the_snapshot() {
        let dir = temp_dir();
        let initial = ConfigSnapshot::load(dir.path()).unwrap();
        let mut config = initial.config.clone();
        let first = ScopedRule {
            operation: "fs_write".into(),
            scope: "first".into(),
        };
        let second = ScopedRule {
            operation: "fs_patch".into(),
            scope: "second".into(),
        };
        config
            .servers
            .entry("prod".into())
            .or_default()
            .scoped_rules
            .push(first);
        let saved = initial.save(dir.path(), &config).unwrap();
        config
            .servers
            .get_mut("prod")
            .unwrap()
            .scoped_rules
            .push(second.clone());
        let saved = saved.save(dir.path(), &config).unwrap();
        config
            .servers
            .get_mut("prod")
            .unwrap()
            .scoped_rules
            .remove(0);
        saved.save(dir.path(), &config).unwrap();
        assert_eq!(
            load(dir.path()).unwrap().servers["prod"].scoped_rules,
            [second]
        );
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

    #[test]
    fn load_partial_sections_use_defaults() {
        let dir = temp_dir();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[rollback]\nenabled = false\n\n[[profiles]]\nid = \"custom\"\nname = \"Personal\"\n",
        )
        .unwrap();
        let config = load(dir.path()).unwrap();
        let defaults = RollbackConfig::default();
        assert!(!config.rollback.enabled);
        assert_eq!(config.rollback.max_file_bytes, defaults.max_file_bytes);
        assert_eq!(config.rollback.max_total_bytes, defaults.max_total_bytes);
        assert_eq!(config.rollback.retention_hours, defaults.retention_hours);
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].id, "custom");
        assert_eq!(config.profiles[0].fs_read, Decision::Ask);
        assert_eq!(
            config.profiles[0].default_denied_remote_paths,
            crate::presets::critical_denied_paths()
        );
    }

    #[test]
    fn validate_keeps_the_last_duplicate_profile() {
        let mut config = McpConfig {
            profiles: vec![
                PermissionProfile {
                    id: "custom".to_owned(),
                    name: "primero".to_owned(),
                    ..PermissionProfile::default()
                },
                PermissionProfile {
                    id: "otro".to_owned(),
                    name: "otro".to_owned(),
                    ..PermissionProfile::default()
                },
                PermissionProfile {
                    id: "custom".to_owned(),
                    name: "ultimo".to_owned(),
                    ..PermissionProfile::default()
                },
            ],
            ..McpConfig::default()
        };
        config.validate();
        let ids: Vec<&str> = config
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect();
        assert_eq!(ids, vec!["otro", "custom"]);
        assert_eq!(
            crate::presets::resolve_profile(&config, "custom")
                .unwrap()
                .name,
            "ultimo"
        );
    }
    #[test]
    fn snapshot_preserves_comments_order_and_unknown_fields() {
        let dir = temp_dir();
        let file = dir.path().join(CONFIG_FILE_NAME);
        fs::write(
            &file,
            r#"# Ajustes MCP
hide_server_identity = true # privacidad
enabled = false # interruptor

[servers.produccion] # servidor
profile_id = "personal"
notes = "original" # equipo
future = "reservation"

[[profiles]] # perfil propio
id = "personal"
name = "Personal" # etiqueta

[[profiles]] # segundo
id = "perfiles"
name = "Segundo"
"#,
        )
        .unwrap();
        let snapshot = ConfigSnapshot::load(dir.path()).unwrap();
        let mut config = snapshot.config.clone();
        config.enabled = true;
        config.servers.get_mut("produccion").unwrap().notes = Some("cambiado".into());
        config
            .servers
            .get_mut("produccion")
            .unwrap()
            .denied_remote_paths = vec!["/secrets/**".into()];
        config.profiles[0].name = "Nuevo nombre".into();
        snapshot.save(dir.path(), &config).unwrap();
        let output = fs::read_to_string(file).unwrap();
        for expected in [
            "# Ajustes MCP",
            "hide_server_identity = true # privacidad",
            "enabled = true # interruptor",
            "[servers.produccion] # servidor",
            r#"notes = "cambiado" # equipo"#,
            r#"future = "reservation""#,
            r#"name = "Nuevo nombre" # etiqueta"#,
            "[[profiles]] # segundo",
        ] {
            assert!(output.contains(expected), "{expected}: {output}");
        }
        assert!(output.find("hide_server_identity") < output.find("enabled"));
        assert!(output.find("[servers.produccion]") < output.find(r#"id = "personal""#));
        assert!(output.find(r#"id = "personal""#) < output.find(r#"id = "perfiles""#));
        assert_eq!(load(dir.path()).unwrap(), config);
    }

    #[test]
    fn stale_snapshot_refuses_to_overwrite_external_edit() {
        let dir = temp_dir();
        let snapshot = ConfigSnapshot::load(dir.path()).unwrap();
        let file = dir.path().join(CONFIG_FILE_NAME);
        let other = "# escrito fuera de Vela\nenabled = true\n";
        fs::write(&file, other).unwrap();
        let error = snapshot.save(dir.path(), &snapshot.config).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(file).unwrap(), other);
    }

    #[test]
    fn invalid_candidate_does_not_replace_original_file() {
        let dir = temp_dir();
        let file = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&file, "# original\nenabled = false\n").unwrap();
        let snapshot = ConfigSnapshot::load(dir.path()).unwrap();
        let mut config = snapshot.config.clone();
        config.rollback.max_file_bytes = u64::MAX;
        assert!(snapshot.save(dir.path(), &config).is_err());
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            "# original\nenabled = false\n"
        );
    }

    #[test]
    fn old_minimal_server_keeps_conservative_defaults() {
        let dir = temp_dir();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[servers.old]\nnotes = 'original'\n",
        )
        .unwrap();
        let snapshot = ConfigSnapshot::load(dir.path()).unwrap();
        assert!(!snapshot.config.enabled);
        assert!(!snapshot.config.share_ui_context);
        assert!(snapshot.config.hide_server_identity);
        assert_eq!(snapshot.config.headless_ask, HeadlessAskBehavior::Deny);
        assert_eq!(
            snapshot.config.servers["old"].availability,
            ServerAvailability::Disabled
        );
        assert_eq!(snapshot.config.servers["old"].exec_mode, None);
    }
    #[test]
    fn snapshot_preserves_inline_profiles_and_comments_inside_pattern_lists() {
        let dir = temp_dir();
        let file = dir.path().join(CONFIG_FILE_NAME);
        fs::write(
            &file,
            r#"profiles = [
  # personal
  { id = "personal", name = "Antes", future = "dato" },
]
[servers.demo]
allowed_remote_paths = [
  # informes
  "/srv/reportes/**",
] # ámbitos
"#,
        )
        .unwrap();
        let snapshot = ConfigSnapshot::load(dir.path()).unwrap();
        let mut config = snapshot.config.clone();
        config.profiles[0].name = "Después".into();
        config
            .servers
            .get_mut("demo")
            .unwrap()
            .allowed_remote_paths
            .push("/logs/**".into());
        snapshot.save(dir.path(), &config).unwrap();
        let text = fs::read_to_string(file).unwrap();
        for expected in [
            "# personal",
            "# informes",
            "# ámbitos",
            r#"future = "dato""#,
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert_eq!(load(dir.path()).unwrap(), config);
    }
}
