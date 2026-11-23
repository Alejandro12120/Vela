# Plan del MVP de vSCP

Estado: terminal integrada F6 y F7 parte 1 (duplicar servidor y cabecera de sesión) implementadas en la rama `mvp`. Quedan pendientes la validación de la entrada física y el resto de F7 (temas, modos y vidrio). Las prioridades siguen siendo, por orden, RAM, arranque instantáneo y fluidez, y tamaño instalado.

## 1. Qué se descarta del hilo anterior

Durante la sesión anterior se propusieron cuatro stacks distintos: GTK + Python, Swift + AppKit, Rust + egui y Rust + GPUI. El encargo con egui se detuvo antes de escribir archivos. Queda así:

| Decisión | Estado |
| --- | --- |
| Rust + GPUI | **Elegido.** Era el candidato principal y el usuario lo confirmó. |
| egui/eframe | Descartado como camino principal. No hace falta compararlo antes del MVP. |
| GTK + Python, Swift + AppKit | Descartados como aplicación. `probes/file-handoff` se conserva como referencia. |
| Primera plataforma | macOS. Se compila de forma nativa con `cargo` en el MacBook Air. |
| Segunda plataforma | Arch + HyDE (Wayland). Va después del MVP en macOS (confirmado). |

Prueba de arrastre: el usuario indicó el 2026-11-20 que se considera superada. Se da por válida la entrega de una URL de archivo local a Finder mediante arrastre. Copiar y pegar en Finder ya estaba verificado.

## 2. Stack

### Interfaz: GPUI

- Fijar el crate `gpui-pre = "=0.3.11"`, del 2026-11-02, con sus crates de plataforma `gpui-pre-*` en la misma versión. Es la publicación semanal de `zed-industries/zed`. El crate `gpui` de crates.io está parado en 0.2.2, de 2025-10-22. La API es anterior a 1.0 y cambia: solo se actualizará a propósito, con nota en la continuidad.
- Lo que GPUI ya resuelve, según el código fuente de 0.3.11:
  - `uniform_list` para listas virtualizadas. Se usará para el árbol de archivos aplanado.
  - Arrastre interno entre paneles (`on_drag`/`on_drop`).
  - **Arrastre hacia el gestor de archivos** con `external_drag_payload(...)` → `ExternalDragPayload::Files`. En macOS llama a `beginDraggingSessionWithItems` y en Wayland también está implementado. En X11 no está implementado.
  - Recibir archivos arrastrados desde Finder o Dolphin (`FileDropEvent`, `ExternalPaths`).
  - Leer archivos del portapapeles (`ClipboardEntry::ExternalPaths`).
- Lo que GPUI **no** resuelve: escribir archivos en el portapapeles. En macOS, `write` ignora `ExternalPaths`. F4 usa `objc2-app-kit =0.3.2` para escribir `NSURL` en `NSPasteboard`, igual que hace `probes/file-handoff/macos.swift`, que ya funcionó en Finder. En Linux quedará pendiente para la fase de Linux.
- F0 fija Rust 1.98.1. El MacBook usa las Command Line Tools y `xcrun -f metal` no encuentra el compilador. Se activa `runtime_shaders` en `gpui-pre-platform`, que la propaga al backend de macOS para compilar los shaders Metal durante el arranque. La medición de F0 incluye ese coste. F2a activa también `font-kit` para rasterizar el texto en macOS.
- Sin `gpui-component` en el MVP. La 0.9.0 depende de `gpui-pre =0.3.11` y trae árbol y tabla, pero arrastra gráficas, resaltado, `lsp-types`, `ropey` y otras dependencias. Se implementarán solo los controles necesarios: fila de árbol, barra de ruta, botón, campo de texto, diálogo y barra de progreso. Se reconsiderará si el campo de texto resulta costoso, midiendo antes el binario y la RAM.

### Conexión: OpenSSH del sistema

