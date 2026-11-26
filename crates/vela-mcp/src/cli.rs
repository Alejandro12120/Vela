//! Entrada CLI, llamada por `vela mcp` antes de inicializar GPUI.

use std::{ffi::OsString, io, path::PathBuf};

use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};

use crate::{
    headless::{McpRuntime, ProcessLauncher, RuntimeOptions, VelaLauncher},
    ipc::socket_path,
    protocol::serve_stdio,
};

const HELP: &str = "Uso: vela mcp [--headless] [--socket <ruta>] [--config-dir <ruta>] [--help]";

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
