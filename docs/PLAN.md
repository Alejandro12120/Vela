# Plan del MVP de vSCP

Estado: aprobado para empezar por F0, 2026-11-20. Todavía no hay código de la aplicación. Las prioridades siguen siendo, por orden, RAM, arranque instantáneo y fluidez, y tamaño instalado.

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
- Lo que GPUI **no** resuelve: escribir archivos en el portapapeles. En macOS, `write` ignora `ExternalPaths`. Se añadirá un módulo pequeño con `objc2-app-kit` que escriba `NSURL` en `NSPasteboard`, igual que hace `probes/file-handoff/macos.swift`, que ya funcionó en Finder. En Linux quedará pendiente para la fase de Linux.
- Si el MacBook no tiene Xcode completo, se activará la feature `runtime_shaders` para no depender del compilador Metal. Está por confirmar.
- Sin `gpui-component` en el MVP. La 0.9.0 depende de `gpui-pre =0.3.11` y trae árbol y tabla, pero arrastra gráficas, resaltado, `lsp-types`, `ropey` y otras dependencias. Se implementarán solo los controles necesarios: fila de árbol, barra de ruta, botón, campo de texto, diálogo y barra de progreso. Se reconsiderará si el campo de texto resulta costoso, midiendo antes el binario y la RAM.

### Conexión: OpenSSH del sistema

- Todas las conexiones pasan por el binario `ssh` del sistema. Así se respetan `~/.ssh/config`, `known_hosts`, ProxyJump y el agente sin reimplementarlos.
- Hay una conexión maestra por servidor: `ssh -M -N` con `ControlPath` propio y `ControlPersist`. La contraseña o la aprobación del agente se piden una sola vez por sesión.
- SFTP: `ssh -o ControlPath=… -s <destino> sftp` como proceso hijo. Su stdin y stdout se pasan a `openssh_sftp_client::Sftp::new(stdin, stdout, opciones)`, con el crate `openssh-sftp-client` 0.15.x en la última versión que tenga al menos dos semanas. No se usa el crate `openssh` porque hace falta controlar directamente las variables de askpass y las opciones.
- Terminal: se abre la terminal configurada con `ssh` y el mismo `ControlPath`, sin volver a autenticarse.
- `openssh-sftp-client` necesita tokio. Se ejecutará un runtime tokio `current_thread` en un único hilo de red. La interfaz se comunica con él mediante canales (`async-channel`). GPUI recibe los eventos en una tarea `cx.spawn`. Nada de red se ejecuta en el hilo de la interfaz.

### Opciones que vSCP impone por servidor

```text
-o IdentitiesOnly=yes -o IdentityFile=<clave pública elegida> -o IdentityAgent=<socket de Bitwarden>
-o ControlMaster=auto -o ControlPath=<dir corto>/%C -o ControlPersist=10m
-o StrictHostKeyChecking=ask -p <puerto> <usuario>@<host>
```

- `IdentityAgent` hace falta porque una app lanzada desde el Dock no hereda el `SSH_AUTH_SOCK` de la shell. vSCP detecta el socket de Bitwarden en las rutas conocidas y deja configurarlo a mano. Antes de implementarlo, se comprobarán las rutas en la documentación de Bitwarden.
- `IdentityFile` en `ssh_config` se acumula. Si el servidor tiene claves en `~/.ssh/config`, `IdentitiesOnly` también las ofrecería. Al guardar el servidor, vSCP ejecuta `ssh -G` con las mismas opciones y avisa si la configuración efectiva contiene más de una identidad.
- `ControlPath` debe ser corto. El `TMPDIR` de macOS (`/var/folders/…`) puede superar el límite de 104 bytes de `sun_path`. Se usará `/tmp/vscp-<uid>/` con permisos 0700.

### Contraseñas y claves de host