- Todas las conexiones pasan por el binario `ssh` del sistema. Así se respetan `~/.ssh/config`, `known_hosts`, ProxyJump y el agente sin reimplementarlos.
- vSCP conserva un proceso hijo maestro por servidor con `ssh -M -N`, un `ControlPath` propio y `ControlPersist=no`. La contraseña o la aprobación del agente se piden una sola vez por sesión.
- SFTP: `ssh -o ControlPath=… -s <destino> sftp` como proceso hijo. Su stdin y stdout se pasan a `openssh_sftp_client::Sftp::new(stdin, stdout, opciones)`, con el crate `openssh-sftp-client` fijado en 0.15.12. No se usa el crate `openssh` porque hace falta controlar directamente las variables de askpass y las opciones.
- Terminal integrada (MVP, F6): cada sesión SSH tiene dos vistas a pantalla completa, **Archivos** (SFTP) y **Terminal**. Se alterna entre ellas con un control segmentado en la cabecera y con ⌘1 / ⌘2. No hay panel inferior ni terminal local: solo sesiones con servidores.
- Cada sesión recuerda qué vista estaba abierta al cambiar de servidor en la barra lateral.
- La terminal se arranca la primera vez que se abre su vista, en la carpeta remota que muestra el panel de archivos. Después sigue viva aunque se vuelva a Archivos, conservando el proceso y el historial.
- Se dibuja solo mientras su vista está visible. En F6 el cursor es fijo para que cesen los fotogramas en reposo (decisión 16).
- En el MVP hay una terminal por sesión. Ejecuta el mismo `ssh` en un PTY y lo dibuja con GPUI sobre `alacritty_terminal` 0.26 (Apache-2.0). Se adapta el código de las crates `terminal` y `terminal_view` de Zed (GPL-3.0-or-later). Esas crates dependen de partes internas de Zed, así que se copia y adapta el código en vez de usarlas como dependencia.
- Lo que importa no es el emulador, sino que se parezca a la terminal del usuario:
  - fuente, tamaño, colores y cursor importados de `kitty.conf`, siguiendo sus `include`, como los archivos de tema de HyDE;
  - glifos de Nerd Font si la fuente está instalada;
  - `TERM=xterm-256color` y `COLORTERM=truecolor`, para no depender de que el servidor tenga la terminfo `xterm-kitty`;
  - ⌘C y ⌘V.
- El prompt, los alias y el resto de la configuración de shell en una sesión SSH son los del servidor. No dependen del emulador.
- Terminal externa: se mantiene como acción secundaria «Abrir en Kitty», con el mismo `ControlPath`.
- `openssh-sftp-client` necesita tokio. Se ejecutará un runtime tokio `current_thread` en un único hilo de red. La interfaz se comunica con él mediante canales (`async-channel`). GPUI recibe los eventos en una tarea `cx.spawn`. Nada de red se ejecuta en el hilo de la interfaz.

### Opciones que vSCP impone por servidor

```text
-o IdentitiesOnly=yes -o IdentityFile=<clave pública elegida> -o IdentityAgent=<socket de Bitwarden>
-M -N -o ControlMaster=yes -o ControlPath=<dir corto>/%C -o ControlPersist=no
-o StrictHostKeyChecking=ask -p <puerto> <usuario>@<host>
```

- `IdentityAgent` hace falta porque una app lanzada desde el Dock no hereda el `SSH_AUTH_SOCK` de la shell. vSCP detecta el socket de Bitwarden en las rutas conocidas y deja configurarlo a mano. Antes de implementarlo, se comprobarán las rutas en la documentación de Bitwarden.
- `IdentityFile` en `ssh_config` se acumula. Si el servidor tiene claves en `~/.ssh/config`, `IdentitiesOnly` también las ofrecería. Al guardar el servidor, vSCP ejecuta `ssh -G` con las mismas opciones y avisa si la configuración efectiva contiene más de una identidad.
- `ControlPath` debe ser corto. El `TMPDIR` de macOS (`/var/folders/…`) puede superar el límite de 104 bytes de `sun_path`. Se usará `/tmp/vscp-<uid>/` con permisos 0700.

### Contraseñas y claves de host

- `SSH_ASKPASS` apunta a un lanzador privado que ejecuta `vscp askpass` con el texto recibido de OpenSSH. Se usa `SSH_ASKPASS_REQUIRE=force`. Ese proceso pasa la petición a la aplicación por un socket Unix del directorio privado 0700. La aplicación muestra un diálogo y devuelve la respuesta. La contraseña no se guarda.
- Con `StrictHostKeyChecking=ask`, OpenSSH también usa askpass para aceptar la primera clave de host. Ese diálogo muestra la huella. Si la clave cambia, `ssh` rechaza la conexión. vSCP muestra el aviso de stderr y no ofrece aceptarla.

## 3. Arquitectura

```text
crates/
  vscp-core/   sin GPUI: configuración, procesos ssh, sesión SFTP, cola de transferencias,
               sesiones de edición, caché. Se prueba en Linux dentro de un contenedor.
  vscp-app/    GPUI: ventana, paneles, terminal, diálogos, temas y glue de plataforma (macOS: pasteboard).
```

