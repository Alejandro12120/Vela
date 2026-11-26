# CI y Releases de Vela

## 1. Integración continua (`.github/workflows/ci.yml`)

Se ejecuta en cada `push` a cualquier rama y en cada Pull Request:

- **Format** (`ubuntu-latest`): `cargo fmt --all --check`.
- **Build & Test** (`ubuntu-latest` y `macos-15`):
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo clippy --workspace --all-targets --features dev --locked -- -D warnings`
  - `cargo test --workspace --locked`

## 2. Clave de firma (Ed25519)

Las releases se firman con una clave **Ed25519** mediante OpenSSL (`scripts/sign-release.py`). La firma sirve tanto para la verificación manual como para las actualizaciones desde Vela.

### Generar el par de claves (una sola vez, en local)

```sh
openssl genpkey -algorithm ED25519 -out vela-release-private.pem
openssl pkey -in vela-release-private.pem -pubout -out vela-release-public.pem
```

Guarda `vela-release-private.pem` en un gestor de contraseñas (por ejemplo Bitwarden) y **nunca** la subas al repositorio. Guarda también `vela-release-public.pem`: su clave pública de 32 bytes irá embebida en la app cuando se implemente el auto-updater.

### Secreto en GitHub

En el repositorio: **Settings → Secrets and variables → Actions → New repository secret**:

- **Nombre:** `VELA_SIGNING_KEY`
- **Valor:** la clave privada Ed25519 **sin frase de paso** en formato PEM PKCS#8 (`-----BEGIN PRIVATE KEY-----`) o formato OpenSSH (`-----BEGIN OPENSSH PRIVATE KEY-----`). También se acepta codificada en base64.

## 3. Publicar una release (`.github/workflows/release.yml`)

Abre **Actions → Release → Run workflow** y elige la rama que quieres publicar.

- **`prerelease`** es el modo predeterminado. Puedes indicar una versión completa como `0.2.0-rc.3` o una base como `0.2.0` para usar el siguiente `rc.N` disponible. Con el campo vacío se usa la base de Cargo y se incrementa el parche si ya existe el tag estable. La versión se aplica durante la compilación de los artefactos. El tag apunta al commit seleccionado y la rama conserva su historial.
- **`release`** publica una versión estable. Indica una versión `X.Y.Z` o deja el campo vacío para usar la base de Cargo. El tag debe estar disponible. Si cambia la versión, el workflow actualiza los dos manifiestos y `Cargo.lock`, publica el commit en la rama seleccionada y crea la release a partir de ese commit. El bot de Actions necesita permiso para hacer push a la rama.
- **`draft`** está desactivado por defecto. Actívalo para revisar la release antes de que aparezca en el instalador o el actualizador.

### Publicar mediante un tag

1. Actualiza la versión en `crates/vela-app/Cargo.toml` y `crates/vela-core/Cargo.toml` (ambas deben coincidir exactamente) y actualiza `Cargo.lock` si corresponde.
2. Haz commit y crea el tag `v<versión>`:
   ```sh
   git tag v0.1.0
   git push origin main v0.1.0
   ```
   Si la versión lleva sufijo (por ejemplo `v0.1.0-rc.1`), la release se marca automáticamente como prerelease.

### Artefactos generados

- `Vela-<versión>-macos-aarch64.tar.gz` (`Vela.app` con `Info.plist`, icono `Vela.icns` y firma ad-hoc) + `.sig` + `.sshsig`
- `vela-<versión>-linux-x86_64.tar.gz` (`vela`, `LICENSE`, `vela.desktop` con `Icon=vela` e iconos `hicolor` SVG/PNG) + `.sig` + `.sshsig`
- `SHA256SUMS.txt` + `SHA256SUMS.txt.sig` + `SHA256SUMS.txt.sshsig`
- `manifest.json` + `manifest.json.sig` + `manifest.json.sshsig` (esquema `1` para `install.sh` y Vela, disponible en `.../releases/latest/download/manifest.json`)

`scripts/sign-release.py --notes-file <archivo.md>` incluye las notas Markdown en el campo opcional `notes` del manifiesto firmado. El flujo habitual genera las notas en GitHub. Si el manifiesto no incluye notas, el canal estable consulta el cuerpo de la release por su etiqueta con un límite de tres segundos. Con **Include Prereleases** activado, Vela usa las notas de la lista de releases de GitHub. Los repositorios espejo configurados con una URL personalizada usan las notas del manifiesto.

Las firmas `.sig` (Ed25519 raw de 64 bytes) y `.sshsig` (OpenSSH SSHSIG, namespace `vela-release`) se generan a partir del mismo secreto `VELA_SIGNING_KEY`. Las claves públicas correspondientes están en [packaging/vela-release.pub](../packaging/vela-release.pub) y [packaging/vela-release-public.pem](../packaging/vela-release-public.pem).

### Verificar una descarga manualmente

Con OpenSSH (funciona igual en macOS y Linux, usado por `install.sh`):

```sh
printf 'vela@release %s\n' "$(cat packaging/vela-release.pub)" > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I vela@release -n vela-release \
  -s manifest.json.sshsig < manifest.json
```

Con OpenSSL 3+ (Linux):

```sh
openssl pkeyutl -verify -pubin -inkey packaging/vela-release-public.pem \
  -rawin -in manifest.json -sigfile manifest.json.sig
```
