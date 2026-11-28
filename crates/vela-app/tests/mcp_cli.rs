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
    let mut child = isolated_command(config.path())
        .args(["--headless", "--config-dir"])
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

fn isolated_command(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vela"));
    command.env_clear().env("HOME", home).arg("mcp");
    #[cfg(feature = "dev")]
    command.env("VELA_MCP_HARNESS_HOME", home);
    command
}

#[cfg(any(not(target_os = "macos"), feature = "dev"))]
#[test]
fn vela_mcp_registers_and_removes_cursor_only_in_the_selected_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let output = isolated_command(home)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["harness"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["claude-code", "codex", "cursor", "opencode", "antigravity"]
    );
    assert!(rows.iter().all(|row| row["status"] == "not_detected"));

    let output = isolated_command(home)
        .args(["install", "--harness", "cursor", "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("@@"));
    assert!(!home.join(".cursor").exists());

    let output = isolated_command(home)
        .args(["install", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!home.join(".cursor").exists());

    let output = isolated_command(home)
        .args(["install", "--harness", "cursor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = home.join(".cursor/mcp.json");
    let config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_vela")).unwrap();
    assert_eq!(
        config["mcpServers"]["vela"],
        json!({"type":"stdio", "command":exe, "args":["mcp"]})
    );
    let output = isolated_command(home)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows[2]["entry"]["state"], "installed");
    assert_eq!(rows[2]["config_path"], json!(path));

    let output = isolated_command(home)
        .args(["uninstall", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(config, json!({"mcpServers":{}}));
    assert!(home.join(".cursor/mcp.json.vela.bak").is_file());
}

#[cfg(any(not(target_os = "macos"), feature = "dev"))]
#[test]
fn vela_mcp_cli_uses_distinct_exit_codes_for_usage_conflicts_and_errors() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".cursor/mcp.json");
    std::fs::create_dir(path.parent().unwrap()).unwrap();
    let command = |args: &[&str]| isolated_command(home.path()).args(args).output().unwrap();
    assert_eq!(command(&["install"]).status.code(), Some(2));
    assert_eq!(command(&["status", "--unsupported"]).status.code(), Some(2));

    std::fs::write(
        &path,
        r#"{"mcpServers":{"vela":{"env":{"SECRET":"fake"},"command":"other"}}}"#,
    )
    .unwrap();
    let output = command(&["install", "--harness", "cursor"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake"));
    std::fs::write(&path, r#"{"SECRET":fake"#).unwrap();
    let output = command(&["uninstall", "--all"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fake"));
}

#[cfg(any(not(target_os = "macos"), feature = "dev"))]
#[test]
fn vela_mcp_status_describes_conflicts_and_outdated_paths_without_secrets() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".cursor/mcp.json");
    std::fs::create_dir(path.parent().unwrap()).unwrap();
    let run = |args: &[&str]| {
        let output = isolated_command(home.path()).args(args).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("SECRET") && !stdout.contains("fake"));
        stdout
    };
    for (entry, state, label, found) in [
        (
            json!({"command":"other","args":["SECRET"],"env":{"SECRET":"fake"}}),
            "conflict",
            "Ocupada por",
            "other",
        ),
        (
            json!({"url":"https://SECRET:fake@example.org/SECRET?token=fake",
                "headers":{"Authorization":"fake"}}),
            "conflict",
            "Ocupada por",
            "url: example.org",
        ),
        (
            json!({"command":"/old/vela","args":["mcp"],"env":{"SECRET":"fake"}}),
            "outdated",
            "Ejecutable anterior",
            "/old/vela",
        ),
    ] {
        std::fs::write(&path, json!({"mcpServers":{"vela":entry}}).to_string()).unwrap();
        let text = run(&["status"]);
        assert!(
            text.lines()
                .any(|line| line == format!("  {label}: {found}"))
        );
        let json: Vec<Value> = serde_json::from_str(&run(&["status", "--json"])).unwrap();
        assert_eq!(json[2]["entry"], json!({"state":state, "found":found}));
    }
}

#[cfg(feature = "dev")]
#[test]
fn vela_mcp_dev_home_overrides_the_inherited_home_paths_and_harness_overrides() {
    let home = tempfile::tempdir().unwrap();
    let decoy = tempfile::tempdir().unwrap();
    std::fs::create_dir(decoy.path().join(".cursor")).unwrap();
    let run = |args: &[&str]| {
        isolated_command(decoy.path())
            .env("VELA_MCP_HARNESS_HOME", home.path())
            .env("CODEX_HOME", decoy.path())
            .env("CLAUDE_CONFIG_DIR", decoy.path())
            .env("XDG_CONFIG_HOME", decoy.path())
            .env("PATH", "/usr/bin")
            .args(args)
            .output()
            .unwrap()
    };
    let output = run(&["status", "--json"]);
    assert!(output.status.success());
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.iter().all(|row| row["status"] == "not_detected"));
    assert!(run(&["install", "--harness", "codex"]).status.success());
    assert!(home.path().join(".codex/config.toml").is_file());
    assert!(!decoy.path().join("config.toml").exists());
}
