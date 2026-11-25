//! Public keys held by an SSH agent, read with the agent protocol.
//!
//! Only `SSH_AGENTC_REQUEST_IDENTITIES` is ever sent. Nothing is signed or added.

use std::{
    fs,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::timeout,
};

use crate::config::save_private;

const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const FAILURE: u8 = 5;
const MAX_ANSWER: u32 = 256 * 1024;
/// Bitwarden may show its own unlock or approval prompt before it answers.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PUBLIC_KEY_FILE: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error("The SSH agent socket is missing or refused the connection")]
    Unavailable,
    #[error("The SSH agent refused to list its identities")]
    Refused,
    #[error("The SSH agent did not answer in time")]
    Timeout,
    #[error("The SSH agent sent an invalid answer: {0}")]
    Protocol(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentKey {
    pub key_type: String,
    pub blob: Vec<u8>,
    pub comment: String,
}

impl AgentKey {
    /// What `ssh-keygen -l -E sha256` prints: `SHA256:` and unpadded base64.
    pub fn fingerprint(&self) -> String {
        format!(
            "SHA256:{}",
            STANDARD_NO_PAD.encode(Sha256::digest(&self.blob))
        )
    }

    /// One line of an OpenSSH `.pub` file, without the newline.
    pub fn public_line(&self) -> String {
        format!(
            "{} {} {}",
            self.key_type,
            STANDARD.encode(&self.blob),
            self.comment
        )
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(len)?;
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }
}

fn key_type(blob: &[u8]) -> Option<String> {
    String::from_utf8(Reader(blob).string()?.to_vec()).ok()
}

/// Parses the payload of an agent answer, without its length prefix.
fn parse_answer(payload: &[u8]) -> Result<Vec<AgentKey>, AgentError> {
    const TRUNCATED: AgentError = AgentError::Protocol("truncated answer");
    match payload.first() {
        Some(&FAILURE) => return Err(AgentError::Refused),
        Some(&IDENTITIES_ANSWER) => {}
        _ => return Err(AgentError::Protocol("unexpected answer type")),
    }
    let mut reader = Reader(&payload[1..]);
    let count = reader.u32().ok_or(TRUNCATED)?;
    let mut keys = Vec::new();
    for _ in 0..count {
        let blob = reader.string().ok_or(TRUNCATED)?;
        let comment = reader.string().ok_or(TRUNCATED)?;
        keys.push(AgentKey {
            key_type: key_type(blob).ok_or(AgentError::Protocol("invalid key blob"))?,
            blob: blob.to_vec(),
            comment: String::from_utf8_lossy(comment).into_owned(),
        });
    }
    Ok(keys)
}

/// Lists the public keys the agent at `socket` offers, in the agent's order.
pub async fn list_identities(socket: &Path) -> Result<Vec<AgentKey>, AgentError> {
    list_identities_within(socket, ANSWER_TIMEOUT).await
}

async fn list_identities_within(
    socket: &Path,
    limit: Duration,
) -> Result<Vec<AgentKey>, AgentError> {
    timeout(limit, async {
        let mut stream = UnixStream::connect(socket)
            .await
            .map_err(|_| AgentError::Unavailable)?;
        let io_error = |_| AgentError::Protocol("connection closed");
        stream
            .write_all(&[0, 0, 0, 1, REQUEST_IDENTITIES])
            .await
            .map_err(io_error)?;
        let len = stream.read_u32().await.map_err(io_error)?;
        if len == 0 || len > MAX_ANSWER {
            return Err(AgentError::Protocol("invalid answer length"));
        }
        let mut payload = vec![0; len as usize];
        stream.read_exact(&mut payload).await.map_err(io_error)?;
        parse_answer(&payload)
    })
    .await
    .map_err(|_| AgentError::Timeout)?
}

fn key_file_name(key: &AgentKey) -> String {
    use std::fmt::Write;
    Sha256::digest(&key.blob)[..16]
        .iter()
        .fold(String::with_capacity(36), |mut name, byte| {
            let _ = write!(name, "{byte:02x}");
            name
        })
        + ".pub"
}

/// Writes `key` to `<config_dir>/keys/<hash>.pub` and returns the path. The
/// directory is mode 0700 and the file mode 0600. Writing the same key twice
/// replaces the file atomically.
pub fn store_public_key(config_dir: &Path, key: &AgentKey) -> io::Result<PathBuf> {
    let keys = config_dir.join("keys");
    fs::create_dir_all(&keys)?;
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700))?;
    let name = key_file_name(key);
    save_private(&keys, &name, &format!("{}\n", key.public_line()))?;
    Ok(keys.join(name))
}

