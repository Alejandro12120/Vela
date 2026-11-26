# Vela

Read [continuity](.agent/CONTINUITY.md) for the current scope and pending decisions.

## Build workflow

- Use disposable containers for Linux compilation and checks. Run one build at a time. Keep build artifacts in the container or in a single task-owned temporary directory, and remove it **only once the complete feature/task is finished** (never between commands or edit iterations).
- For Rust, use all available CPU cores (Cargo's default, without limiting `CARGO_BUILD_JOBS`) and **reuse one `CARGO_TARGET_DIR` across the entire task** (e.g. `/tmp/vela-target-<task>`) with Cargo's default incremental compilation so dependencies and workspace crates are not rebuilt from scratch on every command. Only apply `CARGO_INCREMENTAL=0` if doing a final clean release build, and delete `CARGO_TARGET_DIR` once at the very end of the completed feature/task to reclaim disk space. Once the framework is selected, pin the Rust toolchain and Linux dependencies in the build image.
- Run desktop integration checks from a regular graphical session on the target OS. Use the macOS SDK on macOS for AppKit compilation. Linux headless checks cannot establish Dolphin or Finder compatibility.
- See the [file handoff probe](probes/file-handoff/README.md) for current build and test commands.
- Design and review all UI with the [Heyrico UI skill](.agent/skills/heyrico-ui/SKILL.md). Read it before changing any view, and pass it to delegated UI work.

Run these commands from the repository root in a macOS graphical session, reusing the same task-level `CARGO_TARGET_DIR` throughout the feature and deleting it only when the whole task is done:

```sh
# Create once per feature/task and reuse across all edit/check/test/build commands:
export CARGO_TARGET_DIR="/tmp/vela-target-<task>"

cargo check --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
"$CARGO_TARGET_DIR/release/vela"

# Only after the complete feature/task is finished and verified:
rm -rf -- "$CARGO_TARGET_DIR"
```

Set `VELA_TRACE_STARTUP=1` on the executable command to print the first-frame startup time.

Run the core SSH/SFTP tests as a regular user with `cargo test -p vela-core` and the same disposable target settings. The integration fixture starts `/usr/sbin/sshd` on loopback and uses temporary keys, a private `ssh-agent`, and isolated client configuration and known hosts. The password case is ignored by default and takes `VELA_TEST_PW_HOST`, `VELA_TEST_PW_PORT`, `VELA_TEST_PW_USER` and `VELA_TEST_PW_PASSWORD` for the Linux password-enabled fixture.

Run the local PTY test without loading the user's Kitty configuration or starting SSH. Use the same disposable Cargo target settings as above.

```sh
cargo test -p vela-app terminal::tests::pty_output_reaches_grid_and_can_resize
```

## Manual SSH fixture

Run `scripts/dev-sshd.sh` in a terminal as a regular user. It needs system OpenSSH, `nc` and Python 3. Enter the printed host, port, user, public key and agent socket in the server form. Ctrl+C stops the loopback server and its private agent and removes their temporary files.

User themes live in `themes/<id>.toml` under the configuration directory. `settings.toml` stores the selected theme IDs (`light_theme` and `dark_theme`), color scheme, glass settings, the remote editor, the global SSH agent socket (`agent_socket`, optional; without it Vela uses the detected Bitwarden socket), the terminal font size (`terminal_font_size`, optional; without it Vela uses `kitty.conf`) and whether selecting terminal text copies it (`terminal_copy_on_select`, optional; without it Vela follows `copy_on_select` in `kitty.conf`). The `terminal_colors` key accepts `"theme"` (the default) or `"kitty"`. When `kitty.conf` is unavailable, Vela uses theme colors and keeps the saved choice. Click Reload in Settings → Terminal to check for the file again. The `include_prereleases` key (default `false`) controls the update channel in Settings → Updates.

Use an isolated configuration directory for development runs. The `dev` Cargo feature enables these environment variables in both debug and release binaries.

- `VELA_CONFIG_DIR` sets the directory for `servers.toml` and the OpenSSH `known_hosts` file.
- `VELA_DEV_AUTOCONNECT` connects to the server with the given display name after loading the list.
- `VELA_DEV_VIEW=terminal` opens the Terminal view after autoconnect.
- `VELA_DEV_TERMINAL_INPUT=<text>` sends the text once after starting a terminal. It decodes `\n`, `\r` and `\\`.
- `VELA_DEV_KITTY_CONF=<path>` overrides the Kitty configuration file for a development run.
- `VELA_DEV_TERMINAL_FONT_SIZE=<pt>` sets the initial terminal font size in points (clamped to 6–48) for captures, without physical keys. Reset still returns to the `Kitty.size` from `kitty.conf`.
- `VELA_DEV_LOG=1` prints connection results, listing durations, visible row counts, transfer events, native file-drag payload decisions and remote-edit watcher, debounce and upload decisions to stderr. Font-size changes print `Terminal: font size <n>`. It also prints a `[keys]` line for each keystroke with its resolved action (or `none`) and the key context stack, to diagnose key routing. When a terminal loads its settings it prints `Terminal: kitty.conf <path or none>; font_family <configured value or default>; resolved <family>; installed <yes/no>`, once with the initial default and again after reading the Kitty configuration.
- `VELA_DEV_THEME=light|dark` overrides system appearance as seen by Vela in System mode.
- `VELA_DEV_THEME_ID=<id>` overrides both themes for the run without saving. Use `VELA_DEV_THEME_ID=<light>,<dark>` to set them independently.
- `VELA_DEV_SETTINGS=appearance|interface|agent|editor|editor-low|terminal|updates` opens Settings on startup and scrolls to Appearance, Interface, SSH Agent, Editor, Terminal or Updates. `editor-low` places the Editor section at the bottom of the viewport so its dropdown opens Above. `VELA_DEV_SETTINGS=focus-moon` focuses the Vela moon orb. `VELA_DEV_SETTINGS=delete:<id>` opens the confirmation for a user theme.
- `VELA_DEV_EDITOR_PICKER=1` shows the Editor picker on Linux for layout captures. `VELA_DEV_EDITOR_ROOT=<absolute path>` supplies a directory holding fake `.app` bundles (Visual Studio Code.app, Cursor.app, Zed.app, Sublime Text.app, BBEdit.app, Nova.app, TextEdit.app) for the picker's detection; launching is unchanged.
- `VELA_DEV_PROMPT=password-rejected` opens a fake Password prompt for destination `example` in the rejected state, so “The server rejected the password.” is shown under the field.
- `VELA_DEV_FORM_FILE=<path>` opens the new form in file-key mode with that public key chosen, for key-row captures. `VELA_DEV_FORM_KEY=<comment>` preselects the agent key with that comment. `VELA_DEV_FORM_ADVANCED=1` opens “Advanced Options” and `VELA_DEV_FORM_PASSWORD=1` starts the form in password mode.
- `VELA_DEV_AGENT_EDIT=1` opens the inline socket editor in Settings. `VELA_DEV_EDITOR_MENU=1` opens the editor dropdown after the first frame, attached to its button; combine it with `VELA_DEV_SETTINGS=editor-low` to see it open Above. `VELA_DEV_EMPTY_CONFIRM=1` opens the “Clear edit copies?” confirmation.
- `VELA_DEV_SETTINGS=create|editor:<id>|picker:<token>` opens the theme editor or opens a color picker for a new theme, for example `picker:accent`. `create:terminal`, `create:contrast` and `create:dark` show the advanced terminal group, a low-contrast text warning and the dark draft preview.
- `VELA_DEV_LOCAL_ROOT=<absolute path>` overrides the local pane's starting folder.
- `VELA_DEV_REMOTE_DELAY_MS=<milliseconds>` delays each remote listing asynchronously.
- `VELA_DEV_EXPAND=<relative path>[,...]` opens the named folders and their ancestors in the initial remote tree.
- `VELA_DEV_WINDOW_WIDTH=<pixels>` requests an initial width of at least 640 logical pixels for layout checks. The window manager may override it.
- `VELA_DEV_WINDOW_FLOAT=1` uses a floating test window to keep the requested width when checking layouts under a tiling window manager.
- `VELA_DEV_HIDE_SIDEBAR=1` starts with the sidebar hidden for layout checks.

- `VELA_DEV_TRANSFER=<upload|download>:<absolute source>:<absolute destination>` enqueues one transfer after autoconnect. Conflicts use the usual dialog.
- `VELA_DEV_TRANSFER_DELAY_MS=<milliseconds>` waits after autoconnect before enqueueing that transfer.
- `VELA_DEV_TRANSFER_EXPANDED=1` opens the transfer history at launch.
- `VELA_DEV_COPY=<absolute remote path>[,...]` prepares a remote clipboard copy using the regular F4 transfer after autoconnect.
- `VELA_DEV_PASTE=remote` runs the remote paste action after autoconnect.
- `VELA_DEV_STAGING_ROOT=<absolute path>` isolates the Finder staging cache for development.
- `VELA_DEV_DROP_HIGHLIGHT=1` previews an external drag over the remote root without submitting a drop.
- `VELA_DEV_FILE_DRAG=<local|remote>:<source relative to the pane root>:<hover relative path or "." for root>` previews the internal drop target after listing, with no drag chip or spring-loading. For local previews, `VELA_DEV_EXPAND` also opens the named folders in the local tree.
- `VELA_DEV_FILE_STATE=<local|remote>:<delete|rename|menu>` selects the first entry and opens the given state for visual checks. The delete trigger only opens the confirmation.
- `VELA_DEV_EDIT=<absolute remote path>` opens a remote edit after autoconnect.
- `VELA_DEV_EDITOR=none` suppresses editor launches for unattended runs.
- `VELA_DEV_EDIT_VIEW=popover|settings` shows Edits on connection or opens Settings on startup for visual checks.
- `VELA_DEV_SERVER_STATE=connecting|connecting-cancel|error|error-details|error-auth` puts the selected server into a visual state after loading the list, without opening SSH: a spinner (with or without “Cancel”), `ConnectFailed` with sample stderr collapsed or expanded inside the error banner, or `AuthFailed` with “Edit Server”. Use `VELA_DEV_THEME=light|dark` and `VELA_DEV_REDUCE_MOTION=1` for deterministic captures.
- `VELA_DEV_UPDATE_STATE=checking|available|confirm|installing:<pct>|error` previews the updater without checking for releases. `available` includes a sample changelog and `confirm` also opens the confirmation dialog. `VELA_DEV_UPDATE_URL`, `VELA_DEV_UPDATE_PUBKEY` and `VELA_DEV_UPDATE_VERSION` override the release endpoint, Ed25519 public key and current version for end-to-end dev runs.
- `VELA_DEV_FORM=new` opens the new-server form at startup. `VELA_DEV_FORM=edit:<display name>` opens the edit form of that server. `VELA_DEV_FORM=duplicate:<display name>` opens the add-server form prefilled from that server with ` duplicate` appended to its name.
- `VELA_DEV_ROW_MENU=<display name>` selects that server and opens its floating right-click menu at the centre of its row.
- `VELA_DEV_DRAG=<display name>:<dy>` shows that server's card dragged `dy` px from its slot, with the other cards making room, and holds it there for visual checks.
- `VELA_DEV_GLASS=<chrome>,<content>` enables frosted glass for the run with opacity percentages from 30 to 100, for example `70,92`. The override stays out of settings.toml.
- `VELA_DEV_REDUCE_TRANSPARENCY=1|0` overrides the system “Reduce Transparency” setting. When enabled, the app uses an opaque window and disables the glass sliders.
- `VELA_DEV_REDUCE_MOTION=1|0` overrides the system "reduce motion" setting. Animations render their end state when it is on.
- Server forms list the keys of the agent socket in their Socket field, or else the first Bitwarden socket under `$HOME`. To check the picker without the real Bitwarden, set `HOME` to a temporary directory holding a `.bitwarden-ssh-agent.sock` symlink to the fixture agent, or a stale socket for the unavailable state.
- With a custom `VELA_CONFIG_DIR`, remote edits use its `edit-cache/` directory.

For example, from the repository root in a macOS graphical session (reusing the task's `CARGO_TARGET_DIR`, and deleting `CARGO_TARGET_DIR` and temporary configs only at the end of the complete task):

```sh
export CARGO_TARGET_DIR="/tmp/vela-target-<task>"
config_dir="$(mktemp -d /tmp/vela-config.XXXXXX)"

cargo build --release --features dev
VELA_CONFIG_DIR="$config_dir" VELA_DEV_LOG=1 "$CARGO_TARGET_DIR/release/vela"

# Clean up temporary config after the run; remove $CARGO_TARGET_DIR only when the full task is done:
rm -rf -- "$config_dir"
```

## Linux (Arch, Wayland)

Build in a disposable `archlinux:latest` container when Podman or Docker is available, mounting the repository at `/src` and using a container-only target directory. The container needs `base-devel rustup pkgconf wayland libxkbcommon libxkbcommon-x11 vulkan-icd-loader vulkan-headers fontconfig freetype2 openssh openbsd-netcat python libx11 libxcb`; `rustup` then installs the toolchain from `rust-toolchain.toml`. Without a container runtime, build on the host using all available CPU cores and a single task-owned `CARGO_TARGET_DIR` in `/tmp` reused across all commands during the feature (leaving Cargo's default incremental compilation enabled while iterating, and deleting `CARGO_TARGET_DIR` only once the complete feature/task is done); the host needs the same runtime libraries plus a Vulkan ICD or Mesa EGL. The manual fixture needs `nc` from `openbsd-netcat` and Python 3; the GPUI renderer needs a Vulkan driver, such as `vulkan-intel` on Intel graphics, or Mesa EGL.

Local delete (Delete) moves items to the freedesktop Trash: the home trash, or `.Trash-<uid>` on another disk mounted from a block device. Items on tmpfs such as `/tmp`, or on a disk whose trash cannot be prepared, are copied into the home trash, as Dolphin does, because Dolphin does not list trashes on memory disks. Run the dev release binary in a Wayland session. `VELA_CONFIG_DIR` isolates the configuration and `VELA_DEV_LOG=1` prints the usual diagnostics. The window sets the `vela` app id for Hyprland rules. On Linux, Cmd shortcuts are Ctrl (`secondary-`), and the integrated terminal uses Ctrl+Shift+C, Ctrl+Shift+V and Ctrl+Shift+A so Ctrl+C, Ctrl+V and Ctrl+A reach the shell. The terminal font shortcuts are Ctrl+Shift+`+` (grow), Ctrl+Shift+`-` (shrink) and Ctrl+Shift+0 (reset to `kitty.conf`); on a Spanish QWERTY layout those physical keys are encoded by GPUI as `ctrl-*`, `ctrl-_` and `ctrl-=`, with `ctrl-+` also bound for the US `=` key. Steps are 1 pt within 6–48, applied to every open terminal. Ctrl+`-` and Ctrl+0 without Shift are not bound. Files open with `xdg-open`, Kitty launches with `kitty --detach`, and “Show in Folder” opens the parent folder. Dolphin drag-out and the file clipboard are still pending in F9.

For an exact-width layout capture on Hyprland, launch through a temporary wrapper script that sets the dev variables and ends with `exec /path/to/vela`, then run `hyprctl dispatch 'hl.dsp.exec_cmd("/tmp/vela-run/run.sh", { workspace = "3 silent", float = true, size = {736, 700} })'`. Check `hyprctl clients -j` reports class `vela`, `floating: true` and the requested size; focus workspace 3 with `hl.dsp.focus({ workspace = "3" })` for `grim`, then workspace 1 and close with `hl.dsp.window.close({ window = "class:^vela$" })`. The exec rule works on Hyprland 0.57.2 even with workspaces 1 and 2 occupied; `VELA_DEV_WINDOW_FLOAT=1` alone does not make the window float.

Dragging within a file pane moves items. Dragging into the opposite pane copies through the transfer queue.
