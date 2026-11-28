# MCP de Vela

Ejecuta `vela mcp` como proceso stdio de tu cliente MCP. El cliente envía una solicitud JSON-RPC por línea en stdin y recibe respuestas y notificaciones en stdout. Vela escribe los diagnósticos en stderr. El servidor negocia las versiones `2024-11-05` y `2025-03-26` y publica los esquemas de las 15 herramientas v1 en `tools/list`. [Especificación del transporte MCP stdio](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports).

```sh
vela mcp [--headless] [--socket <ruta>] [--config-dir <ruta>] [--help]
```

- `--config-dir` prevalece sobre `VELA_CONFIG_DIR`. Sin un directorio explícito, Vela carga `servers.toml`, `settings.toml` y `mcp.toml` desde su directorio habitual. Un directorio explícito también selecciona su fichero `known_hosts` para SSH.
- `--socket` prevalece sobre `VELA_MCP_SOCK`. La ruta predeterminada es `/tmp/vela-<uid>/mcp.sock`. Una ruta alternativa debe ser absoluta y tener un directorio padre privado, de tu usuario y con permisos `0700`.
- `--headless` desactiva el puente y el lanzamiento de la interfaz, incluso si `headless_ask = "open_vela"`.
- `--help` imprime el uso en stderr y termina.

## Connect Agents

Vela registra un servidor de usuario llamado `vela` en Linux y macOS. El comando usa la ruta absoluta y canónica del ejecutable que se está usando. En macOS es el binario dentro de `Vela.app/Contents/MacOS`.

```sh
vela mcp status
vela mcp status --json
vela mcp install --harness claude-code --harness codex --dry-run
vela mcp install --all
vela mcp uninstall --harness cursor
```

`status` enumera los cinco agentes y, cuando están detectados, su ruta y el estado de la entrada. `--json` devuelve una lista con los campos `harness`, `status` y, según el estado, `config_path`, `config_exists`, `entry`, `error` o `reason` y `snippet`. `install` y `uninstall` aceptan varios `--harness`. `--all` añade los agentes detectados por configuración, ejecutable en `PATH` o app de macOS en `/Applications` o `~/Applications`. Los resultados van a stdout y los diagnósticos a stderr. `vela mcp` sin subcomando sigue usando stdout solo para el protocolo.

En los estados `outdated` y `conflict`, `entry.found` describe el ejecutable anterior, el programa en conflicto o el host de una URL HTTP(S) con el formato `url: <host>`. Se muestra también debajo del agente en la salida de texto. Se limita a unos 200 caracteres y omite argumentos, variables de entorno, cabeceras, credenciales de URL, puerto, ruta, consulta y fragmento. Si falta el programa se muestra `(no command)`. Si el host no se puede extraer de forma segura se muestra `url: (no host)`.

Fuentes oficiales consultadas el **2026-11-26**. En los ejemplos, `<vela>` representa la ruta canónica del binario.

