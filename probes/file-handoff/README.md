# Prueba de entrega de archivos

Comprueba si el gestor de archivos acepta un archivo local ya descargado a caché. La prueba usa GTK 4 en Linux y AppKit en macOS. La conexión SFTP y la interfaz definitiva se validarán por separado.

## Ejecutar en una sesión gráfica

En Linux se necesita un compilador C, `pkg-config` y GTK 4 con sus cabeceras. En macOS se necesita `swiftc` con el SDK de macOS.

```bash
bash probes/file-handoff/run.sh
```

1. Con la ventana enfocada, pulsa **Copiar archivo**, **Ctrl+C** en Linux o **⌘C** en macOS.
2. Pega en una carpeta vacía de Dolphin o Finder. Comprueba que aparece `Prueba vSCP #1.txt` y que contiene `Prueba de transferencia de archivos vSCP`.
3. Arrastra el texto de la ventana a otra carpeta vacía. Comprueba el nombre y el contenido de la copia.
4. Mantén la prueba abierta hasta terminar. Al cerrarla se borran el ejecutable y el archivo de ejemplo.

Puedes pasar un archivo existente como argumento. El script solo elimina su propio directorio temporal. En una sesión de HyDE, usa `GDK_BACKEND=wayland` para probar Wayland y `GDK_BACKEND=x11` para probar X11 o XWayland si está disponible. Anota el backend y el resultado de cada gesto.

## Verificar compilación de Linux en Docker

Desde la raíz del proyecto, con Docker disponible y acceso al repositorio de paquetes de la imagen:

```bash
docker run --rm --memory=512m --cpus=1 \
  --mount "type=bind,src=$PWD/probes/file-handoff,dst=/src,readonly" \
  -e DEBIAN_FRONTEND=noninteractive python:3.12-slim sh -ec '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends gcc libc6-dev pkg-config libgtk-4-dev
    cc -std=c17 -Wall -Wextra -Werror -O2 $(pkg-config --cflags gtk4) \
      /src/linux.c -o /tmp/file-handoff $(pkg-config --libs gtk4)
    /tmp/file-handoff && exit 1
    test "$?" -eq 2
  '
```

El contenedor elimina los paquetes y el binario al salir. Esta comprobación cubre la compilación y el tratamiento de argumentos.
