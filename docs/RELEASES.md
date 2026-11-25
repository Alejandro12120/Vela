# CI y Releases de vSCP

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
openssl genpkey -algorithm ED25519 -out vscp-release-private.pem
openssl pkey -in vscp-release-private.pem -pubout -out vscp-release-public.pem
```

Guarda `vscp-release-private.pem` en un gestor de contraseñas (por ejemplo Bitwarden) y **nunca** la subas al repositorio. Guarda también `vscp-release-public.pem`: su clave pública de 32 bytes irá embebida en la app cuando se implemente el auto-updater.

### Secreto en GitHub

En el repositorio: **Settings → Secrets and variables → Actions → New repository secret**:

- **Nombre:** `VSCP_SIGNING_KEY`
- **Valor:** el contenido completo de `vscp-release-private.pem` (desde `-----BEGIN PRIVATE KEY-----` hasta `-----END PRIVATE KEY-----`). También se acepta codificado en base64.

## 3. Publicar una release (`.github/workflows/release.yml`)

1. Actualiza la versión en `crates/vscp-app/Cargo.toml` y `crates/vscp-core/Cargo.toml` (ambas deben coincidir exactamente) y actualiza `Cargo.lock` si corresponde.
2. Haz commit y crea el tag `v<versión>`:
   ```sh
   git tag v0.1.0
   git push origin main v0.1.0
   ```
   Si la versión lleva sufijo (por ejemplo `v0.1.0-rc.1`), la release se marca automáticamente como *pre-release*. También puedes probar el flujo desde la pestaña **Actions → Release → Run workflow** con `draft: true`.

### Artefactos generados

- `vSCP-<versión>-macos-aarch64.tar.gz` (`vSCP.app` con `Info.plist` y firma ad-hoc) + `.sig`
- `vscp-<versión>-linux-x86_64.tar.gz` (`vscp`, `LICENSE`, `vscp.desktop`) + `.sig`
- `SHA256SUMS.txt` + `SHA256SUMS.txt.sig`
- `manifest.json` + `manifest.json.sig` (esquema `1` pensado para el futuro auto-updater, consultable vía `.../releases/latest/download/manifest.json`)

### Verificar una descarga manualmente

```sh
openssl pkeyutl -verify -pubin -inkey vscp-release-public.pem \
  -rawin -in manifest.json -sigfile manifest.json.sig
```
