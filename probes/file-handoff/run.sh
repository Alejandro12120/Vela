#!/usr/bin/env bash
set -euo pipefail

if (( $# > 1 )); then
    printf 'Uso: bash %s [ARCHIVO_LOCAL]\n' "$0" >&2
    exit 2
fi

source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/vscp-handoff.XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT

file=${1:-}
if [[ -z "$file" ]]; then
    file="$scratch/Prueba vSCP #1.txt"
    printf 'Prueba de transferencia de archivos vSCP\n' > "$file"
elif [[ ! -f "$file" ]]; then
    printf 'Se necesita un archivo local regular.\n' >&2
    exit 2
fi

case $(uname -s) in
    Linux)
        pkg-config --exists gtk4
        read -r -a gtk_cflags <<< "$(pkg-config --cflags gtk4)"
        read -r -a gtk_libs <<< "$(pkg-config --libs gtk4)"
        "${CC:-cc}" -std=c17 -Wall -Wextra -Werror -O2 "${gtk_cflags[@]}" \
            "$source_dir/linux.c" -o "$scratch/file-handoff" "${gtk_libs[@]}"
        ;;
    Darwin)
        swiftc -O -module-cache-path "$scratch/modules" \
            "$source_dir/macos.swift" -o "$scratch/file-handoff"
        ;;
    *)
        printf 'Esta prueba requiere Linux o macOS.\n' >&2
        exit 2
        ;;
esac

"$scratch/file-handoff" "$file"