- **Configuración**: `servers.toml`, en `~/Library/Application Support/vSCP/` en macOS y en `$XDG_CONFIG_HOME/vscp/` o `~/.config/vscp/` en Linux. Por servidor se guardan un identificador estable, nombre, host, puerto, usuario, modo de autenticación (`agent` o `password`), clave pública (elegida del agente o desde archivo) y socket opcional para el agente, ruta remota inicial, fijado y orden manual (decisión 9). La aplicación elegida para editar archivos remotos se guarda en `settings.toml` con permisos 0600. Ajustes muestra la ruta importada de `kitty.conf` y la fuente de la terminal, con una acción para volver a leer el archivo.
- **Modelo de árbol**: cada panel guarda un árbol de nodos con hijos cargados de forma perezosa. La interfaz recibe un `Vec` aplanado de filas visibles, con profundidad y estado expandido, y lo pinta con `uniform_list`. Las carpetas expandidas se recuerdan en memoria por servidor, panel y ruta raíz.
- **Caché**: hay un directorio de sesión para descargas temporales de arrastre y portapapeles, y otro para copias de edición. Se acota por tamaño y se limpia al cerrar, salvo las copias con cambios sin subir.
- **Temas** (F7):
  - **Archivos**: `themes/*.toml` en el directorio de configuración. Incluye dos temas de serie, «Claro» y «Oscuro», derivados de `.agent/skills/heyrico-ui/assets/tokens.css`. Fuente del sistema, base de 13 px y radios de 6 y 8 px.
  - **Modo**: Sistema, Claro u Oscuro. En modo Sistema sigue el aspecto de macOS con `observe_window_appearance`. Cada modo tiene asignado un tema.
  - **Colores editables**: fondo, panel, barra lateral, texto principal y secundario, borde, selección, acento, estados (correcto, aviso, error) y paleta de la terminal (fondo, texto, cursor y 16 colores). La paleta de la terminal se puede importar de `kitty.conf`.
  - **Editor** en Ajustes › Apariencia:
    - muestra de cada color, campo hexadecimal y selector HSV sencillo;
    - vista previa en vivo;
    - duplicar, restablecer y exportar temas;
    - aviso si un texto queda por debajo de 4.5:1 de contraste.
  - **Vidrio esmerilado**:
    - `WindowBackgroundAppearance::Blurred` de GPUI. En macOS coloca un `NSVisualEffectView` detrás de la ventana, con material `UnderWindowBackground` y modo `BehindWindow`.
    - Ajustes: activar o desactivar, y opacidad por zona. La barra lateral y la cabecera son más translúcidas. Las listas de archivos y la terminal son más opacas para que el texto se lea bien.
    - GPUI no expone el material ni la intensidad del desenfoque. Cambiarlos necesitaría código `objc2` propio, que queda fuera del MVP.
    - Si macOS tiene activado «Reducir transparencia», la ventana se vuelve opaca.
    - En Linux, GPUI usa el protocolo de desenfoque de KDE. En Hyprland (HyDE), el desenfoque lo hace el compositor según sus propias reglas. Se revisará en F9.

## 4. Alcance del MVP

### Entra

1. Lista de servidores en la barra lateral: crear, editar, borrar y conectar. Incluye el estado de la sesión activa, una sección «Fijados» y orden por arrastre (decisión 9).
2. Autenticación con el agente de Bitwarden usando una clave elegida de la lista del agente (decisión 10), o con contraseña pedida al conectar. Se acepta la clave de host en el primer uso.
3. Dos paneles, local a la izquierda y remoto a la derecha:
   - barra de ruta editable;
   - atrás, adelante y subir;
   - carpetas expandibles en el sitio, al estilo de Finder;
   - columnas de nombre, tamaño, fecha y permisos. Al estrechar un panel desaparecen permisos, fecha y tamaño en ese orden. ⌘S alterna la barra lateral (decisión 13);
   - selección múltiple con ⌘ y ⇧. ↑/↓ mueven la selección, ⇧↑/⇧↓ extienden el rango y ⌘A selecciona todas las filas visibles;
   - → expande y ← contrae o selecciona la carpeta contenedora. ⌘↓ abre la carpeta seleccionada, ⌘↑ sube, ⌘[ retrocede y ⌘] avanza. Tab cambia de panel;
   - ⌘⇧. alterna los archivos ocultos y ⌘R actualiza la raíz y las carpetas expandidas.
4. Transferencias recursivas en ambos sentidos. Se inician arrastrando entre paneles, con el botón de copia o con F5. Cada sesión procesa un trabajo y un archivo a la vez, en orden de llegada. Se crean las carpetas vacías. Los enlaces a archivos se copian como contenido. Los enlaces a carpetas y los enlaces rotos se omiten y aparecen en el historial.
   - Ante un conflicto se puede sobrescribir, omitir, renombrar con el primer sufijo libre «(N)» o cancelar el trabajo. Se puede aplicar una elección a los demás conflictos del trabajo. Los errores se muestran por elemento y la cola continúa.
   - Las descargas se escriben en `<nombre>.vscp-part` y las subidas de archivos nuevos en `.<nombre>.vscp-part`. El archivo temporal se renombra al completar la copia y se elimina al cancelar o fallar. Sobrescribir un archivo remoto modifica el mismo inodo y conserva propietario y permisos. Una subida interrumpida tras abrir ese destino se indica como «Incompleto en destino».
   - Se conserva la fecha de modificación de los archivos. Los nuevos reciben los permisos de origen enmascarados con `0o777`. Los sobrescritos conservan los permisos del destino.
