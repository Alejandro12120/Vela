# Plan del MVP de vSCP

Estado: la terminal integrada F6, las mejoras de F7 en servidores, cabecera y arrastre y el resto de F7 (parte 3: Ajustes como página, temas, editor y vidrio) están implementados en la rama `mvp`. Queda pendiente la validación de la entrada física. Las prioridades siguen siendo, por orden, RAM, arranque instantáneo y fluidez, y tamaño instalado.

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
- **Temas** (F7 parte 3, decidido con el usuario el 2026-11-24 a partir de capturas de referencia):
  - **Ajustes como página**: Ajustes deja de ser un diálogo y ocupa la zona principal, a la derecha de la barra lateral, que sigue visible. Una sola columna centrada (ancho máximo ~760 px) con secciones «Apariencia» (Esquema de color y Temas), «Interfaz» (vidrio), «Editor» (aplicación para archivos remotos) y «Terminal» (`kitty.conf`). Se abre con ⌘, / Ctrl+, o el botón Ajustes y se cierra con Esc o con el botón de cerrar, volviendo a la vista anterior.
  - **Modelo** (`vscp-core`, sin GPUI): un tema tiene id, nombre y dos variantes, clara y oscura, con colores `u32` (con alfa cuando hace falta). `settings.toml` guarda el esquema (Sistema, Claro u Oscuro), los ids de los temas claro y oscuro (`light_theme` y `dark_theme`) y los ajustes de vidrio. Las configuraciones antiguas aplican el id guardado en `theme` a ambas variantes. Los temas del usuario son `themes/<id>.toml` en el directorio de configuración. Los temas de serie viven en el código y no se editan: se duplican.
  - **Esquema de color**: tres tarjetas, Sistema, Claro y Oscuro, con una miniatura de la ventana pintada con los colores elegidos para cada variante; Sistema muestra la mitad clara y la mitad oscura. En Sistema se sigue el aspecto del sistema con `observe_window_appearance`.
  - **Temas**: rejilla de tarjetas de igual ancho (3 columnas, 2 en ventanas estrechas). Cada tarjeta muestra dos esferas, la variante clara y la oscura, el nombre y el botón de duplicar; las del usuario añaden editar y eliminar. La esfera del sol elige el tema claro y la de la luna el oscuro. Pulsar el resto de la tarjeta elige ambos. Cada esfera seleccionada lleva un anillo de acento, atenuado si el esquema oculta su variante. Cabecera con «Crear tema» y «Añadir tema» (importar un `.toml`). Seis temas de serie con nombres propios: «vSCP» (los grises actuales, por defecto), «Frambuesa», «Bosque», «Océano», «Ámbar» y «Lavanda», todos con contraste de texto ≥ 4.5:1 y de controles ≥ 3:1.
  - **Colores editables**: superficies (fondo, barra lateral, panel, elevado, velo), texto (principal, secundario, sobre primario), bordes e interacción (borde, borde de control, hover, selección, selección de texto), acción (acento, primario, primario en hover), estados (correcto, aviso y su fondo, error y su fondo) y terminal (fondo, texto, cursor, texto bajo el cursor, selección y 16 colores ANSI).
  - **Terminal**: cada color de terminal del tema es opcional; si falta, se usa el de `kitty.conf` y después el de Kitty por defecto. «vSCP» no fija ninguno, así que la terminal sigue como hoy. El editor tiene «Usar colores de kitty.conf» (vacía todos) e «Importar de kitty.conf» (copia la paleta leída).
  - **Editor de temas**: panel flotante a la derecha de la página (~420 px) con título «Crear tema» o «Editar tema», Nombre, Apariencia (Claro / Oscuro: la variante que se edita), filtro de colores e interruptor «Avanzado» (apagado: fondo, barra lateral, panel, texto, texto secundario y acento; encendido: todos, agrupados). Cada fila: nombre, muestra redonda y campo hexadecimal (`#RGB`, `#RRGGBB` o `#RRGGBBAA`). Al pulsar la muestra se abre un selector flotante con cuadro de saturación y brillo, barra de tono y, si el color tiene alfa, barra de opacidad. Vista previa en vivo en toda la ventana mientras el panel está abierto. Aviso junto a los textos por debajo de 4.5:1. Pie con «Exportar…», «Cancelar» y «Crear tema» / «Guardar».
  - **Vidrio esmerilado** (sección Interfaz):
    - Interruptor «Vidrio esmerilado» y dos deslizadores: «Barra lateral y cabecera» (70 % por defecto) y «Listas y terminal» (92 %). Desactivados sin vidrio.
    - Con vidrio se usa `WindowBackgroundAppearance::Blurred` de GPUI y esas superficies se pintan con esa opacidad. El texto sigue opaco. En macOS GPUI coloca un `NSVisualEffectView` detrás de la ventana; GPUI no expone el material ni la intensidad del desenfoque, y cambiarlos necesitaría `objc2` propio, fuera del MVP.
    - Si macOS tiene «Reducir transparencia», la ventana es opaca y la sección lo explica.
    - En Linux, GPUI usa el protocolo de desenfoque de KDE. En Hyprland (HyDE) el desenfoque depende de las reglas del compositor. Se revisa en F9.
  - **Barra de título en macOS**: franja integrada de 32 pt con los colores y la opacidad de la barra lateral y la cabecera. Conserva los controles nativos de la ventana y se oculta en pantalla completa.
  - **Entregas**: A) modelo, página de Ajustes, esquema, rejilla, temas de serie, duplicar/eliminar/importar/exportar y colores de terminal; B) editor, selector y avisos de contraste; C) vidrio. Un commit por entrega.

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
| F7 Temas y vidrio | **Hecho (F7 parte 2):** mover arrastrando dentro del mismo panel, pedido por el usuario el 2026-11-23. Comportamiento como Finder: soltar en el mismo panel mueve, y en el panel contrario sigue copiando con la cola de transferencias. Destino según lo que hay bajo el puntero: una carpeta cerrada se resalta con una fila de esquinas redondeadas, su icono pasa a carpeta abierta y tras ~1 s encima se despliega sola; una carpeta abierta, o cualquier fila dentro de ella, marca la fila de la carpeta redondeada y rodea la carpeta y todo su contenido visible con un borde redondeado y un relleno de acento al 20 %; un elemento de la raíz o un hueco marca todo el panel igual. No se marca nada si soltar no movería nada (mismo padre) o si el destino es el propio elemento o uno de sus descendientes. Si el nombre ya existe en el destino, el diálogo de conflicto de las transferencias: Sobrescribir, Omitir, Renombrar como «nombre (2)», Cancelar y «Aplicar a todos»; nunca se sobrescribe una carpeta que contiene lo que se mueve. No se mueve lo que tiene ediciones remotas abiertas. Opción/Alt para copiar queda fuera. **Hecho (F7 parte 3A):** Ajustes como página, esquema de color, seis temas de serie, temas del usuario, duplicación, eliminación, importación, exportación en el modelo y colores de terminal por tema. **Hecho (F7 parte 3B):** editor con selector de color, vista previa en vivo, avisos de contraste y exportación desde Ajustes. **Hecho (F7 parte 3C):** vidrio esmerilado, opacidad por zonas y «Reducir transparencia» en macOS; detalle en la sección 3, «Temas». **Hecho (F7, pulido):** tarjetas del mismo ancho, elección independiente del tema claro y oscuro y barra de título integrada con el tema en macOS. **Hecho (F7 parte 4):** formulario de servidor y secciones Agente SSH, Editor y Terminal de Ajustes según la maqueta aprobada `docs/mockups/servidor-y-ajustes.html` y la decisión 17 de la sección 8. Sin verificar: entradas físicas y el selector real de `.app` en macOS. **Hecho (F7):** tamaño de letra de la terminal desde el teclado, pedido por el usuario el 2026-11-23: en macOS ⌘+ / ⌘= agrandan, ⌘− reduce y ⌘0 vuelve al tamaño de `kitty.conf`; en Linux Ctrl+Mayús++, Ctrl+Mayús+− y Ctrl+Mayús+0, como el resto de atajos de la terminal, porque Ctrl+− llega a la shell como `^_` (deshacer en readline). El usuario usa un teclado QWERTY español: «+» y «−» son teclas propias a la derecha de la P y del punto, y con Mayúsculas dan «*» y «_», y Mayús+0 da «=». GPUI elimina Mayús en las teclas de símbolo, por lo que esas teclas físicas llegan como `ctrl-*`, `ctrl-_` y `ctrl-=`; se enlaza además `ctrl-+` para Ctrl+Mayús+= de la distribución estadounidense, sin conflicto con el reinicio español. Pasos de 1 pt entre 6 y 48, en vivo en todas las terminales, con rejilla y PTY redimensionados; no se guarda entre arranques. **Pendiente:** comprobar las teclas físicas en teclado español con las líneas `[keys]` de `VSCP_DEV_LOG`. **Hecho (F7 parte 1):** repaso de la cabecera de sesión, pedido por el usuario el 2026-11-22: más separación entre los botones «Archivos» y «Terminal» del control segmentado, que hoy se tocan, y que el nombre del servidor ocupe el espacio libre en lugar de cortarse a 98 px (`max_w` fijo en `file_workspace.rs`). `usuario@host` sigue siendo lo primero que se recorta. **Hecho (F7 parte 1):** opción «Duplicar…» en el menú contextual de cada servidor, pedida por el usuario el 2026-11-23: abre el formulario de «Añadir servidor» relleno con los datos del servidor y el nombre con « duplicate» al final; al guardar se crea un servidor nuevo con su propio id y el original no cambia. | **Pendiente (F7 parte 2):** comprobar el arrastre físico en ambos paneles. **Comprobado en F7 parte 1:** en la cabecera, el nombre del servidor solo se recorta cuando de verdad falta espacio, comprobado a 736 px y al ancho por defecto; «Duplicar…» abre el formulario relleno, guarda una copia independiente y borrar una de las dos no afecta a la otra. **Comprobado:** los atajos de tamaño cambian la terminal en vivo (pruebas GPUI); al crecer la letra bajan las columnas y filas del PTY, confirmado en el fixture con `stty size` y `htop` a 1100 px: 118×57 a 11 pt y 71×32 a 20 pt; el reinicio vuelve al tamaño de `kitty.conf`, incluso fuera de 6–48. **Pendiente:** comprobar las teclas físicas en teclado español. **Comprobado (F7 parte 3):** cambio de esquema y tema con teclado en GPUI, actualización de la paleta con el PTY en marcha, contraste de serie ≥ 4.5:1 en texto y ≥ 3:1 en controles y opacidad desactivada mediante la simulación de «Reducir transparencia». En macOS, el proceso con Ajustes abierto usó 62,6 MB sin vidrio y 92,0 MB con vidrio en `vmmap`. |
| F8 Pulido y recorrido final | Reconexión, mensajes de error y revisión de accesibilidad con la skill `apple-design`. | Recorrido completo con Bitwarden real y una clave elegida, sin agotar intentos de autenticación. Mediciones finales frente a los presupuestos, con la terminal abierta. |
| F9 Linux (post-MVP) | Backend Wayland y Dolphin: arrastre de salida soportado, pero portapapeles de archivos pendiente. El borrado local usa la Papelera freedesktop (`$XDG_DATA_HOME/Trash`, o `.Trash-<uid>` en otros discos). El 2026-11-22 se confirmó en HyDE (Arch, Wayland, ICD Intel de Vulkan): conexión al fixture de `sshd` con el panel remoto listando `/home/alejandro`, árbol con `demo/sub` expandido y menú contextual con iconos, subida y bajada de `prueba con espacios ñ.txt` (200 KB) comparadas con `cmp`, y terminal integrada con `pwd`, `stty size` (74×90) y colores. Capturas en claro y oscuro sin tofu, texto cortado ni iconos rotos. Tras 10 s inactivo: Archivos 174.696 kB de VmRSS y 0,0/0,0/0,2 % de CPU; Terminal 189.592 kB y 0,0/0,0/0,0 %. Sin cambios de código. El desglose de VmRSS mostró ~80 MB de `libLLVM`, ~36 MB de Mesa `libgallium` y ~5 MB de `libvulkan_intel` como páginas compartidas, frente a ~42 MB privados (Private_Dirty) de ~192 MB de VmRSS en Terminal, cifra comparable con `phys_footprint` en macOS. Dolphin y el portapapeles de archivos siguen pendientes. | Lo mismo que F4, F6 y F7 en HyDE. |

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

