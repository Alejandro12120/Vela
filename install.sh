#!/bin/sh
# Vela installer for macOS (Apple Silicon) and Linux (x86_64, Wayland).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Alejandro12120/Vela/main/install.sh | sh
#
# Optional environment variables:
#   VELA_VERSION           Install a specific release tag (e.g. v0.1.0). Defaults to latest stable.
#   VELA_PRERELEASE=1      Include prereleases when choosing the latest version (requires Python 3).
#   VELA_INSTALL_DIR       Linux binary directory (default: ~/.local/bin).
#   VELA_DATA_DIR          Linux XDG data directory (default: ~/.local/share).
#   VELA_APPS_DIR          macOS Applications directory (default: /Applications).
#   VELA_RELEASE_BASE_URL  Override release download base URL (mirrors / testing).
#   VELA_SIGNING_PUBKEY    Override embedded Ed25519 OpenSSH public key.

set -eu

VELA_REPO="Alejandro12120/Vela"
VELA_SIGNER_IDENTITY="vela@release"
VELA_SIGNER_NAMESPACE="vela-release"
VELA_DEFAULT_PUBKEY="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAgeCkjVNnR0gfJF3QeavTEs2HlrvT0cWrBrU3oc3dko"

say() {
    printf '==> %s\n' "$*"
}

warn() {
    printf 'Warning: %s\n' "$*" >&2
}

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Vela installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/Alejandro12120/Vela/main/install.sh | sh
  sh install.sh [--version <tag>] [--prerelease]

Options:
  -v, --version <tag>  Install a specific release version or tag (e.g. 0.1.0 or v0.1.0)
  -p, --prerelease     Include prereleases when choosing the latest version (requires Python 3)
  -h, --help           Show this help message
EOF
}

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        die "Required tool '$1' was not found in PATH."
    fi
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -v|--version)
                [ "$#" -ge 2 ] || die "Option $1 requires a version argument."
                VELA_VERSION="$2"
                shift 2
                ;;
            -p|--prerelease)
                VELA_PRERELEASE=1
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "Unknown argument: $1 (try --help)"
                ;;
        esac
    done
}

latest_release_tag() {
    need_cmd python3
    curl -q -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 \
        --max-time 60 --max-filesize 4194304 \
        --header 'Accept: application/vnd.github+json' --user-agent 'Vela installer' \
        -o "$1" "https://api.github.com/repos/${VELA_REPO}/releases?per_page=20" \
        || die "Failed to fetch the GitHub release list."
    python3 - "$1" <<'PY'
import json
import re
import sys

number = r"(0|[1-9][0-9]*)"
identifier = r"[0-9A-Za-z-]+"
semver = re.compile(
    rf"v?{number}\.{number}\.{number}(?:-({identifier}(?:\.{identifier})*))?"
    rf"(?:\+{identifier}(?:\.{identifier})*)?"
)


def version_key(tag):
    match = semver.fullmatch(tag)
    if not match:
        return None
    pre = match[4].split(".") if match[4] else []
    if any(part.isdecimal() and len(part) > 1 and part.startswith("0") for part in pre):
        return None
    identifiers = tuple((0, int(part)) if part.isdecimal() else (1, part) for part in pre)
    return tuple(map(int, match.groups()[:3])), not pre, identifiers


with open(sys.argv[1]) as source:
    releases = json.load(source)

candidates = []
for release in releases:
    if release["draft"]:
        continue
    assets = {asset["name"] for asset in release["assets"]}
    if not {"manifest.json", "manifest.json.sshsig"} <= assets:
        continue
    tag = release["tag_name"]
    key = version_key(tag)
    if key is not None:
        candidates.append((key, tag))

if not candidates:
    sys.exit("No published Vela release has a signed manifest.")
print(max(candidates, key=lambda candidate: candidate[0])[1])
PY
}

