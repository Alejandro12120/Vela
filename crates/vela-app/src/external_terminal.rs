use std::{
    env,
    ffi::OsString,
    io,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use vela_core::settings::TerminalConfigSource;

const SSH: &str = "/usr/bin/ssh";
const MISSING: &str = "No supported external terminal was found. Install one to open this session.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    executable: &'static str,
    prefix: &'static [&'static str],
    bundle: &'static str,
}

const KITTY: Preset = preset("kitty", "Kitty", "kitty", &["--detach"], "kitty.app");
const GHOSTTY: Preset = preset("ghostty", "Ghostty", "ghostty", &["-e"], "Ghostty.app");
const WEZTERM: Preset = preset(
    "wezterm",
    "WezTerm",
    "wezterm",
    &[
        "start",
        "--new-tab",
        "--no-auto-connect",
        "--domain",
        "local",
        "--",
    ],
    "WezTerm.app",
);
const ALACRITTY: Preset = preset(
    "alacritty",
    "Alacritty",
    "alacritty",
    &["-e"],
    "Alacritty.app",
);
const FOOT: Preset = preset("foot", "foot", "foot", &["--"], "");
const KONSOLE: Preset = preset("konsole", "Konsole", "konsole", &["--new-tab", "-e"], "");
const GNOME: Preset = preset(
    "gnome-terminal",
    "GNOME Terminal",
    "gnome-terminal",
    &["--tab", "--"],
    "",
);
const XTERM: Preset = preset("xterm", "xterm", "xterm", &["-e"], "");
const APPLE_TERMINAL: Preset = preset("terminal", "Terminal", "", &[], "Terminal.app");
const ITERM: Preset = preset("iterm2", "iTerm2", "", &[], "iTerm.app");

const LINUX: &[Preset] = &[
    KITTY, GHOSTTY, WEZTERM, ALACRITTY, FOOT, KONSOLE, GNOME, XTERM,
];
const MAC: &[Preset] = &[KITTY, APPLE_TERMINAL, ITERM, GHOSTTY, WEZTERM, ALACRITTY];

const fn preset(
    id: &'static str,
    name: &'static str,
    executable: &'static str,
    prefix: &'static [&'static str],
    bundle: &'static str,
) -> Preset {
    Preset {
        id,
        name,
        executable,
        prefix,
        bundle,
    }
}

impl Preset {
    pub fn config_source(self) -> Option<TerminalConfigSource> {
        match self.id {
            "kitty" => Some(TerminalConfigSource::Kitty),
            "ghostty" => Some(TerminalConfigSource::Ghostty),
            "wezterm" => Some(TerminalConfigSource::Wezterm),
            "alacritty" => Some(TerminalConfigSource::Alacritty),
            "foot" => Some(TerminalConfigSource::Foot),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Installed {
    pub preset: Preset,
    path: PathBuf,
}

pub struct ExternalTerminals {
    pub installed: Vec<Installed>,
    pub preferred: Option<String>,
}

impl gpui::Global for ExternalTerminals {}

impl ExternalTerminals {
    pub fn selected(cx: &gpui::App) -> Option<&Installed> {
        let terminals = cx.try_global::<Self>()?;
        select(&terminals.installed, terminals.preferred.as_deref())
    }
}

fn select<'a>(installed: &'a [Installed], preferred: Option<&str>) -> Option<&'a Installed> {
    installed
        .iter()
        .find(|terminal| Some(terminal.preset.id) == preferred)
        .or_else(|| installed.first())
}

pub fn detect() -> Vec<Installed> {
    let mac = cfg!(target_os = "macos");
    let roots = if let Some(root) = crate::dev::terminal_root() {
        vec![root]
    } else if mac {
        env::home_dir()
            .into_iter()
            .map(|home| home.join("Applications"))
            .chain(
                [
                    "/Applications",
                    "/Applications/Utilities",
                    "/System/Applications/Utilities",
                ]
                .map(PathBuf::from),
            )
            .collect()
    } else {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
            .filter_map(|path| {
                let path = if path.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    &path
                };
                std::path::absolute(path).ok()
            })
            .collect()
    };
    detect_in(&roots, mac)
}