### Revisión de formulario y Ajustes (2026-11-24)

17. **Formulario de servidor y Ajustes de Agente SSH, Editor y Terminal (F7 parte 4).** El usuario aprobó la maqueta [`docs/mockups/servidor-y-ajustes.html`](mockups/servidor-y-ajustes.html) («quiero que quede como el boceto»). La implementación la reproduce con los componentes y tokens de la app.
    - **Sin emojis.** Las etiquetas no llevan emojis. Solo «Autenticación» (`lock`), «Clave» (`key-round`) y «Opciones avanzadas» (`chevron-right`, girado 90° al abrir) llevan un icono Lucide fino (`stroke-width` 1.75, 14 px, color del texto). Los campos de texto no llevan icono. Motivos: estilo de la skill `heyrico-ui`, emojis distintos o ausentes según el sistema (en Linux depende de tener una fuente de emojis), alineación con el texto de 13 px y lectores de pantalla.
    - **Nombre, Host, Puerto y Usuario** conservan el orden y la disposición actuales (Host y Puerto en una fila, Usuario debajo). Solo cambia Nombre: etiqueta «Nombre (opcional)», marcador «Igual que el host»; si se guarda vacío, el servidor se muestra con su host en la barra lateral y la cabecera.
    - **Autenticación**: un control segmentado único «Clave SSH | Contraseña» en lugar de los botones «Agente (Bitwarden)» y «Contraseña». Con Contraseña se oculta el bloque Clave, como hoy.
    - **Clave**: una lista con borde y radio de 8 px.
      - Cabecera de 12 px con «Bitwarden · ● disponible»: punto verde; ámbar si está bloqueado o no responde; gris si no se encuentra. Se mantienen los mensajes de error actuales. A la derecha, un botón de icono `rotate-cw` («Volver a consultar»).
      - Cada clave es una fila con círculo de selección (relleno de acento si está elegida) y fondo de acento al 14 % si está elegida. A la izquierda va el nombre recortado; a la derecha, el tipo y los 4 últimos caracteres de la huella en monoespaciada de 11,5–12 px y color secundario («ed25519 · …XXXX»). La huella completa va en el nombre accesible.
      - La última fila, separada por un borde, es «+ Usar un archivo de clave…» en color de acento, en lugar del enlace «Desde archivo…». Con un archivo elegido, esa fila muestra su nombre y queda marcada.
      - Se mantiene la navegación con flechas. Sin buscador por ahora.
    - **Opciones avanzadas**: plegadas por defecto. Dentro van «Carpeta inicial» (antes «Ruta remota inicial», marcador «Carpeta personal del servidor») y «Socket del agente», opcional por servidor, con marcador «El de Ajustes». Se abren solas si alguno tiene valor o error.
    - **Pie**: «Cancelar» y «Guardar», como hoy. «Probar conexión» queda fuera por decisión del usuario.
    - **Socket del agente en Ajustes**: pasa a ser un ajuste global en `settings.toml` (`agent_socket`, opcional; si falta, se detecta el de Bitwarden como hoy). En cada servidor, `agent_socket` ausente significa usar el de Ajustes.
      - Migración al cargar: un servidor cuyo socket coincide con el detectado pierde el campo, y se guarda una vez. Si tiene otro socket, lo conserva como propio.
    - **Ajustes**: debajo de Apariencia e Interfaz, tres secciones con el mismo patrón. Cada sección es un panel con borde, radio de 10 px y fondo de barra lateral. Sus filas llevan la etiqueta (y una descripción opcional de 12 px) a la izquierda y el control a la derecha, separadas por bordes.
      - **Agente SSH**:
        - Fila «Agente»: «Lo usan todos los servidores con clave SSH.», el nombre y el estado del agente con el punto del formulario, y «Cambiar…», que elige el socket. Con un socket propio aparece «Usar el de Bitwarden».
        - Fila «Socket»: la ruta en monoespaciada de 12 px, con `~` y recortada por el medio.
      - **Editor**:
        - Fila «Abrir archivos remotos con»: «Al guardar en el editor, vSCP sube el cambio al servidor.» y un botón desplegable con el nombre de la aplicación y un icono de flechas arriba y abajo (`chevrons-up-down`, que hay que añadir de Lucide). El menú ofrece:
          - «Predeterminada (<aplicación>)», con el nombre real de la aplicación por defecto para texto cuando se pueda obtener;
          - los editores instalados, detectados por bundle (VS Code, Cursor, Zed, Sublime Text, BBEdit, Nova y TextEdit), solo los que existan;
          - un separador y «Otra aplicación…».
        - En Linux esa fila es fija: «Aplicación predeterminada (xdg-open)».
        - Fila «Copias de edición»: «N archivos · X MB. Las que tienen cambios sin subir no se borran.», con «Mostrar» (abre `edit-cache` en Finder o con `xdg-open`) y «Vaciar…».
          - «Vaciar…» pide confirmación y no borra copias con cambios sin subir ni de sesiones de edición abiertas.
          - El tamaño se calcula en segundo plano al abrir Ajustes.
      - **Terminal**:
        - Arriba, una vista previa estática con la fuente, el tamaño y los colores de terminal actuales: prompt verde, carpeta azul y cursor. Texto: `root@ubuntu:~$ ls` / `docs  scripts  notas.txt` / prompt con cursor. Es texto de GPUI, sin PTY.
        - Fila «Tamaño de letra»:
          - Descripción «También con ⌘+, ⌘− y ⌘0. Se guarda entre arranques.»; en Linux, con los atajos Ctrl+Mayús.
          - Control `− | 13 pt | +` y un botón de icono para volver al tamaño de `kitty.conf`. Pasos de 1 pt entre 6 y 48.
          - Se guarda en `settings.toml` (`terminal_font_size`, opcional; si falta, el de `kitty.conf`). Los atajos cambian el mismo valor y lo guardan.
        - Fila «Fuente»: «<familia>, de kitty.conf» o «<familia>, por defecto».
        - Fila «Colores»: enlace «Los del tema «<nombre>»» con flecha, que lleva a Apariencia. Usa el tema de la variante visible.
        - Fila «kitty.conf»: la ruta abreviada, «Mostrar» y «Volver a leer» (con `rotate-cw`). Sin archivo: «No se encontró; se usan los valores de Kitty» y solo «Volver a leer».

