# MCP de Vela

Ejecuta `vela mcp` como proceso stdio de tu cliente MCP. El cliente envía una solicitud JSON-RPC por línea en stdin y recibe respuestas y notificaciones en stdout. Vela escribe los diagnósticos en stderr. El servidor negocia las versiones `2024-11-05` y `2025-03-26` y publica los esquemas de las 15 herramientas v1 en `tools/list`. [Especificación del transporte MCP stdio](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports).

```sh
vela mcp [--headless] [--socket <ruta>] [--config-dir <ruta>] [--help]
```

- `--config-dir` prevalece sobre `VELA_CONFIG_DIR`. Sin un directorio explícito, Vela carga `servers.toml`, `settings.toml` y `mcp.toml` desde su directorio habitual. Un directorio explícito también selecciona su fichero `known_hosts` para SSH.
- `--socket` prevalece sobre `VELA_MCP_SOCK`. La ruta predeterminada es `/tmp/vela-<uid>/mcp.sock`. Una ruta alternativa debe ser absoluta y tener un directorio padre privado, de tu usuario y con permisos `0700`.
- `--headless` desactiva el puente y el lanzamiento de la interfaz, incluso si `headless_ask = "open_vela"`.
- `--help` imprime el uso en stderr y termina.

## Política y conexiones

El acceso MCP está desactivado de fábrica. Para usar el modo headless, habilítalo en `mcp.toml` y configura el ID de un servidor de `servers.toml` con `availability = "headless"`, un perfil y el ámbito remoto que quieras compartir. Este ejemplo comparte `/srv/proyecto/` con el perfil de solo lectura.

```toml
enabled = true
headless_ask = "deny"

[servers."id-del-servidor"]
availability = "headless"
profile_id = "readonly"
allowed_remote_paths = ["/srv/proyecto/**"]
```

El runtime solo abre sesiones propias para servidores `headless` que usan autenticación por agente. El socket del agente se toma del servidor, del ajuste global en `settings.toml` o del primer socket de Bitwarden detectado, en ese orden. OpenSSH conserva su resolución habitual si ninguno está configurado. El modo headless rechaza los prompts de SSH, incluida la confirmación de una clave de host desconocida. Los sockets de control viven en directorios privados por conexión, bajo un directorio temporal del proceso que se elimina al cerrar stdin. Las copias de rollback se guardan en `mcp-rollback/` dentro de la configuración.

Si un servidor IPC responde al inicio, las llamadas se envían por el puente. Con `headless_ask = "open_vela"`, una aprobación pendiente o el acceso a un servidor `on_demand_ask` o `connected_only` pueden iniciar Vela. El runtime espera hasta cinco segundos a que aparezca el IPC. Una vez conectado, cierra sus sesiones SSH propias, reenvía la llamada para una nueva evaluación y mantiene el puente para las siguientes. La fase M3 proporciona el servidor IPC. Las fases M5/M6 lo conectarán con la interfaz y su cola de aprobaciones.

## Integración del anfitrión

- `protocol::serve_stdio` ejecuta el servidor con cualquier `AsyncBufRead` y `AsyncWrite`. `ToolHandler` recibe una `ToolCall` tipada, el nombre y la versión declarados por el cliente, y un `ProgressReporter`.
- `ipc::IpcServer::bind` valida el directorio, verifica el UID de cada cliente, crea el socket con permisos `0600` y aplica un límite de 16 MiB por trama. `IpcServer::run` recibe el handler y un futuro de apagado. `IpcClient` mantiene una conexión multiplexada y envía `Cancel` al descartar una llamada.
- El anfitrión puede evaluar una `ToolCall` contra su `Executor` con `ApprovalMode::Normal` y usar `ApprovedOnce` tras la aprobación. El futuro de la llamada debe mantener la inscripción en la cola de aprobación. Cuando se recibe una cancelación o se cierra la conexión, ese futuro se descarta y la inscripción puede retirarse. Una operación ya enviada a SSH puede completarse.
- `headless::McpRuntime` selecciona y mantiene el modo. Recibe un `VelaLauncher` inyectable y expone `shutdown` para esperar la salida del worker SSH antes de limpiar el directorio de control.
