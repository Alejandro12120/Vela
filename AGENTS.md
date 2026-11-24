# vSCP

Read [continuity](.agent/CONTINUITY.md) for the current scope and pending decisions.

## Build workflow

- Use disposable containers for Linux compilation and checks. Run one build at a time. Keep build artifacts in the container or in a task-owned temporary directory, and remove them at the end.
- For Rust, use `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0` and a disposable `CARGO_TARGET_DIR`. Once the framework is selected, pin the Rust toolchain and Linux dependencies in the build image.
- Run desktop integration checks from a regular graphical session on the target OS. Use the macOS SDK on macOS for AppKit compilation. Linux headless checks cannot establish Dolphin or Finder compatibility.
- See the [file handoff probe](probes/file-handoff/README.md) for current build and test commands.
- Design and review all UI with the [Heyrico UI skill](.agent/skills/heyrico-ui/SKILL.md). Read it before changing any view, and pass it to delegated UI work.
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

Run the local PTY test without loading the user's Kitty configuration or starting SSH. Use the same disposable Cargo target settings as above.

```sh
cargo test -p vscp-app terminal::tests::pty_output_reaches_grid_and_can_resize
```

## Manual SSH fixture

Run `scripts/dev-sshd.sh` in a terminal as a regular user. It needs system OpenSSH, `nc` and Python 3. Enter the printed host, port, user, public key and agent socket in the server form. Ctrl+C stops the loopback server and its private agent and removes their temporary files.

User themes live in `themes/<id>.toml` under the configuration directory. `settings.toml` stores the selected theme IDs (`light_theme` and `dark_theme`), color scheme, glass settings, the remote editor, the global SSH agent socket (`agent_socket`, optional; without it vSCP uses the detected Bitwarden socket) and the terminal font size (`terminal_font_size`, optional; without it vSCP uses `kitty.conf`).

Use an isolated configuration directory for development runs. The `dev` Cargo feature enables these environment variables in both debug and release binaries.

