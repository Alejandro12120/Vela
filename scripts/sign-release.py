#!/usr/bin/env python3
"""Sign vSCP release archives with Ed25519 and generate manifest.json for the updater.

Reads the private key from the VSCP_SIGNING_KEY environment variable (raw PEM or
base64-encoded PEM), writes detached raw Ed25519 `.sig` files, SHA256SUMS.txt,
and manifest.json, then verifies every signature against the derived public key.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile


def decode_private_key(raw: str) -> bytes:
    cleaned = raw.strip()
    if not cleaned:
        raise ValueError("VSCP_SIGNING_KEY is empty")
    if "BEGIN " in cleaned:
        return (cleaned + "\n").encode("utf-8")
    try:
        decoded = base64.b64decode(cleaned, validate=True)
    except Exception as exc:
        raise ValueError(
            "VSCP_SIGNING_KEY must be an Ed25519 private key in PEM format "
            "(raw PEM or base64-encoded PEM)"
        ) from exc
    if b"BEGIN " not in decoded:
        raise ValueError("Decoded VSCP_SIGNING_KEY does not contain a PEM header")
    return decoded


def sha256_hex(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_openssl(args: list[str]) -> bytes:
    proc = subprocess.run(
        ["openssl", *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"openssl {' '.join(args)} failed: {stderr}")
    return proc.stdout


def sign_file(target: pathlib.Path, key_path: pathlib.Path) -> pathlib.Path:
    sig_path = target.with_name(target.name + ".sig")
    run_openssl(
        [
            "pkeyutl",
            "-sign",
            "-inkey",
            str(key_path),
            "-rawin",
            "-in",
            str(target),
            "-out",
            str(sig_path),
        ]
    )
    return sig_path


def verify_file(
    target: pathlib.Path, sig_path: pathlib.Path, pub_path: pathlib.Path
) -> None:
    run_openssl(
        [
            "pkeyutl",
            "-verify",
            "-pubin",
            "-inkey",
            str(pub_path),
            "-rawin",
            "-in",
            str(target),
            "-sigfile",
            str(sig_path),
        ]
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", required=True, type=pathlib.Path)
    parser.add_argument("--repo", required=True, help="owner/name")
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument(
        "--platform",
        action="append",
        required=True,
        help="platform_id=asset_filename (repeatable)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    dist_dir: pathlib.Path = args.dir
    if not dist_dir.is_dir():
        sys.exit(f"Directory not found: {dist_dir}")

    key_pem = decode_private_key(os.environ.get("VSCP_SIGNING_KEY", ""))

    platform_assets: list[tuple[str, pathlib.Path]] = []
    for entry in args.platform:
        if "=" not in entry:
            sys.exit(f"Invalid --platform value '{entry}', expected platform=asset")
        platform, filename = entry.split("=", 1)
        asset_path = dist_dir / filename
        if not asset_path.is_file():
            sys.exit(f"Missing release asset for {platform}: {asset_path}")
        platform_assets.append((platform, asset_path))

    with tempfile.TemporaryDirectory(prefix="vscp-sign-") as tmp:
        tmp_dir = pathlib.Path(tmp)
        os.chmod(tmp_dir, 0o700)
        key_path = tmp_dir / "private.pem"
        pub_path = tmp_dir / "public.pem"
        pub_der = tmp_dir / "public.der"

        fd = os.open(key_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "wb") as fh:
            fh.write(key_pem)

        # Derive public key and validate the key is Ed25519.
        key_text = run_openssl(
            ["pkey", "-in", str(key_path), "-pubout", "-text", "-noout"]
        ).decode("utf-8", errors="replace")
        if "ED25519" not in key_text.upper():
            sys.exit("VSCP_SIGNING_KEY must be an Ed25519 private key")

        run_openssl(
            ["pkey", "-in", str(key_path), "-pubout", "-out", str(pub_path)]
        )
        run_openssl(
            [
                "pkey",
                "-in",
                str(key_path),
                "-pubout",
                "-outform",
                "DER",
                "-out",
                str(pub_der),
            ]
        )
        # SubjectPublicKeyInfo for Ed25519 is 12-byte ASN.1 header + 32-byte raw key.
        der_bytes = pub_der.read_bytes()
        if len(der_bytes) != 44:
            sys.exit(
                f"Unexpected Ed25519 SubjectPublicKeyInfo DER length ({len(der_bytes)} bytes)"
            )
        raw_pub = der_bytes[-32:]
        pub_b64 = base64.b64encode(raw_pub).decode("ascii")
        pub_fp = hashlib.sha256(raw_pub).hexdigest()
        print(f"Ed25519 public key (base64, 32 bytes): {pub_b64}")
        print(f"Ed25519 public key SHA-256: {pub_fp}")

        download_base = f"https://github.com/{args.repo}/releases/download/{args.tag}"
        sums_lines: list[str] = []
        platforms_manifest: dict[str, dict[str, object]] = {}

        for platform, asset_path in sorted(platform_assets, key=lambda x: x[0]):
            digest = sha256_hex(asset_path)
            size = asset_path.stat().st_size
            sig_path = sign_file(asset_path, key_path)
            verify_file(asset_path, sig_path, pub_path)
            sig_bytes = sig_path.read_bytes()
            if len(sig_bytes) != 64:
                sys.exit(
                    f"Unexpected Ed25519 signature length ({len(sig_bytes)} bytes) for {asset_path.name}"
                )
            sig_b64 = base64.b64encode(sig_bytes).decode("ascii")
            sums_lines.append(f"{digest}  {asset_path.name}")
            platforms_manifest[platform] = {
                "asset": asset_path.name,
                "url": f"{download_base}/{asset_path.name}",
                "sha256": digest,
                "size": size,
                "signature": sig_b64,
            }

        sums_path = dist_dir / "SHA256SUMS.txt"
        sums_path.write_text("\n".join(sums_lines) + "\n", encoding="utf-8")
        sums_sig = sign_file(sums_path, key_path)
        verify_file(sums_path, sums_sig, pub_path)

        manifest = {
            "schema": 1,
            "version": args.version,
            "tag": args.tag,
            "published_at": dt.datetime.now(dt.timezone.utc)
            .replace(microsecond=0)
            .isoformat()
            .replace("+00:00", "Z"),
            "notes_url": f"https://github.com/{args.repo}/releases/tag/{args.tag}",
            "platforms": platforms_manifest,
        }
        manifest_path = dist_dir / "manifest.json"
        manifest_path.write_text(
            json.dumps(manifest, indent=2, sort_keys=False) + "\n",
            encoding="utf-8",
        )
        manifest_sig = sign_file(manifest_path, key_path)
        verify_file(manifest_path, manifest_sig, pub_path)

        print(f"Signed and verified {len(platform_assets)} platform archive(s), SHA256SUMS.txt and manifest.json.")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
