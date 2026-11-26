#!/usr/bin/env python3
"""Sign Vela release archives with Ed25519 and generate manifest.json for the updater.

Reads the private key from the VELA_SIGNING_KEY environment variable (raw PEM or
base64-encoded PEM), writes both detached raw Ed25519 `.sig` files (for the in-app
updater and OpenSSL 3+) and OpenSSH `.sshsig` files (for `install.sh` on macOS and
Linux via `ssh-keygen -Y verify`), SHA256SUMS.txt, and manifest.json, then verifies
every signature against the derived public key.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import pathlib
import secrets
import struct
import subprocess
import sys
import tempfile

SSHSIG_IDENTITY = "vela@release"
SSHSIG_NAMESPACE = "vela-release"
PKCS8_ED25519_PREFIX = bytes.fromhex("302e020100300506032b657004220420")
SPKI_ED25519_PREFIX = bytes.fromhex("302a300506032b6570032100")


def decode_private_key(raw: str) -> bytes:
    cleaned = raw.strip()
    if not cleaned:
        raise ValueError("VELA_SIGNING_KEY is empty")
    if "BEGIN " in cleaned:
        return (cleaned + "\n").encode("utf-8")
    try:
        decoded = base64.b64decode(cleaned, validate=True)
    except Exception as exc:
        raise ValueError(
            "VELA_SIGNING_KEY must be an Ed25519 private key in PEM format "
            "(raw PEM or base64-encoded PEM)"
        ) from exc
    if b"BEGIN " not in decoded:
        raise ValueError("Decoded VELA_SIGNING_KEY does not contain a PEM header")
    return decoded


def sha256_hex(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_cmd(cmd: list[str], stdin_bytes: bytes | None = None) -> bytes:
    proc = subprocess.run(
        cmd,
        input=stdin_bytes,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"{' '.join(cmd)} failed: {stderr}")
    return proc.stdout


def run_openssl(args: list[str]) -> bytes:
    return run_cmd(["openssl", *args])


def ssh_string(data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + data


def read_ssh_string(buf: bytes, offset: int) -> tuple[bytes, int]:
    if offset + 4 > len(buf):
        raise ValueError("Truncated OpenSSH private key")
    length = struct.unpack(">I", buf[offset : offset + 4])[0]
    offset += 4
    if offset + length > len(buf):
        raise ValueError("Truncated OpenSSH private key field")
    return buf[offset : offset + length], offset + length


def parse_openssh_ed25519_private_key(pem_bytes: bytes) -> tuple[bytes, bytes]:
    """Parse an unencrypted OpenSSH Ed25519 private key into (raw_seed, raw_pub)."""
    lines = [
        line.strip()
        for line in pem_bytes.decode("ascii").splitlines()
        if line.strip() and not line.strip().startswith("-----")
    ]
    blob = base64.b64decode("".join(lines), validate=True)
    magic = b"openssh-key-v1\x00"
    if not blob.startswith(magic):
        raise ValueError("Unsupported OpenSSH private key format (missing openssh-key-v1 header)")
    offset = len(magic)
    cipher, offset = read_ssh_string(blob, offset)
    kdf, offset = read_ssh_string(blob, offset)
    _kdf_opts, offset = read_ssh_string(blob, offset)
    if cipher != b"none" or kdf != b"none":
        raise ValueError(
            "VELA_SIGNING_KEY is encrypted with a passphrase "
            f"(cipher={cipher.decode('ascii', errors='replace')}, "
            f"kdf={kdf.decode('ascii', errors='replace')}). "
            "GitHub Actions requires an unencrypted Ed25519 private key."
        )
    if offset + 4 > len(blob):
        raise ValueError("Truncated OpenSSH private key (missing key count)")
    nkeys = struct.unpack(">I", blob[offset : offset + 4])[0]
    offset += 4
    if nkeys != 1:
        raise ValueError(f"Expected 1 key in OpenSSH private key, found {nkeys}")
    _pub_blob, offset = read_ssh_string(blob, offset)
    priv_blob, offset = read_ssh_string(blob, offset)

    if len(priv_blob) < 8:
        raise ValueError("Truncated OpenSSH private section")
    check1, check2 = struct.unpack(">II", priv_blob[:8])
    if check1 != check2:
        raise ValueError("Corrupt OpenSSH private key (checkint mismatch)")
    poff = 8
    key_type, poff = read_ssh_string(priv_blob, poff)
    if key_type != b"ssh-ed25519":
        raise ValueError(
            f"VELA_SIGNING_KEY must be an Ed25519 key (got {key_type.decode('ascii', errors='replace')})"
        )
    raw_pub, poff = read_ssh_string(priv_blob, poff)
    raw_priv64, poff = read_ssh_string(priv_blob, poff)
    if len(raw_pub) != 32 or len(raw_priv64) != 64:
        raise ValueError("Unexpected Ed25519 key lengths in OpenSSH private key")
    raw_seed = raw_priv64[:32]
    if raw_priv64[32:] != raw_pub:
        raise ValueError("OpenSSH Ed25519 private key public-key suffix mismatch")
    return raw_seed, raw_pub


def build_pkcs8_ed25519_pem(raw_seed: bytes) -> bytes:
    """Wrap a 32-byte raw Ed25519 seed in RFC 8410 PKCS#8 PEM."""
    if len(raw_seed) != 32:
        raise ValueError("Ed25519 seed must be 32 bytes")
    b64 = base64.b64encode(PKCS8_ED25519_PREFIX + raw_seed).decode("ascii")
    lines = [b64[i : i + 64] for i in range(0, len(b64), 64)]
    return (
        "-----BEGIN PRIVATE KEY-----\n"
        + "\n".join(lines)
        + "\n-----END PRIVATE KEY-----\n"
    ).encode("ascii")