- `VSCP_CONFIG_DIR` sets the directory for `servers.toml` and the OpenSSH `known_hosts` file.
- `VSCP_DEV_AUTOCONNECT` connects to the server with the given display name after loading the list.
- `VSCP_DEV_VIEW=terminal` opens the Terminal view after autoconnect.
- `VSCP_DEV_TERMINAL_INPUT=<text>` sends the text once after starting a terminal. It decodes `\n`, `\r` and `\\`.
- `VSCP_DEV_KITTY_CONF=<path>` overrides the Kitty configuration file for a development run.
- `VSCP_DEV_TERMINAL_FONT_SIZE=<pt>` sets the initial terminal font size in points (clamped to 6–48) for captures, without physical keys. Reset still returns to the `Kitty.size` from `kitty.conf`.
- `VSCP_DEV_LOG=1` prints connection results, listing durations, visible row counts, transfer events, native file-drag payload decisions and remote-edit watcher, debounce and upload decisions to stderr. Font-size changes print `Terminal: font size <n>`. It also prints a `[keys]` line for each keystroke with its resolved action (or `none`) and the key context stack, to diagnose key routing. When a terminal loads its settings it prints `Terminal: kitty.conf <path or none>; font_family <configured value or default>; resolved <family>; installed <yes/no>`, once with the initial default and again after reading the Kitty configuration.
- `VSCP_DEV_THEME=light|dark` overrides system appearance as seen by vSCP in Sistema mode.
- `VSCP_DEV_THEME_ID=<id>` overrides both themes for the run without saving. Use `VSCP_DEV_THEME_ID=<light>,<dark>` to set them independently.
- `VSCP_DEV_SETTINGS=appearance|interface|agent|editor|editor-low|terminal` opens Ajustes on startup and scrolls to Apariencia, Interfaz, Agente SSH, Editor or Terminal. `editor-low` places the Editor section at the bottom of the viewport so its dropdown opens Above. `VSCP_DEV_SETTINGS=focus-moon` focuses the vSCP moon orb. `VSCP_DEV_SETTINGS=delete:<id>` opens the confirmation for a user theme.
- `VSCP_DEV_EDITOR_PICKER=1` shows the Editor picker on Linux for layout captures. `VSCP_DEV_EDITOR_ROOT=<absolute path>` supplies a directory holding fake `.app` bundles (Visual Studio Code.app, Cursor.app, Zed.app, Sublime Text.app, BBEdit.app, Nova.app, TextEdit.app) for the picker's detection; launching is unchanged.
- `VSCP_DEV_PROMPT=password-rejected` opens a fake Password prompt for destination `prueba` in the rejected state, so «El servidor rechazó la contraseña.» is shown under the field.
- `VSCP_DEV_FORM_FILE=<path>` opens the new form in file-key mode with that public key chosen, for key-row captures. `VSCP_DEV_FORM_KEY=<comment>` preselects the agent key with that comment. `VSCP_DEV_FORM_ADVANCED=1` opens «Opciones avanzadas» and `VSCP_DEV_FORM_PASSWORD=1` starts the form in password mode.
- `VSCP_DEV_AGENT_EDIT=1` opens the inline socket editor in Ajustes. `VSCP_DEV_EDITOR_MENU=1` opens the editor dropdown after the first frame, attached to its button; combine it with `VSCP_DEV_SETTINGS=editor-low` to see it open Above. `VSCP_DEV_EMPTY_CONFIRM=1` opens the «Vaciar las copias de edición?» confirmation.
- `VSCP_DEV_SETTINGS=create|editor:<id>|picker:<token>` opens the theme editor or opens a color picker for a new theme, for example `picker:accent`. `create:terminal`, `create:contrast` and `create:dark` show the advanced terminal group, a low-contrast text warning and the dark draft preview.
- `VSCP_DEV_LOCAL_ROOT=<absolute path>` overrides the local pane's starting folder.
- `VSCP_DEV_REMOTE_DELAY_MS=<milliseconds>` delays each remote listing asynchronously.
- `VSCP_DEV_EXPAND=<relative path>[,...]` opens the named folders and their ancestors in the initial remote tree.
- `VSCP_DEV_WINDOW_WIDTH=<pixels>` requests an initial width of at least 640 logical pixels for layout checks. The window manager may override it.
- `VSCP_DEV_WINDOW_FLOAT=1` uses a floating test window to keep the requested width when checking layouts under a tiling window manager.
- `VSCP_DEV_HIDE_SIDEBAR=1` starts with the sidebar hidden for layout checks.

- `VSCP_DEV_TRANSFER=<upload|download>:<absolute source>:<absolute destination>` enqueues one transfer after autoconnect. Conflicts use the usual dialog.
- `VSCP_DEV_TRANSFER_DELAY_MS=<milliseconds>` waits after autoconnect before enqueueing that transfer.
- `VSCP_DEV_TRANSFER_EXPANDED=1` opens the transfer history at launch.
- `VSCP_DEV_COPY=<absolute remote path>[,...]` prepares a remote clipboard copy using the regular F4 transfer after autoconnect.
- `VSCP_DEV_PASTE=remote` runs the remote paste action after autoconnect.
- `VSCP_DEV_STAGING_ROOT=<absolute path>` isolates the Finder staging cache for development.
- `VSCP_DEV_DROP_HIGHLIGHT=1` previews an external drag over the remote root without submitting a drop.
- `VSCP_DEV_FILE_DRAG=<local|remote>:<source relative to the pane root>:<hover relative path or "." for root>` previews the internal drop target after listing, with no drag chip or spring-loading. For local previews, `VSCP_DEV_EXPAND` also opens the named folders in the local tree.
- `VSCP_DEV_FILE_STATE=<local|remote>:<delete|rename|menu>` selects the first entry and opens the given state for visual checks. The delete trigger only opens the confirmation.
- `VSCP_DEV_EDIT=<absolute remote path>` opens a remote edit after autoconnect.
- `VSCP_DEV_EDITOR=none` suppresses editor launches for unattended runs.
- `VSCP_DEV_EDIT_VIEW=popover|settings` shows Ediciones on connection or opens Ajustes on startup for visual checks.
- `VSCP_DEV_FORM=new` opens the new-server form at startup. `VSCP_DEV_FORM=edit:<display name>` opens the edit form of that server. `VSCP_DEV_FORM=duplicate:<display name>` opens the add-server form prefilled from that server with ` duplicate` appended to its name.
- `VSCP_DEV_ROW_MENU=<display name>` selects that server and opens its floating right-click menu at the centre of its row.
- `VSCP_DEV_DRAG=<display name>:<dy>` shows that server's card dragged `dy` px from its slot, with the other cards making room, and holds it there for visual checks.
- `VSCP_DEV_GLASS=<chrome>,<content>` enables frosted glass for the run with opacity percentages from 30 to 100, for example `70,92`. The override stays out of settings.toml.
- `VSCP_DEV_REDUCE_TRANSPARENCY=1|0` overrides the system “Reduce Transparency” setting. When enabled, the app uses an opaque window and disables the glass sliders.
- `VSCP_DEV_REDUCE_MOTION=1|0` overrides the system "reduce motion" setting. Animations render their end state when it is on.
- Server forms list the keys of the agent socket in their Socket field, or else the first Bitwarden socket under `$HOME`. To check the picker without the real Bitwarden, set `HOME` to a temporary directory holding a `.bitwarden-ssh-agent.sock` symlink to the fixture agent, or a stale socket for the unavailable state.
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

