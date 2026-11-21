//! Askpass requests and the helper process entry point.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixStream as BlockingStream},
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    process::Command,
    sync::{oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep_until, timeout},
};

use crate::ssh::control_dir;

pub(crate) const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);

const MAX_PROMPT: usize = 64 * 1024;
const MAX_REPLY: usize = 1023; // OpenSSH's askpass response buffer, excluding its trailing NUL.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Password,
    HostKeyConfirm { fingerprint: String },
    Passphrase,
    Other,
}

pub struct Request {
    pub destination: String,
    pub prompt: String,
    pub hint: Option<String>,
    pub kind: Kind,
    reply: oneshot::Sender<Option<String>>,
}

impl Request {
    pub fn is_pending(&self) -> bool {
        !self.reply.is_closed()
    }

    /// Send the response, or refuse the prompt with None. Dropping the request also refuses it.
    pub fn respond(self, answer: Option<String>) {
        let _ = self.reply.send(answer);
    }
}

fn classify(prompt: &str) -> Kind {
    if let Some(fingerprint) = prompt
        .lines()
        .find_map(|line| line.split_once("key fingerprint is: "))
    {
        Kind::HostKeyConfirm {
            fingerprint: fingerprint.1.trim().to_owned(),
        }
    } else if prompt.starts_with("Enter passphrase for key ") {
        Kind::Passphrase
    } else if prompt
        .trim_end()
        .to_ascii_lowercase()
        .ends_with("password:")
    {
        Kind::Password
    } else {
        Kind::Other
    }
}

pub(crate) struct Server {
    directory: PathBuf,
    socket: PathBuf,
    launcher: PathBuf,
    app: PathBuf,
    token: String,
    task: Option<JoinHandle<()>>,
    activity: watch::Sender<PromptActivity>,
}

impl Server {
    pub(crate) fn start(
        app: PathBuf,
        requests: async_channel::Sender<Request>,
        destination: String,
    ) -> io::Result<Self> {
        let directory = control_dir()?;
        let id = random_hex::<8>()?;
        let server = Self {
            socket: directory.join(format!("askpass-{id}.sock")),
            launcher: directory.join(format!("askpass-{id}")),
            directory,
            app,
            token: random_hex::<32>()?,
            task: None,
            activity: watch::Sender::new(PromptActivity::default()),
        };
        server.listen(requests, destination)
    }

    fn listen(
        mut self,
        requests: async_channel::Sender<Request>,
        destination: String,
    ) -> io::Result<Self> {
        // OpenSSH passes exactly one argument (the prompt). The launcher inserts
        // the app's subcommand and works with paths containing spaces.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&self.launcher)?
            .write_all(b"#!/bin/sh\nexec \"$VSCP_ASKPASS_APP\" askpass \"$@\"\n")?;
        let listener = UnixListener::bind(&self.socket)?;
        let token = self.token.clone();
        let activity = self.activity.clone();
        self.task = Some(tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept(), if handlers.len() < 8 => {
                        let Ok((stream, _)) = result else { break };
                        let token = token.clone();
                        let requests = requests.clone();
                        let destination = destination.clone();
                        let activity = activity.clone();
                        handlers.spawn(async move {
                            let _ = answer_prompt(stream, token, requests, destination, activity).await;
                        });
                    }
                    _ = handlers.join_next(), if !handlers.is_empty() => {}
                }
            }
        }));
        Ok(self)
    }

    pub(crate) fn readiness_timer(&self, budget: Duration) -> impl Future<Output = ()> + use<> {
        readiness_timer(self.activity.subscribe(), Instant::now(), budget)
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn configure(&self, command: &mut Command) {
        command
            .env("SSH_ASKPASS", &self.launcher)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("VSCP_ASKPASS_APP", &self.app)
            .env("VSCP_ASKPASS_SOCK", &self.socket)
            .env("VSCP_ASKPASS_TOKEN", &self.token);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.launcher);
    }
}

pub(crate) fn random_hex<const N: usize>() -> io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    let mut hex = String::with_capacity(2 * N);
    for byte in bytes {
        write!(hex, "{byte:02x}").expect("writing into a String");
    }
    Ok(hex)
}

#[derive(Clone, Copy, Default)]
struct PromptActivity {
    count: usize,
    since: Option<Instant>,
    elapsed: Duration,
}

struct PendingPrompt(watch::Sender<PromptActivity>);

impl PendingPrompt {
    fn start(activity: watch::Sender<PromptActivity>) -> Self {
        activity.send_modify(|state| {
            if state.count == 0 {
                state.since = Some(Instant::now());
            }
            state.count += 1;
        });
        Self(activity)
    }
}

impl Drop for PendingPrompt {
    fn drop(&mut self) {
        self.0.send_modify(|state| {
            state.count -= 1;
            if state.count == 0
                && let Some(since) = state.since.take()
            {
                state.elapsed += since.elapsed();
            }
        });
    }
}

