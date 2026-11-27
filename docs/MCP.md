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

En los estados `outdated` y `conflict`, `entry.found` describe el ejecutable anterior, el programa en conflicto o el host de una URL HTTP(S) con el formato `url: <host>`. Se muestra también debajo del agente en la salida de texto. Se limita a unos 200 caracteres y omite argumentos, variables de entorno, cabeceras, credenciales de URL, puerto, ruta, consulta y fragmento. Si falta el programa se muestra `(sin comando)`. Si el host no se puede extraer de forma segura se muestra `url: (sin host)`.

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

Si un servidor IPC responde al inicio, las llamadas se envían por el puente. Con `headless_ask = "open_vela"`, una aprobación pendiente o el acceso a un servidor `on_demand_ask` o `connected_only` pueden iniciar Vela. El runtime espera hasta cinco segundos a que aparezca el IPC. Una vez conectado, cierra sus sesiones SSH propias, reenvía la llamada para una nueva evaluación y mantiene el puente para las siguientes. La fase M3 proporciona el servidor IPC. M6 conectará el servidor IPC a la ventana y a la cola de aprobaciones.

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
| Compartir carpeta abierta | `share_ui_context`, herramienta `get_ui_context` | Existe; la carpeta de la ventana requiere el puente gráfico de M6 |
| Espera máxima | `approval_timeout_secs` | Existe; la espera de aprobación en la ventana corresponde a M6 |
| Perfiles de fábrica y propios | `presets` y `profiles` | Existe |
| Duplicación, resumen y protección de perfiles en uso | Operaciones del editor sobre `profiles` y `servers` | Añadido |
| Selector y simulador | `PolicyEngine` | Existe |
| Respaldo automático, tamaño, cuota y caducidad | `rollback` | Existe |
| Ocupación, carpeta y vaciado | `RollbackStore::usage`, `dir`, `clear` | Existe |
| Disponibilidad | `ServerAvailability`; autenticación de `servers.toml` | Existe |
| Perfil, alias de cabecera y notas | `profile_id`, `alias`, `notes` | Existe |
| Heredar/mostrar/ocultar identidad | `hide_server_identity: Option<bool>` | Existe |
| Patrones remotos y locales | `allowed_remote_paths`, `denied_remote_paths`, `allowed_local_paths` y glob de M1 | Existe |
| SSH exec, elevación y patrones de comandos | `exec_mode`, `sudo`, `allowed_commands`, `denied_commands` | Existe |
| Validación de entradas, guardado con comentarios y recarga | Editor de `mcp.toml` | Añadido |


En **Ajustes → Agentes (MCP)**, el interruptor local aplica a las llamadas de herramientas. Con MCP apagado, `vela mcp` devuelve una denegación con la regla `mcp_disabled`, incluida `list_servers`. La negociación de stdio sigue disponible. Los demás ajustes se pueden editar con el interruptor apagado. **Con Vela cerrada** elige entre denegar una acción pendiente de aprobación e intentar abrir Vela. El runtime headless falla de forma segura si el puente no aparece. La espera que configura la sección (1, 3 o 5 minutos) corresponde a la aprobación en la ventana, que se incorpora en M6. **Compartir carpeta abierta** controla si `get_ui_context` puede compartir el servidor y la carpeta remota activos a través del puente de M6. Los interruptores y selectores de Ajustes se desactivan mientras se carga o guarda la configuración, o cuando hace falta recargarla tras un conflicto.

**Conectar agentes…** enumera los cinco clientes. Configurado permite quitar la entrada, Detectado añadirla y Desactualizado actualizar el ejecutable. Ocupado exige confirmar el reemplazo. No encontrado, Archivo inválido y Sin acceso informan del estado. Manual muestra el motivo y el fragmento que se puede copiar. Al marcar una fila aparece la operación pendiente junto al estado detectado. **Ver cambios** muestra un resumen y un diff en seco con colores para las líneas añadidas y quitadas. **Aplicar** ejecuta las operaciones marcadas en segundo plano, mantiene la lista visible con los controles desactivados y vuelve a detectar antes de mostrar el resultado y la ruta de la copia `.vela.bak` cuando existe. Si no se detecta ningún cliente, el instalador indica dónde encontrar el JSON de configuración manual. Cancelar, Escape y el fondo cierran el instalador sin aplicar los cambios marcados.

**Perfiles de permisos** muestra las reglas de los perfiles predefinidos y permite duplicarlos para editar un perfil propio. En el perfil «Solo lectura», las escrituras requieren aprobación, el borrado se deniega y SSH exec está desactivado. Un perfil asignado a un servidor no se puede borrar. El simulador evalúa la misma política sin SSH con el servidor conectado y la interfaz disponible. Incluye las reglas del servidor si se elige uno. La simulación usa las rutas de entrada, sin resolver enlaces remotos ni conceder permisos temporales.

**Acceso MCP…** abre la hoja de un servidor desde el menú de su fila o la cabecera. La disponibilidad inicial es Desactivado. Desatendido requiere autenticación con clave mediante agente SSH y excluye la elevación desatendida. La hoja usa el perfil elegido y los patrones propios, muestra un candado en los patrones heredados y permite ocultar la identidad o heredar el ajuste general. En esta hoja y en el editor de perfil, pulsa **Añadir** o **Enter** en el campo para incorporar un patrón pendiente antes de guardar. Si falta un perfil válido o la disponibilidad no es compatible con la autenticación, el pie explica por qué no se puede guardar. Las notas se exponen en `list_servers` y el esquema no fija una longitud máxima. Las prohibiciones siempre prevalecen sobre los ámbitos permitidos. Los patrones de rutas remotas son absolutos o empiezan por `**/`. Las rutas locales también admiten `~/`. El directorio de configuración de Vela, incluido `mcp.toml`, está protegido incluso con un ámbito local amplio.

**Copias de rollback** muestra el número de copias vigentes y el espacio que ocupan respecto de la cuota. Desde esa sección se pueden editar el tamaño por archivo, la cuota y la caducidad. **Mostrar** abre la carpeta `mcp-rollback`. **Vaciar…** elimina las copias tras confirmación. Se conservan el registro de copias y el comportamiento de caducidad de M2.

La configuración sigue en el formato v1. Los archivos antiguos conservan sus valores por defecto. Los ajustes globales guardan cada opción al cambiarla. Las hojas y editores guardan al pulsar **Guardar**. Vela edita el TOML con `toml_edit`, conserva el orden y los comentarios de las entradas editadas y valida el documento antes de publicarlo por renombrado atómico desde un temporal con permisos `0600`. Si el archivo cambió desde la lectura, la ventana informa del conflicto y ofrece **Recargar**.

El proceso `vela mcp` en marcha relee `mcp.toml` al comienzo de cada llamada de herramienta en modo headless. Un cambio publicado rige desde la siguiente llamada, sin reiniciar ni esperar un intervalo de sondeo. La relectura falla de forma cerrada ante un archivo inválido. En modo puente el cliente también comprueba el interruptor global antes de reenviar. La evaluación de permisos sobre las sesiones de la ventana, las aprobaciones, su actividad, el acceso temporal, el corte de agentes y la reversión desde el registro quedan para M6.
