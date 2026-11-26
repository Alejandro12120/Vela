#!/bin/sh
# End-to-end verification test for install.sh (and scripts/sign-release.py when
# OpenSSL 3+ and Python 3 are available). Runs on both Linux and macOS.

set -eu

REPO_ROOT="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
INSTALLER="${REPO_ROOT}/install.sh"

say() {
    printf '==> %s\n' "$*"
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

say "Checking POSIX shell syntax of install.sh"
sh -n "$INSTALLER"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/vela-installer-test.XXXXXX")"
trap 'rm -rf -- "$TMP"' EXIT INT TERM

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}:${ARCH}" in
    Linux:x86_64|Linux:amd64)
        PLATFORM="linux-x86_64"
        ASSET="vela-0.9.9-linux-x86_64.tar.gz"
        ;;
    Darwin:arm64|Darwin:aarch64)
        PLATFORM="macos-aarch64"
        ASSET="Vela-0.9.9-macos-aarch64.tar.gz"
        ;;
    *)
        fail "Unsupported test host: ${OS} ${ARCH}"
        ;;
esac

MOCK_DIST="${TMP}/dist"
STAGE="${TMP}/stage"
mkdir -p "$MOCK_DIST" "$STAGE"

build_mock_archive() {
    if [ "$PLATFORM" = "linux-x86_64" ]; then
        pkg="${STAGE}/vela-0.9.9-linux-x86_64"
        mkdir -p "${pkg}/icons/hicolor/scalable/apps" "${pkg}/icons/hicolor/256x256/apps"
        cat > "${pkg}/vela" <<'EOF'
#!/bin/sh
printf 'vela mock 0.9.9\n'
EOF
        chmod 0755 "${pkg}/vela"
        cp "${REPO_ROOT}/packaging/icons/vela.svg" "${pkg}/icons/hicolor/scalable/apps/vela.svg"
        printf 'PNG-MOCK' > "${pkg}/icons/hicolor/256x256/apps/vela.png"
        cat > "${pkg}/vela.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Vela
Comment=Lightweight SSH and SFTP client
Exec=vela
Icon=vela
Terminal=false
Categories=Network;FileTransfer;Utility;
StartupWMClass=vela
EOF
        tar -C "$STAGE" -czf "${MOCK_DIST}/${ASSET}" "vela-0.9.9-linux-x86_64"
    else
        app="${STAGE}/Vela.app"
        mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources"
        cat > "${app}/Contents/MacOS/vela" <<'EOF'
#!/bin/sh
printf 'vela mock 0.9.9\n'
EOF
        chmod 0755 "${app}/Contents/MacOS/vela"
        printf 'ICNS-MOCK' > "${app}/Contents/Resources/Vela.icns"
        cat > "${app}/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>vela</string>
  <key>CFBundleIconFile</key><string>Vela</string>
</dict></plist>
EOF
        tar -C "$STAGE" -czf "${MOCK_DIST}/${ASSET}" "Vela.app"
    fi
}

build_mock_archive