5. Crear carpeta, renombrar en línea y eliminar con confirmación en ambos paneles. En macOS, la eliminación local mueve a la Papelera. La eliminación remota es recursiva y permanente, y elimina los enlaces sin recorrer sus destinos.
6. Integración con Finder:
   - Arrastrar desde Finder o pulsar ⌘V en el panel remoto sube los archivos a la carpeta de destino o a la raíz del panel. ⌘C en el panel local publica las URL originales. ⌘V en ese panel descarga los originales remotos de la copia de vSCP. La copia local entre carpetas queda fuera de F4.
   - ⌘C de archivos remotos y el inicio de un arrastre remoto preparan descargas recursivas en `~/Library/Caches/vSCP/staging/<id aleatorio>/`. Los directorios tienen permisos 0700 y se conservan los nombres y fechas de modificación. El límite es de 2 GB por preparación.
   - Una nueva copia al portapapeles sustituye y borra la anterior al completarse. Al iniciar y salir se conservan las descargas todavía referenciadas por las URL del portapapeles. El cierre limpia las demás descargas de preparación y deja intacto el portapapeles. Pegar los originales en el panel local requiere la sesión remota de la copia.
   - El arrastre remoto reutiliza la preparación por selección mientras siga vigente. Ofrece las URL si la descarga terminó al salir de la ventana. En caso contrario, el usuario vuelve a arrastrar cuando aparezca «Listo para arrastrar».
   - En `gpui-pre-macos` 0.3.11, `src/window.rs:3789–3792` ofrece únicamente `Copy` a Finder (`OutsideApplication`) y `Copy | Move` dentro de la aplicación y para contextos desconocidos. Se permite arrastrar archivos locales a Finder con sus rutas reales.
7. Un doble clic abre los archivos de texto en el editor y los binarios con la aplicación predeterminada. «Ejecutar» está en el menú contextual (decisión 11). Para archivos remotos:
   - descarga a una copia local reutilizable y pide confirmación por encima de 100 MB. Al volver a abrir una copia sincronizada, se actualiza si el remoto ha cambiado;
   - «Ajustes» (⌘,) permite elegir la aplicación predeterminada del sistema o un paquete `.app`. Se abre con `open` o `open -a <ruta>`;
   - `notify` 8.2.1 vigila la carpeta para detectar guardados directos y reemplazos atómicos. Cada lote espera 300 ms de silencio y comprueba estabilidad y SHA-256 antes de subir. Los cambios durante una subida se agrupan para la siguiente;
   - antes de subir, compara fecha y tamaño remotos. Si cambian, ofrece «Sobrescribir remoto», «Guardar remoto como copia» (junto a la copia local, con el sufijo « (remoto)») o «Más tarde». La subida modifica el archivo existente para conservar su inodo, permisos y propietario;
   - la barra muestra el estado de la subida. «Ediciones» lista las copias del servidor y permite abrirlas, subirlas, mostrarlas en Finder o terminar la edición. Descartar cambios requiere confirmación;
   - al reconectar, se indican los cambios sin subir y se reintentan los pendientes con la misma comprobación remota. Los conflictos esperan una decisión.
8. Vista Terminal de cada sesión, a pantalla completa, alternable con la vista Archivos (⌘1 / ⌘2). Incluye la acción secundaria «Abrir en Kitty».
9. Reconexión manual si la sesión maestra cae. Las ediciones pendientes no se pierden.
10. Temas: modo claro, oscuro o del sistema; colores editables; vidrio esmerilado configurable.

### No entra en el MVP

- Cambiar permisos o propietario. Solo se muestran.
- Reanudar transferencias interrumpidas.
- Proveer el archivo arrastrado mediante promesas, sin descargarlo antes.
- Guardar contraseñas en el llavero.
- Terminal local con la shell del Mac.
- Linux (F9).
- Windows.
- Varias vistas de archivos o varias terminales por sesión.
- Sincronizar carpetas.
- Los enlaces simbólicos se muestran con su destino. Las carpetas enlazadas se abren y no se editan como enlace.

## 5. Fases

