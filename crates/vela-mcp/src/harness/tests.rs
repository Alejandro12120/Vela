use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use super::*;

const DEFAULT: InstallOptions = InstallOptions {
    dry_run: false,
    force: false,
};
const FORCE: InstallOptions = InstallOptions {
    force: true,
    ..DEFAULT
};

struct Fixture {
    dir: tempfile::TempDir,
    env: HarnessEnv,
    cmd: VelaCommand,
    path: PathBuf,
}

impl Fixture {
    fn new(id: HarnessId) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let env = HarnessEnv::isolated(dir.path().join("home"), TargetOs::Linux);
        let executable = dir.path().join("Vela test \"build\"").join("vela");
        fs::create_dir(executable.parent().unwrap()).unwrap();
        fs::write(&executable, "").unwrap();
        let cmd = VelaCommand::new(&executable).unwrap();
        let path = env.config(id).path;
        Self {
            dir,
            env,
            cmd,
            path,
        }
    }

    fn put(&self, content: &str) {
        fs::create_dir_all(self.path.parent().unwrap()).unwrap();
        fs::write(&self.path, content).unwrap();
    }

    fn state(&self, id: HarnessId) -> EntryState {
        let HarnessStatus::Detected { entry, .. } = detect(id, &self.env, &self.cmd) else {
            panic!("{id:?} debería estar detectado");
        };
        entry
    }

    fn install(&self, id: HarnessId, opts: &InstallOptions) -> Result<ChangeReport, HarnessError> {
        install(id, &self.env, &self.cmd, opts)
    }

    fn uninstall(
        &self,
        id: HarnessId,
        opts: &InstallOptions,
    ) -> Result<ChangeReport, HarnessError> {
        uninstall(id, &self.env, &self.cmd, opts)
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn backup(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.vela.bak",
        path.file_name().unwrap().to_str().unwrap()
    ))
}

fn original(id: HarnessId) -> &'static str {
    match id {
        HarnessId::Codex => {
            "# General\nmodel = 'm'  # first\n\n# Keep\n[mcp_servers.otro] # server\ncommand   = 'other'\n\n[profiles.x]\nmodel = \"y\"\n# EOF\n"
        }
        HarnessId::OpenCode => {
            "{\r\n\t\"before\":7,\r\n\t\"mcp\": {\r\n\t\t\"servers\": {\r\n\t\t\t\"otro\": {\"type\":\"remote\", \"url\":\"https://example.com//server\"}\r\n\t\t},\r\n\t\t\"timeout\": 8\r\n\t},\r\n\t\"after\":true\r\n}"
        }
        _ => {
            "{\n    \"before\":8.10e-5,\n    \"mcpServers\" : {\n        \"otro\": {\"command\":\"other\"}\n    },\n    \"after\":true\n}\n"
        }
    }
}

fn expected(id: HarnessId, cmd: &VelaCommand) -> String {
    let program = serde_json::to_string(cmd.program_str()).unwrap();
    match id {
        HarnessId::Codex => format!(
            "[mcp_servers.vela]\ncommand = '{}'\nargs = [\"mcp\"]\n",
            cmd.program_str()
        ),
        HarnessId::ClaudeCode => format!(
            "{{\n  \"mcpServers\": {{\n    \"vela\": {{\n      \"type\": \"stdio\",\n      \"command\": {program},\n      \"args\": [\n        \"mcp\"\n      ],\n      \"env\": {{}}\n    }}\n  }}\n}}\n"
        ),
        HarnessId::Cursor => format!(
            "{{\n  \"mcpServers\": {{\n    \"vela\": {{\n      \"type\": \"stdio\",\n      \"command\": {program},\n      \"args\": [\n        \"mcp\"\n      ]\n    }}\n  }}\n}}\n"
        ),
        HarnessId::Antigravity => format!(
            "{{\n  \"mcpServers\": {{\n    \"vela\": {{\n      \"command\": {program},\n      \"args\": [\n        \"mcp\"\n      ]\n    }}\n  }}\n}}\n"
        ),
        HarnessId::OpenCode => format!(
            "{{\n  \"mcp\": {{\n    \"servers\": {{\n      \"vela\": {{\n        \"type\": \"local\",\n        \"command\": [\n          {program},\n          \"mcp\"\n        ]\n      }}\n    }}\n  }}\n}}\n"
        ),
    }
}

