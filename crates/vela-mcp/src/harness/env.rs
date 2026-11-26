use std::{
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use super::HarnessId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetOs {
    Linux,
    MacOs,
}

#[derive(Clone, Debug)]
pub struct HarnessEnv {
    pub home: PathBuf,
    pub xdg_config_home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
    pub path_dirs: Vec<PathBuf>,
    pub os: TargetOs,
    pub app_dirs: Vec<PathBuf>,
}

impl HarnessEnv {
    pub fn from_process() -> io::Result<Self> {
        let os = if cfg!(target_os = "macos") {
            TargetOs::MacOs
        } else {
            TargetOs::Linux
        };
        #[cfg(feature = "dev")]
        if let Some(home) = env::var_os("VELA_MCP_HARNESS_HOME") {
            return Ok(Self::isolated(absolute_home(Some(home.into()))?, os));
        }

        let home = absolute_home(env::home_dir())?;
        let app_dirs = if os == TargetOs::MacOs {
            vec![PathBuf::from("/Applications"), home.join("Applications")]
        } else {
            Vec::new()
        };
        Ok(Self {
            home,
            os,
            app_dirs,
            xdg_config_home: env_path("XDG_CONFIG_HOME").filter(|path| path.is_absolute()),
            codex_home: env_path("CODEX_HOME")
                .map(std::path::absolute)
                .transpose()?,
            claude_config_dir: env_path("CLAUDE_CONFIG_DIR")
                .map(std::path::absolute)
                .transpose()?,
            path_dirs: env::var_os("PATH")
                .map(|paths| env::split_paths(&paths).collect())
                .unwrap_or_default(),
        })
    }

    pub fn isolated(home: PathBuf, os: TargetOs) -> Self {
        Self {
            home,
            os,
            xdg_config_home: None,
            codex_home: None,
            claude_config_dir: None,
            path_dirs: Vec::new(),
            app_dirs: Vec::new(),
        }
    }

    pub(super) fn config(&self, id: HarnessId) -> Config {
        let mut ambiguous = false;
        let mut extra_marker = false;
        let (path, directory, binaries, apps): (_, _, &[_], &[_]) = match id {
            HarnessId::ClaudeCode => {
                let directory = self
                    .claude_config_dir
                    .clone()
                    .unwrap_or_else(|| self.home.join(".claude"));
                let path = self
                    .claude_config_dir
                    .as_ref()
                    .unwrap_or(&self.home)
                    .join(".claude.json");
                (path, directory, &["claude"], &[])
            }
            HarnessId::Codex => {
                let directory = self
                    .codex_home
                    .clone()
                    .unwrap_or_else(|| self.home.join(".codex"));
                (directory.join("config.toml"), directory, &["codex"], &[])
            }
            HarnessId::Cursor => {
                let directory = self.home.join(".cursor");
                (
                    directory.join("mcp.json"),
                    directory,
                    &["cursor", "cursor-agent"],
                    &["Cursor.app"],
                )
            }
            HarnessId::OpenCode => {
                let base = self
                    .xdg_config_home
                    .clone()
                    .unwrap_or_else(|| self.home.join(".config"));
                let directory = base.join("opencode");
                let json = directory.join("opencode.json");
                let jsonc = directory.join("opencode.jsonc");
                ambiguous = present(&json) && present(&jsonc);
                let path = if present(&jsonc) { jsonc } else { json };
                (path, directory, &["opencode"], &[])
            }
            HarnessId::Antigravity => {
                let gemini = self.home.join(".gemini");
                let directory = gemini.join("config");
                let current = directory.join("mcp_config.json");
                let legacy_dir = gemini.join("antigravity");
                let legacy = legacy_dir.join("mcp_config.json");
                extra_marker = legacy_dir.is_dir() || gemini.join("antigravity-cli").is_dir();
                let path = if !present(&current) && present(&legacy) && !self.has_binary(&["agy"]) {
                    legacy
                } else {
                    current
                };
                (
                    path,
                    directory,
                    &["antigravity", "agy"],
                    &["Antigravity.app", "Antigravity IDE.app"],
                )
            }
        };

        let detected = present(&path)
            || directory.is_dir()
            || extra_marker
            || self.has_binary(binaries)
            || id == HarnessId::Cursor
                && self.path_dirs.iter().any(|dir| {
                    let agent = dir.join("agent");
                    executable(&agent)
                        && agent.canonicalize().is_ok_and(|path| {
                            path.file_name().is_some_and(|name| name == "cursor-agent")
                        })
                })
            || self.os == TargetOs::MacOs
                && self
                    .app_dirs
                    .iter()
                    .any(|dir| apps.iter().any(|app| dir.join(app).is_dir()));
        Config {
            path,
            detected,
            ambiguous,
        }
    }
    fn has_binary(&self, binaries: &[&str]) -> bool {
        self.path_dirs
            .iter()
            .any(|dir| binaries.iter().any(|bin| executable(&dir.join(bin))))
    }
}

fn absolute_home(home: Option<PathBuf>) -> io::Result<PathBuf> {
    home.filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Se requiere un HOME absoluto."))
}

fn present(path: &Path) -> bool {
    // An inaccessible path still needs an I/O diagnostic, not a missing status.
    !matches!(path.symlink_metadata(), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

pub(super) struct Config {
    pub path: PathBuf,
    pub detected: bool,
    pub ambiguous: bool,
}