18. **Copiar al seleccionar en la terminal (hecho, 2026-11-24).** Pedido por el usuario el 2026-11-24: una opción para que el texto seleccionado en la terminal integrada se copie solo al portapapeles. Irá como interruptor en Ajustes → Terminal. El valor inicial sale de `copy_on_select` de `kitty.conf`, igual que la fuente.
    - Se construyó el interruptor «Copiar al seleccionar» en Ajustes → Terminal. Su valor inicial viene de `copy_on_select` de `kitty.conf`, donde `clipboard` o `yes` lo activan y cualquier otro valor lo desactiva; se guarda como `terminal_copy_on_select` (ausente: sigue `kitty.conf`). El texto se copia al portapapeles al soltar el ratón después de una selección.

19. **Pulido de F7 parte 4 (hecho, 2026-11-24).** Detalles que el usuario anotó el 2026-11-24 al probar en el Mac, con la causa encontrada en el código:
    - **Control de tamaño de letra** (`− | 12 pt | +`): el texto no está centrado. Cada celda debe ser `flex`, centrada en los dos ejes y de alto completo, con anchos fijos para «−» y «+» y la misma altura de línea en las tres.
      - Solución: las tres celdas son `flex`, centradas en los dos ejes, con anchos fijos de 28 y 52 px y `line-height` de 24 px; la celda central añade borde derecho.
    - **Desplegable «Abrir archivos remotos con»**: no funciona como un desplegable.
      - Causas:
        - Se abre en el punto del clic (`event.position()`), en coordenadas de ventana fijas. Por eso aparece en cualquier sitio, con el teclado se abre en (0, 0) y al desplazar la página se queda flotando.
        - Al abrirse no recibe el foco, así que Escape no le llega.
        - No tiene `on_mouse_down_out`, así que no se cierra al pulsar fuera.
      - Comportamiento pedido:
        - Se abre pegado al botón: debajo si cabe y, si no, encima. Debe seguir al botón al desplazar la página.
        - Si al desplazar ya no cabe debajo, pasa arriba. Si tampoco cabe arriba, o el botón sale de la zona visible, se cierra.
        - Se cierra con Escape o con un clic fuera, y el foco vuelve al botón.
        - Al abrirse, el foco va a la opción marcada; las flechas recorren las opciones y Enter elige.
        - Animación corta de aparición y desaparición (opacidad y desplazamiento de unos 4 px desde el botón) con las constantes de `motion.rs`, respetando «Reducir movimiento».
      - Debe convertirse en un componente reutilizable (`components/dropdown.rs`), tomando como referencia `context_menu.rs`, que sí funciona bien.
      - Solución: `components/dropdown.rs` calcula la colocación (`place`), dibuja el panel y las filas, y el menú se ancla al botón con `deferred`; sigue el desplazamiento mediante dos lienzos, se cierra con Escape, clic fuera o Tab, recupera el foco del botón y recorre las opciones con flechas, Inicio y Fin, con animación corta que respeta «Reducir movimiento».
    - **«Usar el de Bitwarden»**: vuelve a `agent_socket` vacío, es decir, al socket de Bitwarden detectado. En macOS se busca primero `~/Library/Containers/com.bitwarden.desktop/Data/.bitwarden-ssh-agent.sock` (versión de la App Store) y después `~/.bitwarden-ssh-agent.sock`. En Linux, `~/.bitwarden-ssh-agent.sock`, la ruta de Snap y la de Flatpak. Falta decidir si el botón debe mostrar la ruta a la que vuelve. Sin cambios: la decisión sigue abierta.
    - **Control «Clave SSH | Contraseña»**: ocupa todo el ancho porque la columna de la etiqueta estira a sus hijos. Debe medir lo que su contenido, como en la maqueta (`self_start` o un contenedor `items_start`).
      - Solución: la columna de «Autenticación» lleva `items_start`, así que el control segmentado mide solo sus opciones.
    - **Contraseña rechazada**: cuando el servidor rechaza la contraseña, el diálogo vuelve a salir sin ningún aviso. Solo al agotar los intentos se cierra y aparece «autenticación rechazada». A partir del segundo aviso de `askpass` para la misma conexión y el mismo tipo de dato, el diálogo debe mostrar un error junto al campo y quizá cuántos intentos quedan, si se conocen.
      - El mensaje debe ser neutro, porque el cliente no puede saber la causa: con `PermitRootLogin prohibit-password`, `sshd` pide la contraseña igualmente y la rechaza siempre, igual que una incorrecta. El usuario lo señaló el 2026-11-24.
      - Texto: solo «El servidor rechazó la contraseña.», sin explicación debajo. El usuario lo decidió el 2026-11-24: si no se puede saber la causa, no se apunta ninguna.
      - Solución: el diálogo cuenta los avisos de contraseña por destino y muestra ese único texto en rojo y 12 px bajo el campo desde el segundo aviso; el resultado de la conexión y una cancelación reinician el contador.
    - **Marcador del socket en Opciones avanzadas**: al usuario no le gusta «El de Ajustes». Propuesta: usar como marcador la ruta efectiva abreviada y añadir debajo una ayuda de 12 px: «Déjalo vacío para usar el agente de Ajustes.» Pendiente de confirmar con el usuario.
      - Solución: el marcador es la ruta efectiva de Ajustes abreviada, o «Ningún agente detectado» cuando no hay socket; no se añade texto de ayuda.
    - **Sin comprobar en macOS**: el comportamiento real del desplegable con ratón, teclado y desplazamiento, la opción «Otra aplicación…» y su selector nativo (compilado con una comprobación temporal, no ejecutado), el mensaje de contraseña rechazada contra un `sshd` real, la centración del stepper y la copia al seleccionar con entrada física. Las capturas se hicieron en Hyprland (Arch).

