//! SSH and SFTP sessions over system OpenSSH.

pub mod agent;
pub mod askpass;
pub mod config;
pub mod edit;
mod error;
pub mod local;
mod session;
pub mod settings;
mod sftp;
mod ssh;

pub use error::Error;
pub use session::{Client, Session};
pub use sftp::{Entry, EntryKind};
pub use ssh::{
    Auth, ConnectionSpec, bitwarden_agent_sockets, run_script_command, shell_quote,
    terminal_start_command,
};

pub mod transfer;