sign_with_python_or_ssh() {
    if command -v python3 >/dev/null 2>&1 \
        && openssl version 2>/dev/null | grep -Eq '^OpenSSL [34]\.'
    then
        say "Signing mock release via scripts/sign-release.py (OpenSSL PKCS#8 key)"
        openssl genpkey -algorithm ED25519 -out "${TMP}/priv.pem" >/dev/null 2>&1
        chmod 600 "${TMP}/priv.pem"
        VELA_SIGNING_KEY="$(cat "${TMP}/priv.pem")" \
            python3 "${REPO_ROOT}/scripts/sign-release.py" \
            --dir "$MOCK_DIST" \
            --repo "Alejandro12120/Vela" \
            --version "0.9.9" \
            --tag "v0.9.9" \
            --platform "${PLATFORM}=${ASSET}" > "${TMP}/sign.log"
        PUBKEY="$(awk -F': ' '/^Ed25519 public key \(OpenSSH\): /{print $2}' "${TMP}/sign.log")"
        [ -n "$PUBKEY" ] || fail "sign-release.py did not print OpenSSH public key"

        say "Testing scripts/sign-release.py with OpenSSH Ed25519 private key"
        OPENSSH_DIST="${TMP}/dist-openssh"
        mkdir -p "$OPENSSH_DIST"
        cp "${MOCK_DIST}/${ASSET}" "${OPENSSH_DIST}/${ASSET}"
        ssh-keygen -q -t ed25519 -N "" -f "${TMP}/id_ed25519"
        VELA_SIGNING_KEY="$(cat "${TMP}/id_ed25519")" \
            python3 "${REPO_ROOT}/scripts/sign-release.py" \
            --dir "$OPENSSH_DIST" \
            --repo "Alejandro12120/Vela" \
            --version "0.9.9" \
            --tag "v0.9.9" \
            --platform "${PLATFORM}=${ASSET}" > "${TMP}/sign-openssh.log"
        OPENSSH_PUB="$(awk -F': ' '/^Ed25519 public key \(OpenSSH\): /{print $2}' "${TMP}/sign-openssh.log")"
        EXPECTED_OPENSSH_PUB="$(awk '{print $1 " " $2}' "${TMP}/id_ed25519.pub")"
        [ "$OPENSSH_PUB" = "$EXPECTED_OPENSSH_PUB" ] \
            || fail "OpenSSH key public mismatch: ${OPENSSH_PUB} != ${EXPECTED_OPENSSH_PUB}"
    else
        say "Signing mock release via ssh-keygen -Y sign"
        ssh-keygen -q -t ed25519 -N "" -f "${TMP}/test_key"
        PUBKEY="$(awk '{print $1 " " $2}' "${TMP}/test_key.pub")"
        digest="$(sha256_file "${MOCK_DIST}/${ASSET}")"
        size="$(wc -c < "${MOCK_DIST}/${ASSET}" | tr -d ' ')"
        ssh-keygen -Y sign -f "${TMP}/test_key" -n vela-release \
            < "${MOCK_DIST}/${ASSET}" > "${MOCK_DIST}/${ASSET}.sshsig"
        cat > "${MOCK_DIST}/manifest.json" <<EOF
{
  "schema": 1,
  "version": "0.9.9",
  "tag": "v0.9.9",
  "published_at": "2026-11-25T00:00:00Z",
  "notes_url": "https://github.com/Alejandro12120/Vela/releases/tag/v0.9.9",
  "platforms": {
    "other-platform": {
      "asset": "other.tar.gz",
      "url": "https://github.com/Alejandro12120/Vela/releases/download/v0.9.9/other.tar.gz",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
      "size": 1,
      "signature": "mock"
    },
    "${PLATFORM}": {
      "asset": "${ASSET}",
      "url": "https://github.com/Alejandro12120/Vela/releases/download/v0.9.9/${ASSET}",
      "sha256": "${digest}",
      "size": ${size},
      "signature": "mock"
    }
  }
}
EOF
        ssh-keygen -Y sign -f "${TMP}/test_key" -n vela-release \
            < "${MOCK_DIST}/manifest.json" > "${MOCK_DIST}/manifest.json.sshsig"
    fi
}

sign_with_python_or_ssh

# 1. Valid installation via piped stdin (matches curl | sh usage)
DEST_HOME="${TMP}/home"
mkdir -p "$DEST_HOME"
say "Testing valid install (piped to sh)"
if [ "$PLATFORM" = "linux-x86_64" ]; then
    HOME="$DEST_HOME" \
    VELA_INSTALL_DIR="${DEST_HOME}/bin with space" \
    VELA_DATA_DIR="${DEST_HOME}/share" \
    VELA_RELEASE_BASE_URL="file://${MOCK_DIST}" \
    VELA_SIGNING_PUBKEY="$PUBKEY" \
    sh < "$INSTALLER"

    [ -x "${DEST_HOME}/bin with space/vela" ] || fail "Linux binary missing"
    out="$("${DEST_HOME}/bin with space/vela")"
    [ "$out" = "vela mock 0.9.9" ] || fail "Unexpected binary output: $out"
    [ -f "${DEST_HOME}/share/applications/vela.desktop" ] || fail "vela.desktop missing"
    grep -Fq "Exec=\"${DEST_HOME}/bin with space/vela\"" "${DEST_HOME}/share/applications/vela.desktop" \
        || fail "vela.desktop Exec line was not quoted/rewritten properly"
    grep -Fq "Icon=vela" "${DEST_HOME}/share/applications/vela.desktop" \
        || fail "vela.desktop missing Icon=vela"
    [ -f "${DEST_HOME}/share/icons/hicolor/scalable/apps/vela.svg" ] || fail "SVG icon missing"
    [ -f "${DEST_HOME}/share/icons/hicolor/256x256/apps/vela.png" ] || fail "PNG icon missing"