20. **Vista de servidor sin conectar: estados de conexión y error (hecho, 2026-11-25).** El usuario revisó el 2026-11-24 la vista principal de un servidor sin sesión abierta (bloque `details` de `workspace.rs`). No le gusta el orden de los botones, a Reintentar le falta un icono y no se puede cancelar una conexión que tarda. Aún no hay código. Acordado:
    - **Ediciones en la cabecera.** El botón «Ediciones» pasa a la derecha de la fila del nombre (`nombre` y `usuario@host` a la izquierda) en todos los estados sin sesión: antes de conectar (Conectar), conectando, desconectando y error. Así no cambia de sitio según el estado.
    - **Contador de Ediciones oculto cuando vale 0.** Solo se muestra el número cuando hay al menos una edición de ese servidor (`edit_header` en `edits_view.rs`).
    - **Banner de error con icono.** El mensaje lleva el icono `Error` a la izquierda.
    - **Detalles de SSH dentro del banner.** «Ver detalles de SSH» deja de ser un botón suelto: es un enlace discreto con chevron (› cerrado, ⌄ abierto) bajo el mensaje, y el stderr en monoespaciada se despliega dentro del mismo banner.
    - **Reintentar con icono.** Es la única acción principal, debajo del banner, con `RotateCcw`.
    - **«Editar servidor» cuando reintentar no va a servir.** Botón secundario con `Pencil` junto a Reintentar, que abre el formulario de edición de ese servidor. Solo aparece en `Error::AuthFailed`, `Error::HostKeyChanged` y `Error::HostKeyRejected`.
    - **Conectando.** Spinner (`Spinner`) y «Conectando…» desde el principio. A los 7 s aparece «Cancelar» como botón secundario con el icono `Close`, sin contador de segundos. Al cancelar se vuelve al estado inicial (botón Conectar) sin banner ni mensaje, porque cancelar no es un error. Si hay un diálogo de contraseña abierto para esa conexión, también se cierra. Escape no cancela la conexión; el usuario lo decidió el 2026-11-24.
    - **Cancelación en el core.** No basta con dejar de esperar en la app: `Client::connect` envía la conexión al runtime del core (`connect_with_timeout` en `session.rs`) y el proceso `ssh` sigue vivo hasta `CONNECT_TIMEOUT` (30 s). Hace falta una forma de cancelar en `vscp-core` que pare esa tarea. El proceso ya usa `kill_on_drop`, así que al soltarla muere `ssh`. Necesita una prueba que compruebe que no queda ningún `ssh` tras cancelar.
    - **Revisión visual** con la skill `heyrico-ui`: capturas en claro y oscuro del estado inicial, de conectando antes y después de 7 s, del error con y sin detalles desplegados y del error de autenticación con «Editar servidor».
    - **Solución:** `ConnectCancel` en `vscp-core` detiene el trabajo del worker y mata los procesos `ssh` antes del timeout de 30 s; la prueba de integración usa un listener TCP sin banner y comprueba que no queda ningún `ssh` vivo. En la app, `Connection::Connecting { attempt, cancel, can_cancel }` muestra «Cancelar» a los 7 s, sin contador; cancelar no muestra error, limpia los intentos de contraseña y cierra los diálogos y avisos pendientes de ese destino. `Escape` no cancela. El bloque `details` mantiene la cabecera y «Ediciones» arriba a la derecha en todos los estados, oculta el contador cuando vale 0, usa spinner en Conectando y Desconectando y coloca el stderr dentro del banner de error tras un desplegable accesible con chevron. «Reintentar» lleva `RotateCcw` y «Editar servidor» solo aparece en `AuthFailed`, `HostKeyChanged` y `HostKeyRejected`.
    - **Sin confirmar:** la cancelación con entrada física en macOS, el temporizador real de 7 s fuera de las capturas y el consentimiento de clave de host durante una cancelación real.