| ID | Configuración de usuario | Entrada |
| --- | --- | --- |
| `claude-code` | `~/.claude.json` o `$CLAUDE_CONFIG_DIR/.claude.json` | `mcpServers.vela` con `type = "stdio"`, `command = "<vela>"`, `args = ["mcp"]` y `env = {}`. [Anthropic MCP](https://code.claude.com/docs/en/mcp) · [Ruta con override](https://code.claude.com/docs/en/mcp-quickstart) |
| `codex` | `~/.codex/config.toml` o `$CODEX_HOME/config.toml` | Tabla `[mcp_servers.vela]` con `command = "<vela>"` y `args = ["mcp"]`. [Tipos MCP de Codex](https://github.com/openai/codex/blob/main/codex-rs/config/src/mcp_types.rs) · [Resolución de CODEX_HOME](https://github.com/openai/codex/blob/main/codex-rs/utils/home-dir/src/lib.rs) |
| `cursor` | `~/.cursor/mcp.json` | `mcpServers.vela` con `type = "stdio"`, `command = "<vela>"` y `args = ["mcp"]`. Vela reconoce `cursor`, `cursor-agent` y el alias oficial `agent` que resuelve a `cursor-agent`. [Cursor MCP](https://cursor.com/docs/mcp) · [Instalador oficial](https://cursor.com/install) |
| `opencode` | `~/.config/opencode/opencode.json` o `opencode.jsonc`. Si `XDG_CONFIG_HOME` es absoluto, se usa su subdirectorio `opencode/`. | En v2, `mcp.servers.vela` con `type = "local"` y `command = ["<vela>", "mcp"]`. Si ya hay servidores v1 directamente bajo `mcp` y no existe `mcp.servers`, mantiene esa estructura y añade `enabled = true`. [Configuración](https://opencode.ai/docs/config/) · [MCP v2](https://opencode.ai/docs/mcp-servers/) · [Migración v1](https://opencode.ai/docs/migrate-v1/) |
| `antigravity` | `~/.gemini/config/mcp_config.json` en App, IDE y CLI. Si solo existe el archivo legado `~/.gemini/antigravity/mcp_config.json` y no se detecta `agy`, Vela usa ese archivo. | `mcpServers.vela` con `command = "<vela>"` y `args = ["mcp"]`. [Google Antigravity MCP](https://antigravity.google/docs/mcp) · [CLI](https://antigravity.google/docs/install) |

- La instalación crea la entrada si falta. Si apunta a una ruta absoluta anterior cuyo ejecutable se llama `vela` y cuyo único argumento es `mcp`, cambia solo la ruta en `command` (`command[0]` en OpenCode). Conserva los argumentos, los ajustes personalizados y las subtablas de Codex. Otro comando con ese nombre requiere `--force`. En ese caso, la instalación sustituye la entrada completa y la desinstalación la elimina. Volver a instalar la misma entrada conserva el archivo y su fecha de modificación. La desinstalación conserva el archivo y deja vacío el contenedor que pierde su última entrada.
- Vela edita por posición en JSON y usa `toml_edit` 0.25.19 en Codex para conservar el orden y el formato existentes. Actualizar solo la ruta conserva todos los finales de línea. Al crear, sustituir o quitar una entrada TOML, conserva los finales uniformes y normaliza a LF si se mezclan LF y CRLF. Si el archivo falta, crea el mínimo necesario con permisos `0600` y directorios nuevos con `0700`. Si ya existe, conserva su modo y guarda el original en `<archivo>.vela.bak` con `0600`. Cada cambio reemplaza la copia anterior. Un enlace simbólico se conserva, y tanto la escritura como la copia se hacen junto a su destino.
- Vela edita los JSONC de OpenCode, incluidos los `.json` con comentarios o comas finales. Conserva los comentarios entre entradas y el texto de las cadenas, incluidas las URL con `//` o `/*`. Al añadir un miembro, mantiene la coma final si el contenedor ya la tenía. Desinstalar la entrada recién añadida recupera los bytes originales cuando el contenedor ya existía. Si la instalación tuvo que crear un contenedor, este se conserva al desinstalar. Los comentarios dentro de la entrada `vela` pueden perderse al quitarla o sustituirla con `--force`.
- Los archivos inválidos y las claves duplicadas en la ruta editada se conservan intactos. OpenCode requiere edición manual si coexisten `opencode.json` y `opencode.jsonc`, o si una entrada v1 `mcp.vela` comparte archivo con `mcp.servers`. OpenCode 2.4.2 carga ambos archivos y da mayor prioridad al JSONC. Las configuraciones MCP inline o punteadas de Codex también requieren edición manual. [Descubrimiento en OpenCode 2.4.2](https://github.com/anomalyco/opencode/blob/v2.4.2/packages/core/src/config/discovery.ts#L13) · [Selector de escritura en OpenCode 2.4.2](https://github.com/anomalyco/opencode/blob/v2.4.2/packages/core/src/config.ts#L510)
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

Si un servidor IPC responde al inicio, las llamadas se envían por el puente. Con `headless_ask = "open_vela"`, una aprobación pendiente o el acceso a un servidor `on_demand_ask` o `connected_only` pueden iniciar Vela. El runtime espera hasta cinco segundos a que aparezca el IPC. Una vez conectado, cierra sus sesiones SSH propias, reenvía la llamada para una nueva evaluación y mantiene el puente para las siguientes. Cuando MCP está habilitado, la ventana de Vela mantiene un broker IPC y usa sus sesiones SSH abiertas. Puede abrir una sesión bajo demanda tras una aprobación específica de conexión y volver a evaluar la operación con el servidor conectado.

## Integración del anfitrión

- `harness` expone `detect_all`, `install`, `uninstall` y `snippet` para integrar el registro en una interfaz. Las operaciones de disco son síncronas y reciben un `HarnessEnv` explícito y un `VelaCommand` canónico. El informe incluye la ruta seleccionada, la acción y, cuando corresponde, la copia o el diff de la simulación.
- `protocol::serve_stdio` ejecuta el servidor con cualquier `AsyncBufRead` y `AsyncWrite`. `ToolHandler` recibe una `ToolCall` tipada, el nombre y la versión declarados por el cliente, y un `ProgressReporter`.
- `ipc::IpcServer::bind` valida el directorio, verifica el UID de cada cliente, crea el socket con permisos `0600` y aplica un límite de 16 MiB por trama. `IpcServer::run` recibe el handler y un futuro de apagado. `IpcClient` mantiene una conexión multiplexada y envía `Cancel` al descartar una llamada.
- El anfitrión puede evaluar una `ToolCall` contra su `Executor` con `ApprovalMode::Normal` y usar `ApprovedOnce` tras la aprobación. El futuro de la llamada debe mantener la inscripción en la cola de aprobación. Cuando se recibe una cancelación o se cierra la conexión, ese futuro se descarta y la inscripción puede retirarse. Una operación ya enviada a SSH puede completarse.
- `headless::McpRuntime` selecciona y mantiene el modo. Recibe un `VelaLauncher` inyectable y expone `shutdown` para esperar la salida del worker SSH antes de limpiar el directorio de control.

## Interfaz

| Control | Modelo | Estado |
| --- | --- | --- |
| Servidor MCP local y dirección IPC/stdio | `enabled` y `ipc::socket_path` | Existe |
| Harnesses, detección y acciones | `harness::detect_all`, `install`, `uninstall`, `dry_run`, `force` | Existe |
| Estado, ruta, ejecutable y copia de cada harness | `HarnessStatus`, `EntryState`, `ChangeReport`, `VelaCommand` | Existe |
| JSON manual y snippets por cliente | `harness::snippet` y comando canónico | Existe |
| Ruta personalizada del harness | Sin soporte en M4 | Pendiente |
| Con Vela cerrada | `headless_ask` | Existe |
| Ocultar dirección y usuario globalmente | `hide_server_identity` | Existe |
| Compartir carpeta abierta | `share_ui_context`, herramienta `get_ui_context` | Disponible con el puente gráfico |
| Espera máxima | `approval_timeout_secs` | Disponible con el puente gráfico |
| Perfiles de fábrica y propios | `presets` y `profiles` | Existe |
| Duplicación, resumen y protección de perfiles en uso | Operaciones del editor sobre `profiles` y `servers` | Añadido |
| Selector y simulador | `PolicyEngine` | Existe |
| Respaldo automático, tamaño, cuota y caducidad | `rollback` | Existe |
| Ocupación, carpeta y vaciado | `RollbackStore::usage`, `dir`, `clear` | Existe |
| Disponibilidad | `ServerAvailability`; autenticación de `servers.toml` | Existe |
| Perfil, alias del servidor y notas | `profile_id`, `alias`, `notes` | Existe |
| Heredar/mostrar/ocultar identidad | `hide_server_identity: Option<bool>` | Existe |
| Patrones remotos y locales | `allowed_remote_paths`, `denied_remote_paths`, `allowed_local_paths` y glob de M1 | Existe |
| SSH exec, elevación y patrones de comandos | `exec_mode`, `sudo`, `allowed_commands`, `denied_commands` | Existe |
| Validación de entradas, guardado con comentarios y recarga | Editor de `mcp.toml` | Añadido |


En **Settings → MCP**, **Local MCP Server** activa las llamadas de herramientas y **Connected Agents** resume qué agentes tienen configurada la entrada de Vela. Con MCP apagado, `vela mcp` devuelve una denegación con la regla `mcp_disabled`, incluida `list_servers`. La negociación de stdio sigue disponible y se pueden editar los demás ajustes.

En **Behavior & Privacy**, **When Vela Is Closed** permite denegar una solicitud pendiente de aprobación o intentar abrir Vela. El modo headless deniega la operación si el puente no aparece. **Approval Timeout** permite elegir 1, 3 o 5 minutos y el valor inicial es 3. **Hide Server Addresses** limita la identidad que se comparte al ID, el alias y las notas. **Share Active Folder** controla si `get_ui_context` comparte el servidor y la carpeta remota activos. Los servidores desactivados quedan excluidos. Los interruptores y selectores se desactivan durante la carga, el guardado o un conflicto de configuración hasta que se pueda recargar.

**Connect Agents…** enumera los cinco clientes. **Configured** permite quitar la entrada, **Detected** añadirla y **Update Available** actualizar el ejecutable. **Conflict** exige confirmar el reemplazo. **Not Found**, **Invalid File** y **Unreadable** informan del estado. **Manual Setup** muestra el motivo y el fragmento que se puede copiar. Al marcar una fila aparece la operación pendiente junto al estado detectado. **View Changes** muestra un resumen y un diff en seco con colores para las líneas añadidas y quitadas. **Apply** ejecuta las operaciones marcadas en segundo plano, mantiene la lista visible con los controles desactivados y vuelve a detectar antes de mostrar el resultado y la ruta de la copia `.vela.bak` cuando existe. Si no se detecta ningún cliente, el instalador indica dónde encontrar el JSON de configuración manual. **Cancel**, Escape y el fondo cierran el instalador sin aplicar los cambios marcados.

**Permission Profiles** enumera los perfiles **Read-only**, **Diagnostics**, **Development** y **Production Protected**, además de los perfiles guardados. **New Profile…** permite crear un perfil a partir de uno integrado. También se puede usar **Duplicate…** al abrir un perfil integrado con **View**. Las escrituras requieren aprobación en **Read-only**, el borrado se deniega y los comandos SSH están desactivados. Un perfil asignado a un servidor no se puede borrar.

Abre **Test a Rule** al final de la tarjeta para evaluar la misma política con el servidor conectado y la interfaz disponible como condiciones simuladas. Si eliges un servidor, la simulación también evalúa sus reglas. Usa las rutas de entrada y omite la resolución de enlaces remotos y las concesiones temporales.

**MCP Access…** abre la hoja de un servidor desde el menú de su fila o la cabecera. La disponibilidad inicial es **Off**. **Unattended** requiere autenticación con clave mediante agente SSH y excluye la elevación desatendida. En **Permissions**, elige un perfil y el **Folder Access**. **Entire Server** guarda el patrón `/**`. **Selected Folders** abre el editor de rutas permitidas. **Ask** deja vacía la lista de rutas del servidor. Si el perfil aporta rutas, siguen aplicándose y la hoja las muestra. Si ninguna lista aporta rutas, las operaciones fuera de ámbito requieren aprobación cuando la política permite solicitarla. **Advanced Settings** muestra los controles de SSH exec y sudo, las prohibiciones personalizadas, las rutas locales, la privacidad, las notas y las reglas guardadas. La lista de comandos aparece cuando el SSH exec efectivo está habilitado. En esta hoja y en el editor de perfil, pulsa **Add** o **Enter** en el campo para incorporar un patrón pendiente antes de guardar. Si falta un perfil válido o la disponibilidad no es compatible con la autenticación, el pie explica por qué no se puede guardar. Las notas se exponen en `list_servers` y el esquema no fija una longitud máxima. Las prohibiciones siempre prevalecen sobre los ámbitos permitidos. Los patrones de rutas remotas son absolutos o empiezan por `**/`. Las rutas locales también admiten `~/`. El directorio de configuración de Vela, incluido `mcp.toml`, está protegido incluso con un ámbito local amplio.

**Advanced & Storage** muestra el estado del respaldo y el espacio ocupado. Al abrirlo, **Rollback Backups** permite activar el respaldo, revisar el uso y ajustar el tamaño por archivo, la cuota y la caducidad en **Limits…**. **Open Folder** abre `mcp-rollback` y **Clear…** elimina las copias tras confirmación. **Local Socket** muestra la ruta del socket Unix y el transporte stdio.

La configuración sigue en el formato v1. Los archivos antiguos conservan sus valores por defecto. Los ajustes globales guardan cada opción al cambiarla. Las hojas y editores guardan al pulsar **Save**. Vela edita el TOML con `toml_edit`, conserva el orden y los comentarios de las entradas editadas y valida el documento antes de publicarlo por renombrado atómico desde un temporal con permisos `0600`. Si el archivo cambió desde la lectura, la ventana informa del conflicto y ofrece **Reload**.

El proceso `vela mcp` en marcha relee `mcp.toml` al comienzo de cada llamada de herramienta en modo headless. Un cambio publicado rige desde la siguiente llamada, sin reiniciar ni esperar un intervalo de sondeo. La relectura falla de forma cerrada ante un archivo inválido. En modo puente, el cliente también comprueba el interruptor global antes de reenviar. Los guardados en **Settings → MCP** y **MCP Access…** actualizan al broker en vivo. Desactivar MCP o cerrar Vela cierra el listener y cancela las solicitudes en espera.

### Aprobaciones

Al pasar el cursor sobre la operación, el panel muestra la ruta o el comando completo. En traslados y transferencias, **From** y **To** identifican el origen y el destino. Las transferencias también indican qué ruta es local y cuál corresponde al servidor. Los caracteres de control y las marcas de dirección aparecen como secuencias visibles, por ejemplo `\u{202e}`. El texto completo también está disponible para lectores de pantalla.

El panel de aprobación aparece en la esquina inferior derecha sin cambiar el foco. Muestra la operación, el destino, el motivo del agente, los avisos, la vista previa y el tiempo restante. Cuando Vela está en segundo plano, el aviso nativo incluye solo el nombre del agente, la operación y el servidor. Los clientes que solicitan progreso reciben actualizaciones mientras esperan. Al cancelar la llamada, desconectarse o vencer el plazo, la aprobación se retira y se registra. El panel conserva la solicitud visible y su borrador aunque se retire otra solicitud de la cola. Si se retira la solicitud visible, el panel pasa a la primera disponible y descarta el borrador.

- **Allow Once** vuelve a evaluar la solicitud y aplica la decisión a esa llamada. Si el destino, el diff, el comando o el riesgo cambian, Vela vuelve a solicitar aprobación.
- **Allow for 15 Min** además concede una excepción temporal para la misma operación y el mismo destino.
- **Save Rule** guarda la excepción tras completar la operación. Se puede quitar desde **MCP Access… → Advanced Settings → Saved Approval Rules**. La regla se limita a la operación y el destino canónico, incluido el directorio de trabajo de un comando si se especificó. También se aplica con Vela cerrada en servidores con disponibilidad **Unattended**, antes de decidir si queda alguna aprobación pendiente. Los bloqueos de política y de Ediciones prevalecen.
- **Deny** admite un motivo opcional que se devuelve al agente. Escape cierra el campo, recupera el foco anterior y deja la solicitud pendiente.

### Actividad, JIT y rollback

**Agents** abre el registro de las últimas 200 operaciones en memoria y muestra quién está conectado. El botón de la cabecera muestra el contador de aprobaciones pendientes y el indicador JIT cuando corresponden. Los filtros limitan la lista por estado o por disponibilidad de rollback. **Restore** restaura una copia vigente en la sesión activa del servidor y actualiza la vista remota. Una operación bloqueada por cambios locales pendientes en Ediciones, incluida una subida con `transfer_upload`, queda registrada y muestra un aviso en el workspace.

**Temporary Access** eleva el servidor activo a **Development** durante 15 minutos, 30 minutos o una hora. El perfil base vuelve al vencer el plazo o al pulsar **Revoke Now**. **Disconnect Agents** cancela las aprobaciones pendientes, desconecta los clientes IPC actuales y revoca las concesiones temporales. Los clientes pueden establecer una nueva conexión con el broker mientras MCP siga habilitado. Las operaciones enviadas a SSH antes del corte pueden haber producido cambios.