#[test]
fn creates_missing_config_and_private_directories() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        assert_eq!(detect(id, &f.env, &f.cmd), HarnessStatus::NotDetected);
        assert_eq!(
            f.uninstall(id, &DEFAULT).unwrap().action,
            Action::NothingToRemove
        );
        assert!(!f.env.home.exists());
        let first = f.install(id, &DEFAULT).unwrap();
        assert_eq!(first.action, Action::Created, "{id:?}");
        assert_eq!(first.backup, None);
        assert_eq!(
            fs::read_to_string(&f.path).unwrap(),
            expected(id, &f.cmd),
            "{id:?}"
        );
        assert_eq!(mode(&f.path), 0o600);
        for dir in f
            .path
            .ancestors()
            .skip(1)
            .take_while(|dir| *dir != f.dir.path())
        {
            assert_eq!(mode(dir), 0o700, "{dir:?}");
        }
        assert_eq!(f.state(id), EntryState::Installed);
        assert_eq!(f.uninstall(id, &DEFAULT).unwrap().action, Action::Removed);
        assert_eq!(f.state(id), EntryState::NotInstalled);
        let result = fs::read_to_string(&f.path).unwrap();
        match id {
            HarnessId::Codex => assert!(
                ::toml::from_str::<::toml::Table>(&result).unwrap()["mcp_servers"]
                    .as_table()
                    .unwrap()
                    .is_empty()
            ),
            HarnessId::OpenCode => assert!(
                serde_json::from_str::<serde_json::Value>(&result).unwrap()["mcp"]["servers"]
                    .as_object()
                    .unwrap()
                    .is_empty()
            ),
            _ => assert!(
                serde_json::from_str::<serde_json::Value>(&result).unwrap()["mcpServers"]
                    .as_object()
                    .unwrap()
                    .is_empty()
            ),
        }
    }
}

#[test]
fn empty_file_is_supported_and_backed_up() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        f.put("");
        assert_eq!(f.state(id), EntryState::NotInstalled);
        let report = f.install(id, &DEFAULT).unwrap();
        assert_eq!(fs::read_to_string(&f.path).unwrap(), expected(id, &f.cmd));
        assert_eq!(fs::read(report.backup.unwrap()).unwrap(), b"");
    }
}

#[test]
fn preserves_other_servers_order_bytes_permissions_and_mtime() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        let initial = original(id);
        f.put(initial);
        fs::set_permissions(&f.path, fs::Permissions::from_mode(0o644)).unwrap();
        let first = f.install(id, &DEFAULT).unwrap();
        assert_eq!(mode(&f.path), 0o644);
        let copy = backup(&fs::canonicalize(&f.path).unwrap());
        assert_eq!(first.backup.as_deref(), Some(copy.as_path()));
        assert_eq!(mode(&copy), 0o600);
        assert_eq!(fs::read_to_string(&copy).unwrap(), initial);
        let bytes = fs::read_to_string(&f.path).unwrap();
        let mtime = fs::metadata(&f.path).unwrap().modified().unwrap();
        let second = f.install(id, &DEFAULT).unwrap();
        assert_eq!(second.action, Action::AlreadyUpToDate, "{id:?}");
        assert_eq!(second.backup, None);
        assert_eq!(fs::metadata(&f.path).unwrap().modified().unwrap(), mtime);
        assert!(bytes.contains("otro") && f.state(id) == EntryState::Installed);
        if id == HarnessId::OpenCode {
            assert!(bytes.contains("\r\n\t\t\t\"vela\":"));
            assert!(!bytes.ends_with('\n'));
        } else if id != HarnessId::Codex {
            assert!(bytes.contains("\n        \"vela\":"));
        }
        f.uninstall(id, &DEFAULT).unwrap();
        assert_eq!(fs::read_to_string(&f.path).unwrap(), initial, "{id:?}");
        assert_eq!(fs::read_to_string(&copy).unwrap(), bytes);
        assert_eq!(mode(&f.path), 0o644);
    }
}

