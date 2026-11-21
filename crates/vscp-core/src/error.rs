use std::process::ExitStatus;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SSH authentication failed ({status})\n{stderr}")]
    AuthFailed { status: ExitStatus, stderr: String },
    #[error("The SSH host key was rejected ({status})\n{stderr}")]
    HostKeyRejected { status: ExitStatus, stderr: String },
    #[error("The SSH host key changed ({status})\n{stderr}")]
    HostKeyChanged { status: ExitStatus, stderr: String },
    #[error("SSH connection failed{status}\n{stderr}", status = .status.map(|s| format!(" ({s})")).unwrap_or_default())]
    ConnectFailed {
        status: Option<ExitStatus>,
        stderr: String,
    },
    #[error("SFTP failed\n{0}")]
    Sftp(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Operation cancelled")]
    Cancelled,
}

impl Error {
    pub(crate) fn ssh(status: ExitStatus, stderr: String) -> Self {
        let text = stderr.to_ascii_lowercase();
        if text.contains("remote host identification has changed")
            || text.contains("host key has changed")
        {
            Self::HostKeyChanged { status, stderr }
        } else if text.contains("host key verification failed") {
            Self::HostKeyRejected { status, stderr }
        } else if text.contains("permission denied")
            || text.contains("too many authentication failures")
            || text.contains("no supported authentication methods")
        {
            Self::AuthFailed { status, stderr }
        } else {
            Self::ConnectFailed {
                status: Some(status),
                stderr,
            }
        }
    }
}

impl From<openssh_sftp_client::Error> for Error {
    fn from(error: openssh_sftp_client::Error) -> Self {
        Self::Sftp(error.to_string())
    }
}
