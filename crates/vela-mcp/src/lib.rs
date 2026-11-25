//! Núcleo del sistema MCP de Vela (Fase M1).
//!
//! Este crate no depende de GPUI ni de la interfaz: contiene el esquema de
//! `mcp.toml`, los perfiles de permisos, el ámbito de rutas locales y remotas,
//! el validador estructurado de comandos, las elevaciones temporales (JIT) y el
//! [`PolicyEngine`] que decide, con explicación en español, si cada acción del
//! agente se permite, se pregunta o se deniega.
//!
//! El motor es puro: todas las decisiones derivan de la configuración, el
//! perfil efectivo, las rutas canónicas informadas por SFTP y el estado del
//! entorno pasado en [`EvalContext`]. No realiza E/S de red ni toca archivos.

pub mod command;
pub mod config;
pub mod jit;
pub mod path;
pub mod policy;
pub mod presets;

pub use command::{CommandCheck, CommandError, CommandWarning, OperatorKind, Token};
pub use config::{
    Decision, ExecMode, HeadlessAskBehavior, McpConfig, RollbackConfig, ServerAvailability,
    ServerMcpPolicy, to_json,
};
pub use jit::JitGrants;
pub use path::{PathAccess, PathError};
pub use policy::{
    AgentServerInfo, EvalContext, McpAction, PolicyDecision, PolicyEngine, PolicyWarning,
    ServerIdentity,
};
pub use presets::{PermissionProfile, critical_denied_paths};