#[test]
fn updates_outdated_and_requires_force_for_conflicts() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        let initial = expected(id, &f.cmd);
        let path = f.cmd.program_str();
        let with_program = |program: &str| match id {
            HarnessId::Codex => initial.replace(path, program),
            _ => initial.replace(
                &serde_json::to_string(path).unwrap(),
                &serde_json::to_string(program).unwrap(),
            ),
        };
        let obsolete = with_program("/old/Vela.app/Contents/MacOS/vela");
        f.put(&obsolete);
        assert!(matches!(f.state(id), EntryState::Outdated { .. }));
        assert_eq!(f.uninstall(id, &DEFAULT).unwrap().action, Action::Removed);
        f.put(&obsolete);
        assert_eq!(f.install(id, &DEFAULT).unwrap().action, Action::Updated);
        assert_eq!(f.state(id), EntryState::Installed);

        let competing = with_program("/bin/other");
        f.put(&competing);
        assert!(matches!(f.state(id), EntryState::Conflict { .. }));
        for result in [f.install(id, &DEFAULT), f.uninstall(id, &DEFAULT)] {
            assert!(matches!(result, Err(HarnessError::Conflict { .. })));
        }
        assert_eq!(fs::read_to_string(&f.path).unwrap(), competing);
        assert_eq!(f.install(id, &FORCE).unwrap().action, Action::Updated);
        assert_eq!(fs::read_to_string(&f.path).unwrap(), initial);
        f.put(&competing);
        assert_eq!(f.uninstall(id, &FORCE).unwrap().action, Action::Removed);
        assert_eq!(f.state(id), EntryState::NotInstalled);
    }
}

#[test]
fn updates_only_the_program_and_preserves_custom_settings() {
    const OLD: &str = "/old/Vela.app/Contents/MacOS/vela";
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        let initial = match id {
            HarnessId::Codex => {
                r#"# Keep
[mcp_servers.vela] # Vela
command   =   '/old/Vela.app/Contents/MacOS/vela'   # program
args = [ "mcp" ] # arguments
startup_timeout_sec = 30
tool_timeout_sec = 45
enabled_tools = ["list"]
enabled = false
cwd = '/tmp'

[mcp_servers.vela.env] # custom environment
VAR = 'fake'
"#
            }
            HarnessId::OpenCode => {
                r#"{"mcp":{"servers":{"vela":{
  "type":"local","command":[ "/old/Vela.app/Contents/MacOS/vela" , "mcp" ],
  "environment":{"VAR":"fake"},"enabled":false,"timeout":30000
}}}}"#
            }
            _ => {
                r#"{"mcpServers" : {"vela":{
  "command" : "/old/Vela.app/Contents/MacOS/vela", "args" : [ "mcp" ],
  "env":{"VAR":"fake"},"cwd":"/tmp","disabled":true,"timeout":30000
}}}"#
            }
        };
        let new = if id == HarnessId::Codex {
            format!("'{}'", f.cmd.program_str())
        } else {
            serde_json::to_string(f.cmd.program_str()).unwrap()
        };
        let old = if id == HarnessId::Codex {
            format!("'{OLD}'")
        } else {
            serde_json::to_string(OLD).unwrap()
        };
        for initial in [
            initial.to_owned(),
            initial.replace('\n', "\r\n"),
            initial.trim_end().replacen('\n', "\r\n", 1),
        ] {
            f.put(&initial);
            assert_eq!(f.state(id), EntryState::Outdated { found: OLD.into() });
            assert_eq!(f.install(id, &DEFAULT).unwrap().action, Action::Updated);
            assert_eq!(f.state(id), EntryState::Installed);
            assert_eq!(
                fs::read_to_string(&f.path).unwrap(),
                initial.replace(&old, &new)
            );
        }
        f.put(&initial.replace(OLD, "/bin/other"));
        assert!(matches!(f.state(id), EntryState::Conflict { .. }));
        f.install(id, &FORCE).unwrap();
        assert_eq!(f.state(id), EntryState::Installed);
        let forced = fs::read_to_string(&f.path).unwrap();
        assert!(!forced.contains("fake") && !forced.contains("timeout"));
    }
}