detect_platform() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Darwin)
            case "$arch" in
                arm64|aarch64)
                    PLATFORM="macos-aarch64"
                    OS_KIND="macos"
                    ;;
                x86_64)
                    die "Vela requires Apple Silicon (arm64) on macOS; Intel Macs are not supported."
                    ;;
                *)
                    die "Unsupported macOS architecture: $arch"
                    ;;
            esac
            ;;
        Linux)
            case "$arch" in
                x86_64|amd64)
                    PLATFORM="linux-x86_64"
                    OS_KIND="linux"
                    ;;
                *)
                    die "Prebuilt Linux releases are currently only available for x86_64 (detected: $arch). You can build from source with Cargo."
                    ;;
            esac
            ;;
        *)
            die "Unsupported operating system: $os (Vela supports macOS Apple Silicon and Linux x86_64)."
            ;;
    esac
}

download() {
    url="$1"
    dest="$2"
    if [ -n "${VELA_RELEASE_BASE_URL:-}" ]; then
        curl -fsSL --retry 3 --retry-delay 1 -o "$dest" "$url" \
            || die "Failed to download $url"
    else
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 1 -o "$dest" "$url" \
            || die "Failed to download $url"
    fi
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "Neither 'sha256sum' nor 'shasum' is available to verify checksums."
    fi
}

verify_sshsig() {
    file_path="$1"
    sig_path="$2"
    allowed_signers="$3"
    label="$4"

    if ! ssh-keygen -Y verify \
        -f "$allowed_signers" \
        -I "$VELA_SIGNER_IDENTITY" \
        -n "$VELA_SIGNER_NAMESPACE" \
        -s "$sig_path" < "$file_path" >/dev/null 2>&1
    then
        die "Ed25519 signature verification failed for ${label}. Installation aborted."
    fi
}

parse_manifest_top() {
    manifest="$1"
    key="$2"
    awk -v k="\"$key\"" '
        BEGIN { depth = 0 }
        {
            if (depth == 1 && index($0, k) > 0) {
                line = $0
                sub(".*" k "[[:space:]]*:[[:space:]]*\"", "", line)
                sub("\".*", "", line)
                print line
                exit
            }
            len = length($0)
            for (i = 1; i <= len; i++) {
                ch = substr($0, i, 1)
                if (ch == "{") depth++
                else if (ch == "}") depth--
            }
        }
    ' "$manifest"
}

parse_manifest_platform() {
    manifest="$1"
    platform="$2"
    key="$3"
    awk -v plat="\"$platform\"" -v k="\"$key\"" '
        BEGIN { in_plat = 0 }
        {
            if (!in_plat && index($0, plat) > 0 && index($0, "{") > 0) {
                in_plat = 1
                next
            }
            if (in_plat) {
                if (index($0, "}") > 0) {
                    exit
                }
                if (index($0, k) > 0) {
                    line = $0
                    sub(".*" k "[[:space:]]*:[[:space:]]*\"", "", line)
                    sub("\".*", "", line)
                    print line
                    exit
                }
            }
        }
    ' "$manifest"
}