def build_openssh_ed25519_keys(raw_seed: bytes, raw_pub: bytes) -> tuple[bytes, str]:
    """Build an unencrypted OpenSSH Ed25519 private key PEM and matching .pub line."""
    if len(raw_seed) != 32 or len(raw_pub) != 32:
        raise ValueError("Ed25519 seed and public key must each be 32 bytes")

    pub_blob = ssh_string(b"ssh-ed25519") + ssh_string(raw_pub)
    pub_line = f"ssh-ed25519 {base64.b64encode(pub_blob).decode('ascii')}"

    check = secrets.randbits(32)
    priv = bytearray()
    priv += struct.pack(">II", check, check)
    priv += ssh_string(b"ssh-ed25519")
    priv += ssh_string(raw_pub)
    priv += ssh_string(raw_seed + raw_pub)
    priv += ssh_string(SSHSIG_IDENTITY.encode("ascii"))
    pad = 1
    while len(priv) % 8 != 0:
        priv.append(pad)
        pad += 1

    body = bytearray(b"openssh-key-v1\x00")
    body += ssh_string(b"none")  # ciphername
    body += ssh_string(b"none")  # kdfname
    body += ssh_string(b"")      # kdfoptions
    body += struct.pack(">I", 1)  # number of keys
    body += ssh_string(pub_blob)
    body += ssh_string(bytes(priv))

    b64 = base64.b64encode(bytes(body)).decode("ascii")
    lines = [b64[i : i + 70] for i in range(0, len(b64), 70)]
    pem = (
        "-----BEGIN OPENSSH PRIVATE KEY-----\n"
        + "\n".join(lines)
        + "\n-----END OPENSSH PRIVATE KEY-----\n"
    ).encode("ascii")
    return pem, pub_line


def write_secret_file(path: pathlib.Path, data: bytes) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as fh:
        fh.write(data)


def sign_file_raw(target: pathlib.Path, key_path: pathlib.Path) -> pathlib.Path:
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


