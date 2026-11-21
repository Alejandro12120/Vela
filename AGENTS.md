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

## Manual SSH fixture

Run `scripts/dev-sshd.sh` in a terminal as a regular user. It needs system OpenSSH, `nc` and Python 3. Enter the printed host, port, user, public key and agent socket in the server form. Ctrl+C stops the loopback server and its private agent and removes their temporary files.

Use an isolated configuration directory for development runs. The `dev` Cargo feature enables these environment variables in both debug and release binaries.

- `VSCP_CONFIG_DIR` sets the directory for `servers.toml` and the OpenSSH `known_hosts` file.
- `VSCP_DEV_AUTOCONNECT` connects to the server with the given display name after loading the list.
- `VSCP_DEV_LOG=1` prints connection results, listing durations, visible row counts, transfer events, native file-drag payload decisions and remote-edit watcher, debounce and upload decisions to stderr.
- `VSCP_DEV_THEME=light|dark` overrides the application theme without changing system appearance.
- `VSCP_DEV_LOCAL_ROOT=<absolute path>` overrides the local pane's starting folder.
- `VSCP_DEV_REMOTE_DELAY_MS=<milliseconds>` delays each remote listing asynchronously.
- `VSCP_DEV_EXPAND=<relative path>[,...]` opens the named folders and their ancestors in the initial remote tree.
- `VSCP_DEV_WINDOW_WIDTH=<pixels>` requests an initial width of at least 640 logical pixels for layout checks. The window manager may override it.
- `VSCP_DEV_WINDOW_FLOAT=1` uses a floating test window to keep the requested width when checking layouts under a tiling window manager.

- `VSCP_DEV_TRANSFER=<upload|download>:<absolute source>:<absolute destination>` enqueues one transfer after autoconnect. Conflicts use the usual dialog.
- `VSCP_DEV_TRANSFER_DELAY_MS=<milliseconds>` waits after autoconnect before enqueueing that transfer.
- `VSCP_DEV_TRANSFER_EXPANDED=1` opens the transfer history at launch.
- `VSCP_DEV_COPY=<absolute remote path>[,...]` prepares a remote clipboard copy using the regular F4 transfer after autoconnect.
- `VSCP_DEV_PASTE=remote` runs the remote paste action after autoconnect.
- `VSCP_DEV_STAGING_ROOT=<absolute path>` isolates the Finder staging cache for development.
- `VSCP_DEV_DROP_HIGHLIGHT=1` previews an external drag over the remote root without submitting a drop.
- `VSCP_DEV_FILE_STATE=<local|remote>:<delete|rename|menu>` selects the first entry and opens the given state for visual checks. The delete trigger only opens the confirmation.
- `VSCP_DEV_EDIT=<absolute remote path>` opens a remote edit after autoconnect.
- `VSCP_DEV_EDITOR=none` suppresses editor launches for unattended runs.
- `VSCP_DEV_EDIT_VIEW=popover|settings` shows Ediciones on connection or opens Ajustes on startup for visual checks.
- With a custom `VSCP_CONFIG_DIR`, remote edits use its `edit-cache/` directory.

For example, from the repository root in a macOS graphical session:

```sh
(
  set -e
  config_dir="$(mktemp -d /tmp/vscp-config.XXXXXX)"
  CARGO_TARGET_DIR="$(mktemp -d /tmp/vscp-target.XXXXXX)"
  export CARGO_TARGET_DIR CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
  trap 'rm -rf -- "$config_dir" "$CARGO_TARGET_DIR"' EXIT

  cargo build --release --features dev
  VSCP_CONFIG_DIR="$config_dir" VSCP_DEV_LOG=1 "$CARGO_TARGET_DIR/release/vscp"
)
```