## Linux (Arch, Wayland)

Build in a disposable `archlinux:latest` container when Podman or Docker is available, mounting the repository at `/src` and using a container-only target directory. The container needs `base-devel rustup pkgconf wayland libxkbcommon libxkbcommon-x11 vulkan-icd-loader vulkan-headers fontconfig freetype2 openssh openbsd-netcat python libx11 libxcb`; `rustup` then installs the toolchain from `rust-toolchain.toml`. Without a container runtime, build on the host with a disposable `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=1` and `CARGO_INCREMENTAL=0`; the host needs the same runtime libraries plus a Vulkan ICD or Mesa EGL. The manual fixture needs `nc` from `openbsd-netcat` and Python 3; the GPUI renderer needs a Vulkan driver, such as `vulkan-intel` on Intel graphics, or Mesa EGL.

Local delete (Supr) moves items to the freedesktop Trash: the home trash, or `.Trash-<uid>` on another disk mounted from a block device. Items on tmpfs such as `/tmp`, or on a disk whose trash cannot be prepared, are copied into the home trash, as Dolphin does, because Dolphin does not list trashes on memory disks. Run the dev release binary in a Wayland session. `VSCP_CONFIG_DIR` isolates the configuration and `VSCP_DEV_LOG=1` prints the usual diagnostics. The window sets the `vscp` app id for Hyprland rules. On Linux, Cmd shortcuts are Ctrl (`secondary-`), and the integrated terminal uses Ctrl+Shift+C, Ctrl+Shift+V and Ctrl+Shift+A so Ctrl+C, Ctrl+V and Ctrl+A reach the shell. The terminal font shortcuts are Ctrl+Shift+`+` (grow), Ctrl+Shift+`-` (shrink) and Ctrl+Shift+0 (reset to `kitty.conf`); on a Spanish QWERTY layout those physical keys are encoded by GPUI as `ctrl-*`, `ctrl-_` and `ctrl-=`, with `ctrl-+` also bound for the US `=` key. Steps are 1 pt within 6–48, applied to every open terminal. Ctrl+`-` and Ctrl+0 without Shift are not bound. Files open with `xdg-open`, Kitty launches with `kitty --detach`, and «Mostrar en la carpeta» opens the parent folder. Dolphin drag-out and the file clipboard are still pending in F9.

For an exact-width layout capture on Hyprland, launch through a temporary wrapper script that sets the dev variables and ends with `exec /path/to/vscp`, then run `hyprctl dispatch 'hl.dsp.exec_cmd("/tmp/vscp-run/run.sh", { workspace = "3 silent", float = true, size = {736, 700} })'`. Check `hyprctl clients -j` reports class `vscp`, `floating: true` and the requested size; focus workspace 3 with `hl.dsp.focus({ workspace = "3" })` for `grim`, then workspace 1 and close with `hl.dsp.window.close({ window = "class:^vscp$" })`. The exec rule works on Hyprland 0.57.2 even with workspaces 1 and 2 occupied; `VSCP_DEV_WINDOW_FLOAT=1` alone does not make the window float.

Dragging within a file pane moves items. Dragging into the opposite pane copies through the transfer queue.