async fn readiness_timer(
    mut activity: watch::Receiver<PromptActivity>,
    started: Instant,
    budget: Duration,
) {
    loop {
        let state = *activity.borrow_and_update();
        if state.since.is_some() {
            if activity.changed().await.is_err() {
                return;
            }
        } else {
            tokio::select! {
                biased;
                changed = activity.changed() => if changed.is_err() { return },
                _ = sleep_until(started + budget + state.elapsed) => return,
            }
        }
    }
}

async fn answer_prompt(
    mut stream: UnixStream,
    token: String,
    requests: async_channel::Sender<Request>,
    destination: String,
    activity: watch::Sender<PromptActivity>,
) -> io::Result<()> {
    let request = timeout(IO_TIMEOUT, async {
        let mut supplied = [0; 64];
        stream.read_exact(&mut supplied).await?;
        if supplied != token.as_bytes() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Invalid askpass token",
            ));
        }
        let hint = read_field(&mut stream).await?;
        let prompt = read_field(&mut stream).await?;
        Ok((hint, prompt))
    })
    .await
    .map_err(io::Error::other)??;
    let (hint, prompt) = request;
    let (reply, answer) = oneshot::channel();
    let request = Request {
        destination,
        kind: classify(&prompt),
        hint: (!hint.is_empty()).then_some(hint),
        prompt,
        reply,
    };
    let _pending = PendingPrompt::start(activity);
    let answer = timeout(PROMPT_TIMEOUT, async {
        requests.send(request).await.ok()?;
        answer.await.ok().flatten()
    })
    .await
    .ok()
    .flatten();
    if let Some(answer) =
        answer.filter(|value| value.len() <= MAX_REPLY && !value.contains(['\r', '\n', '\0']))
    {
        timeout(IO_TIMEOUT, async {
            stream.write_u8(1).await?;
            stream.write_all(answer.as_bytes()).await
        })
        .await
        .map_err(io::Error::other)??;
    }
    Ok(())
}

async fn read_field(stream: &mut UnixStream) -> io::Result<String> {
    let length = stream.read_u32().await? as usize;
    if length > MAX_PROMPT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid askpass field length",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Run the askpass helper without starting UI or a Tokio runtime.
pub fn client_main() -> i32 {
    match request_reply() {
        Ok(Some(reply)) => i32::from(io::stdout().lock().write_all(&reply).is_err()),
        Ok(None) => 1,
        Err(error) => {
            eprintln!("vSCP askpass: {error}");
            1
        }
    }
}

fn request_reply() -> io::Result<Option<Vec<u8>>> {
    let socket = std::env::var_os("VSCP_ASKPASS_SOCK").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "VSCP_ASKPASS_SOCK is missing")
    })?;
    let token = std::env::var("VSCP_ASKPASS_TOKEN")
        .ok()
        .filter(|token| token.len() == 64)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "VSCP_ASKPASS_TOKEN is missing or invalid",
            )
        })?;
    let prompt = std::env::args_os()
        .nth(2)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "SSH prompt is missing"))?;
    let hint = std::env::var_os("SSH_ASKPASS_PROMPT").unwrap_or_default();
    let mut stream = BlockingStream::connect(socket)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.set_read_timeout(Some(PROMPT_TIMEOUT + IO_TIMEOUT * 2))?;
    stream.write_all(token.as_bytes())?;
    for field in [hint, prompt] {
        let field = field.as_encoded_bytes();
        if field.len() > MAX_PROMPT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH prompt is too long",
            ));
        }
        stream.write_all(&(field.len() as u32).to_be_bytes())?;
        stream.write_all(field)?;
    }
    let mut answer = Vec::new();
    stream
        .take((MAX_REPLY + 2) as u64)
        .read_to_end(&mut answer)?;
    Ok((answer.first() == Some(&1) && answer.len() <= MAX_REPLY + 1).then(|| answer.split_off(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_classification() {
        assert_eq!(classify("user@host's password: "), Kind::Password);
        assert_eq!(
            classify("Enter passphrase for key '/tmp/c': "),
            Kind::Passphrase
        );
        assert_eq!(
            classify("ED25519 key fingerprint is: SHA256:abcdefgh\nContinue?"),
            Kind::HostKeyConfirm {
                fingerprint: "SHA256:abcdefgh".into(),
            }
        );
        assert_eq!(classify("Verification code: "), Kind::Other);
    }

    #[tokio::test]
    async fn rejects_incorrect_token_without_forwarding_a_prompt() {
        let (send, requests) = async_channel::bounded(1);
        let server = Server::start(PathBuf::from("/unused"), send, "test".into()).unwrap();
        timeout(IO_TIMEOUT, async {
            let mut stream = UnixStream::connect(&server.socket).await.unwrap();
            let mut wrong_token = server.token.clone().into_bytes();
            wrong_token[0] ^= 1;
            stream.write_all(&wrong_token).await.unwrap();
            assert_eq!(stream.read(&mut [0]).await.unwrap(), 0);
            assert!(requests.is_empty());
        })
        .await
        .unwrap();
    }
}