else
    HOME="$DEST_HOME" \
    VELA_APPS_DIR="${DEST_HOME}/Applications" \
    VELA_RELEASE_BASE_URL="file://${MOCK_DIST}" \
    VELA_SIGNING_PUBKEY="$PUBKEY" \
    sh < "$INSTALLER"

    [ -x "${DEST_HOME}/Applications/Vela.app/Contents/MacOS/vela" ] || fail "macOS binary missing"
    out="$("${DEST_HOME}/Applications/Vela.app/Contents/MacOS/vela")"
    [ "$out" = "vela mock 0.9.9" ] || fail "Unexpected binary output: $out"
    [ -f "${DEST_HOME}/Applications/Vela.app/Contents/Resources/Vela.icns" ] || fail "Vela.icns missing"
fi

# 2. Wrong public key must fail closed
say "Testing rejection with wrong public key"
ssh-keygen -q -t ed25519 -N "" -f "${TMP}/other_key"
WRONG_PUBKEY="$(awk '{print $1 " " $2}' "${TMP}/other_key.pub")"
BAD_HOME="${TMP}/bad-key-home"
mkdir -p "$BAD_HOME"
if HOME="$BAD_HOME" \
    VELA_INSTALL_DIR="${BAD_HOME}/bin" \
    VELA_DATA_DIR="${BAD_HOME}/share" \
    VELA_APPS_DIR="${BAD_HOME}/Applications" \
    VELA_RELEASE_BASE_URL="file://${MOCK_DIST}" \
    VELA_SIGNING_PUBKEY="$WRONG_PUBKEY" \
    sh "$INSTALLER" >/dev/null 2>&1
then
    fail "Installer accepted manifest signed by a different key!"
fi
[ ! -e "${BAD_HOME}/bin/vela" ] && [ ! -e "${BAD_HOME}/Applications/Vela.app" ] \
    || fail "Failed key check left files behind"

# 3. Tampered manifest must fail closed
say "Testing rejection of tampered manifest.json"
TAMPER_MANIFEST="${TMP}/tamper-manifest"
cp -R "$MOCK_DIST" "$TAMPER_MANIFEST"
printf ' \n' >> "${TAMPER_MANIFEST}/manifest.json"
if HOME="$BAD_HOME" \
    VELA_INSTALL_DIR="${BAD_HOME}/bin" \
    VELA_DATA_DIR="${BAD_HOME}/share" \
    VELA_APPS_DIR="${BAD_HOME}/Applications" \
    VELA_RELEASE_BASE_URL="file://${TAMPER_MANIFEST}" \
    VELA_SIGNING_PUBKEY="$PUBKEY" \
    sh "$INSTALLER" >/dev/null 2>&1
then
    fail "Installer accepted tampered manifest.json!"
fi

# 4. Tampered archive must fail closed
say "Testing rejection of tampered archive"
TAMPER_ARCHIVE="${TMP}/tamper-archive"
cp -R "$MOCK_DIST" "$TAMPER_ARCHIVE"
printf 'x' >> "${TAMPER_ARCHIVE}/${ASSET}"
if HOME="$BAD_HOME" \
    VELA_INSTALL_DIR="${BAD_HOME}/bin" \
    VELA_DATA_DIR="${BAD_HOME}/share" \
    VELA_APPS_DIR="${BAD_HOME}/Applications" \
    VELA_RELEASE_BASE_URL="file://${TAMPER_ARCHIVE}" \
    VELA_SIGNING_PUBKEY="$PUBKEY" \
    sh "$INSTALLER" >/dev/null 2>&1
then
    fail "Installer accepted tampered archive!"
fi

# 5. Embedded public key matches packaging/vela-release.pub
say "Verifying embedded public key matches packaging/vela-release.pub"
EXPECTED_PUB="$(tr -d '\r\n' < "${REPO_ROOT}/packaging/vela-release.pub")"
grep -Fq "VELA_DEFAULT_PUBKEY=\"${EXPECTED_PUB}\"" "$INSTALLER" \
    || fail "install.sh embedded public key does not match packaging/vela-release.pub"