def verify_file_raw(
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


def sign_file_sshsig(target: pathlib.Path, ssh_key_path: pathlib.Path) -> pathlib.Path:
    sig_path = target.with_name(target.name + ".sshsig")
    with target.open("rb") as fh:
        proc = subprocess.run(
            [
                "ssh-keygen",
                "-Y",
                "sign",
                "-f",
                str(ssh_key_path),
                "-n",
                SSHSIG_NAMESPACE,
            ],
            stdin=fh,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"ssh-keygen -Y sign failed for {target.name}: {stderr}")
    sig_path.write_bytes(proc.stdout)
    return sig_path


def verify_file_sshsig(
    target: pathlib.Path, sig_path: pathlib.Path, allowed_signers: pathlib.Path
) -> None:
    with target.open("rb") as fh:
        proc = subprocess.run(
            [
                "ssh-keygen",
                "-Y",
                "verify",
                "-f",
                str(allowed_signers),
                "-I",
                SSHSIG_IDENTITY,
                "-n",
                SSHSIG_NAMESPACE,
                "-s",
                str(sig_path),
            ],
            stdin=fh,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    if proc.returncode != 0:
        stderr = proc.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"ssh-keygen -Y verify failed for {target.name}: {stderr}")


def sign_and_verify_all(
    target: pathlib.Path,
    key_path: pathlib.Path,
    pub_path: pathlib.Path,
    ssh_key_path: pathlib.Path,
    allowed_signers: pathlib.Path,
) -> tuple[pathlib.Path, pathlib.Path]:
    raw_sig = sign_file_raw(target, key_path)
    verify_file_raw(target, raw_sig, pub_path)
    ssh_sig = sign_file_sshsig(target, ssh_key_path)
    verify_file_sshsig(target, ssh_sig, allowed_signers)
    return raw_sig, ssh_sig


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", required=True, type=pathlib.Path)
    parser.add_argument("--repo", required=True, help="owner/name")
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument(
        "--notes-file",
        type=pathlib.Path,
        help="Optional UTF-8 Markdown to include in the signed update manifest",
    )
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

    key_pem = decode_private_key(os.environ.get("VELA_SIGNING_KEY", ""))

    platform_assets: list[tuple[str, pathlib.Path]] = []
    for entry in args.platform:
        if "=" not in entry:
            sys.exit(f"Invalid --platform value '{entry}', expected platform=asset")
        platform, filename = entry.split("=", 1)
        asset_path = dist_dir / filename
        if not asset_path.is_file():
            sys.exit(f"Missing release asset for {platform}: {asset_path}")
        platform_assets.append((platform, asset_path))

    with tempfile.TemporaryDirectory(prefix="vela-sign-") as tmp:
        tmp_dir = pathlib.Path(tmp)
        os.chmod(tmp_dir, 0o700)
        key_path = tmp_dir / "private.pem"
        priv_der_path = tmp_dir / "private.der"
        pub_path = tmp_dir / "public.pem"
        pub_der_path = tmp_dir / "public.der"
        ssh_key_path = tmp_dir / "openssh_ed25519"
        ssh_pub_path = tmp_dir / "openssh_ed25519.pub"
        allowed_signers = tmp_dir / "allowed_signers"

        if b"BEGIN ENCRYPTED PRIVATE KEY" in key_pem:
            sys.exit(
                "VELA_SIGNING_KEY is encrypted with a passphrase. "
                "GitHub Actions requires an unencrypted Ed25519 private key."
            )

        if b"BEGIN OPENSSH PRIVATE KEY" in key_pem:
            try:
                raw_seed, expected_pub = parse_openssh_ed25519_private_key(key_pem)
            except ValueError as exc:
                sys.exit(str(exc))
            write_secret_file(key_path, build_pkcs8_ed25519_pem(raw_seed))
        else:
            expected_pub = None
            write_secret_file(key_path, key_pem)

        # Derive public key and validate the key is Ed25519.
        key_text = run_openssl(
            ["pkey", "-in", str(key_path), "-pubout", "-text", "-noout"]
        ).decode("utf-8", errors="replace")
        if "ED25519" not in key_text.upper():
            sys.exit("VELA_SIGNING_KEY must be an Ed25519 private key")

        run_openssl(
            ["pkey", "-in", str(key_path), "-outform", "DER", "-out", str(priv_der_path)]
        )
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
                str(pub_der_path),
            ]
        )

        priv_der = priv_der_path.read_bytes()
        if len(priv_der) != 48 or not priv_der.startswith(PKCS8_ED25519_PREFIX):
            sys.exit(
                f"Unexpected Ed25519 PKCS#8 DER format ({len(priv_der)} bytes)"
            )
        raw_seed = priv_der[-32:]

        pub_der = pub_der_path.read_bytes()
        if len(pub_der) != 44 or not pub_der.startswith(SPKI_ED25519_PREFIX):
            sys.exit(
                f"Unexpected Ed25519 SubjectPublicKeyInfo DER format ({len(pub_der)} bytes)"
            )
        raw_pub = pub_der[-32:]
        if expected_pub is not None and raw_pub != expected_pub:
            sys.exit("Derived OpenSSL public key does not match OpenSSH private key")

        ssh_key_pem, ssh_pub_line = build_openssh_ed25519_keys(raw_seed, raw_pub)
        write_secret_file(ssh_key_path, ssh_key_pem)
        ssh_pub_path.write_text(ssh_pub_line + "\n", encoding="ascii")
        derived_ssh_pub = " ".join(
            run_cmd(["ssh-keygen", "-y", "-f", str(ssh_key_path)])
            .decode("ascii")
            .split()[:2]
        )
        if derived_ssh_pub != ssh_pub_line:
            sys.exit(
                f"Derived OpenSSH public key ({derived_ssh_pub}) "
                f"does not match OpenSSL Ed25519 key ({ssh_pub_line})"
            )
        allowed_signers.write_text(
            f"{SSHSIG_IDENTITY} {ssh_pub_line}\n", encoding="ascii"
        )

        pub_b64 = base64.b64encode(raw_pub).decode("ascii")
        pub_fp = hashlib.sha256(raw_pub).hexdigest()
        print(f"Ed25519 public key (base64, 32 bytes): {pub_b64}")
        print(f"Ed25519 public key (OpenSSH): {ssh_pub_line}")
        print(f"Ed25519 public key SHA-256: {pub_fp}")

        download_base = f"https://github.com/{args.repo}/releases/download/{args.tag}"
        sums_lines: list[str] = []
        platforms_manifest: dict[str, dict[str, object]] = {}

        for platform, asset_path in sorted(platform_assets, key=lambda x: x[0]):
            digest = sha256_hex(asset_path)
            size = asset_path.stat().st_size
            sig_path, _ = sign_and_verify_all(
                asset_path, key_path, pub_path, ssh_key_path, allowed_signers
            )
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
        sign_and_verify_all(
            sums_path, key_path, pub_path, ssh_key_path, allowed_signers
        )

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
        if args.notes_file is not None:
            notes = args.notes_file.read_text(encoding="utf-8").strip()
            if notes:
                manifest["notes"] = notes
        manifest_path = dist_dir / "manifest.json"
        manifest_path.write_text(
            json.dumps(manifest, indent=2, sort_keys=False) + "\n",
            encoding="utf-8",
        )
        sign_and_verify_all(
            manifest_path, key_path, pub_path, ssh_key_path, allowed_signers
        )

        print(
            f"Signed and verified {len(platform_assets)} platform archive(s), "
            "SHA256SUMS.txt and manifest.json (.sig + .sshsig)."
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