Cada fase termina con compilación de release en el Mac, `cargo fmt --check`, `cargo clippy -D warnings` y pruebas. La continuidad se actualiza al final de cada fase. Las compilaciones siguen las normas de `AGENTS.md`: un trabajo, sin compilación incremental, `CARGO_TARGET_DIR` desechable y limpieza al terminar.

| Fase | Contenido | Criterio de aceptación |
| --- | --- | --- |
| F0 Andamiaje | Comprobar `cargo`, `rustc`, `xcode-select -p` y `xcrun -f metal` en el Mac. Workspace con dos crates. `rust-toolchain.toml` fijado después de comprobar la versión mínima de `gpui-pre`. Ventana GPUI vacía con el tema. | La ventana abre en macOS. Se registran la RAM de reposo, el tiempo hasta la ventana y el tamaño del binario. Sirve de referencia frente a los presupuestos (sección 6). |
| F1 Núcleo SSH/SFTP | `vscp-core` con la conexión maestra, askpass, SFTP sobre `ssh -s`, listado, `stat`, lectura y escritura en streaming. Comprobación `ssh -G` de identidades. | Pruebas de integración contra un `sshd` de usuario: puerto alto, claves de host y de cliente generadas, `StrictHostKeyChecking`. En macOS se usa `/usr/sbin/sshd` sin root. En Linux se usa un contenedor `--rm`. Además, un caso con contraseña. |
| F2a Servidores y conexión | Barra lateral, edición y persistencia de `servers.toml`, conexión y diálogos SSH. Operaciones SFTP concurrentes y listado inicial de verificación. | Conectar al servidor de pruebas, mostrar la ruta y el recuento de entradas y listar mientras se descarga otro archivo. |
| F2b Paneles | Panel local y remoto con árbol virtualizado, navegación, columnas y selección. Sustituye la vista provisional de F2a. | 10.000 filas con desplazamiento fluido. Expandir no bloquea la interfaz con latencia de red simulada. |
| F3 Transferencias y operaciones | Cola, arrastre entre paneles, conflictos, crear carpeta, renombrar y borrar. | Subida y bajada recursivas de un árbol de prueba con nombres con espacios, `#` y acentos. El contenido coincide byte a byte. Cancelar deja el estado coherente. |
| F4 Finder | Arrastre de entrada y de salida, ⌘C y ⌘V. Módulo `objc2` de pasteboard. | Prueba manual con trackpad en Finder: el nombre y los bytes coinciden en los cuatro gestos. |
| F5 Edición remota | Copias de edición, `open` o editor, vigilancia, comprobación de conflicto y subida. | Con VS Code: dos guardados separados por más de 300 ms se suben por separado. Una ráfaga sube su contenido final. Un cambio remoto hecho entretanto muestra el conflicto. Desconectar conserva la copia. |
| F5b Revisión de uso | Decisiones 9–15 de la sección 8. Repaso visual de todas las pantallas con la skill `heyrico-ui`. Corregir Enter, que no confirma el cambio de nombre en línea ni el nombre de la carpeta nueva. Pasar `servers.toml` del campo `favorite` a fijado más orden, y conservar las rutas de clave existentes como «Desde archivo». Listar el agente con el protocolo SSH (`REQUEST_IDENTITIES`) y guardar la clave elegida en `keys/<huella>.pub` con permisos 0600 para `IdentityFile`. | Enter confirma el cambio de nombre y la carpeta nueva, con una prueba de teclado. Fijar, soltar y reordenar sobreviven a reiniciar. Se listan las claves de un agente de prueba con dos claves y la elegida es la única identidad de `ssh -G`. Doble clic en `.sh` abre el editor. Capturas en claro y oscuro con los iconos nuevos. **Implementada** (partes 1–3). Quedan pendientes las comprobaciones con entrada física del usuario: clic, arrastre, teclado, Bitwarden real. |
| F6 Terminal integrada (implementada) | Vista Terminal a pantalla completa por sesión, alternable con Archivos. Arranca en la carpeta remota actual. PTY con `ssh` sobre la sesión maestra. Dibujado, teclado, selección, copiar y pegar, IME, ratón y cambio de tamaño. Importación de `kitty.conf`. Historial limitado, unas 5.000 líneas como punto de partida. Acción «Abrir en Kitty». | `vim`, `htop` y `tmux` funcionan. Se pueden escribir acentos con IME. El cambio de tamaño se propaga al remoto. Al alternar de vista, la terminal conserva el proceso y el historial, y sin actividad no consume CPU. Fuente y colores comparados con Kitty. Teclado, composición con teclado español y gestos de trackpad UNCONFIRMED con entrada física. `htop` y `tmux` también quedan UNCONFIRMED. |
| F7 Temas y vidrio | **Pendiente (F7 parte 2, antes de los temas):** mover arrastrando dentro del mismo panel, pedido por el usuario el 2026-11-23. Hoy solo se puede soltar en el panel contrario, que copia. Comportamiento como Finder: soltar en el mismo panel mueve, y en el panel contrario sigue copiando con la cola de transferencias. Destino según lo que hay bajo el puntero: una carpeta cerrada se resalta con una fila de esquinas redondeadas, su icono pasa a carpeta abierta y tras ~1 s encima se despliega sola; una carpeta abierta, o cualquier fila dentro de ella, marca la fila de la carpeta redondeada y rodea la carpeta y todo su contenido visible con un borde redondeado y un relleno de acento al 20 %; un elemento de la raíz o un hueco marca todo el panel igual. No se marca nada si soltar no movería nada (mismo padre) o si el destino es el propio elemento o uno de sus descendientes. Si el nombre ya existe en el destino, el diálogo de conflicto de las transferencias: Sobrescribir, Omitir, Renombrar como «nombre (2)», Cancelar y «Aplicar a todos»; nunca se sobrescribe una carpeta que contiene lo que se mueve. No se mueve lo que tiene ediciones remotas abiertas. Opción/Alt para copiar queda fuera. **Pendiente:** modelo de temas, modos, editor, vidrio esmerilado y «Reducir transparencia». **Hecho (F7):** tamaño de letra de la terminal desde el teclado, pedido por el usuario el 2026-11-23: en macOS ⌘+ / ⌘= agrandan, ⌘− reduce y ⌘0 vuelve al tamaño de `kitty.conf`; en Linux Ctrl+Mayús++, Ctrl+Mayús+− y Ctrl+Mayús+0, como el resto de atajos de la terminal, porque Ctrl+− llega a la shell como `^_` (deshacer en readline). El usuario usa un teclado QWERTY español: «+» y «−» son teclas propias a la derecha de la P y del punto, y con Mayúsculas dan «*» y «_», y Mayús+0 da «=». GPUI elimina Mayús en las teclas de símbolo, por lo que esas teclas físicas llegan como `ctrl-*`, `ctrl-_` y `ctrl-=`; se enlaza además `ctrl-+` para Ctrl+Mayús+= de la distribución estadounidense, sin conflicto con el reinicio español. Pasos de 1 pt entre 6 y 48, en vivo en todas las terminales, con rejilla y PTY redimensionados; no se guarda entre arranques. **Pendiente:** comprobar las teclas físicas en teclado español con las líneas `[keys]` de `VSCP_DEV_LOG`. **Hecho (F7 parte 1):** repaso de la cabecera de sesión, pedido por el usuario el 2026-11-22: más separación entre los botones «Archivos» y «Terminal» del control segmentado, que hoy se tocan, y que el nombre del servidor ocupe el espacio libre en lugar de cortarse a 98 px (`max_w` fijo en `file_workspace.rs`). `usuario@host` sigue siendo lo primero que se recorta. **Hecho (F7 parte 1):** opción «Duplicar…» en el menú contextual de cada servidor, pedida por el usuario el 2026-11-23: abre el formulario de «Añadir servidor» relleno con los datos del servidor y el nombre con « duplicate» al final; al guardar se crea un servidor nuevo con su propio id y el original no cambia. | **Comprobado en F7 parte 1:** en la cabecera, el nombre del servidor solo se recorta cuando de verdad falta espacio, comprobado a 736 px y al ancho por defecto; «Duplicar…» abre el formulario relleno, guarda una copia independiente y borrar una de las dos no afecta a la otra. **Comprobado:** los atajos de tamaño cambian la terminal en vivo (pruebas GPUI); al crecer la letra bajan las columnas y filas del PTY, confirmado en el fixture con `stty size` y `htop` a 1100 px: 118×57 a 11 pt y 71×32 a 20 pt; el reinicio vuelve al tamaño de `kitty.conf`, incluso fuera de 6–48. **Pendiente:** comprobar las teclas físicas en teclado español; cambiar de tema o modo se aplica en vivo, sin reiniciar; los temas de serie pasan el contraste de 4.5:1; se mide la RAM con el desenfoque activado y desactivado. |
| F8 Pulido y recorrido final | Reconexión, mensajes de error y revisión de accesibilidad con la skill `apple-design`. | Recorrido completo con Bitwarden real y una clave elegida, sin agotar intentos de autenticación. Mediciones finales frente a los presupuestos, con la terminal abierta. |
| F9 Linux (post-MVP) | Backend Wayland y Dolphin: arrastre de salida soportado, pero portapapeles de archivos pendiente. El 2026-11-22 se confirmó en HyDE (Arch, Wayland, ICD Intel de Vulkan): conexión al fixture de `sshd` con el panel remoto listando `/home/alejandro`, árbol con `demo/sub` expandido y menú contextual con iconos, subida y bajada de `prueba con espacios ñ.txt` (200 KB) comparadas con `cmp`, y terminal integrada con `pwd`, `stty size` (74×90) y colores. Capturas en claro y oscuro sin tofu, texto cortado ni iconos rotos. Tras 10 s inactivo: Archivos 174.696 kB de VmRSS y 0,0/0,0/0,2 % de CPU; Terminal 189.592 kB y 0,0/0,0/0,0 %. Sin cambios de código. El desglose de VmRSS mostró ~80 MB de `libLLVM`, ~36 MB de Mesa `libgallium` y ~5 MB de `libvulkan_intel` como páginas compartidas, frente a ~42 MB privados (Private_Dirty) de ~192 MB de VmRSS en Terminal, cifra comparable con `phys_footprint` en macOS. Dolphin y el portapapeles de archivos siguen pendientes. | Lo mismo que F4, F6 y F7 en HyDE. |