#[test]
fn conflict_descriptions_only_show_the_program_or_remote_host() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        let local = match id {
            HarnessId::Codex => {
                "[mcp_servers.vela]\ncommand = 'other'\nargs = ['SECRET']\n[mcp_servers.vela.env]\nSECRET = 'fake'\n"
            }
            HarnessId::OpenCode => {
                r#"{"mcp":{"servers":{"vela":{"type":"local","command":["other","SECRET"],"environment":{"SECRET":"fake"}}}}}"#
            }
            _ => {
                r#"{"mcpServers":{"vela":{"command":"other","args":["SECRET"],"env":{"SECRET":"fake"}}}}"#
            }
        };
        f.put(local);
        assert_eq!(
            f.state(id),
            EntryState::Conflict {
                found: "other".into()
            }
        );
        let remote = match id {
            HarnessId::Codex => {
                "[mcp_servers.vela]\nurl = 'https://SECRET:fake@example.org:8443/SECRET?token=fake#SECRET'\n[mcp_servers.vela.http_headers]\nAuthorization = 'fake'\n"
            }
            HarnessId::OpenCode => {
                r#"{"mcp":{"servers":{"vela":{"type":"remote","url":"https://SECRET:fake@example.org:8443/SECRET?token=fake#SECRET","headers":{"Authorization":"fake"}}}}}"#
            }
            _ => {
                r#"{"mcpServers":{"vela":{"url":"https://SECRET:fake@example.org:8443/SECRET?token=fake#SECRET","headers":{"Authorization":"fake"}}}}"#
            }
        };
        f.put(remote);
        assert_eq!(
            f.state(id),
            EntryState::Conflict {
                found: "url: example.org".into()
            }
        );
    }
    let id = HarnessId::Cursor;
    let f = Fixture::new(id);
    f.put(r#"{"mcpServers":{"vela":{"env":{"SECRET":"fake"}}}}"#);
    assert_eq!(
        f.state(id),
        EntryState::Conflict {
            found: "(sin comando)".into()
        }
    );
    f.put(
        r#"{"mcpServers":{"vela":{"httpUrl":"http://SECRET:fake@[::1]:1234/private?token=fake"}}}"#,
    );
    assert_eq!(
        f.state(id),
        EntryState::Conflict {
            found: "url: [::1]".into()
        }
    );
    f.put(r#"{"mcpServers":{"vela":{"serverUrl":"https://SECRET:fake"}}}"#);
    assert_eq!(
        f.state(id),
        EntryState::Conflict {
            found: "url: (sin host)".into()
        }
    );
    let program = format!("other\n{}", "é".repeat(250));
    f.put(&serde_json::json!({"mcpServers":{"vela":{"command":program}}}).to_string());
    let EntryState::Conflict { found } = f.state(id) else {
        panic!("se esperaba un conflicto");
    };
    assert!(found.starts_with("other�") && found.ends_with('…'));
    assert_eq!(found.chars().count(), 201);
}

#[test]
fn malformed_configs_leave_no_side_effects() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        let broken = "secret = [token";
        f.put(broken);
        assert!(matches!(
            detect(id, &f.env, &f.cmd),
            HarnessStatus::Malformed { .. }
        ));
        for result in [f.install(id, &FORCE), f.uninstall(id, &FORCE)] {
            let error = result.unwrap_err();
            assert!(matches!(error, HarnessError::Malformed { .. }));
            assert!(!error.to_string().contains("token"));
        }
        assert_eq!(fs::read_to_string(&f.path).unwrap(), broken);
        assert!(!backup(&f.path).exists());
    }
}

#[test]
fn dry_runs_write_neither_configs_nor_backups() {
    let dry = InstallOptions {
        dry_run: true,
        ..DEFAULT
    };
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        assert!(f.install(id, &dry).unwrap().diff.contains("+"));
        assert!(!f.env.home.exists());
        let initial = original(id);
        f.put(initial);
        let report = f.install(id, &dry).unwrap();
        assert!(report.diff.contains("@@") && report.diff.contains("vela"));
        assert_eq!(report.backup, None);
        assert_eq!(fs::read_to_string(&f.path).unwrap(), initial);
        assert!(!backup(&f.path).exists());

        f.put(&expected(id, &f.cmd));
        assert_eq!(f.uninstall(id, &dry).unwrap().action, Action::Removed);
        assert_eq!(f.state(id), EntryState::Installed);
        assert!(!backup(&f.path).exists());
    }
}

#[test]
fn config_symlinks_remain_links_and_backup_is_by_target() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        fs::create_dir_all(f.path.parent().unwrap()).unwrap();
        let target = f.dir.path().join("dotfiles");
        fs::write(&target, original(id)).unwrap();
        symlink(&target, &f.path).unwrap();
        let report = f.install(id, &DEFAULT).unwrap();
        assert_eq!(report.config_path, f.path);
        let resolved = target.canonicalize().unwrap();
        assert_eq!(report.backup, Some(backup(&resolved)));
        assert!(f.path.is_symlink());
        assert_eq!(fs::read_link(&f.path).unwrap(), target);
        assert_eq!(f.state(id), EntryState::Installed);
        assert_ne!(fs::read_to_string(&target).unwrap(), original(id));
        f.uninstall(id, &DEFAULT).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), original(id));
    }
}

