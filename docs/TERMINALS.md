# Terminal settings

**External Terminal** in Settings → Terminal chooses the application used by **Open in …** in the Terminal header and **Run in …** in the remote file menu. Each invocation starts a separate SSH channel through the current connection. Open starts in the remote pane's directory. Run executes the selected script and leaves an interactive shell open.

Automatic prefers Kitty and otherwise uses the first available preset in the order shown below for the platform. Vela looks for executables in its Linux `PATH` and app bundles in the standard macOS application directories. Unknown or unavailable saved IDs also use Automatic.

| Platform | Terminal | Setting ID | Launch |
| --- | --- | --- | --- |
| Linux | Kitty | `kitty` | `kitty --detach` opens a window. [Kitty CLI](https://sw.kovidgoyal.net/kitty/invocation/) |
| Linux | Ghostty | `ghostty` | `ghostty +new-tab -e` uses D-Bus when available (Ghostty 1.4+), with `ghostty -e` as the new-window fallback. [Ghostty 1.4 release notes](https://ghostty.org/docs/install/release-notes/1-4-0) |
| Linux | WezTerm | `wezterm` | `wezterm start --new-tab --no-auto-connect --domain local --` prefers a new tab in a running GUI, otherwise opens a window. [WezTerm release notes](https://wezterm.org/changelog.html) |
| Linux | Alacritty | `alacritty` | `alacritty -e` opens a window. [Alacritty CLI](https://alacritty.org/releases/0.16.1/cmd-alacritty.html) |
| Linux | foot | `foot` | `foot --` opens a Wayland window. [foot manual](https://man.archlinux.org/man/extra/foot/foot.1.en) |
| Linux | Konsole | `konsole` | `konsole --new-tab -e` opens a new tab or window. [Konsole handbook](https://docs.kde.org/stable_kf6/es/konsole/konsole/command-line-options.html) |
| Linux | GNOME Terminal | `gnome-terminal` | `gnome-terminal --tab --` opens a new tab or window. [GNOME Terminal help](https://help.gnome.org/gnome-terminal/pref-tab-window.html) |
| Linux | xterm | `xterm` | `xterm -e` opens an X11 window. [xterm manual](https://invisible-island.net/xterm/manpage/xterm.html) |
| macOS | Kitty | `kitty` | `open -na kitty --args` opens a new instance. [Kitty CLI](https://sw.kovidgoyal.net/kitty/invocation/) |
| macOS | Terminal | `terminal` | Launch Services opens a private `.command` file in Terminal. [Apple command file guide](https://developer.apple.com/library/archive/documentation/Porting/Conceptual/PortingUnix/unix_environments/unix_environments.html) |
| macOS | iTerm2 | `iterm2` | AppleScript creates a tab in the current window or opens a window. macOS may ask for Automation permission. [iTerm2 scripting reference](https://iterm2.com/documentation-scripting.html) |
| macOS | Ghostty | `ghostty` | `open -na Ghostty.app --args --window-save-state=never -e` opens a new instance. Ghostty 1.4 or later includes the macOS argv fix. [Ghostty 1.4 release notes](https://ghostty.org/docs/install/release-notes/1-4-0) |
| macOS | WezTerm | `wezterm` | The bundle's CLI uses the same `start` flags as on Linux. [WezTerm CLI](https://wezterm.org/config/launch.html) |
| macOS | Alacritty | `alacritty` | The bundle's executable accepts `-e` and opens a window. [Alacritty CLI](https://alacritty.org/releases/0.16.1/cmd-alacritty.html) |

Direct launch recipes append `/usr/bin/ssh` and the SSH argument vector. Terminal and iTerm2 receive only the path to the launcher. The launcher reads the arguments from a private file and passes them individually to SSH.

## Integrated terminal configuration

**Configuration Source** selects which configuration to read into Vela. Automatic tries the selected external terminal's config, then Kitty, then Vela defaults. Choose **None** to use defaults directly. **Reload** rereads the source and applies the supported values to all open integrated terminals. Saved Vela font size and copy-on-select preferences override imported values. **Colors → Imported** uses the imported palette while available. The old `terminal_colors = "kitty"` setting is accepted as an alias for `"imported"`.

| Source | File discovery | Imported values |
| --- | --- | --- |
| Kitty | `KITTY_CONFIG_DIRECTORY/kitty.conf` or `$XDG_CONFIG_HOME/kitty/kitty.conf`, using `~/.config` as the default XDG directory | `font_family`, `bold_font`, `italic_font`, `bold_italic_font`, `font_size`, `foreground`, `background`, `cursor`, `cursor_text_color`, `selection_foreground`, `selection_background`, `color0`–`color15`, `cursor_shape`, `scrollback_lines`, `copy_on_select`, `macos_option_as_alt`, `include`, `globinclude`. [Kitty configuration](https://sw.kovidgoyal.net/kitty/conf/) |
| Ghostty | XDG `ghostty/config` and `ghostty/config.ghostty`, followed by the same files in `~/Library/Application Support/com.mitchellh.ghostty` on macOS | `font-family` and its three style variants, `font-size`, foreground, background, cursor and selection colors, `palette` indices 0–15, `cursor-style`, `copy-on-select`, `macos-option-as-alt`, and `config-file` (applied after the default files). Uses hex colors. [Ghostty configuration](https://ghostty.org/docs/config) |
| Alacritty | XDG `alacritty/alacritty.toml` or `alacritty.toml`, then `~/.config/alacritty/alacritty.toml`, `~/.alacritty.toml`, or `/etc/alacritty/alacritty.toml` | `font.*.family`, `font.size`, `colors.primary`, `colors.normal`, `colors.bright`, `colors.cursor`, `colors.selection`, `cursor.style`, `scrolling.history`, `selection.save_to_clipboard`, `window.option_as_alt`, and `general.import` (also reads legacy root-level `import`). Imports load in order before the file that includes them. [Alacritty configuration](https://alacritty.org/releases/0.16.1/config-alacritty.html) |
| foot | `foot/foot.ini` under XDG config home, then `XDG_CONFIG_DIRS` (default `/etc/xdg`) | First `font` family and its point `size`, style font families, `cursor.style`, `scrollback.lines`, `selection-target`, ANSI 0–15 and foreground/background/cursor/selection colors from the palette chosen by `initial-color-theme`, and in-place absolute or home-relative `include`. [foot configuration](https://man.archlinux.org/man/foot.ini.5.en) |
| WezTerm (partial) | `WEZTERM_CONFIG_FILE`, XDG `wezterm/wezterm.lua`, `~/.config/wezterm/wezterm.lua`, or `~/.wezterm.lua` | Simple `config.font_size`, `config.font = wezterm.font("Family")`, `config.scrollback_lines`, and `config.send_composed_key_when_*_alt_is_pressed` assignments. Vela reads these as text. Lua evaluation, modules, and WezTerm palettes are unsupported. [WezTerm configuration](https://wezterm.org/config/files.html) |

Vela skips missing includes and protects against cyclic references. Other terminal profiles, Ghostty named themes and colors, extended palettes above ANSI 15, and foot font sizes in pixels are outside the imported subset.