## 6. Presupuestos y medición

- Menos de 100 MB en reposo con la ventana abierta. Menos de 180 MB con una conexión y navegación normal. Objetivo extra: menos de 60 MB en reposo.
- Unos 1 s hasta tener una ventana utilizable, sin esperar a la red.
- Se mide el binario de release. Se cuentan los procesos propios y los `ssh` hijos aparte. En macOS se usa `footprint` o la memoria de Monitor de Actividad. Hay que comprobar que la memoria se estabiliza tras conexiones, navegación y transferencias repetidas.

## 7. Riesgos

- **API de GPUI inestable**: se fija la versión exacta y no se actualiza durante el MVP.
- **Ventana de GPUI sin acceso a `NSView` para el pasteboard**: no hace falta acceso a la ventana. `NSPasteboard.generalPasteboard` es global.
- **Arrastre de salida de archivos grandes**: si la descarga no termina antes de salir de la ventana, en el MVP no se ofrece el arrastre y se muestra «Preparando…». El usuario lo acepta. La solución completa es usar promesas de archivo después del MVP.
- **Identidades heredadas de `ssh_config`**: se avisa con `ssh -G` (sección 2).
- **Recursos del MacBook**: un solo trabajo de compilación y limpieza de `target/` al terminar cada fase.

## 8. Decisiones del usuario (2026-11-20)

