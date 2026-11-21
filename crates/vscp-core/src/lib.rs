//! SSH and SFTP sessions over system OpenSSH.

pub mod askpass;
pub mod config;
mod error;
pub mod local;
mod session;
mod sftp;
mod ssh;

pub use error::Error;
pub use session::{Client, Session};
pub use sftp::{Entry, EntryKind};
pub use ssh::{Auth, ConnectionSpec, bitwarden_agent_sockets};

pub mod transfer;
