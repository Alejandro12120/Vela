# CI y Releases de Vela

## 1. Integración continua (`.github/workflows/ci.yml`)

Se ejecuta en cada `push` a cualquier rama y en cada Pull Request:

- **Format** (`ubuntu-latest`): `cargo fmt --all --check`.
- **Build & Test** (`ubuntu-latest` y `macos-15`):
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo clippy --workspace --all-targets --features dev --locked -- -D warnings`
  - `cargo test --workspace --locked`

## 2. Clave de firma (Ed25519)

Las releases se firman con una clave **Ed25519** mediante OpenSSL (`scripts/sign-release.py`). La firma sirve tanto para verificación manual como para el futuro auto-updater.

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

1. Actualiza la versión en `crates/vela-app/Cargo.toml` y `crates/vela-core/Cargo.toml` (ambas deben coincidir exactamente) y actualiza `Cargo.lock` si corresponde.
2. Haz commit y crea el tag `v<versión>`:
   ```sh
   git tag v0.1.0
   git push origin main v0.1.0
   ```
   Si la versión lleva sufijo (por ejemplo `v0.1.0-rc.1`), la release se marca automáticamente como *pre-release*. También puedes probar el flujo desde la pestaña **Actions → Release → Run workflow** con `draft: true`.

### Artefactos generados

- `Vela-<versión>-macos-aarch64.tar.gz` (`Vela.app` con `Info.plist`, icono `Vela.icns` y firma ad-hoc) + `.sig` + `.sshsig`
- `vela-<versión>-linux-x86_64.tar.gz` (`vela`, `LICENSE`, `vela.desktop` con `Icon=vela` e iconos `hicolor` SVG/PNG) + `.sig` + `.sshsig`
- `SHA256SUMS.txt` + `SHA256SUMS.txt.sig` + `SHA256SUMS.txt.sshsig`
- `manifest.json` + `manifest.json.sig` + `manifest.json.sshsig` (esquema `1` pensado para `install.sh` y el futuro auto-updater, consultable vía `.../releases/latest/download/manifest.json`)

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
