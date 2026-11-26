#!/usr/bin/env python3
"""Resolve release versions and stamp just Vela's Cargo package versions."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import tomllib


MANIFESTS = {
    name: Path(f"crates/{name}/Cargo.toml") for name in ("vela-app", "vela-core")
}
LOCKFILE = Path("Cargo.lock")
NUMBER = r"(?:0|[1-9][0-9]*)"
BASE = rf"{NUMBER}\.{NUMBER}\.{NUMBER}"
IDENTIFIER = r"[0-9A-Za-z-]+"
IDENTIFIERS = rf"{IDENTIFIER}(?:\.{IDENTIFIER})*"
SEMVER = re.compile(
    rf"(?P<base>{BASE})(?:-(?P<pre>{IDENTIFIERS}))?(?:\+{IDENTIFIERS})?"
)


def parse_version(version):
    match = SEMVER.fullmatch(version)
    if match and not any(
        part.isdecimal() and len(part) > 1 and part.startswith("0")
        for part in (match["pre"] or "").split(".")
    ):
        return match
    raise ValueError(f"Invalid SemVer version: {version!r}")


def package_version(path):
    return tomllib.loads(path.read_text())["package"]["version"]


def select_version(event, mode, requested, app_version, tags, ref):
    if event == "push":
        if ref != f"v{app_version}":
            raise ValueError(f"Tag {ref!r} does not match Cargo.toml version {app_version!r}")
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", app_version):
            raise ValueError(f"Invalid version for a tag push: {app_version!r}")
        return app_version

    base = parse_version(app_version)["base"]
    if mode == "release":
        version = requested or base
        if not re.fullmatch(BASE, version):
            raise ValueError("A stable release version must have the form X.Y.Z")
    elif mode == "prerelease":
        parsed = parse_version(requested) if requested else None
        if parsed and parsed["pre"]:
            version = requested
        else:
            if requested:
                if not re.fullmatch(BASE, requested):
                    raise ValueError("Use X.Y.Z or an explicit prerelease version")
                base = requested
            elif f"v{base}" in tags:
                major, minor, patch = map(int, base.split("."))
                base = f"{major}.{minor}.{patch + 1}"
            rc_pattern = re.compile(rf"v{re.escape(base)}-rc\.({NUMBER})(?:\+{IDENTIFIERS})?")
            candidates = (rc_pattern.fullmatch(tag) for tag in tags)
            next_rc = max((int(match[1]) for match in candidates if match), default=0) + 1
            version = f"{base}-rc.{next_rc}"
    else:
        raise ValueError(f"Unknown release mode: {mode!r}")

    if f"v{version}" in tags:
        raise ValueError(f"Tag v{version} already exists")
    return version


def replace_version(section, version):
    updated, count = re.subn(
        r'(?m)^(version\s*=\s*)"[^"\n]+"', rf'\g<1>"{version}"', section
    )
    if count != 1:
        raise ValueError("Expected exactly one version in the Cargo package section")
    return updated


def stamp(version):
    parse_version(version)
    updates = {}
    for path in MANIFESTS.values():
        sections = re.split(r"(?m)(?=^\[)", path.read_text())
        package, = (index for index, part in enumerate(sections) if part.startswith("[package]\n"))
        sections[package] = replace_version(sections[package], version)
        updates[path] = "".join(sections)

    sections = re.split(r"(?m)(?=^\[\[package\]\]$)", LOCKFILE.read_text())
    for name in MANIFESTS:
        package, = (
            index for index, part in enumerate(sections)
            if part.startswith(f'[[package]]\nname = "{name}"\n')
        )
        sections[package] = replace_version(sections[package], version)
    updates[LOCKFILE] = "".join(sections)

    for path, updated in updates.items():
        if path.read_text() != updated:
            path.write_text(updated)


def prepare():
    event = os.environ["GITHUB_EVENT_NAME"]
    mode = os.environ.get("INPUT_MODE", "")
    ref = os.environ["GITHUB_REF_NAME"]
    if event == "workflow_dispatch" and os.environ["GITHUB_REF_TYPE"] != "branch":
        raise ValueError("Select a branch when starting a release manually")

    app_version, core_version = map(package_version, MANIFESTS.values())
    if event == "push" and app_version != core_version:
        raise ValueError(f"Version mismatch: vela-app={app_version} != vela-core={core_version}")

    tags = set(subprocess.check_output(["git", "tag", "--list"], text=True).splitlines())
    version = select_version(
        event, mode, os.environ.get("INPUT_VERSION", "").strip(), app_version, tags, ref
    )
    prerelease = "-" in version if event == "push" else bool(parse_version(version)["pre"])
    draft = event == "workflow_dispatch" and os.environ.get("INPUT_DRAFT") == "true"
    stamp_version = event == "workflow_dispatch" and mode == "prerelease"
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        print(f"version={version}", file=output)
        print(f"tag=v{version}", file=output)
        print(f"prerelease={str(prerelease).lower()}", file=output)
        print(f"draft={str(draft).lower()}", file=output)
        print(f"stamp_version={str(stamp_version).lower()}", file=output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stamp", metavar="VERSION", help="Stamp the Cargo manifests and lockfile")
    args = parser.parse_args()
    try:
        if args.stamp is None:
            prepare()
        else:
            stamp(args.stamp)
    except ValueError as error:
        parser.exit(1, f"Error: {error}\n")


if __name__ == "__main__":
    main()
