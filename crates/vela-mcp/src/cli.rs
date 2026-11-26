//! Entrada CLI, llamada por `vela mcp` antes de inicializar GPUI.

use std::{ffi::OsString, io, path::PathBuf};

use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};

use crate::{
    harness::{
        self, Action, EntryState, HarnessEnv, HarnessError, HarnessId, HarnessStatus,
        InstallOptions, VelaCommand,
    },
    headless::{McpRuntime, ProcessLauncher, RuntimeOptions, VelaLauncher},
    ipc::socket_path,
    protocol::serve_stdio,
};

const HELP: &str = "Uso: vela mcp [--headless] [--socket <ruta>] [--config-dir <ruta>] [--help]
     vela mcp status [--json]
     vela mcp install|uninstall [--harness <id>]... [--all] [--dry-run] [--force]

IDs: claude-code, codex, cursor, opencode, antigravity";

fn options(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<RuntimeOptions>, &'static str> {
    let mut args = args.into_iter();
    let mut options = RuntimeOptions::default();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--headless") => options.force_headless = true,
            Some("--socket") => options.socket_path = socket_path(Some(path_argument(&mut args)?)),
            Some("--config-dir") => options.config_dir = Some(path_argument(&mut args)?),
            _ => return Err("Opción no válida."),
        }
    }
    Ok(Some(options))
}

fn path_argument(args: &mut impl Iterator<Item = OsString>) -> Result<PathBuf, &'static str> {
    args.next()
        .filter(|arg| !arg.is_empty() && !arg.as_encoded_bytes().starts_with(b"--"))
        .map(PathBuf::from)
        .ok_or("Falta una ruta para la opción.")
}

/// Ejecuta el mismo servicio que el binario sobre un reader y writer inyectados.
pub async fn run<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
    reader: R,
    writer: W,
    options: RuntimeOptions,
    executable: PathBuf,
    launcher: impl VelaLauncher,
) -> io::Result<()> {
    let runtime = McpRuntime::new(options, executable, launcher);
    let result = serve_stdio(reader, writer, runtime.clone()).await;
    runtime.shutdown().await;
    result
}

pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    let mut args = args.into_iter().peekable();
    match args.peek().and_then(|arg| arg.to_str()) {
        Some("status") => {
            args.next();
            return status(args);
        }
        Some("install" | "uninstall") => {
            let installing = args.next().is_some_and(|arg| arg == "install");
            return change(args, installing);
        }
        _ => {}
    }
    let options = match options(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            eprintln!("{HELP}");
            return 0;
        }
        Err(error) => {
            eprintln!("{error}\n{HELP}");
            return 2;
        }
    };
    let result = (|| {
        let executable = std::env::current_exe()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(run(
            BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
            options,
            executable,
            ProcessLauncher,
        ));
        // Tokio stdin uses a blocking read. A failed output pipe must not wait
        // for the client to close stdin after the service has shut down.
        runtime.shutdown_background();
        result
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Vela MCP: {error}");
            1
        }
    }
}

fn harness_context() -> Option<(HarnessEnv, VelaCommand)> {
    match HarnessEnv::from_process().and_then(|env| Ok((env, VelaCommand::current()?))) {
        Ok(context) => Some(context),
        Err(error) => {
            eprintln!("Vela MCP: {error}");
            None
        }
    }
}

fn usage_error(message: &str) -> i32 {
    eprintln!("{message}\n{HELP}");
    2
}

