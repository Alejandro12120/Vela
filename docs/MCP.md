# MCP de Vela

Ejecuta `vela mcp` como proceso stdio de tu cliente MCP. El cliente envía una solicitud JSON-RPC por línea en stdin y recibe respuestas y notificaciones en stdout. Vela escribe los diagnósticos en stderr. El servidor negocia las versiones `2024-11-05` y `2025-03-26` y publica los esquemas de las 15 herramientas v1 en `tools/list`. [Especificación del transporte MCP stdio](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports).

```sh
vela mcp [--headless] [--socket <ruta>] [--config-dir <ruta>] [--help]
```

- `--config-dir` prevalece sobre `VELA_CONFIG_DIR`. Sin un directorio explícito, Vela carga `servers.toml`, `settings.toml` y `mcp.toml` desde su directorio habitual. Un directorio explícito también selecciona su fichero `known_hosts` para SSH.
- `--socket` prevalece sobre `VELA_MCP_SOCK`. La ruta predeterminada es `/tmp/vela-<uid>/mcp.sock`. Una ruta alternativa debe ser absoluta y tener un directorio padre privado, de tu usuario y con permisos `0700`.
- `--headless` desactiva el puente y el lanzamiento de la interfaz, incluso si `headless_ask = "open_vela"`.
- `--help` imprime el uso en stderr y termina.

## Conectar agentes (harnesses)

Vela registra un servidor de usuario llamado `vela` en Linux y macOS. El comando usa la ruta absoluta y canónica del ejecutable que se está usando. En macOS es el binario dentro de `Vela.app/Contents/MacOS`.

```sh
vela mcp status
vela mcp status --json
vela mcp install --harness claude-code --harness codex --dry-run
vela mcp install --all
vela mcp uninstall --harness cursor
```

`status` enumera los cinco agentes y, cuando están detectados, su ruta y el estado de la entrada. `--json` devuelve una lista con los campos `harness`, `status` y, según el estado, `config_path`, `config_exists`, `entry`, `error` o `reason` y `snippet`. `install` y `uninstall` aceptan varios `--harness`. `--all` añade los agentes detectados por configuración, ejecutable en `PATH` o app de macOS en `/Applications` o `~/Applications`. Los resultados van a stdout y los diagnósticos a stderr. `vela mcp` sin subcomando sigue usando stdout solo para el protocolo.

Fuentes oficiales consultadas el **2026-11-26**. En los ejemplos, `<vela>` representa la ruta canónica del binario.

