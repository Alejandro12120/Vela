use std::fs;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf},
    time::{sleep, timeout},
};
use vela_mcp::{
    ServerMcpPolicy,
    config::{ConfigSnapshot, save},
    ipc::IpcClient,
    protocol::{ProgressReporter, serve_stdio},
};

use super::*;
use vela_core::{
    config::Server,
    edit::{OpenRemote, Target},
};

#[path = "../../../vela-mcp/tests/common/mod.rs"]
mod sshd;

const WAIT: Duration = Duration::from_secs(10);

async fn until<T>(mut get: impl FnMut() -> Option<T>) -> T {
    timeout(WAIT, async {
        loop {
            if let Some(value) = get() {
                return value;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("broker state did not change")
}

struct Stdio {
    from: BufReader<ReadHalf<DuplexStream>>,
    to: WriteHalf<DuplexStream>,
}

impl Stdio {
    async fn connect(client: IpcClient) -> Self {
        let (bridge, agent) = tokio::io::duplex(16 * 1024);
        let (input, output) = tokio::io::split(bridge);
        tokio::spawn(serve_stdio(
            BufReader::new(input),
            output,
            move |ctx: CallContext, tool| {
                let client = client.clone();
                async move { client.call(tool, ctx.progress).await }
            },
        ));
        let (from, to) = tokio::io::split(agent);
        let mut stdio = Self {
            from: BufReader::new(from),
            to,
        };
        stdio
            .send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                    "protocolVersion": "2025-03-26", "capabilities": {},
                    "clientInfo": {"name": "loopback", "version": "1"}
                }
            }))
            .await;
        assert_eq!(stdio.receive().await["id"], 1);
        stdio
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        stdio
    }

    async fn send(&mut self, message: Value) {
        self.to
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        timeout(WAIT, self.from.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn write(&mut self, id: u64, path: &Path, progress: bool) {
        let mut params = json!({"name": "fs_write", "arguments": {
            "server_id": "srv", "path": path, "content": format!("change {id}\n"),
        }});
        if progress {
            params["_meta"] = json!({"progressToken": id});
        }
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}))
            .await;
    }
}

async fn queued(broker: &Broker) -> Arc<Approval> {
    until(|| broker.state().pending().first().cloned()).await
}

fn audit(broker: &Broker, status: Status) -> Option<Activity> {
    broker
        .state()
        .history
        .iter()
        .find(|row| row.status == status && row.operation == "fs_write")
        .cloned()
}