fn status(mut args: impl Iterator<Item = OsString>) -> i32 {
    let json = match args.next().as_deref().and_then(|arg| arg.to_str()) {
        Some("--json") if args.next().is_none() => true,
        Some("--help" | "-h") => {
            println!("{HELP}");
            return 0;
        }
        None => false,
        _ => return usage_error("Opción no válida para status."),
    };
    let Some((env, cmd)) = harness_context() else {
        return 1;
    };
    let statuses = harness::detect_all(&env, &cmd);
    if json {
        #[derive(serde::Serialize)]
        struct Row {
            harness: HarnessId,
            #[serde(flatten)]
            status: HarnessStatus,
        }
        let rows: Vec<_> = statuses
            .into_iter()
            .map(|(harness, status)| Row { harness, status })
            .collect();
        match serde_json::to_string(&rows) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("Vela MCP: {error}");
                return 1;
            }
        }
    } else {
        println!("{:<16} {:<21} Archivo", "Agente", "Estado");
        for (id, status) in statuses {
            let (label, path) = match &status {
                HarnessStatus::NotDetected => ("No detectado", None),
                HarnessStatus::Detected {
                    entry, config_path, ..
                } => (
                    match entry {
                        EntryState::NotInstalled => "Sin instalar",
                        EntryState::Installed => "Instalado",
                        EntryState::Outdated { .. } => "Ruta anterior",
                        EntryState::Conflict { .. } => "Conflicto",
                    },
                    Some(config_path),
                ),
                HarnessStatus::Malformed { config_path, .. } => {
                    ("Archivo inválido", Some(config_path))
                }
                HarnessStatus::ManualRequired { config_path, .. } => {
                    ("Edición manual", Some(config_path))
                }
                HarnessStatus::Unreadable { config_path, .. } => {
                    ("Error de lectura", Some(config_path))
                }
            };
            println!(
                "{:<16} {label:<21} {}",
                id.display_name(),
                path.map_or_else(String::new, |path| path.display().to_string())
            );
        }
    }
    0
}

fn change(mut args: impl Iterator<Item = OsString>, installing: bool) -> i32 {
    let mut ids = Vec::new();
    let mut all = false;
    let mut opts = InstallOptions::default();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!("{HELP}");
                return 0;
            }
            Some("--all") => all = true,
            Some("--dry-run") => opts.dry_run = true,
            Some("--force") => opts.force = true,
            Some("--harness") => {
                let Some(id) = args
                    .next()
                    .and_then(|arg| HarnessId::ALL.into_iter().find(|id| arg == id.id()))
                else {
                    return usage_error("Indica un ID válido tras --harness.");
                };
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            _ => return usage_error("Opción no válida para install o uninstall."),
        }
    }
    if ids.is_empty() && !all {
        return usage_error("Indica --harness o --all.");
    }
    let Some((env, cmd)) = harness_context() else {
        return 1;
    };
    if all {
        for (id, status) in harness::detect_all(&env, &cmd) {
            if !matches!(status, HarnessStatus::NotDetected) && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }

    let mut exit = 0;
    for id in ids {
        let result = if installing {
            harness::install(id, &env, &cmd, &opts)
        } else {
            harness::uninstall(id, &env, &cmd, &opts)
        };
        match result {
            Ok(report) => {
                let action = match report.action {
                    Action::Created => "Alta",
                    Action::Updated => "Actualización",
                    Action::AlreadyUpToDate => "Ya instalado",
                    Action::Removed => "Eliminación",
                    Action::NothingToRemove => "Sin entrada que eliminar",
                };
                let prefix = if opts.dry_run { "[simulación] " } else { "" };
                println!(
                    "{prefix}{}: {action} · {}",
                    id.display_name(),
                    report.config_path.display()
                );
                if let Some(backup) = report.backup {
                    println!("  Copia: {}", backup.display());
                }
                if !report.diff.is_empty() {
                    print!("{}", report.diff);
                }
            }
            Err(error) => {
                eprintln!("{}: {error}", id.display_name());
                let code = match error {
                    HarnessError::ManualRequired { snippet, .. } => {
                        if installing {
                            eprint!("{snippet}");
                        } else {
                            eprintln!("Elimina solo la entrada vela.");
                        }
                        3
                    }
                    HarnessError::Conflict { .. } => 3,
                    _ => 1,
                };
                exit = exit.max(code);
            }
        }
    }
    exit
}