- `SSH_ASKPASS` apunta al propio ejecutable (`vscp askpass`), con `SSH_ASKPASS_REQUIRE=force`. Ese proceso pasa la petición a la aplicación por un socket Unix del directorio privado 0700. La aplicación muestra un diálogo y devuelve la respuesta. La contraseña no se guarda.
- Con `StrictHostKeyChecking=ask`, OpenSSH también usa askpass para aceptar la primera clave de host. Ese diálogo muestra la huella. Si la clave cambia, `ssh` rechaza la conexión. vSCP muestra el aviso de stderr y no ofrece aceptarla.

## 3. Arquitectura

```text
crates/
  vscp-core/   sin GPUI: configuración, procesos ssh, sesión SFTP, cola de transferencias,
               sesiones de edición, caché. Se prueba en Linux dentro de un contenedor.
  vscp-app/    GPUI: ventana, paneles, diálogos, tema y glue de plataforma (macOS: pasteboard).
```

- **Configuración**: `servers.toml`, en `~/Library/Application Support/vSCP/` o `~/.config/vscp/`. Por servidor: nombre, host, puerto, usuario, modo de autenticación (`agent` o `password`), ruta de la clave pública, ruta remota inicial y favorito. También se guardan los ajustes de terminal, editor y socket del agente.
- **Modelo de árbol**: cada panel guarda un árbol de nodos con hijos cargados de forma perezosa. La interfaz recibe un `Vec` aplanado de filas visibles, con profundidad y estado expandido, y lo pinta con `uniform_list`. Las carpetas expandidas se recuerdan por servidor.
- **Caché**: hay un directorio de sesión para descargas temporales de arrastre y portapapeles, y otro para copias de edición. Se acota por tamaño y se limpia al cerrar, salvo las copias con cambios sin subir.
- **Tema**: los tokens de `.agent/skills/heyrico-ui/assets/tokens.css` se pasan a constantes Rust. Habrá modo claro y oscuro, fuente del sistema, base de 13 px y radios de 6 y 8 px.

## 4. Alcance del MVP

### Entra

1. Lista de servidores en la barra lateral: crear, editar, borrar y conectar. Incluye el estado de la sesión activa.
2. Autenticación con el agente de Bitwarden usando una clave elegida por servidor, o con contraseña pedida al conectar. Se acepta la clave de host en el primer uso.
3. Dos paneles, local a la izquierda y remoto a la derecha:
   - barra de ruta editable;
   - atrás, adelante y subir;
   - carpetas expandibles en el sitio, al estilo de Finder;
   - columnas de nombre, tamaño, fecha y permisos;
   - selección múltiple;
   - archivos ocultos ocultables;
   - refrescar.
4. Transferencias en los dos sentidos, recursivas. Se pueden iniciar arrastrando entre paneles o con un botón. La cola muestra progreso y permite cancelar. Ante un conflicto se puede sobrescribir, omitir o renombrar.
5. Operaciones de archivo en los dos lados: crear carpeta, renombrar y borrar con confirmación.
6. Integración con Finder:
   - arrastrar desde Finder al panel remoto sube los archivos;
   - ⌘V en el panel remoto sube los archivos del portapapeles;
   - ⌘C en el panel remoto descarga a caché y publica la URL;
   - arrastrar del panel remoto a Finder empieza a descargar en cuanto empieza el arrastre. Se ofrece el archivo al salir de la ventana si ya está completo (limitación aceptada, sección 8).
7. Edición remota con doble clic:
   - descarga a una copia local;
   - abre la app predeterminada (`open`) o el editor configurado;
   - vigila los cambios con `notify` 8.2.x y espera 300 ms entre eventos. Así se cubren el guardado atómico y los guardados seguidos;
   - antes de subir, compara la fecha y el tamaño remotos con los descargados. Si cambiaron, ofrece sobrescribir, guardar como copia o cancelar;
   - sube sobrescribiendo el archivo original, para conservar el inodo, los permisos y el propietario;
   - la barra de estado muestra el resultado de cada subida.
8. Abrir una terminal SSH sobre la sesión activa, con un comando configurable.
9. Reconexión manual si la sesión maestra cae. Las ediciones pendientes no se pierden.