/// Parses the first line of an OpenSSH public key file.
pub fn read_public_key(path: &Path) -> io::Result<AgentKey> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_PUBLIC_KEY_FILE)
        .read_to_string(&mut text)?;
    let line = text.lines().next().unwrap_or_default();
    let mut parts = line.trim().splitn(3, char::is_whitespace);
    let (Some(declared), Some(encoded)) = (parts.next(), parts.next()) else {
        return Err(invalid("not a public key"));
    };
    let blob = STANDARD
        .decode(encoded)
        .map_err(|_| invalid("invalid base64"))?;
    if key_type(&blob).as_deref() != Some(declared) {
        return Err(invalid("key type does not match"));
    }
    Ok(AgentKey {
        key_type: declared.to_owned(),
        blob,
        comment: parts.next().unwrap_or_default().trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    const VECTOR_LINE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILrfxV3tw+cElsy5Gvmyzn8qm0XhpBm/KNBSk5lgSvcj vector ñ";
    const VECTOR_FINGERPRINT: &str = "SHA256:Nv9k5CyzST5gm9sN9kt+A2d/j0Nw8kvUzIVrJ8cyTC0";

    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend((bytes.len() as u32).to_be_bytes());
        out.extend(bytes);
    }

    fn blob(kind: &str, body: &[u8]) -> Vec<u8> {
        let mut blob = Vec::new();
        string(&mut blob, kind.as_bytes());
        string(&mut blob, body);
        blob
    }

    fn answer(keys: &[(Vec<u8>, &str)]) -> Vec<u8> {
        let mut payload = vec![IDENTITIES_ANSWER];
        payload.extend((keys.len() as u32).to_be_bytes());
        for (blob, comment) in keys {
            string(&mut payload, blob);
            string(&mut payload, comment.as_bytes());
        }
        payload
    }

    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut message = (payload.len() as u32).to_be_bytes().to_vec();
        message.extend(payload);
        message
    }

    fn vector() -> AgentKey {
        let mut parts = VECTOR_LINE.splitn(3, ' ');
        AgentKey {
            key_type: parts.next().unwrap().into(),
            blob: STANDARD.decode(parts.next().unwrap()).unwrap(),
            comment: parts.next().unwrap().into(),
        }
    }

    #[test]
    fn fingerprint_and_public_line_match_ssh_keygen() {
        let key = vector();
        assert_eq!(key.fingerprint(), VECTOR_FINGERPRINT);
        assert_eq!(key.public_line(), VECTOR_LINE);
        assert_eq!(key.key_type, "ssh-ed25519");
        // Unpadded for the fingerprint, padded in the public line.
        assert!(!key.fingerprint().ends_with('='));
        let padded = AgentKey {
            blob: vec![1, 2],
            ..key
        };
        assert!(padded.public_line().contains("AQI="));
    }

    #[test]
    fn parses_answers_with_zero_one_and_two_keys() {
        assert_eq!(parse_answer(&answer(&[])).unwrap(), []);
        let one = parse_answer(&answer(&[(blob("ssh-ed25519", b"k1"), "uno")])).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].key_type, "ssh-ed25519");
        assert_eq!(one[0].comment, "uno");
        let two = parse_answer(&answer(&[
            (blob("ssh-rsa", b"k1"), "primera"),
            (blob("ecdsa-sha2-nistp256", b"k2"), "segunda ñ"),
        ]))
        .unwrap();
        assert_eq!(
            two.iter()
                .map(|key| key.key_type.as_str())
                .collect::<Vec<_>>(),
            ["ssh-rsa", "ecdsa-sha2-nistp256"]
        );
        assert_eq!(two[1].comment, "segunda ñ");
    }

    #[test]
    fn invalid_utf8_comments_are_lossy() {
        let mut payload = vec![IDENTITIES_ANSWER, 0, 0, 0, 1];
        string(&mut payload, &blob("ssh-ed25519", b"k"));
        string(&mut payload, &[b'a', 0xff]);
        assert_eq!(parse_answer(&payload).unwrap()[0].comment, "a\u{fffd}");
    }

    #[test]
    fn failure_and_malformed_answers_are_errors() {
        assert_eq!(parse_answer(&[FAILURE]), Err(AgentError::Refused));
        assert!(matches!(parse_answer(&[]), Err(AgentError::Protocol(_))));
        assert!(matches!(parse_answer(&[6]), Err(AgentError::Protocol(_))));
        let full = answer(&[(blob("ssh-ed25519", b"k1"), "uno")]);
        for cut in 1..full.len() {
            assert!(
                matches!(parse_answer(&full[..cut]), Err(AgentError::Protocol(_))),
                "cut at {cut}"
            );
        }
        // More keys announced than present.
        let mut announced = answer(&[]);
        announced[4] = 3;
        assert!(matches!(
            parse_answer(&announced),
            Err(AgentError::Protocol(_))
        ));
        // A blob without a key type string.
        assert!(matches!(
            parse_answer(&answer(&[(vec![0, 0], "x")])),
            Err(AgentError::Protocol(_))
        ));
    }

    /// Serves one connection, reads the request and replies with `reply`.
    async fn serve(reply: Vec<u8>, keep_open: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("vela-agent-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 5];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [0, 0, 0, 1, REQUEST_IDENTITIES]);
            stream.write_all(&reply).await.unwrap();
            if keep_open {
                std::future::pending::<()>().await;
            }
        });
        (dir, socket)
    }

    #[tokio::test]
    async fn lists_identities_over_a_socket() {
        let keys = [
            (blob("ssh-ed25519", b"k1"), "uno"),
            (blob("ssh-rsa", b"k2"), "dos"),
        ];
        let (_dir, socket) = serve(framed(&answer(&keys)), false).await;
        let listed = list_identities(&socket).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].comment, "uno");
        assert_eq!(listed[1].key_type, "ssh-rsa");
    }

    #[tokio::test]
    async fn socket_errors_map_to_agent_errors() {
        let (_dir, socket) = serve(framed(&[FAILURE]), false).await;
        assert_eq!(list_identities(&socket).await, Err(AgentError::Refused));

        let (_dir, socket) = serve((MAX_ANSWER + 1).to_be_bytes().to_vec(), true).await;
        assert!(matches!(
            list_identities(&socket).await,
            Err(AgentError::Protocol(_))
        ));

        let mut truncated = 100u32.to_be_bytes().to_vec();
        truncated.extend([IDENTITIES_ANSWER, 0, 0]);
        let (_dir, socket) = serve(truncated, false).await;
        assert!(matches!(
            list_identities(&socket).await,
            Err(AgentError::Protocol(_))
        ));

        let (_dir, socket) = serve(Vec::new(), true).await;
        assert_eq!(
            list_identities_within(&socket, Duration::from_millis(100)).await,
            Err(AgentError::Timeout)
        );

        let dir = tempfile::Builder::new()
            .prefix("vela-agent-")
            .tempdir_in("/tmp")
            .unwrap();
        assert_eq!(
            list_identities(&dir.path().join("missing")).await,
            Err(AgentError::Unavailable)
        );
    }

    #[test]
    fn stores_and_reads_public_keys_privately() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("configuración");
        let key = vector();
        let path = store_public_key(&config, &key).unwrap();
        assert_eq!(path.parent().unwrap(), config.join("keys"));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 32 + ".pub".len());
        assert!(name.ends_with(".pub") && name[..32].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{VECTOR_LINE}\n")
        );
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&config.join("keys")), 0o700);
        assert_eq!(read_public_key(&path).unwrap(), key);

        // An existing directory with loose permissions is fixed, and storing
        // the same key again is fine.
        fs::set_permissions(config.join("keys"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store_public_key(&config, &key).unwrap(), path);
        assert_eq!(mode(&config.join("keys")), 0o700);
    }

    #[test]
    fn rejects_files_that_are_not_public_keys() {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("empty", ""),
            ("text", "hello world\n"),
            ("base64", "ssh-ed25519 !!!! c\n"),
            (
                "mismatch",
                "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAILrfxV3tw+cElsy5Gvmyzn8qm0XhpBm/KNBSk5lgSvcj c\n",
            ),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, text).unwrap();
            assert_eq!(
                read_public_key(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{name}"
            );
        }
        let path = dir.path().join("nocomment");
        fs::write(
            &path,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILrfxV3tw+cElsy5Gvmyzn8qm0XhpBm/KNBSk5lgSvcj\n",
        )
        .unwrap();
        assert_eq!(read_public_key(&path).unwrap().comment, "");
    }
}
