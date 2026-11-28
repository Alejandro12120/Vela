//! SSH and SFTP sessions over system OpenSSH.

pub mod agent;
pub mod appearance;
pub mod askpass;
pub mod config;
pub mod edit;
mod error;
pub mod local;
pub mod path;
mod session;
pub mod settings;
mod sftp;
mod ssh;

pub use error::Error;
pub use session::{Client, ConnectCancel, Session};
pub use sftp::{Entry, EntryKind};
pub use ssh::{
    Auth, ConnectionSpec, ExecOptions, ExecOutput, MAX_EXEC_TIMEOUT, bitwarden_agent_sockets,
    exec_remote_command, run_script_command, shell_quote, terminal_start_command,
};

pub mod transfer;
pub mod updater;
