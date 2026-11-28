//! Núcleo del sistema MCP de Vela (Fases M1 a M4).
//!
//! Este crate no depende de GPUI ni de la interfaz: contiene el esquema de
//! `mcp.toml`, los perfiles de permisos, el ámbito de rutas locales y remotas,
//! el validador estructurado de comandos, las elevaciones temporales (JIT), el
//! [`PolicyEngine`] que decide con explicación en español, el almacén de
//! copias de seguridad, el parcheador de texto y el ejecutor de las 15
//! herramientas v1 sobre `vela_core::Session`. El transporte stdio puede
//! usar sesiones SSH propias o reenviar las llamadas a Vela por IPC local.
//!
//! El motor de políticas es puro. El ejecutor hace E/S únicamente después de
//! que la política permita la acción o de que la persona haya aprobado una
//! llamada `Ask`, y vuelve a evaluar la política en ese segundo caso.

pub mod cli;
pub mod command;
pub mod config;
pub mod executor;
pub mod harness;
pub mod headless;
pub mod ipc;
pub mod jit;
mod ndjson;
pub mod patch;
pub mod path;
pub mod policy;
pub mod presets;
pub mod protocol;
pub mod rollback;
pub mod tools;

pub use command::{CommandCheck, CommandError, CommandWarning, OperatorKind, Token};
pub use config::{
    Decision, ExecMode, HeadlessAskBehavior, McpConfig, RollbackConfig, ServerAvailability,
    ServerMcpPolicy, to_json,
};
pub use executor::{
    ApprovalMode, ApprovalPreview, ChmodOutput, DeleteOutput, DescribePermissionsInput,
    ExecCommandInput, Executor, ExecutorError, FileWriteOutput, FsChmodInput, FsDeleteInput,
    FsEntry, FsListInput, FsListOutput, FsMkdirInput, FsMoveInput, FsPatchInput, FsReadInput,
    FsReadOutput, FsStatInput, FsStatOutput, FsWriteInput, GetUiContextInput, ListServersInput,
    McpBackend, MoveOutput, PathOutput, PermissionsInfo, ToolOutcome, TransferDownloadInput,
    TransferOutput, TransferUploadInput, UiContext,
};
pub use jit::JitGrants;
pub use patch::{PatchError, PatchResult, TextEdit, apply_edits, unified_diff};
pub use path::{PathAccess, PathError};
pub use policy::{
    AgentServerInfo, EvalContext, McpAction, PolicyDecision, PolicyEngine, PolicyWarning,
    ServerIdentity,
};
pub use presets::{PermissionProfile, critical_denied_paths};
pub use rollback::{
    BackupEntry, ROLLBACK_DIR_NAME, RollbackStore, RollbackUsage, restore_snapshot,
};