fn detect_in(roots: &[PathBuf], mac: bool) -> Vec<Installed> {
    let presets = if mac { MAC } else { LINUX };
    presets
        .iter()
        .filter_map(|&preset| {
            let name = if mac {
                preset.bundle
            } else {
                preset.executable
            };
            let path = roots
                .iter()
                .map(|root| root.join(name))
                .find(|path| if mac { path.is_dir() } else { executable(path) })?;
            Some(Installed { preset, path })
        })
        .collect()
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

pub fn action_label(action: &str, terminal: Option<&Installed>) -> String {
    let name = terminal.map_or("External Terminal", |terminal| terminal.preset.name);
    format!("{action} in {name}")
}

pub async fn launch(terminal: Option<Installed>, args: &[OsString]) -> Result<(), String> {
    if crate::dev::launches_suppressed() {
        return Ok(());
    }
    let terminal = terminal.ok_or_else(|| MISSING.to_owned())?;
    terminal
        .run(args)
        .await
        .map_err(|err| format!("Couldn't open {}: {err}", terminal.preset.name))
}

impl Installed {
    fn command(&self, args: &[OsString], mac: bool) -> Command {
        let mut command = match (mac, self.preset.id) {
            (true, "kitty") => {
                let mut command = Command::new("/usr/bin/open");
                // Preserve Kitty's Launch Services invocation.
                command.args(["-na", "kitty", "--args"]);
                command
            }
            (true, "ghostty") => {
                let mut command = Command::new("/usr/bin/open");
                command.arg("-na").arg(&self.path).args([
                    "--args",
                    "--window-save-state=never",
                    "-e",
                ]);
                command
            }
            (true, _) => {
                let mut command = Command::new(
                    self.path
                        .join("Contents/MacOS")
                        .join(self.preset.executable),
                );
                command.args(self.preset.prefix);
                command
            }
            (false, _) => {
                let mut command = Command::new(&self.path);
                command.args(self.preset.prefix);
                command
            }
        };
        command.arg(SSH).args(args);
        command
    }

    fn ghostty_tab_command(&self, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.path);
        command.args(["+new-tab", "-e", SSH]).args(args);
        command
    }

    async fn run(&self, args: &[OsString]) -> io::Result<()> {
        if cfg!(target_os = "macos") && matches!(self.preset.id, "terminal" | "iterm2") {
            return self.run_command_file(args).await;
        }
        if cfg!(target_os = "linux") && self.preset == GHOSTTY {
            let mut tab = self.ghostty_tab_command(args);
            tab.stderr(Stdio::null());
            // Older versions and installations without D-Bus activation use a new window.
            if status(tab).await.is_ok() {
                return Ok(());
            }
        }
        status(self.command(args, cfg!(target_os = "macos"))).await
    }

    async fn run_command_file(&self, args: &[OsString]) -> io::Result<()> {
        let (dir, script) = command_file(args)?;
        let result = status(self.command_file_launcher(&script)).await;
        if result.is_ok() {
            // The terminal consumes the private directory once the script runs.
            let _ = dir.keep();
        }
        result
    }

    fn command_file_launcher(&self, path: &Path) -> Command {
        if self.preset == APPLE_TERMINAL {
            let mut command = Command::new("/usr/bin/open");
            command.arg("-a").arg(&self.path).arg(path);
            return command;
        }

        let mut command = Command::new("/usr/bin/osascript");
        command.args(["-e", ITERM_SCRIPT, "--"]).arg(path);
        command
    }
}

const ITERM_SCRIPT: &str = r#"
on run argv
    set launcher to item 1 of argv
    tell application id "com.googlecode.iterm2"
        if (count of windows) = 0 then
            create window with default profile command launcher
        else
            tell current window to create tab with default profile command launcher
        end if
        activate
    end tell
end run
"#;