21. **Transición al abrir y cerrar Ajustes (hecho, 2026-11-25).** El usuario pidió el 2026-11-24 una transición que dé fluidez al abrir y cerrar Ajustes. Hoy el cambio es instantáneo: `Workspace::render` cambia de `self.content(…)` a `self.render_settings(…)` según `settings_page.is_open()` (`workspace.rs`), y `close_settings` (`settings_view.rs`) devuelve el foco y repinta sin ningún estado intermedio. Aún no hay código. Propuesta, pendiente de confirmar los detalles con el usuario:
    - **Al abrir:** la página de Ajustes aparece con un fundido y un desplazamiento corto de unos 8 px hacia arriba, durante `motion::BASE` (180 ms). Puede usar `motion::appear`.
    - **Al cerrar:** la página se desvanece durante `motion::FAST` (120 ms) sobre el contenido, que ya está pintado debajo, y después se retira. GPUI no anima un elemento que desaparece, así que hace falta un estado «cerrando» en `settings_page`: se sigue pintando la página encima mientras dura el fundido y después se quita. El foco vuelve al instante, sin esperar al final de la animación.
    - Reabrir durante el cierre cancela el fundido de salida y la página vuelve a mostrarse sin saltos.
    - Con «Reducir movimiento» activado (`VSCP_DEV_REDUCE_MOTION=1` para probarlo), el cambio sigue siendo instantáneo.
    - La barra de título de macOS cambia de color con Ajustes (`settings_open` en `workspace.rs`); debe seguir el mismo fundido para que no salte antes que la página.
    - Revisar a 60 fps en el Mac y en Hyprland que no haya parpadeos, y que el contenido de debajo, sobre todo la terminal, no se vuelva a crear al abrir o cerrar.
    - **Solución:** `SettingsPage` añade `opening`, `closing` y `transition_generation`. Al abrir, la página se dibuja sobre el contenido con fundido y subida de 8 px en `motion::BASE` (180 ms); al cerrar, el foco vuelve en cuanto se pulsa y la página sigue pintándose durante `motion::FAST` (120 ms) antes de retirarse. Reabrir durante el cierre incrementa la generación e ignora el temporizador anterior. Con Reduce Motion no hay estados intermedios. Cuando la página está asentada se pinta sola, sin duplicar fondos con vidrio. La barra de título de macOS usa un overlay con la misma generación y fundido.
    - **Sin confirmar:** la fluidez real a 60 fps, posibles parpadeos y el fundido de la barra de título en macOS; las capturas de Hyprland son estáticas y no pueden demostrar la animación.

