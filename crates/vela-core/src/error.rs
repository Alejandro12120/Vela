use std::{io, process::ExitStatus};

use openssh_sftp_client::error::SftpErrorKind;

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
    #[error("Transfer byte limit exceeded ({0} bytes)")]
    SizeLimit(u64),
}

impl Error {
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Self::Io(error) => Some(error.kind()),
            _ => None,
        }
    }

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
        use openssh_sftp_client::Error as SftpError;
        match error {
            SftpError::IOError(error) => Self::Io(error),
            SftpError::SftpError(status, _) => match status_kind(status) {
                Some(kind) => Self::Io(kind.into()),
                None => Self::Sftp(error.to_string()),
            },
            SftpError::UnsupportedExtension(_) | SftpError::UnsupportedSftpProtocol { .. } => {
                Self::Io(io::ErrorKind::Unsupported.into())
            }
            _ => Self::Sftp(error.to_string()),
        }
    }
}

fn status_kind(status: SftpErrorKind) -> Option<io::ErrorKind> {
    match status {
        SftpErrorKind::NoSuchFile => Some(io::ErrorKind::NotFound),
        SftpErrorKind::PermDenied => Some(io::ErrorKind::PermissionDenied),
        SftpErrorKind::OpUnsupported => Some(io::ErrorKind::Unsupported),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sftp_v3_statuses_preserve_specific_io_kinds() {
        for (status, kind) in [
            (SftpErrorKind::NoSuchFile, Some(io::ErrorKind::NotFound)),
            (
                SftpErrorKind::PermDenied,
                Some(io::ErrorKind::PermissionDenied),
            ),
            (
                SftpErrorKind::OpUnsupported,
                Some(io::ErrorKind::Unsupported),
            ),
            (SftpErrorKind::Failure, None),
            (SftpErrorKind::BadMessage, None),
            (SftpErrorKind::Unknown, None),
        ] {
            assert_eq!(status_kind(status), kind);
        }
    }
}