1. Terminal en macOS: Kitty. Lo que se busca es que la terminal integrada se parezca a Kitty, no usar ese emulador. «Abrir en Kitty» se conserva como acción secundaria y se lanza con `open -na kitty --args ssh …`.
2. Los servidores se guardan solo en `servers.toml` de vSCP. No se importan los `Host` de `~/.ssh/config`. Ese archivo se sigue leyendo porque `ssh` lo carga, y la comprobación con `ssh -G` sigue siendo necesaria.
3. Se acepta en el MVP que el arrastre de un archivo remoto grande a Finder requiera haberlo descargado antes.
4. Linux queda para después del MVP en macOS (F9).
5. La terminal integrada entra en el MVP (F6), en lugar de abrir Kitty. Solo hay sesiones SSH a servidores, sin terminal local. Sustituye a la decisión anterior de dejarla para después del MVP.
6. Se acepta GPL-3.0 para vSCP. En F0 se fija `license = "GPL-3.0-or-later"` y se añade `LICENSE`. Esto permite adaptar la terminal de Zed. GPUI y `alacritty_terminal` son Apache-2.0, compatibles con GPLv3.
7. Hay un módulo de temas en el MVP (F7): modo claro y oscuro, colores personalizables y efecto de vidrio esmerilado.
8. La terminal no va en un panel inferior. Cada sesión SSH alterna entre dos vistas a pantalla completa, Archivos y Terminal. Sustituye al panel inferior con pestañas de la decisión 5.

### Revisión manual de F0–F5 (2026-11-21)

Funciona: arrastrar entre paneles, de Finder a los paneles y de los paneles a Finder. Dos guardados separados en el editor se suben por separado. Los cambios pedidos están en la fase F5b.

