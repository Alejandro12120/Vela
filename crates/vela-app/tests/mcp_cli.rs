use std::{
    io::Write,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

#[test]
fn vela_mcp_responds_and_exits_without_a_display() {
    let config = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vela"))
        .args(["mcp", "--headless", "--config-dir"])
        .arg(config.path())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                       "clientInfo": {"name": "smoke", "version": "1"}},
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})
    )
    .unwrap();
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("vela mcp no terminó tras EOF");
        }
        sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "vela");
    assert_eq!(lines[1], json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
}
