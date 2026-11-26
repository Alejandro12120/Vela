//! Detección y edición de la configuración MCP de usuario en Linux y macOS.
//! Las operaciones de biblioteca usan un entorno explícito y no arrancan el agente.

mod env;
mod fsx;
mod json;
mod toml;

use std::{
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

pub use env::{HarnessEnv, TargetOs};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    Cursor,
    #[serde(rename = "opencode")]
    OpenCode,
    Antigravity,
}

impl HarnessId {
    pub const ALL: [Self; 5] = [
        Self::ClaudeCode,
        Self::Codex,
        Self::Cursor,
        Self::OpenCode,
        Self::Antigravity,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Antigravity => "antigravity",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::OpenCode => "OpenCode",
            Self::Antigravity => "Antigravity",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VelaCommand {
    program: String,
}

impl VelaCommand {
    pub fn current() -> io::Result<Self> {
        Self::new(std::env::current_exe()?)
    }

    pub fn new(executable: impl AsRef<Path>) -> io::Result<Self> {
        let program = executable
            .as_ref()
            .canonicalize()?
            .into_os_string()
            .into_string()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "La ruta de Vela debe ser UTF-8.",
                )
            })?;
        Ok(Self { program })
    }

    pub fn program(&self) -> &Path {
        Path::new(&self.program)
    }

    pub const fn args(&self) -> [&'static str; 1] {
        ["mcp"]
    }

    fn program_str(&self) -> &str {
        &self.program
    }

    fn classify(&self, program: Option<&str>, args_match: bool) -> EntryState {
        if let Some(found) = program.filter(|_| args_match) {
            if found == self.program {
                return EntryState::Installed;
            }
            let path = Path::new(found);
            if path.is_absolute() && path.file_name().is_some_and(|name| name == "vela") {
                return EntryState::Outdated {
                    found: found.to_owned(),
                };
            }
        }
        EntryState::Conflict {
            found: "La entrada está ocupada por otro comando o transporte.".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EntryState {
    NotInstalled,
    Installed,
    Outdated { found: String },
    Conflict { found: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HarnessStatus {
    NotDetected,
    Detected {
        config_path: PathBuf,
        config_exists: bool,
        entry: EntryState,
    },
    Malformed {
        config_path: PathBuf,
        error: String,
    },
    ManualRequired {
        config_path: PathBuf,
        reason: String,
        snippet: String,
    },
    Unreadable {
        config_path: PathBuf,
        error: String,
    },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub force: bool,
}

#[derive(Debug)]
pub struct ChangeReport {
    pub harness: HarnessId,
    pub config_path: PathBuf,
    pub action: Action,
    pub diff: String,
    pub backup: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Created,
    Updated,
    AlreadyUpToDate,
    Removed,
    NothingToRemove,
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("Error de E/S en {} ({source}).", .config_path.display())]
    Io {
        config_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Configuración inválida en {}: {error}", .config_path.display())]
    Malformed { config_path: PathBuf, error: String },
    #[error("La entrada vela de {} ya está ocupada. Usa --force para modificarla.", .config_path.display())]
    Conflict { config_path: PathBuf },
    #[error("Se requiere edición manual en {}: {reason}", .config_path.display())]
    ManualRequired {
        config_path: PathBuf,
        reason: String,
        snippet: String,
    },
    #[error("La configuración de {} cambió durante la operación.", .config_path.display())]
    ConcurrentModification { config_path: PathBuf },
}

#[derive(Debug)]
enum Problem {
    Malformed(String),
    Manual(String, bool),
}

impl Problem {
    fn at(self, id: HarnessId, config_path: &Path, cmd: &VelaCommand) -> HarnessError {
        let config_path = config_path.to_owned();
        match self {
            Self::Malformed(error) => HarnessError::Malformed { config_path, error },
            Self::Manual(reason, legacy) => HarnessError::ManualRequired {
                config_path,
                reason,
                snippet: make_snippet(id, legacy, cmd),
            },
        }
    }
}

enum Document<'a> {
    Json(json::Config<'a>),
    Toml(toml::Config),
}

impl<'a> Document<'a> {
    fn parse(
        id: HarnessId,
        path: &Path,
        snapshot: Option<&'a fsx::Snapshot>,
    ) -> Result<Self, Problem> {
        let source = std::str::from_utf8(snapshot.map_or(&[], |s| &s.bytes))
            .map_err(|_| Problem::Malformed("El archivo no es UTF-8.".into()))?;
        if id == HarnessId::Codex {
            toml::Config::parse(source).map(Self::Toml)
        } else {
            json::Config::parse(
                id,
                source,
                path.extension().is_some_and(|ext| ext == "jsonc"),
            )
            .map(Self::Json)
        }
    }

    fn entry(&self, cmd: &VelaCommand) -> EntryState {
        match self {
            Self::Json(config) => config.entry(cmd),
            Self::Toml(config) => config.entry(cmd),
        }
    }

    fn edit(self, cmd: Option<&VelaCommand>) -> String {
        match self {
            Self::Json(config) => config.edit(cmd),
            Self::Toml(config) => config.edit(cmd),
        }
    }
}

pub fn detect(id: HarnessId, env: &HarnessEnv, cmd: &VelaCommand) -> HarnessStatus {
    let config = env.config(id);
    if !config.detected {
        return HarnessStatus::NotDetected;
    }
    let path = config.path;
    let result = (|| {
        let (_target, snapshot) = open(&path)?.ok_or_else(|| changed(&path))?;
        let doc = document(id, &path, snapshot.as_ref(), config.ambiguous, cmd)?;
        Ok(HarnessStatus::Detected {
            config_path: path.clone(),
            config_exists: snapshot.is_some(),
            entry: doc.entry(cmd),
        })
    })();
    match result {
        Ok(status) => status,
        Err(HarnessError::Malformed { config_path, error }) => {
            HarnessStatus::Malformed { config_path, error }
        }
        Err(HarnessError::ManualRequired {
            config_path,
            reason,
            snippet,
        }) => HarnessStatus::ManualRequired {
            config_path,
            reason,
            snippet,
        },
        Err(error) => HarnessStatus::Unreadable {
            config_path: path,
            error: error.to_string(),
        },
    }
}

pub fn detect_all(env: &HarnessEnv, cmd: &VelaCommand) -> Vec<(HarnessId, HarnessStatus)> {
    HarnessId::ALL
        .into_iter()
        .map(|id| (id, detect(id, env, cmd)))
        .collect()
}

/// Fragmento para fusionar con la configuración existente. Usa el formato actual del agente.
pub fn snippet(id: HarnessId, cmd: &VelaCommand) -> String {
    make_snippet(id, false, cmd)
}

fn make_snippet(id: HarnessId, legacy: bool, cmd: &VelaCommand) -> String {
    match id {
        HarnessId::Codex => toml::snippet(cmd),
        _ => json::snippet(id, legacy, cmd),
    }
}

pub fn install(
    id: HarnessId,
    env: &HarnessEnv,
    cmd: &VelaCommand,
    opts: &InstallOptions,
) -> Result<ChangeReport, HarnessError> {
    change(id, env, cmd, opts, true, || {})
}

pub fn uninstall(
    id: HarnessId,
    env: &HarnessEnv,
    cmd: &VelaCommand,
    opts: &InstallOptions,
) -> Result<ChangeReport, HarnessError> {
    change(id, env, cmd, opts, false, || {})
}

fn change(
    id: HarnessId,
    env: &HarnessEnv,
    cmd: &VelaCommand,
    opts: &InstallOptions,
    installing: bool,
    mut before_publish: impl FnMut(),
) -> Result<ChangeReport, HarnessError> {
    for _ in 0..2 {
        let config = env.config(id);
        let path = config.path;
        let Some((target, snapshot)) = open(&path)? else {
            continue;
        };
        let doc = document(id, &path, snapshot.as_ref(), config.ambiguous, cmd)?;
        let entry = doc.entry(cmd);
        if matches!(entry, EntryState::Conflict { .. }) && !opts.force {
            return Err(HarnessError::Conflict { config_path: path });
        }
        let action = match (installing, entry) {
            (true, EntryState::Installed) => Action::AlreadyUpToDate,
            (true, EntryState::NotInstalled) => Action::Created,
            (true, _) => Action::Updated,
            (false, EntryState::NotInstalled) => Action::NothingToRemove,
            (false, _) => Action::Removed,
        };
        let mut report = ChangeReport {
            harness: id,
            config_path: path.clone(),
            action,
            diff: String::new(),
            backup: None,
        };
        if matches!(action, Action::AlreadyUpToDate | Action::NothingToRemove) {
            return Ok(report);
        }
        let updated = doc.edit(installing.then_some(cmd));
        if opts.dry_run {
            let original = snapshot.as_ref().map_or("", |s| {
                std::str::from_utf8(&s.bytes).expect("validated UTF-8")
            });
            report.diff = unified_diff(original, &updated);
            return Ok(report);
        }

        let result = fsx::write(
            &path,
            &target,
            snapshot.as_ref(),
            updated.as_bytes(),
            || {
                before_publish();
                let selected = env.config(id);
                !selected.ambiguous && selected.path == path
            },
        )
        .map_err(|source| io_error(&path, source))?;
        if let fsx::WriteResult::Written { backup } = result {
            report.backup = backup;
            return Ok(report);
        }
    }
    Err(changed(&env.config(id).path))
}

fn unified_diff(original: &str, updated: &str) -> String {
    let old: Vec<_> = original.split_inclusive('\n').collect();
    let new: Vec<_> = updated.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let old = &old[prefix..];
    let new = &new[prefix..];
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old = &old[..old.len() - suffix];
    let new = &new[..new.len() - suffix];
    let mut output = String::from("--- original\n+++ propuesta\n");
    let line = |count| prefix + usize::from(count != 0);
    writeln!(
        output,
        "@@ -{},{} +{},{} @@",
        line(old.len()),
        old.len(),
        line(new.len()),
        new.len()
    )
    .expect("writing to a String");
    for (marker, lines) in [('-', old), ('+', new)] {
        for content in lines {
            output.push(marker);
            output.push_str(content);
            if !content.ends_with('\n') {
                output.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    output
}

fn document<'a>(
    id: HarnessId,
    path: &Path,
    snapshot: Option<&'a fsx::Snapshot>,
    ambiguous: bool,
    cmd: &VelaCommand,
) -> Result<Document<'a>, HarnessError> {
    if ambiguous {
        let legacy = matches!(
            Document::parse(id, path, snapshot),
            Err(Problem::Manual(_, true))
        );
        return Err(Problem::Manual(
            "Coexisten opencode.json y opencode.jsonc. Revisa ambos archivos.".into(),
            legacy,
        )
        .at(id, path, cmd));
    }
    Document::parse(id, path, snapshot).map_err(|problem| problem.at(id, path, cmd))
}

fn open(path: &Path) -> Result<Option<(PathBuf, Option<fsx::Snapshot>)>, HarnessError> {
    let target = fsx::resolve(path).map_err(|source| io_error(path, source))?;
    match fsx::read(&target).map_err(|source| io_error(path, source))? {
        fsx::ReadResult::Stable(snapshot) => Ok(Some((target, snapshot))),
        fsx::ReadResult::Changed => Ok(None),
    }
}

fn io_error(path: &Path, source: io::Error) -> HarnessError {
    HarnessError::Io {
        config_path: path.to_owned(),
        source,
    }
}

fn changed(path: &Path) -> HarnessError {
    HarnessError::ConcurrentModification {
        config_path: path.to_owned(),
    }
}

#[cfg(test)]
mod tests;