#[test]
fn concurrent_change_retries_once_then_leaves_the_other_writer_intact() {
    for id in HarnessId::ALL {
        let f = Fixture::new(id);
        for writes in [1, 2] {
            let initial = original(id);
            f.put(initial);
            let copy = backup(&f.path);
            fs::write(&copy, "previo").unwrap();
            let mtime = fs::metadata(&f.path).unwrap().modified().unwrap();
            let mut calls = 0;
            let mut external = initial.to_owned();
            let result = change(id, &f.env, &f.cmd, &DEFAULT, true, || {
                calls += 1;
                if calls <= writes {
                    // Preserve length and mtime so the retry also exercises the content hash.
                    let name = if calls == 1 { "otro" } else { "ext1" };
                    external = external.replacen(name, if calls == 1 { "ext1" } else { "ext2" }, 1);
                    fs::write(&f.path, &external).unwrap();
                    fs::File::open(&f.path)
                        .unwrap()
                        .set_modified(mtime)
                        .unwrap();
                }
            });
            assert_eq!(calls, 2);
            if writes == 1 {
                assert_eq!(result.unwrap().action, Action::Created);
                assert_eq!(f.state(id), EntryState::Installed);
                assert_eq!(fs::read_to_string(&copy).unwrap(), external);
            } else {
                assert!(matches!(
                    result,
                    Err(HarnessError::ConcurrentModification { .. })
                ));
                assert_eq!(fs::read_to_string(&f.path).unwrap(), external);
                assert_eq!(fs::read_to_string(&copy).unwrap(), "previo");
                assert_eq!(fs::read_dir(f.path.parent().unwrap()).unwrap().count(), 2);
            }
        }
    }
}