#[tokio::test]
async fn stdio_ipc_approval_progress_rollback_rules_and_kill_switch() {
    let fixture = sshd::Fixture::start();
    let session = sshd::connect(&fixture).await;
    let path = fixture.path("mcp.sock");
    let config_dir = fixture.root().join("vela");
    fs::create_dir(&config_dir).unwrap();
    let config = McpConfig {
        enabled: true,
        servers: [(
            "srv".into(),
            ServerMcpPolicy {
                availability: ServerAvailability::ConnectedOnly,
                profile_id: "production_protected".into(),
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    save(&config_dir, &config).unwrap();
    let edited_file = fixture.path("edited.conf");
    fs::write(&edited_file, b"remote original\n").unwrap();
    let spec = fixture.spec();
    let server = Server {
        id: "srv".into(),
        name: "Loopback test".into(),
        host: spec.host,
        port: spec.port,
        user: spec.user,
        auth: spec.auth,
        initial_remote_path: None,
        pinned: false,
    };
    let edit_target = Target::new(&server, edited_file.clone());
    let copies = fixture.path("edit-copies");
    fs::create_dir(&copies).unwrap();
    let edit = OpenRemote::inspect(&session, &edit_target)
        .await
        .unwrap()
        .open(&session, edit_target, &copies)
        .await
        .unwrap();
    let backend = Backend {
        dir: config_dir.clone(),
        labels: [("srv".into(), "Loopback test".into())].into(),
        servers: vec![(
            "srv".into(),
            ServerIdentity {
                host: "127.0.0.1".into(),
                port: fixture.port,
                user: fixture.user.clone(),
            },
        )],
        edits: vec![edit.clone()],
        sessions: [("srv".into(), session.clone())].into(),
        context: Some(("srv".into(), fixture.root().to_string_lossy().into_owned())),
        ..Backend::default()
    };
    let broker = Broker::default();
    broker.configure(config.clone());
    let (ui, mut requests) = mpsc::unbounded_channel();
    let worker = Worker::start(broker.clone(), path.clone(), ui).unwrap();
    let live = Arc::new(Mutex::new(backend.clone()));
    let ui = tokio::spawn({
        let (broker, live, session) = (broker.clone(), live.clone(), session.clone());
        async move {
            while let Some(request) = requests.recv().await {
                match request {
                    UiRequest::Backend(reply) => {
                        let _ = reply.send(live.lock().unwrap().clone());
                    }
                    UiRequest::Approval(_) | UiRequest::Refresh(_) => {}
                    UiRequest::SaveRule(id, rule, reply) => {
                        let dir = live.lock().unwrap().dir.clone();
                        let initial = ConfigSnapshot::load(&dir).unwrap();
                        let mut config = initial.config.clone();
                        config.servers.get_mut(&id).unwrap().scoped_rules.push(rule);
                        let saved = initial.save(&dir, &config).unwrap();
                        broker.configure(saved.config);
                        let _ = reply.send(Ok(()));
                    }
                    UiRequest::Connect(id, reply) => {
                        live.lock().unwrap().sessions.insert(id, session.clone());
                        let _ = reply.send(Some(session.clone()));
                    }
                }
            }
        }
    });
    until(|| path.exists().then_some(())).await;
    let peer = ClientInfo {
        name: "Loopback".into(),
        version: "1".into(),
    };
    let client = IpcClient::connect(&path, &peer).await.unwrap();
    let mut stdio = Stdio::connect(client.clone()).await;
    let target = fixture.path("api.conf");
    fs::write(&target, b"original\n").unwrap();
    stdio.write(2, &target, true).await;
    let approval = queued(&broker).await;
    assert!(
        approval
            .preview
            .unified_diff
            .as_ref()
            .unwrap()
            .contains("-original")
    );
    for progress in [1, 2] {
        let event = stdio.receive().await;
        assert_eq!(event["method"], "notifications/progress");
        assert_eq!(event["params"]["progressToken"], 2);
        assert_eq!(event["params"]["progress"], progress);
    }
    assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
    broker.answer(approval.id, Answer::Once);
    assert_eq!(stdio.receive().await["result"]["isError"], false);
    assert_eq!(fs::read_to_string(&target).unwrap(), "change 2\n");
    let activity = audit(&broker, Status::Approved).unwrap();
    let result = broker.restore(&activity, backend.clone()).await;
    assert!(result.is_ok(), "{result:?}");
    broker.restored(activity.clone(), result);
    assert_eq!(fs::read_to_string(&target).unwrap(), "original\n");
    assert!(
        broker
            .state()
            .history
            .iter()
            .any(|row| row.id == activity.id && row.status == Status::Reverted)
    );

    stdio.write(3, &target, false).await;
    broker.answer(
        queued(&broker).await.id,
        Answer::Deny("Validate first".into()),
    );
    let denied = stdio.receive().await;
    assert_eq!(denied["result"]["isError"], true);
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Validate first")
    );
    assert!(audit(&broker, Status::Denied).is_some());

    stdio.write(4, &target, false).await;
    broker.answer(queued(&broker).await.id, Answer::Session);
    assert_eq!(stdio.receive().await["result"]["isError"], false);
    stdio.write(5, &target, false).await;
    assert_eq!(stdio.receive().await["result"]["isError"], false);
    assert!(broker.state().pending().is_empty());
    broker.revoke("srv");

    stdio.write(6, &target, false).await;
    broker.answer(queued(&broker).await.id, Answer::Save);
    assert_eq!(stdio.receive().await["result"]["isError"], false);
    assert_eq!(
        ConfigSnapshot::load(&config_dir).unwrap().config.servers["srv"]
            .scoped_rules
            .len(),
        1
    );
    stdio.write(7, &target, false).await;
    assert_eq!(stdio.receive().await["result"]["isError"], false);

    // Change policy while a pending request is visible.
    let other = fixture.path("other.conf");
    stdio.write(8, &other, false).await;
    let approval = queued(&broker).await;
    let mut denied_config = config.clone();
    denied_config
        .servers
        .get_mut("srv")
        .unwrap()
        .denied_remote_paths
        .push(other.to_string_lossy().into());
    broker.configure(denied_config);
    broker.answer(approval.id, Answer::Once);
    assert_eq!(stdio.receive().await["result"]["isError"], true);
    assert!(!other.exists());

    stdio.write(9, &target, false).await;
    queued(&broker).await;
    stdio
        .send(json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 9}
        }))
        .await;
    until(|| audit(&broker, Status::Cancelled)).await;
    assert!(broker.state().pending().is_empty());

    let mut short_config = config.clone();
    short_config.approval_timeout_secs = 1;
    broker.configure(short_config);
    stdio.write(10, &target, false).await;
    assert_eq!(stdio.receive().await["result"]["isError"], true);
    assert!(audit(&broker, Status::Expired).is_some());
    assert!(broker.state().pending().is_empty());

    // A local save can arrive before the watcher publishes Pending.
    broker.configure(config.clone());
    stdio.write(12, &edited_file, false).await;
    let pending_edit = queued(&broker).await;
    fs::write(&edit.local_path, b"unuploaded local changes\n").unwrap();
    broker.answer(pending_edit.id, Answer::Once);
    assert_eq!(stdio.receive().await["result"]["isError"], true);
    assert_eq!(
        fs::read_to_string(&edited_file).unwrap(),
        "remote original\n"
    );
    assert!(broker.state().conflict.is_some());

    // The broker requests connection approval before it can construct the diff.
    live.lock().unwrap().sessions.clear();
    let mut on_demand = config.clone();
    on_demand.servers.get_mut("srv").unwrap().availability = ServerAvailability::OnDemandAsk;
    broker.configure(on_demand);
    stdio.write(13, &target, false).await;
    let connect = queued(&broker).await;
    assert!(connect.preview.operation.starts_with("Connect via SSH"));
    assert!(live.lock().unwrap().sessions.is_empty());
    broker.answer(connect.id, Answer::Once);
    let write = queued(&broker).await;
    assert_eq!(write.preview.operation, "fs_write");
    assert!(write.preview.unified_diff.is_some());
    broker.answer(write.id, Answer::Once);
    assert_eq!(stdio.receive().await["result"]["isError"], false);

    broker.configure(config);
    stdio.write(11, &target, false).await;
    queued(&broker).await;
    broker.grant_profile("srv", SESSION_DURATION);
    broker.kill();
    until(|| broker.state().agents.is_empty().then_some(())).await;
    let response = client
        .call(
            ToolCall::parse("list_servers", json!({})).unwrap(),
            ProgressReporter::default(),
        )
        .await
        .unwrap();
    assert!(response.is_error);
    assert_eq!(broker.state().jit.active_count(SystemTime::now()), 0);
    assert!(broker.state().pending().is_empty());
    assert!(
        broker
            .state()
            .history
            .iter()
            .any(|row| row.operation == "Disconnect Agents")
    );
    assert!(
        path.exists(),
        "Kill switch must keep the bridge available for a new client"
    );
    let new = IpcClient::connect(&path, &peer).await.unwrap();
    let response = new
        .call(
            ToolCall::parse("list_servers", json!({})).unwrap(),
            ProgressReporter::default(),
        )
        .await
        .unwrap();
    assert!(!response.is_error);

    worker.set_enabled(false);
    until(|| (!path.exists()).then_some(())).await;
    worker.set_enabled(true);
    until(|| path.exists().then_some(())).await;
    let closing_client = IpcClient::connect(&path, &peer).await.unwrap();
    timeout(WAIT, worker.shutdown()).await.unwrap();
    assert!(!path.exists());
    assert!(
        closing_client
            .call(
                ToolCall::parse("list_servers", json!({})).unwrap(),
                ProgressReporter::default()
            )
            .await
            .unwrap()
            .is_error
    );
    ui.await.unwrap();
    session.disconnect().await.unwrap();
}
