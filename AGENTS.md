# vSCP

Read [continuity](.agent/CONTINUITY.md) for the current scope and pending decisions.

## Build workflow

- Use disposable containers for Linux compilation and checks. Run one build at a time. Keep build artifacts in the container or in a task-owned temporary directory, and remove them at the end.
- For Rust, use `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0` and a disposable `CARGO_TARGET_DIR`. Once the framework is selected, pin the Rust toolchain and Linux dependencies in the build image.
- Run desktop integration checks from a regular graphical session on the target OS. Use the macOS SDK on macOS for AppKit compilation. Linux headless checks cannot establish Dolphin or Finder compatibility.
- See the [file handoff probe](probes/file-handoff/README.md) for current build and test commands.
- See the [MVP plan](docs/PLAN.md) for the selected stack, phases and open questions.

Run these commands from the repository root in a macOS graphical session. The release executable runs before the temporary build directory is removed.

```sh
(
  set -e
  CARGO_TARGET_DIR="$(mktemp -d /tmp/vscp-target.XXXXXX)"
  export CARGO_TARGET_DIR CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
  trap 'rm -rf -- "$CARGO_TARGET_DIR"' EXIT

  cargo check --workspace
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo build --release
  "$CARGO_TARGET_DIR/release/vscp"
)
```

Set `VSCP_TRACE_STARTUP=1` on the executable command to print the first-frame startup time.

Run the core SSH/SFTP tests as a regular user with `cargo test -p vscp-core` and the same disposable target settings. The integration fixture starts `/usr/sbin/sshd` on loopback and uses temporary keys, a private `ssh-agent`, and isolated client configuration and known hosts. The password case is ignored by default and takes `VSCP_TEST_PW_HOST`, `VSCP_TEST_PW_PORT`, `VSCP_TEST_PW_USER` and `VSCP_TEST_PW_PASSWORD` for the Linux password-enabled fixture.