validate_asset_name() {
    case "$1" in
        ""|*/*|*..*)
            die "Invalid archive name in manifest: '$1'"
            ;;
    esac
    case "$1" in
        *[!A-Za-z0-9._+-]*)
            die "Unexpected characters in archive name: '$1'"
            ;;
    esac
}

validate_sha256() {
    case "$1" in
        *[!0-9a-f]*)
            die "Invalid SHA-256 digest in manifest: '$1'"
            ;;
    esac
    if [ "${#1}" -ne 64 ]; then
        die "Invalid SHA-256 digest length in manifest: '$1'"
    fi
}

quote_desktop_exec() {
    # Per the freedesktop Desktop Entry Specification, wrap paths containing
    # whitespace or reserved characters in double quotes with backslash escapes.
    path="$1"
    case "$path" in
        *[[:space:]\"\'\\\`\$\<\>\~\|\&\;\*\?\#\(\)]*)
            escaped="$(printf '%s' "$path" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\$/\\$/g; s/`/\\`/g')"
            printf '"%s"' "$escaped"
            ;;
        *)
            printf '%s' "$path"
            ;;
    esac
}

install_macos() {
    extract_dir="$1"
    src_app="${extract_dir}/Vela.app"
    [ -d "$src_app" ] && [ -x "$src_app/Contents/MacOS/vela" ] \
        || die "Archive did not contain a valid Vela.app bundle."

    if [ -n "${VELA_APPS_DIR:-}" ]; then
        apps_dir="$VELA_APPS_DIR"
        mkdir -p "$apps_dir" 2>/dev/null || true
    else
        apps_dir="/Applications"
    fi

    use_sudo=0
    if [ ! -w "$apps_dir" ]; then
        if [ -z "${VELA_APPS_DIR:-}" ]; then
            if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
                use_sudo=1
            elif command -v sudo >/dev/null 2>&1 && [ -r /dev/tty ]; then
                say "Administrator permission is needed to install into /Applications."
                if sudo -v < /dev/tty; then
                    use_sudo=1
                else
                    apps_dir="${HOME}/Applications"
                    mkdir -p "$apps_dir"
                    warn "Falling back to ${apps_dir}."
                fi
            else
                apps_dir="${HOME}/Applications"
                mkdir -p "$apps_dir"
                warn "/Applications is not writable; installing to ${apps_dir}."
            fi
        else
            die "Target directory '${apps_dir}' is not writable."
        fi
    fi

    dest_app="${apps_dir}/Vela.app"
    staged_app="${apps_dir}/.Vela.app.new.$$"
    backup_app="${apps_dir}/.Vela.app.old.$$"

    say "Installing Vela.app to ${dest_app}..."
    if [ "$use_sudo" -eq 1 ]; then
        sudo rm -rf -- "$staged_app" "$backup_app"
        sudo ditto "$src_app" "$staged_app"
        if [ -e "$dest_app" ]; then
            sudo mv -- "$dest_app" "$backup_app"
        fi
        if ! sudo mv -- "$staged_app" "$dest_app"; then
            if [ -e "$backup_app" ]; then
                sudo mv -- "$backup_app" "$dest_app" || true
            fi
            sudo rm -rf -- "$staged_app" || true
            die "Failed to move Vela.app into ${apps_dir}."
        fi
        sudo rm -rf -- "$backup_app"
        sudo xattr -dr com.apple.quarantine "$dest_app" 2>/dev/null || true
        sudo touch "$dest_app" 2>/dev/null || true
    else
        rm -rf -- "$staged_app" "$backup_app"
        if command -v ditto >/dev/null 2>&1; then
            ditto "$src_app" "$staged_app"
        else
            cp -R "$src_app" "$staged_app"
        fi
        if [ -e "$dest_app" ]; then
            mv -- "$dest_app" "$backup_app"
        fi
        if ! mv -- "$staged_app" "$dest_app"; then
            if [ -e "$backup_app" ]; then
                mv -- "$backup_app" "$dest_app" || true
            fi
            rm -rf -- "$staged_app" || true
            die "Failed to move Vela.app into ${apps_dir}."
        fi
        rm -rf -- "$backup_app"
        if command -v xattr >/dev/null 2>&1; then
            xattr -dr com.apple.quarantine "$dest_app" 2>/dev/null || true
        fi
        touch "$dest_app" 2>/dev/null || true
    fi

    lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
    if [ -x "$lsregister" ]; then
        "$lsregister" -f "$dest_app" >/dev/null 2>&1 || true
    fi

    say "Vela ${VERSION} installed to ${dest_app}"
    printf 'Open it from Spotlight, the Dock, or run: open -a Vela\n'
}

install_linux() {
    extract_dir="$1"
    src_dir=""
    if [ -f "${extract_dir}/vela" ]; then
        src_dir="$extract_dir"
    else
        for candidate in "${extract_dir}"/vela-*; do
            if [ -d "$candidate" ] && [ -f "${candidate}/vela" ]; then
                src_dir="$candidate"
                break
            fi
        done
    fi
    [ -n "$src_dir" ] || die "Archive did not contain the 'vela' binary."

    bin_dir="${VELA_INSTALL_DIR:-${XDG_BIN_HOME:-${HOME}/.local/bin}}"
    data_dir="${VELA_DATA_DIR:-${XDG_DATA_HOME:-${HOME}/.local/share}}"
    apps_dir="${data_dir}/applications"
    icons_dir="${data_dir}/icons/hicolor"

    mkdir -p "$bin_dir" "$apps_dir"

    # Atomic replace so updating while Vela is running avoids ETXTBSY.
    tmp_bin="${bin_dir}/.vela.tmp.$$"
    cp "$src_dir/vela" "$tmp_bin"
    chmod 0755 "$tmp_bin"
    mv -f -- "$tmp_bin" "${bin_dir}/vela"
    say "Installed binary to ${bin_dir}/vela"

    # Install hicolor icons (SVG + PNG sizes) for Wayland/X11 launchers.
    if [ -d "${src_dir}/icons/hicolor" ]; then
        mkdir -p "$icons_dir"
        if [ -f "${src_dir}/icons/hicolor/scalable/apps/vela.svg" ]; then
            mkdir -p "${icons_dir}/scalable/apps"
            cp "${src_dir}/icons/hicolor/scalable/apps/vela.svg" \
                "${icons_dir}/scalable/apps/vela.svg"
            chmod 0644 "${icons_dir}/scalable/apps/vela.svg"
        fi
        for size_dir in "${src_dir}"/icons/hicolor/*x*; do
            [ -d "$size_dir" ] || continue
            size_name="$(basename "$size_dir")"
            if [ -f "${size_dir}/apps/vela.png" ]; then
                mkdir -p "${icons_dir}/${size_name}/apps"
                cp "${size_dir}/apps/vela.png" "${icons_dir}/${size_name}/apps/vela.png"
                chmod 0644 "${icons_dir}/${size_name}/apps/vela.png"
            fi
        done
        say "Installed app icons to ${icons_dir}"
    fi

    # Install desktop entry with absolute Exec path so Rofi/Fuzzel/Walker
    # always find Vela even when ~/.local/bin is not in the compositor PATH.
    exec_cmd="$(quote_desktop_exec "${bin_dir}/vela")"
    desktop_dest="${apps_dir}/vela.desktop"
    tmp_desktop="${apps_dir}/.vela.desktop.tmp.$$"
    if [ -f "${src_dir}/vela.desktop" ]; then
        EXEC_LINE="Exec=${exec_cmd}" awk '
            /^Exec=/ { print ENVIRON["EXEC_LINE"]; next }
            { print }
        ' "${src_dir}/vela.desktop" > "$tmp_desktop"
    else
        cat > "$tmp_desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Vela
Comment=Lightweight SSH and SFTP client
Exec=${exec_cmd}
Icon=vela
Terminal=false
Categories=Network;FileTransfer;Utility;
StartupWMClass=vela
EOF
    fi
    chmod 0644 "$tmp_desktop"
    mv -f -- "$tmp_desktop" "$desktop_dest"
    say "Installed desktop entry to ${desktop_dest}"

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ -d "$icons_dir" ]; then
        gtk-update-icon-cache -q -f -t "$icons_dir" >/dev/null 2>&1 || true
    fi

    say "Vela ${VERSION} installed."
    case ":${PATH:-}:" in
        *":${bin_dir}:"*) ;;
        *)
            printf '\nAdd Vela to your PATH to launch it from a terminal:\n'
            printf '  export PATH="%s:$PATH"\n' "$bin_dir"
            ;;
    esac
}

main() {
    parse_args "$@"
    detect_platform

    need_cmd curl
    need_cmd tar
    need_cmd ssh-keygen
    need_cmd awk

    pubkey="${VELA_SIGNING_PUBKEY:-$VELA_DEFAULT_PUBKEY}"

    tmp_base="${TMPDIR:-/tmp}"
    tmp_base="${tmp_base%/}"
    tmp_dir="$(mktemp -d "${tmp_base}/vela-install.XXXXXX")"
    chmod 700 "$tmp_dir"
    trap 'rm -rf -- "$tmp_dir"' EXIT INT TERM

    selected_tag=""
    if [ -n "${VELA_RELEASE_BASE_URL:-}" ]; then
        base_url="${VELA_RELEASE_BASE_URL%/}"
    elif [ -n "${VELA_VERSION:-}" ]; then
        case "$VELA_VERSION" in
            v*) tag="$VELA_VERSION" ;;
            *) tag="v${VELA_VERSION}" ;;
        esac
        base_url="https://github.com/${VELA_REPO}/releases/download/${tag}"
    elif [ "${VELA_PRERELEASE:-}" = "1" ]; then
        selected_tag="$(latest_release_tag "${tmp_dir}/releases.json")" \
            || die "Could not select a published Vela release."
        base_url="https://github.com/${VELA_REPO}/releases/download/${selected_tag}"
    else
        base_url="https://github.com/${VELA_REPO}/releases/latest/download"
    fi

    allowed_signers="${tmp_dir}/allowed_signers"
    printf '%s %s\n' "$VELA_SIGNER_IDENTITY" "$pubkey" > "$allowed_signers"

    manifest_file="${tmp_dir}/manifest.json"
    manifest_sig="${tmp_dir}/manifest.json.sshsig"

    say "Fetching Vela release manifest (${PLATFORM})..."
    download "${base_url}/manifest.json" "$manifest_file"
    download "${base_url}/manifest.json.sshsig" "$manifest_sig"

    say "Verifying manifest Ed25519 signature..."
    verify_sshsig "$manifest_file" "$manifest_sig" "$allowed_signers" "manifest.json"

    VERSION="$(parse_manifest_top "$manifest_file" "version")"
    asset="$(parse_manifest_platform "$manifest_file" "$PLATFORM" "asset")"
    asset_url="$(parse_manifest_platform "$manifest_file" "$PLATFORM" "url")"
    expected_sha256="$(parse_manifest_platform "$manifest_file" "$PLATFORM" "sha256")"

    [ -n "$VERSION" ] || die "Could not read release version from manifest.json."
    if [ -n "$selected_tag" ]; then
        [ "$VERSION" = "${selected_tag#v}" ] &&
            [ "$(parse_manifest_top "$manifest_file" "tag")" = "$selected_tag" ] \
            || die "The signed version and tag do not match the selected release."
    fi
    [ -n "$asset" ] || die "Platform '${PLATFORM}' was not found in manifest.json."
    validate_asset_name "$asset"
    validate_sha256 "$expected_sha256"

    if [ -n "${VELA_RELEASE_BASE_URL:-}" ]; then
        archive_url="${base_url}/${asset}"
        archive_sig_url="${base_url}/${asset}.sshsig"
    else
        case "$asset_url" in
            "https://github.com/${VELA_REPO}/releases/download/"*) ;;
            *) die "Unexpected download URL in manifest.json: ${asset_url}" ;;
        esac
        archive_url="$asset_url"
        archive_sig_url="${asset_url}.sshsig"
    fi

    archive_file="${tmp_dir}/${asset}"
    archive_sig="${tmp_dir}/${asset}.sshsig"

    say "Downloading Vela ${VERSION} (${asset})..."
    download "$archive_url" "$archive_file"
    download "$archive_sig_url" "$archive_sig"

    say "Verifying archive Ed25519 signature and SHA-256 checksum..."
    verify_sshsig "$archive_file" "$archive_sig" "$allowed_signers" "$asset"
    actual_sha256="$(sha256_file "$archive_file")"
    if [ "$actual_sha256" != "$expected_sha256" ]; then
        die "SHA-256 mismatch for ${asset} (expected ${expected_sha256}, got ${actual_sha256})."
    fi

    extract_dir="${tmp_dir}/extract"
    mkdir -p "$extract_dir"
    tar -xzf "$archive_file" -C "$extract_dir"

    case "$OS_KIND" in
        macos) install_macos "$extract_dir" ;;
        linux) install_linux "$extract_dir" ;;
    esac
}

main "$@"