if command -v python3 >/dev/null 2>&1; then
    say "Testing prerelease discovery, numeric RC ordering and explicit version precedence"
    RC_DIST="${TMP}/dist-rc"
    RC_TAG=v0.10.0-rc.10
    if [ -d "${TMP}/dist-openssh" ]; then
        cp -R "${TMP}/dist-openssh" "$RC_DIST"
        RC_KEY="${TMP}/id_ed25519"
        RC_PUB="$OPENSSH_PUB"
    else
        cp -R "$MOCK_DIST" "$RC_DIST"
        RC_KEY="${TMP}/test_key"
        RC_PUB="$PUBKEY"
    fi
    sed "s/\"0\.9\.9\"/\"${RC_TAG#v}\"/; s/v0\.9\.9/$RC_TAG/g" \
        "${RC_DIST}/manifest.json" > "${TMP}/rc-manifest.json"
    mv "${TMP}/rc-manifest.json" "${RC_DIST}/manifest.json"
    ssh-keygen -Y sign -f "$RC_KEY" -n vela-release \
        < "${RC_DIST}/manifest.json" > "${RC_DIST}/manifest.json.sshsig"

    python3 - "$RC_TAG" > "${TMP}/releases.json" <<'PY'
import json
import sys


def release(tag, draft=False, signed=True):
    assets = [{"name": "manifest.json"}]
    if signed:
        assets.append({"name": "manifest.json.sshsig"})
    return {"tag_name": tag, "draft": draft, "assets": assets}


json.dump([
    release("v0.10.0-rc.2"),
    release("v9.0.0", draft=True),
    release("v8.0.0", signed=False),
    release("v7.0.0-rc.01"),
    release(sys.argv[1]),
    release("v0.9.9"),
], sys.stdout)
PY

    mkdir "${TMP}/mock-bin"
    cat > "${TMP}/mock-bin/curl" <<'EOF'
#!/bin/sh
set -eu
while [ "$#" -gt 1 ]; do
    if [ "$1" = "-o" ]; then
        dest="$2"
        shift 2
    else
        shift
    fi
done
case "$1" in
    "https://api.github.com/repos/Alejandro12120/Vela/releases?per_page=20")
        [ "$MOCK_TAG" != "v0.9.9" ] || exit 1
        cp "$MOCK_RELEASES" "$dest"
        ;;
    "https://github.com/Alejandro12120/Vela/releases/download/${MOCK_TAG}/"*)
        cp "${MOCK_PAYLOAD}/${1##*/}" "$dest"
        ;;
    *)
        printf 'Unexpected URL: %s\n' "$1" >&2
        exit 1
        ;;
esac
EOF
    chmod +x "${TMP}/mock-bin/curl"

    mock_install() {
        PATH="${TMP}/mock-bin:$PATH" \
        MOCK_TAG="$tag" MOCK_PAYLOAD="$payload" MOCK_RELEASES="${TMP}/releases.json" \
        VELA_VERSION="" VELA_RELEASE_BASE_URL="" VELA_SIGNING_PUBKEY="$key" \
        VELA_INSTALL_DIR="${TMP}/rc-bin" VELA_DATA_DIR="${TMP}/rc-share" \
        VELA_APPS_DIR="${TMP}/rc-apps" \
        sh -s -- "$@" < "$INSTALLER"
    }

    tag="$RC_TAG" payload="$RC_DIST" key="$RC_PUB"
    result="$(mock_install -p)"
    printf '%s\n' "$result" | grep -Fq "Vela ${RC_TAG#v} installed" \
        || fail "The prerelease flag did not install the highest signed RC."
    result="$(VELA_PRERELEASE=1 mock_install)"
    printf '%s\n' "$result" | grep -Fq "Vela ${RC_TAG#v} installed" \
        || fail "VELA_PRERELEASE did not select the highest signed RC."
    if key="$WRONG_PUBKEY" mock_install --prerelease >/dev/null 2>&1; then
        fail "Prerelease mode accepted a manifest signed by a different key."
    fi
    tag=v0.9.9 payload="$MOCK_DIST" key="$PUBKEY"
    result="$(mock_install --prerelease --version 0.9.9)"
    printf '%s\n' "$result" | grep -Fq "Vela 0.9.9 installed" \
        || fail "An explicit version did not take precedence."
fi

say "All installer checks passed!"