| ID | Configuración de usuario | Entrada |
| --- | --- | --- |
| `claude-code` | `~/.claude.json` o `$CLAUDE_CONFIG_DIR/.claude.json` | `mcpServers.vela` con `type = "stdio"`, `command = "<vela>"`, `args = ["mcp"]` y `env = {}`. [Anthropic MCP](https://code.claude.com/docs/en/mcp) · [Ruta con override](https://code.claude.com/docs/en/mcp-quickstart) |
| `codex` | `~/.codex/config.toml` o `$CODEX_HOME/config.toml` | Tabla `[mcp_servers.vela]` con `command = "<vela>"` y `args = ["mcp"]`. [Tipos MCP de Codex](https://github.com/openai/codex/blob/main/codex-rs/config/src/mcp_types.rs) · [Resolución de CODEX_HOME](https://github.com/openai/codex/blob/main/codex-rs/utils/home-dir/src/lib.rs) |
| `cursor` | `~/.cursor/mcp.json` | `mcpServers.vela` con `type = "stdio"`, `command = "<vela>"` y `args = ["mcp"]`. Vela reconoce `cursor`, `cursor-agent` y el alias oficial `agent` que resuelve a `cursor-agent`. [Cursor MCP](https://cursor.com/docs/mcp) · [Instalador oficial](https://cursor.com/install) |
| `opencode` | `~/.config/opencode/opencode.json` o `opencode.jsonc`. Si `XDG_CONFIG_HOME` es absoluto, se usa su subdirectorio `opencode/`. | En v2, `mcp.servers.vela` con `type = "local"` y `command = ["<vela>", "mcp"]`. Si ya hay servidores v1 directamente bajo `mcp` y no existe `mcp.servers`, mantiene esa estructura y añade `enabled = true`. [Configuración](https://opencode.ai/docs/config/) · [MCP v2](https://opencode.ai/docs/mcp-servers/) · [Migración v1](https://opencode.ai/docs/migrate-v1/) |
| `antigravity` | `~/.gemini/config/mcp_config.json` en App, IDE y CLI. Si solo existe el archivo legado `~/.gemini/antigravity/mcp_config.json` y no se detecta `agy`, Vela usa ese archivo. | `mcpServers.vela` con `command = "<vela>"` y `args = ["mcp"]`. [Google Antigravity MCP](https://antigravity.google/docs/mcp) · [CLI](https://antigravity.google/docs/install) |

- La instalación crea la entrada si falta y actualiza una ruta absoluta anterior cuyo ejecutable se llama `vela` y cuyo único argumento es `mcp`. Otro comando con ese nombre requiere `--force` para reemplazarlo o eliminarlo. Volver a instalar la misma entrada conserva el archivo y su fecha de modificación. La desinstalación conserva el archivo y deja vacío el contenedor que pierde su última entrada.
- Vela edita por posición en JSON y usa `toml_edit` 0.25.19 en Codex para conservar el orden y el formato existentes. En TOML conserva finales de línea uniformes. Si se mezclan LF y CRLF, el editor los normaliza a LF. Si el archivo falta, crea el mínimo necesario con permisos `0600` y directorios nuevos con `0700`. Si ya existe, conserva su modo y guarda el original en `<archivo>.vela.bak` con `0600`. Cada cambio reemplaza la copia anterior. Un enlace simbólico se conserva, y tanto la escritura como la copia se hacen junto a su destino.
- Los archivos inválidos se conservan intactos. Los JSONC de OpenCode, incluidos los `.json` que contienen comentarios o comas finales, y las configuraciones MCP inline o punteadas de Codex requieren edición manual. Si coexisten `opencode.json` y `opencode.jsonc`, Vela señala el JSONC y pide revisar ambos archivos. Esto evita modificar una entrada que el otro archivo podría complementar o reemplazar. OpenCode 2.4.2 carga ambos archivos y da mayor prioridad al JSONC. [Descubrimiento en OpenCode 2.4.2](https://github.com/anomalyco/opencode/blob/v2.4.2/packages/core/src/config/discovery.ts#L13) · [Selector de escritura en OpenCode 2.4.2](https://github.com/anomalyco/opencode/blob/v2.4.2/packages/core/src/config.ts#L510)
- Vela sincroniza los archivos temporales en el mismo directorio y publica el cambio atómicamente. Serializa sus propias escrituras y compara tamaño, fecha de modificación y SHA-256 justo antes de publicar. Ante un cambio externo detectado, vuelve a leer, fusionar e intentar una vez. Una segunda modificación aborta la operación. Los procesos externos pueden escribir después de la última comprobación.
- `--dry-run` imprime el diff unificado sin crear archivos, directorios ni copias. En una configuración compacta, una línea modificada del diff puede incluir otros valores que estaban en la misma línea. Las pruebas con una compilación `--features dev` pueden fijar `VELA_MCP_HARNESS_HOME` a un directorio absoluto temporal. El override aísla las rutas de configuración y desactiva la detección por `PATH` o apps.

### Códigos de salida

- `0` indica éxito. `status` también lo usa cuando informa de un problema en un archivo.
- `1` indica un error operativo, un archivo inválido o dos cambios concurrentes detectados.
- `2` indica uso incorrecto, incluida la falta de `--harness` o `--all` en una operación.
- `3` indica un conflicto o que hace falta edición manual. Las operaciones sobre varios agentes continúan con los demás y devuelven el mayor código de los agentes procesados.

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

- `harness` expone `detect_all`, `install`, `uninstall` y `snippet` para integrar el registro en una interfaz. Las operaciones de disco son síncronas y reciben un `HarnessEnv` explícito y un `VelaCommand` canónico. El informe incluye la ruta seleccionada, la acción y, cuando corresponde, la copia o el diff de la simulación.
- `protocol::serve_stdio` ejecuta el servidor con cualquier `AsyncBufRead` y `AsyncWrite`. `ToolHandler` recibe una `ToolCall` tipada, el nombre y la versión declarados por el cliente, y un `ProgressReporter`.
- `ipc::IpcServer::bind` valida el directorio, verifica el UID de cada cliente, crea el socket con permisos `0600` y aplica un límite de 16 MiB por trama. `IpcServer::run` recibe el handler y un futuro de apagado. `IpcClient` mantiene una conexión multiplexada y envía `Cancel` al descartar una llamada.
- El anfitrión puede evaluar una `ToolCall` contra su `Executor` con `ApprovalMode::Normal` y usar `ApprovedOnce` tras la aprobación. El futuro de la llamada debe mantener la inscripción en la cola de aprobación. Cuando se recibe una cancelación o se cierra la conexión, ese futuro se descarta y la inscripción puede retirarse. Una operación ya enviada a SSH puede completarse.
- `headless::McpRuntime` selecciona y mantiene el modo. Recibe un `VelaLauncher` inyectable y expone `shutdown` para esperar la salida del worker SSH antes de limpiar el directorio de control.