22. **Sin borde de acento al pulsar un servidor (hecho, 2026-11-25).** Al usuario no le gusta que, al pulsar un servidor de la barra lateral, la tarjeta quede rodeada por el borde de acento. Aún no hay código. Causa: la fila `select-server` de `sidebar.rs` tiene `tab_index(0)` y `.focus(|style| style.border_color(theme.accent))`, así que el clic le da el foco y aparece el anillo. La selección ya se distingue por el fondo `theme.selection`.
    - Al pulsar con el ratón no se dibuja borde: la tarjeta seleccionada solo cambia de fondo.
    - El anillo de foco se mantiene al llegar con el teclado (Tab), para no perder el foco visible que pide la skill `heyrico-ui`. Usar el equivalente de `:focus-visible` si la versión de GPUI fijada lo ofrece (comprobar `focus_visible` en el commit de `Cargo.lock`). Si no, recordar si el foco llegó por teclado o por ratón.
    - Lo mismo para el botón de fijar de la tarjeta (`pin_button`, mismo `.focus` con borde de acento).
    - El borde de acento mientras el menú contextual de esa tarjeta está abierto (`menu_open`) no cambia, porque el usuario no lo mencionó.
    - **Solución:** `gpui-pre` 0.3.11 sí expone `InteractiveElement::focus_visible`, el equivalente a `:focus-visible`. Se usa en `select-server` y en `pin_button` en lugar de `.focus`, sin estado propio en `Workspace`; la tarjeta seleccionada solo cambia de fondo con el ratón y el borde aparece al llegar con Tab. El borde de `menu_open` queda intacto. Prueba GPUI: el clic no activa el modo de entrada por teclado y Tab sí.
    - **Sin confirmar:** la inspección visual del anillo con teclado y ratón físicos en macOS y en el escritorio de Arch.

Las respuestas también están registradas en `.agent/CONTINUITY.md`.