#[test]
fn opencode_legacy_and_jsonc_preserve_the_active_shape() {
    let id = HarnessId::OpenCode;
    let mut f = Fixture::new(id);
    let legacy = r#"{"mcp":{"otro":{"type":"remote"}},"a":4}"#;
    f.put(legacy);
    f.install(id, &DEFAULT).unwrap();
    let data: serde_json::Value = serde_json::from_slice(&fs::read(&f.path).unwrap()).unwrap();
    assert_eq!(data["mcp"]["vela"]["enabled"], true);
    assert!(data["mcp"].get("servers").is_none());
    f.uninstall(id, &DEFAULT).unwrap();
    assert_eq!(fs::read_to_string(&f.path).unwrap(), legacy);

    fs::remove_file(backup(&f.path)).unwrap();
    for extension in ["json", "jsonc"] {
        f.path.set_extension(extension);
        let jsonc = r#"{"mcp": {/* A note */ "otro": {"type": "remote"},}, "literal": "https://example.org/*text*/,}"}"#;
        f.put(jsonc);
        assert_eq!(f.install(id, &DEFAULT).unwrap().action, Action::Created);
        assert_eq!(f.state(id), EntryState::Installed);
        let installed = fs::read_to_string(&f.path).unwrap();
        assert!(installed.contains(r#""enabled":true"#));
        assert!(installed.contains("/* A note */"));
        assert!(installed.contains("https://example.org/*text*/,}"));
        assert!(!installed.contains(",,"));
        assert_eq!(f.uninstall(id, &DEFAULT).unwrap().action, Action::Removed);
        assert_eq!(fs::read_to_string(&f.path).unwrap(), jsonc);
        fs::remove_file(&f.path).unwrap();
    }
    f.put(legacy);
    let json = f.path.with_extension("json");
    fs::write(&json, "{}").unwrap();
    let HarnessStatus::ManualRequired { snippet, .. } = detect(id, &f.env, &f.cmd) else {
        panic!("se esperaba edición manual por coexistencia de archivos");
    };
    let snippet: serde_json::Value = serde_json::from_str(&snippet).unwrap();
    assert_eq!(snippet["mcp"]["vela"]["enabled"], true);
    for result in [f.install(id, &FORCE), f.uninstall(id, &FORCE)] {
        assert!(matches!(result, Err(HarnessError::ManualRequired { .. })));
    }
    assert_eq!(fs::read_to_string(&f.path).unwrap(), legacy);
    assert_eq!(fs::read_to_string(json).unwrap(), "{}");
    fs::remove_file(f.path.with_extension("json")).unwrap();
    let mixed = r#"{"mcp":{/* v1 */"vela":{"type":"local"},"servers":{},}}"#;
    f.put(mixed);
    for result in [f.install(id, &FORCE), f.uninstall(id, &FORCE)] {
        assert!(matches!(result, Err(HarnessError::ManualRequired { .. })));
    }
    assert_eq!(fs::read_to_string(&f.path).unwrap(), mixed);
}

#[test]
fn opencode_jsonc_v2_round_trip_preserves_comments_trailing_commas_and_crlf() {
    let id = HarnessId::OpenCode;
    let mut f = Fixture::new(id);
    f.path.set_extension("jsonc");
    for last_member in [
        "",
        "\t\t\t\"otro\": {\"type\":\"remote\"} /* ñ, */, // last member\n",
        "\t\t\t\"otro\": {\"type\":\"remote\"} // last member\n",
    ] {
        let initial = format!(
            "// root\n{{\n\t\"mcp\": {{\n\t\t\"servers\": {{ // before members\n{last_member}\t\t\t/* end of servers */\n\t\t}},\n\t}},\n}} // EOF"
        ).replace('\n', "\r\n");
        f.put(&initial);
        assert_eq!(f.install(id, &DEFAULT).unwrap().action, Action::Created);
        assert_eq!(f.state(id), EntryState::Installed);
        let installed = fs::read_to_string(&f.path).unwrap();
        assert!(installed.contains("\r\n\t\t\t\"vela\":"));
        assert!(
            installed
                .split_inclusive('\n')
                .all(|line| !line.ends_with('\n') || line.ends_with("\r\n"))
        );
        f.uninstall(id, &DEFAULT).unwrap();
        assert_eq!(fs::read_to_string(&f.path).unwrap(), initial);
    }
}

#[test]
fn opencode_jsonc_removes_only_the_entry_and_its_separator_near_comments() {
    let id = HarnessId::OpenCode;
    let mut f = Fixture::new(id);
    f.path.set_extension("jsonc");
    let vela = r#""vela":{"type":"local",/* inside */"command":["/old/vela","mcp"]}"#;
    for (members, expected) in [
        (
            format!(r#"{vela} /* a */, /* b */ "next":{{}}"#),
            r#" /* a */ /* b */ "next":{}"#,
        ),
        (
            format!(r#""prev":{{}} /* a */, /* b */ {vela}, /* c */ "next":{{}}"#),
            r#""prev":{} /* a */ /* b */ , /* c */ "next":{}"#,
        ),
        (
            format!(r#""prev":{{}} /* a */, /* b */ {vela}, /* c */"#),
            r#""prev":{} /* a */ /* b */ , /* c */"#,
        ),
        (
            format!(r#"/* a */ {vela} /* b */, /* c */"#),
            r#"/* a */  /* b */ /* c */"#,
        ),
    ] {
        let wrap = |members: &str| format!(r#"{{"mcp":{{"servers":{{{members}}}}}}}"#);
        f.put(&wrap(&members));
        assert_eq!(f.uninstall(id, &DEFAULT).unwrap().action, Action::Removed);
        assert_eq!(f.state(id), EntryState::NotInstalled);
        assert_eq!(fs::read_to_string(&f.path).unwrap(), wrap(expected));
    }
}

#[test]
fn opencode_jsonc_updates_only_the_first_command_item() {
    let id = HarnessId::OpenCode;
    let mut f = Fixture::new(id);
    f.path.set_extension("jsonc");
    let vela = r#""vela":{
      "type":"local",
      "command":[/* before */ "/old/vela" /* after */, // keep this
                 "mcp",],
      "environment":{"VAR":"fake"},"enabled":false,"timeout":30000,
    }"#;
    for members in [vela.to_owned(), format!(r#""servers":{{{vela},}}"#)] {
        let initial = format!(r#"{{"mcp":{{{members},}},}}"#);
        f.put(&initial);
        assert_eq!(
            f.state(id),
            EntryState::Outdated {
                found: "/old/vela".into()
            }
        );
        assert_eq!(f.install(id, &DEFAULT).unwrap().action, Action::Updated);
        assert_eq!(f.state(id), EntryState::Installed);
        assert_eq!(
            fs::read_to_string(&f.path).unwrap(),
            initial.replace(
                r#""/old/vela""#,
                &serde_json::to_string(f.cmd.program_str()).unwrap()
            )
        );
    }
}

#[test]
fn opencode_jsonc_rejects_unclosed_comments_and_invalid_json() {
    let id = HarnessId::OpenCode;
    let mut f = Fixture::new(id);
    f.path.set_extension("jsonc");
    for broken in [
        "/* SECRET",
        "{\"mcp\": {,}}",
        "{\"mcp\": {},,}",
        r#"{"mcp":{"servers":{"vela":{"command":[],"command":["/old/vela","mcp"]}}}}"#,
    ] {
        f.put(broken);
        assert!(matches!(
            detect(id, &f.env, &f.cmd),
            HarnessStatus::Malformed { .. }
        ));
        assert!(matches!(
            f.install(id, &FORCE),
            Err(HarnessError::Malformed { .. })
        ));
        assert_eq!(fs::read_to_string(&f.path).unwrap(), broken);
        assert!(!backup(&f.path).exists());
    }
}

#[test]
fn codex_removes_subtables_and_defers_inline_tables() {
    let id = HarnessId::Codex;
    let f = Fixture::new(id);
    let first = "[mcp_servers.otro] # other\ncommand = 'otro'\n";
    let last = "\n# Keep\n[profiles.x]\nmodel = 'm'\n";
    let configured = expected(id, &f.cmd) + "[mcp_servers.vela.env]\nVAR = 'v'\n";
    f.put(&format!("{first}{configured}{last}"));
    assert_eq!(f.uninstall(id, &DEFAULT).unwrap().action, Action::Removed);
    assert_eq!(
        fs::read_to_string(&f.path).unwrap(),
        format!("{first}{last}")
    );
    for source in [
        "mcp_servers = { vela = { command = 'x' } }",
        "[mcp_servers]\nvela = { command = 'x' }",
    ] {
        f.put(source);
        assert!(matches!(
            detect(id, &f.env, &f.cmd),
            HarnessStatus::ManualRequired { .. }
        ));
        assert!(matches!(
            f.install(id, &FORCE),
            Err(HarnessError::ManualRequired { .. })
        ));
        assert!(matches!(
            f.uninstall(id, &FORCE),
            Err(HarnessError::ManualRequired { .. })
        ));
        assert_eq!(fs::read_to_string(&f.path).unwrap(), source);
    }
}

#[test]
fn claude_large_config_keeps_user_account_and_project_mcp_scopes() {
    let id = HarnessId::ClaudeCode;
    let mut f = Fixture::new(id);
    let override_dir = f.dir.path().join("claude-override");
    f.env.claude_config_dir = Some(override_dir.clone());
    f.path = override_dir.join(".claude.json");
    let projects = (0..1000)
        .map(|n| format!(r#""project{n}":{{"mcpServers":{{"vela":{{"command":"custom"}}}}}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let initial = format!(
        "{{\"projects\":{{{projects}}},\"oauthAccount\":{{\"token\":\"fake\"}},\"mcpServers\":{{}}}}"
    );
    f.put(&initial);
    let report = f.install(id, &DEFAULT).unwrap();
    assert_eq!(report.config_path, f.path);
    f.uninstall(id, &DEFAULT).unwrap();
    assert_eq!(fs::read_to_string(&f.path).unwrap(), initial);
    assert!(!f.env.home.exists());
}

#[test]
fn supports_overrides_path_binaries_and_mac_app_bundles() {
    let f = Fixture::new(HarnessId::Codex);
    let mut env = f.env;
    let bin = f.dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    env.path_dirs.push(bin.clone());
    for (id, name) in
        HarnessId::ALL
            .into_iter()
            .zip(["claude", "codex", "cursor-agent", "opencode", "agy"])
    {
        let executable = bin.join(name);
        fs::write(&executable, "").unwrap();
        assert_eq!(detect(id, &env, &f.cmd), HarnessStatus::NotDetected);
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            detect(id, &env, &f.cmd),
            HarnessStatus::Detected {
                config_exists: false,
                ..
            }
        ));
    }
    env.codex_home = Some(f.dir.path().join("my-codex"));
    assert_eq!(
        env.config(HarnessId::Codex).path,
        f.dir.path().join("my-codex/config.toml")
    );
    env.xdg_config_home = Some(f.dir.path().join("xdg"));
    assert_eq!(
        env.config(HarnessId::OpenCode).path,
        f.dir.path().join("xdg/opencode/opencode.json")
    );
    env.path_dirs.clear();
    let apps = f.dir.path().join("Applications");
    fs::create_dir_all(apps.join("Cursor.app")).unwrap();
    fs::create_dir(apps.join("Antigravity.app")).unwrap();
    env.app_dirs.push(apps);
    assert_eq!(
        detect(HarnessId::Cursor, &env, &f.cmd),
        HarnessStatus::NotDetected
    );
    env.os = TargetOs::MacOs;
    for id in [HarnessId::Cursor, HarnessId::Antigravity] {
        assert!(matches!(
            detect(id, &env, &f.cmd),
            HarnessStatus::Detected {
                config_exists: false,
                ..
            }
        ));
    }
}

#[test]
fn resolves_canonical_executable_and_mcp_argument() {
    let f = Fixture::new(HarnessId::Codex);
    let alias = f.dir.path().join("alias");
    symlink(f.cmd.program(), &alias).unwrap();
    assert_eq!(VelaCommand::new(alias).unwrap(), f.cmd);
    let current = VelaCommand::current().unwrap();
    assert_eq!(
        current.program(),
        std::env::current_exe().unwrap().canonicalize().unwrap()
    );
    assert!(current.program().is_absolute());
    assert_eq!(current.args(), ["mcp"]);
}

#[test]
fn replaces_a_symlink_target_only_if_the_link_is_still_selected() {
    let id = HarnessId::Cursor;
    let f = Fixture::new(id);
    fs::create_dir_all(f.path.parent().unwrap()).unwrap();
    let old = f.dir.path().join("old");
    let new = f.dir.path().join("new");
    fs::write(&old, "{}").unwrap();
    fs::write(&new, "{}").unwrap();
    symlink(&old, &f.path).unwrap();
    let mut retargeted = false;
    change(id, &f.env, &f.cmd, &DEFAULT, true, || {
        if !retargeted {
            fs::remove_file(&f.path).unwrap();
            symlink(&new, &f.path).unwrap();
            retargeted = true;
        }
    })
    .unwrap();
    assert_eq!(fs::read_to_string(&old).unwrap(), "{}");
    assert!(!backup(&old).exists());
    assert_ne!(fs::read_to_string(&new).unwrap(), "{}");
}

#[test]
fn antigravity_prefers_the_current_file_for_agy_and_preserves_existing_legacy_configs() {
    let id = HarnessId::Antigravity;
    let mut f = Fixture::new(id);
    let legacy = f.env.home.join(".gemini/antigravity/mcp_config.json");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, "{}").unwrap();
    assert_eq!(f.env.config(id).path, legacy);
    let bin = f.dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("agy"), "").unwrap();
    fs::set_permissions(bin.join("agy"), fs::Permissions::from_mode(0o700)).unwrap();
    f.env.path_dirs.push(bin);
    assert_eq!(f.install(id, &DEFAULT).unwrap().config_path, f.path);
    f.env.path_dirs.clear();
    assert_eq!(f.env.config(id).path, f.path);
    assert_eq!(fs::read_to_string(legacy).unwrap(), "{}");
}

#[test]
fn cursor_detects_the_official_agent_alias_without_accepting_an_unrelated_agent() {
    let id = HarnessId::Cursor;
    let mut f = Fixture::new(id);
    let bin = f.dir.path().join("bin");
    let versions = f.dir.path().join("versions");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&versions).unwrap();
    f.env.path_dirs.push(bin.clone());
    let alias = bin.join("agent");
    fs::write(&alias, "").unwrap();
    fs::set_permissions(&alias, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(detect(id, &f.env, &f.cmd), HarnessStatus::NotDetected);

    let target = versions.join("cursor-agent");
    fs::rename(&alias, &target).unwrap();
    symlink(target, alias).unwrap();
    assert!(matches!(
        detect(id, &f.env, &f.cmd),
        HarnessStatus::Detected { .. }
    ));
}

#[test]
fn unified_diff_has_valid_zero_line_ranges_and_retains_line_endings() {
    assert_eq!(
        unified_diff("", "new\n"),
        "--- original\n+++ propuesta\n@@ -0,0 +1,1 @@\n+new\n"
    );
    assert_eq!(
        unified_diff("same\r\nold", "same\r\nnew\r\n"),
        "--- original\n+++ propuesta\n@@ -2,1 +2,1 @@\n-old\n\\ No newline at end of file\n+new\r\n"
    );
}

#[test]
fn codex_preserves_other_records_with_crlf_and_no_final_newline() {
    let id = HarnessId::Codex;
    let f = Fixture::new(id);
    for newline in ["\n", "\r\n"] {
        let original =
            "model = 'm'\n\n[mcp_servers.otro] # keep\ncommand = 'other'".replace('\n', newline);
        f.put(&original);
        f.install(id, &DEFAULT).unwrap();
        let updated = fs::read_to_string(&f.path).unwrap();
        assert!(!updated.ends_with('\n'));
        assert!(updated.contains(&format!("[mcp_servers.vela]{newline}")));
        f.uninstall(id, &DEFAULT).unwrap();
        assert_eq!(fs::read_to_string(&f.path).unwrap(), original);
    }
}