// The only command text that Apple Terminal or iTerm2 receives is a generated
// path under /tmp. SSH arguments live in a private NUL-delimited file and are
// passed by Bash as individual arguments, including non-UTF-8 bytes.
fn command_file(args: &[OsString]) -> io::Result<(tempfile::TempDir, PathBuf)> {
    use std::{
        fs,
        io::Write,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    let dir = tempfile::Builder::new()
        .prefix("vela-terminal-")
        .tempdir_in("/tmp")?;
    let script = dir.path().join("ssh.command");
    let mut data = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(dir.path().join("argv"))?;
    for arg in args {
        let bytes = arg.as_os_str().as_bytes();
        if bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH argument contains NUL",
            ));
        }
        data.write_all(bytes)?;
        data.write_all(b"\0")?;
    }
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o700)
        .open(&script)?
        .write_all(
            br#"#!/bin/bash
set -e
dir=${0%/*}
args=()
while IFS= read -r -d '' arg; do args+=("$arg"); done < "$dir/argv"
/bin/rm -- "$dir/argv" "$0"
/bin/rmdir -- "$dir"
exec /usr/bin/ssh "${args[@]}"
"#,
        )?;
    Ok((dir, script))
}

async fn status(mut command: Command) -> io::Result<()> {
    // A new process group avoids propagating terminal signals from Vela.
    // async-process reaps children even if the UI task is dropped.
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let status = async_process::Command::from(command).status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(status.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    };

    #[test]
    fn detection_prefers_kitty_and_falls_back_for_unknown_or_missing_ids() {
        let root = tempfile::tempdir().unwrap();
        let roots = [root.path().to_owned()];
        for name in ["foot", "ghostty", "kitty"] {
            let path = root.path().join(name);
            fs::write(&path, "").unwrap();
            if name != "ghostty" {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let installed = detect_in(&roots, false);
        assert_eq!(installed.len(), 2);
        for preferred in [None, Some("unknown"), Some("ghostty")] {
            assert_eq!(select(&installed, preferred).unwrap().preset, KITTY);
        }
        assert_eq!(select(&installed, Some("foot")).unwrap().preset, FOOT);
        fs::remove_file(root.path().join("kitty")).unwrap();
        assert_eq!(
            select(&detect_in(&roots, false), None).unwrap().preset,
            FOOT
        );
        assert!(select(&[], None).is_none());
    }

    #[test]
    fn mac_detection_preserves_bundle_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("WezTerm.app")).unwrap();
        let found = detect_in(&[root.path().to_owned()], true);
        assert_eq!(found[0].preset, WEZTERM);
        assert_eq!(found[0].path, root.path().join("WezTerm.app"));
    }

    #[test]
    fn launch_commands_preserve_every_ssh_argument() {
        let args = [
            OsString::from("-t"),
            OsString::from("cd 'a b'; $(echo x)\n"),
            OsString::from_vec(vec![0xff]),
        ];
        let cases: &[(Preset, bool, &str, &[&str])] = &[
            (KITTY, false, "/apps/kitty", &["--detach"]),
            (GHOSTTY, false, "/apps/ghostty", &["-e"]),
            (
                WEZTERM,
                false,
                "/apps/wezterm",
                &[
                    "start",
                    "--new-tab",
                    "--no-auto-connect",
                    "--domain",
                    "local",
                    "--",
                ],
            ),
            (ALACRITTY, false, "/apps/alacritty", &["-e"]),
            (FOOT, false, "/apps/foot", &["--"]),
            (KONSOLE, false, "/apps/konsole", &["--new-tab", "-e"]),
            (GNOME, false, "/apps/gnome-terminal", &["--tab", "--"]),
            (XTERM, false, "/apps/xterm", &["-e"]),
            (KITTY, true, "/usr/bin/open", &["-na", "kitty", "--args"]),
            (
                GHOSTTY,
                true,
                "/usr/bin/open",
                &[
                    "-na",
                    "/apps/Ghostty.app",
                    "--args",
                    "--window-save-state=never",
                    "-e",
                ],
            ),
            (
                WEZTERM,
                true,
                "/apps/WezTerm.app/Contents/MacOS/wezterm",
                &[
                    "start",
                    "--new-tab",
                    "--no-auto-connect",
                    "--domain",
                    "local",
                    "--",
                ],
            ),
            (
                ALACRITTY,
                true,
                "/apps/Alacritty.app/Contents/MacOS/alacritty",
                &["-e"],
            ),
        ];
        for &(preset, mac, executable, prefix) in cases {
            let app = if mac {
                preset.bundle
            } else {
                preset.executable
            };
            let installed = Installed {
                preset,
                path: Path::new("/apps").join(app),
            };
            let command = installed.command(&args, mac);
            let expected = prefix
                .iter()
                .map(OsString::from)
                .chain([SSH.into()])
                .chain(args.iter().cloned())
                .collect::<Vec<_>>();
            assert_eq!(command.get_program(), executable);
            assert_eq!(command.get_args().collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn ghostty_passes_ssh_arguments_to_its_tab_action() {
        let terminal = Installed {
            preset: GHOSTTY,
            path: "/apps/ghostty".into(),
        };
        let args = [OsString::from("cd 'directory $a'\n")];
        let command = terminal.ghostty_tab_command(&args);
        assert_eq!(command.get_program(), "/apps/ghostty");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["+new-tab", "-e", SSH, args[0].to_str().unwrap()]
        );
    }

    #[test]
    fn mac_command_file_keeps_ssh_arguments_out_of_the_script() {
        let args = [
            "foo bar".into(),
            "$'quote';\n".into(),
            "".into(),
            OsString::from_vec(vec![0xff]),
        ];
        let (dir, script) = command_file(&args).unwrap();
        assert_eq!(
            fs::read(dir.path().join("argv")).unwrap(),
            b"foo bar\0$'quote';\n\0\0\xff\0"
        );
        assert!(!fs::read_to_string(&script).unwrap().contains("foo bar"));
        let installed = Installed {
            preset: APPLE_TERMINAL,
            path: "/System/Applications/Utilities/Terminal.app".into(),
        };
        let command = installed.command_file_launcher(&script);
        assert_eq!(command.get_program(), "/usr/bin/open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [Path::new("-a"), &installed.path, &script]
        );
        let installed = Installed {
            preset: ITERM,
            path: "/Applications/iTerm.app".into(),
        };
        let command = installed.command_file_launcher(&script);
        assert_eq!(command.get_program(), "/usr/bin/osascript");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                Path::new("-e"),
                Path::new(ITERM_SCRIPT),
                Path::new("--"),
                &script
            ]
        );
    }
}