### No entra en el MVP

- Cambiar permisos o propietario. Solo se muestran.
- Reanudar transferencias interrumpidas.
- Proveer el archivo arrastrado mediante promesas, sin descargarlo antes.
- Guardar contraseñas en el llavero.
- Linux.
- Windows.
- Varias pestañas por servidor.
- Sincronizar carpetas.
- Los enlaces simbólicos se muestran con su destino. Las carpetas enlazadas se abren y no se editan como enlace.

## 5. Fases

Cada fase termina con compilación de release en el Mac, `cargo fmt --check`, `cargo clippy -D warnings` y pruebas. La continuidad se actualiza al final de cada fase. Las compilaciones siguen las normas de `AGENTS.md`: un trabajo, sin compilación incremental, `CARGO_TARGET_DIR` desechable y limpieza al terminar.

| Fase | Contenido | Criterio de aceptación |
| --- | --- | --- |
| F0 Andamiaje | Workspace con dos crates. `rust-toolchain.toml` fijado después de comprobar la versión mínima de `gpui-pre`. Ventana GPUI vacía con el tema. | La ventana abre en macOS. Se registran la RAM de reposo, el tiempo hasta la ventana y el tamaño del binario. Sirve de referencia frente a los presupuestos (sección 6). |
| F1 Núcleo SSH/SFTP | `vscp-core` con la conexión maestra, askpass, SFTP sobre `ssh -s`, listado, `stat`, lectura y escritura en streaming. Comprobación `ssh -G` de identidades. | Pruebas de integración contra un `sshd` de usuario: puerto alto, claves de host y de cliente generadas, `StrictHostKeyChecking`. En macOS se usa `/usr/sbin/sshd` sin root. En Linux se usa un contenedor `--rm`. Además, un caso con contraseña. |
| F2 Paneles | Panel local y remoto con árbol virtualizado, navegación, columnas y selección. Barra lateral de servidores y `servers.toml`. | 10.000 filas con desplazamiento fluido. Expandir no bloquea la interfaz con latencia de red simulada. |
| F3 Transferencias y operaciones | Cola, arrastre entre paneles, conflictos, crear carpeta, renombrar y borrar. | Subida y bajada recursivas de un árbol de prueba con nombres con espacios, `#` y acentos. El contenido coincide byte a byte. Cancelar deja el estado coherente. |
| F4 Finder | Arrastre de entrada y de salida, ⌘C y ⌘V. Módulo `objc2` de pasteboard. | Prueba manual con trackpad en Finder: el nombre y los bytes coinciden en los cuatro gestos. |
| F5 Edición remota | Copias de edición, `open` o editor, vigilancia, comprobación de conflicto y subida. | Con VS Code: guardar dos veces seguidas sube dos veces. Un cambio remoto hecho entretanto muestra el conflicto. Desconectar conserva la copia. |
| F6 Terminal y pulido | Terminal externa, reconexión, mensajes de error, modo oscuro y revisión de accesibilidad con la skill `apple-design`. | Recorrido completo con Bitwarden real y una clave elegida, sin agotar intentos de autenticación. Mediciones finales. |
| F7 Linux (post-MVP) | Backend Wayland y Dolphin: arrastre de salida soportado, pero portapapeles de archivos pendiente. | Lo mismo que F4 en HyDE. |

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

1. Terminal en macOS: Kitty. Comando por defecto: `kitty ssh <opciones de vSCP> <destino>`, lanzado con `open -na kitty --args …`. Se puede configurar.
2. Los servidores se guardan solo en `servers.toml` de vSCP. No se importan los `Host` de `~/.ssh/config`. Ese archivo se sigue leyendo porque `ssh` lo carga, y la comprobación con `ssh -G` sigue siendo necesaria.
3. Se acepta en el MVP que el arrastre de un archivo remoto grande a Finder requiera haberlo descargado antes.
4. Linux queda para después del MVP en macOS (F7).

Las respuestas también están registradas en `.agent/CONTINUITY.md`.