9. **Servidores fijados**, como los threads de T3 Code. Hay una sección «Fijados» arriba y el resto de servidores debajo, cada una en orden manual. Se fija o se suelta con el icono de chincheta al pasar el ratón, que es el único control de la fila. Las demás acciones del servidor (Conectar o Desconectar, Fijar, Editar y Eliminar) están en el menú contextual flotante del clic derecho. Arrastrar reordena dentro de cada sección. Llevar un servidor a la otra sección lo fija o lo suelta. Se quita la casilla «Favorito» del formulario. Sustituye al orden de favoritos primero y luego alfabético.
    - Cabeceras: «Servidores» arriba con el botón de añadir y «Fijados» sobre el grupo fijado. El grupo sin fijar no lleva cabecera: cuando hay fijados, un divisor de 1 px con 6 px de espacio vertical separa los dos grupos. Durante un arrastre sin servidores sin fijar, la zona final dice «Suelta aquí para dejar de fijar». Sin servidores fijados, durante un arrastre la cabecera «Servidores» se convierte en la zona «Suelta aquí para fijar», para que la lista no se desplace.
    - Implementación: el orden manual es el orden del array `servers` de `servers.toml`, con `version = 2` y el campo `pinned`. Un archivo anterior, con `favorite`, se ordena una vez al cargar y se guarda en el formato nuevo con el siguiente cambio.
10. **Clave pública desde Bitwarden.** El formulario lista las claves del agente de Bitwarden con el nombre del elemento, el tipo y la huella SHA256. vSCP guarda la clave pública en su configuración, así que no hace falta ningún archivo. «Desde archivo…» queda como opción secundaria. Si Bitwarden está cerrado, bloqueado o sin claves, se muestra un aviso con «Reintentar».
    - Implementación: la clave elegida se guarda como `keys/<hash>.pub` en la carpeta de configuración (carpeta 0700, archivo 0600), donde `<hash>` son los 16 primeros bytes del SHA-256 del blob. `identity_pub` apunta a ese archivo, así que `Auth` no cambia.
11. **Doble clic y «Ejecutar».**
    - El doble clic abre en el editor de Ajustes los archivos de texto, `.sh` incluido. Se reconocen por extensión y por contenido.
    - Imágenes, PDF y demás binarios se abren con la aplicación predeterminada.
    - El menú contextual añade «Ejecutar». En un archivo remoto lo ejecuta en el servidor, en la vista Terminal. Si la terminal está en la pantalla alternativa, muestra «La terminal está ocupada». «Ejecutar en Kitty» siempre está disponible para archivos remotos ejecutables. En un archivo local, «Ejecutar» abre la aplicación predeterminada.
    - Motivo: con la aplicación predeterminada, un `.sh` se abría en Kitty en vez de editarse.
    - «Ejecutar» remoto usa la conexión maestra de la sesión mediante `ControlPath`, tanto en la terminal integrada como en Kitty.
12. **Iconos Lucide** (licencia ISC) en toda la interfaz: menús contextuales, menú ⋯ de servidor, modales de confirmación y barra de herramientas. Sustituyen a los iconos dibujados a mano de `pane_icons.rs`. Solo se incluyen los SVG que se usan.
    - Implementación: `lucide-static` 1.70.0 (publicada el 2026-11-06), con `stroke-width="1.75"` en lugar de 2 en todos los SVG. Están en `crates/vscp-app/assets/icons/` con su `LICENSE` y se incrustan con `include_bytes!` desde `icons.rs`.
13. **⌘S alterna la barra lateral**, en lugar de ⌃⌘S. vSCP no tiene «Guardar» propio, porque los archivos se editan en una aplicación externa.
14. **Barra de ruta**: doble clic selecciona el segmento entre barras y triple clic, la ruta entera, como en los campos de texto de macOS.
15. **Toda la interfaz sigue la skill `.agent/skills/heyrico-ui`**: tipografía de 13 px, superficies neutras, bordes sutiles, iconos finos y alta densidad. Se aplica en el repaso de F5b y en las fases siguientes con interfaz (F6–F8). Lucide cumple su regla de usar una sola familia de iconos de trazo fino.

16. **Movimiento.** Las transiciones son cortas y nunca retrasan la entrada ni esperan a que acabe una animación para aplicar el estado.
    - Color y opacidad: 120 ms. Posición y tamaño: 180 ms o un muelle (rigidez 520, amortiguación 44, masa 1, unos 250 ms). Constantes en `components/motion.rs`.
    - Se anima la aparición de menús contextuales, modales, popovers y avisos, el giro del chevrón de las carpetas, la entrada de las filas de una carpeta recién expandida, el ancho de la barra lateral, la altura del historial y el relleno de la barra de transferencias, el pulso del punto al conectar y el giro del indicador de carga. Cerrar un menú no tiene animación.
    - El hover, la selección y la navegación con teclado siguen siendo instantáneos.
    - En el arrastre de servidores la tarjeta se levanta y sigue al cursor en vertical mientras las demás hacen sitio con el muelle.
    - Se respeta el ajuste «Reducir movimiento» de macOS: se lee al arrancar y al activar la ventana, y las animaciones muestran su estado final. Después de asentarse, no queda ningún fotograma de animación en marcha, salvo el pulso y el indicador de carga visibles.

Las respuestas también están registradas en `.agent/CONTINUITY.md`.
