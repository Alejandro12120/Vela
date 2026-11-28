#[path = "glass_settings.rs"]
mod glass_settings;
#[path = "theme_editor.rs"]
mod theme_editor;

use std::{
    cell::Cell,
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, ClickEvent, Div, ElementId, Entity, FocusHandle,
    Focusable, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, PathPromptOptions, Pixels,
    Role, ScrollHandle, SharedString, Stateful, canvas, deferred, ease_out_quint,
    linear_color_stop, linear_gradient, point, prelude::*, px, rgba,
};
use vela_core::{
    agent::AgentError,
    appearance::{
        Colors, Scheme, ThemeDef, delete_user_theme, import_theme, load_user_themes,
        save_user_theme, unused_theme_id,
    },
    path::abbreviate_path,
    settings::{Settings, TerminalColorSource, TerminalConfigSource},
};

use super::{Context, Dialog, Theme, Variant, Window, Workspace, button, div};
use crate::{
    appearance::{Appearance, Variant as ColorVariant},
    components::{
        button::{icon_button, icon_text_button, small_button, small_icon_text_button},
        dropdown::{self, Placement, dropdown_item, dropdown_panel, menu_height, place},
        segmented::segmented,
        status_dot::status_dot,
        switch::switch,
        text_input::TextInput,
    },
    context_menu::Tooltip,
    external_terminal::ExternalTerminals,
    icons::{IconName, icon},
    terminal::{
        colors::{effective_source, terminal_palette},
        font_size,
        imported::{self, ImportedConfig},
    },
};

struct ThemeCardFocus {
    card: FocusHandle,
    light: FocusHandle,
    dark: FocusHandle,
}

impl ThemeCardFocus {
    fn new(cx: &Context<Workspace>) -> Self {
        Self {
            card: cx.focus_handle().tab_stop(true),
            light: cx.focus_handle().tab_stop(true),
            dark: cx.focus_handle().tab_stop(true),
        }
    }
}

type AgentProbe = Result<usize, AgentError>;
type ClickHandler = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static>;

const EDITORS: [(&str, &str); 7] = [
    ("Visual Studio Code", "Visual Studio Code.app"),
    ("Cursor", "Cursor.app"),
    ("Zed", "Zed.app"),
    ("Sublime Text", "Sublime Text.app"),
    ("BBEdit", "BBEdit.app"),
    ("Nova", "Nova.app"),
    ("TextEdit", "TextEdit.app"),
];

fn format_copies_size(bytes: u64) -> String {
    let (value, unit) = if bytes >= 1_000_000_000 {
        (bytes as f64 / 1_000_000_000., "GB")
    } else if bytes >= 1_000_000 {
        (bytes as f64 / 1_000_000., "MB")
    } else {
        (bytes as f64 / 1_000., "KB")
    };
    format!("{value:.1} {unit}")
}

/// Installed editor bundles, in menu order. Only paths that exist are returned.
pub(crate) fn installed_editors(roots: &[PathBuf]) -> Vec<(&'static str, PathBuf)> {
    let mut found = Vec::new();
    for (name, bundle) in EDITORS {
        for root in roots {
            let path = root.join(bundle);
            if path.is_dir() {
                found.push((name, path));
                break;
            }
        }
    }
    found
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Editor,
    ExternalTerminal,
    TerminalConfig,
}

impl Picker {
    fn trigger(self, page: &SettingsPage) -> &FocusHandle {
        &page.picker_triggers[self as usize]
    }

    fn bounds(self, page: &SettingsPage) -> &Rc<Cell<Option<Bounds<Pixels>>>> {
        &page.picker_bounds[self as usize]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::ExternalTerminal => "External Terminal",
            Self::TerminalConfig => "Configuration Source",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Editor => "editor-picker",
            Self::ExternalTerminal => "external-terminal-picker",
            Self::TerminalConfig => "terminal-config-picker",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum PickerValue {
    Editor(Option<PathBuf>),
    OtherEditor,
    External(Option<&'static str>),
    Config(TerminalConfigSource),
}

struct SettingsMenu {
    picker: Picker,
    placement: Placement,
    height: Pixels,
    items: Vec<FocusHandle>,
    entries: Vec<(&'static str, PickerValue)>,
    scroll: ScrollHandle,
    closing: bool,
    restore_focus: bool,
    generation: u64,
}

impl SettingsMenu {
    fn focus(&self, index: usize, window: &mut Window, cx: &mut gpui::App) {
        let separator = usize::from(matches!(self.entries[index].1, PickerValue::OtherEditor));
        self.scroll.scroll_to_item(index + separator);
        self.items[index].focus(window, cx);
    }
}

pub(super) struct SettingsPage {
    previous_focus: Option<FocusHandle>,
    opening: bool,
    closing: bool,
    transition_generation: u64,
    focus: FocusHandle,
    scroll: ScrollHandle,
    schemes: [FocusHandle; 3],
    themes: HashMap<String, ThemeCardFocus>,
    busy: bool,
    theme_error: Option<String>,
    editor: Option<theme_editor::ThemeEditor>,
    glass_drag: Option<glass_settings::GlassDrag>,
    agent_probe: Option<AgentProbe>,
    agent_editing: bool,
    agent_input: Option<Entity<TextInput>>,
    picker_triggers: [FocusHandle; 3],
    picker_bounds: [Rc<Cell<Option<Bounds<Pixels>>>>; 3],
    settings_viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    settings_menu: Option<SettingsMenu>,
    menu_generation: u64,
    edit_copies: Option<(usize, u64)>,
    pending_settings: Option<Settings>,
}

impl SettingsPage {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        Self {
            previous_focus: None,
            opening: false,
            closing: false,
            transition_generation: 0,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            schemes: std::array::from_fn(|_| cx.focus_handle()),
            themes: HashMap::new(),
            busy: false,
            theme_error: None,
            editor: None,
            glass_drag: None,
            agent_probe: None,
            agent_editing: false,
            agent_input: None,
            picker_triggers: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            picker_bounds: Default::default(),
            settings_viewport: Rc::new(Cell::new(None)),
            settings_menu: None,
            menu_generation: 0,
            edit_copies: None,
            pending_settings: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.previous_focus.is_some()
    }

    pub(super) fn is_visible(&self) -> bool {
        self.is_open() || self.closing
    }

    pub(super) fn is_opening(&self) -> bool {
        self.is_open() && self.opening
    }

    pub(super) fn is_closing(&self) -> bool {
        !self.is_open() && self.closing
    }

    pub(super) fn transition_generation(&self) -> u64 {
        self.transition_generation
    }
}

impl Workspace {
    pub(crate) fn settings_open(&self) -> bool {
        self.settings_page.is_open()
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir.is_none() || self.dialog.is_some() || self.mcp.is_open() || self.quitting {
            return;
        }
        if self.settings_open() {
            self.close_settings(window, cx);
            return;
        }
        // Reopening while the page is fading out cancels that timer by
        // invalidating its generation.
        self.settings_page.closing = false;
        self.settings_page.transition_generation += 1;
        let generation = self.settings_page.transition_generation;
        self.dismiss_row_menu(window, cx);
        self.edits.popover = false;
        self.settings_page.previous_focus =
            Some(window.focused(cx).unwrap_or_else(|| self.focus.clone()));
        self.settings_page.focus.focus(window, cx);
        crate::terminal::view::font_names(cx);
        self.settings_page.scroll = ScrollHandle::new();
        self.probe_agent(window, cx);
        self.scan_edit_copies(window, cx);
        self.scan_mcp_harnesses(window, cx);
        self.load_mcp(window, cx);
        if cx.reduce_motion() {
            self.settings_page.opening = false;
        } else {
            self.settings_page.opening = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(crate::components::motion::BASE)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.settings_page.transition_generation == generation {
                        this.settings_page.opening = false;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn scan_edit_copies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(root) = crate::dev::edit_root() else {
            self.settings_page.edit_copies = Some((0, 0));
            return;
        };
        let scan = cx.background_executor().spawn(async move {
            fn walk(dir: &std::path::Path, count: &mut usize, bytes: &mut u64) {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    return;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    if metadata.is_dir() {
                        walk(&path, count, bytes);
                    } else if metadata.is_file()
                        && path
                            .file_name()
                            .is_some_and(|name| name != "edits.toml" && name != ".lock")
                    {
                        *count += 1;
                        *bytes += metadata.len();
                    }
                }
            }
            let (mut count, mut bytes) = (0, 0);
            walk(&root, &mut count, &mut bytes);
            (count, bytes)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = scan.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.settings_page.edit_copies = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn empty_edit_copies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_dialog(window, cx);
        let Some(mut index) = self.edits.index.take() else {
            return;
        };
        let targets: Vec<_> = index
            .edits()
            .iter()
            .filter(|edit| {
                edit.state == vela_core::edit::State::Synced
                    && !self.edits.has_active_session(&edit.target)
            })
            .map(|edit| edit.target.clone())
            .collect();
        for target in targets {
            let _ = index.remove(&target, false);
        }
        self.edits.index = Some(index);
        self.scan_edit_copies(window, cx);
    }

    fn reveal_edit_copies(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = crate::dev::edit_root().ok() else {
            return;
        };
        let reveal = cx
            .background_executor()
            .spawn(async move { super::edits_view::reveal(&root) });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = reveal.await {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.settings_error = Some(format!("Couldn't show the folder: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn probe_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.effective_socket().map(Path::to_owned) else {
            self.settings_page.agent_probe = Some(Err(AgentError::Unavailable));
            return;
        };
        self.settings_page.agent_probe = None;
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = match client {
                Some(client) => client.list_agent_keys(path).await.map(|keys| keys.len()),
                None => Err(AgentError::Unavailable),
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.settings_page.agent_probe = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn edit_agent_socket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .effective_socket()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let input =
            cx.new(|cx| TextInput::new(&current, "Agent Socket", "/path/to/agent.sock", false, cx));
        self.settings_page.agent_input = Some(input.clone());
        self.settings_page.agent_editing = true;
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn commit_agent_socket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.settings_page.agent_input.clone() else {
            return;
        };
        let value = input.read(cx).value().trim().to_owned();
        self.settings_page.agent_editing = false;
        self.settings_page.agent_input = None;
        let mut next = self.settings.clone();
        next.agent_socket = if value.is_empty() {
            None
        } else {
            Some(PathBuf::from(value))
        };
        self.save_settings(next, window, cx);
        self.probe_agent(window, cx);
    }

    fn cancel_agent_socket(&mut self, cx: &mut Context<Self>) {
        self.settings_page.agent_editing = false;
        self.settings_page.agent_input = None;
        cx.notify();
    }

    fn use_bitwarden_socket(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        next.agent_socket = None;
        self.save_settings(next, window, cx);
        self.probe_agent(window, cx);
    }

    fn agent_status(&self, theme: Theme) -> (&'static str, gpui::Rgba) {
        match &self.settings_page.agent_probe {
            None => ("consultando…", theme.secondary_text),
            Some(Ok(count)) if *count == 0 => ("bloqueado", theme.warning),
            Some(Ok(_)) => ("disponible", theme.success),
            Some(Err(AgentError::Unavailable)) => ("not found", theme.secondary_text),
            Some(Err(AgentError::Refused | AgentError::Timeout)) => ("bloqueado", theme.warning),
            Some(Err(AgentError::Protocol(_))) => ("error", theme.danger),
        }
    }

    fn agent_name(&self) -> &'static str {
        let socket = self.effective_socket().unwrap_or(Path::new(""));
        if vela_core::bitwarden_agent_sockets()
            .iter()
            .any(|path| path == socket)
        {
            "Bitwarden"
        } else {
            "SSH Agent"
        }
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_glass_drag(window, cx);
        self.close_theme_editor(window, cx);
        self.close_settings_menu(false, window, cx);
        if let Some(previous) = self.settings_page.previous_focus.take() {
            // Focus goes back at once; the page keeps painting while it fades.
            previous.focus(window, cx);
            self.settings_page.transition_generation += 1;
            self.settings_page.opening = false;
            let generation = self.settings_page.transition_generation;
            if cx.reduce_motion() {
                self.settings_page.closing = false;
                cx.notify();
            } else {
                self.settings_page.closing = true;
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(crate::components::motion::FAST)
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.settings_page.closing
                            && this.settings_page.transition_generation == generation
                        {
                            this.settings_page.closing = false;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn align_mcp_preview(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open() && crate::dev::settings_page().as_deref() == Some("mcp") {
            let scroll = self.settings_page.scroll.clone();
            cx.on_next_frame(window, move |_, window, _| {
                scroll.scroll_to_top_of_item(6);
                window.refresh();
            });
        }
    }

    pub(super) fn dev_open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = crate::dev::settings_page();
        if request.is_some() || crate::dev::edit_view().as_deref() == Some("settings") {
            self.open_settings(window, cx);
        }
        if let Some(request) = request.as_deref() {
            self.dev_theme_editor(request, window, cx);
        }
        if request.as_deref() == Some("focus-moon") {
            self.settings_page
                .themes
                .entry("vela".into())
                .or_insert_with(|| ThemeCardFocus::new(cx))
                .dark
                .focus(window, cx);
        }
        let section = match request.as_deref() {
            Some("appearance") => Some(0),
            Some("interface") => Some(1),
            Some("agent") => Some(2),
            Some("editor") | Some("editor-low") => Some(3),
            Some("terminal" | "terminal-menu" | "terminal-config-menu") => Some(4),
            Some("updates") => Some(5),
            Some("mcp") => Some(6),
            _ => None,
        };
        if let Some(section) = section {
            self.settings_page.scroll.scroll_to_top_of_item(section);
        }
        if crate::dev::agent_edit() {
            self.edit_agent_socket(window, cx);
        }
        if crate::dev::editor_menu() || request.as_deref() == Some("editor-low") {
            let low = request.as_deref() == Some("editor-low");
            cx.on_next_frame(window, move |this, window, cx| {
                if low {
                    this.scroll_editor_to_bottom(cx);
                    cx.on_next_frame(window, move |this, window, cx| {
                        this.open_settings_menu(Picker::Editor, window, cx)
                    });
                } else {
                    this.open_settings_menu(Picker::Editor, window, cx);
                }
            });
        }
        let terminal_picker = match request.as_deref() {
            Some("terminal-menu") => Some(Picker::ExternalTerminal),
            Some("terminal-config-menu") => Some(Picker::TerminalConfig),
            _ => None,
        };
        if let Some(picker) = terminal_picker {
            cx.on_next_frame(window, move |this, window, cx| {
                this.open_settings_menu(picker, window, cx);
            });
        }
        if crate::dev::empty_confirm() {
            self.open_dialog(Dialog::EmptyEdits, window, cx);
        }
        if let Some(id) = request
            .as_deref()
            .and_then(|request| request.strip_prefix("delete:"))
            && cx
                .global::<Appearance>()
                .themes
                .iter()
                .any(|theme| theme.id == id && !theme.builtin)
        {
            self.open_dialog(Dialog::DeleteTheme(id.into()), window, cx);
        }
    }

    pub(super) fn read_terminal_config(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.terminal_config_generation += 1;
        let generation = self.terminal_config_generation;
        let source = self.settings.terminal_config;
        let external =
            ExternalTerminals::selected(cx).and_then(|installed| installed.preset.config_source());
        let read = cx
            .background_executor()
            .spawn(async move { imported::load(source, external) });
        cx.spawn_in(window, async move |workspace, cx| {
            let settings = read.await;
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                if workspace.terminal_config_generation != generation {
                    return;
                }
                for connection in workspace.connections.values() {
                    if let super::Connection::Connected { panes, .. } = connection
                        && let Some(view) = &panes.terminal
                    {
                        view.update(cx, |view, cx| view.reload(settings.clone(), window, cx));
                    }
                }
                cx.set_global(settings);
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn choose_editor(&mut self, window: &Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose Application".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.settings_open() {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        match paths.into_iter().next().filter(|path| is_application(path)) {
                            Some(path) => this.save_editor(Some(path), window, cx),
                            None => {
                                this.settings_error = Some("Choose an application (.app).".into())
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => this.settings_error = Some("Couldn't open the application picker.".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    fn save_editor(&mut self, editor: Option<PathBuf>, window: &Window, cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        next.editor = editor;
        self.save_settings(next, window, cx);
    }

    pub(super) fn save_settings(
        &mut self,
        next: Settings,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = self.dir.clone() else { return };
        let external_changed = self.settings.external_terminal != next.external_terminal;
        let config_changed = self.settings.terminal_config != next.terminal_config
            || (external_changed && next.terminal_config == TerminalConfigSource::Auto);
        if external_changed && cx.has_global::<ExternalTerminals>() {
            cx.global_mut::<ExternalTerminals>().preferred = next.external_terminal.clone();
        }
        self.settings = next.clone();
        crate::terminal::copy_on_select::set(next.terminal_copy_on_select, cx);
        if config_changed {
            self.read_terminal_config(window, cx);
        }
        if self.settings_page.busy || self.settings_page.glass_drag.is_some() {
            self.settings_page.pending_settings = Some(next);
            cx.notify();
            return;
        }
        self.settings_page.busy = true;
        self.settings_error = None;
        cx.notify();
        let save = cx.background_executor().spawn(async move {
            next.save(&dir)?;
            Ok::<_, io::Error>(next)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.settings_page.busy = false;
                let glass_changed = cx.global_mut::<Appearance>().glass_preview.take().is_some();
                if glass_changed {
                    cx.refresh_windows();
                }
                match result {
                    Ok(settings) => {
                        if this.settings.appearance == settings.appearance {
                            cx.global_mut::<Appearance>()
                                .set_settings(settings.appearance.clone());
                            Appearance::update_native_appearance(cx);
                        }
                        cx.refresh_windows();
                    }
                    Err(error) => {
                        this.settings_error = Some(format!("Couldn't save settings.toml: {error}"))
                    }
                }
                cx.notify();
                if let Some(pending) = this.settings_page.pending_settings.take() {
                    this.save_settings(pending, window, cx);
                }
            });
        })
        .detach();
    }

    fn set_scheme(&mut self, scheme: Scheme, window: &Window, cx: &mut Context<Self>) {
        if self.settings_page.editor.is_some() {
            return;
        }
        let mut next = self.settings.clone();
        next.appearance.scheme = scheme;
        self.save_settings(next, window, cx);
    }

    fn activate_theme(
        &mut self,
        id: &str,
        variant: Option<ColorVariant>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.settings.clone();
        if variant != Some(ColorVariant::Dark) {
            next.appearance.light_theme = id.into();
        }
        if variant != Some(ColorVariant::Light) {
            next.appearance.dark_theme = id.into();
        }
        self.save_settings(next, window, cx);
    }

    fn change_themes<R: Send + 'static>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        operation: impl FnOnce(&Path) -> io::Result<R> + Send + 'static,
        on_success: impl FnOnce(&mut Self, R, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(dir) = self.dir.clone() else { return };
        if self.settings_page.busy || self.settings_page.glass_drag.is_some() {
            return;
        }
        self.settings_page.busy = true;
        self.settings_page.theme_error = None;
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let result = operation(&dir)?;
            Ok::<_, io::Error>((result, load_user_themes(&dir)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.settings_page.busy = false;
                match result {
                    Ok((result, (users, errors))) => {
                        cx.global_mut::<Appearance>().replace_users(users, errors);
                        on_success(this, result, window, cx);
                        cx.refresh_windows();
                    }
                    Err(error) => this.settings_page.theme_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn duplicate_theme(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        let appearance = cx.global::<Appearance>();
        let Some(mut copy) = appearance
            .themes
            .iter()
            .find(|theme| theme.id == id)
            .cloned()
        else {
            return;
        };
        let base = format!("{} copy", copy.name);
        copy.name = base.clone();
        for suffix in 2.. {
            if !appearance
                .themes
                .iter()
                .any(|theme| names_match(&theme.name, &copy.name))
            {
                break;
            }
            copy.name = format!("{base} {suffix}");
        }
        copy.builtin = false;
        self.change_themes(
            window,
            cx,
            move |dir| {
                copy.id = unused_theme_id(dir, &copy.name)?;
                save_user_theme(dir, &copy)?;
                Ok(copy.id)
            },
            |this, id, window, cx| this.open_theme_editor(Some(&id), window, cx),
        );
    }

    pub(super) fn remove_theme(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let deleted = id.clone();
        self.finish_dialog(window, cx);
        self.change_themes(
            window,
            cx,
            move |dir| delete_user_theme(dir, &deleted),
            move |this, _, window, cx| {
                let mut next = this.settings.clone();
                for selection in [
                    &mut next.appearance.light_theme,
                    &mut next.appearance.dark_theme,
                ] {
                    if *selection == id {
                        *selection = "vela".into();
                    }
                }
                if next.appearance != this.settings.appearance {
                    this.save_settings(next, window, cx);
                }
            },
        );
    }

    fn add_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.settings_page.busy || self.settings_page.editor.is_some() {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add Theme".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = paths.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.settings_open() {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.change_themes(
                                window,
                                cx,
                                move |dir| import_theme(dir, &path).map(|_| ()),
                                |_, _, _, _| {},
                            );
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => {
                        this.settings_page.theme_error =
                            Some("Couldn't open the file picker.".into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_settings(
        &mut self,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        self.settings_page
            .themes
            .retain(|id, _| appearance.themes.iter().any(|theme| &theme.id == id));
        for theme in &appearance.themes {
            self.settings_page
                .themes
                .entry(theme.id.clone())
                .or_insert_with(|| ThemeCardFocus::new(cx));
        }
        let available = window.viewport_size().width.as_f32()
            - if self.sidebar_visible() {
                crate::sidebar::WIDTH
            } else {
                0.
            };
        let columns = if available < 608. { 2 } else { 3 };
        let light = appearance.selected(ColorVariant::Light);
        let dark = appearance.selected(ColorVariant::Dark);
        let colors = appearance
            .preview
            .as_ref()
            .map_or((light.light, dark.dark), |(draft, _)| {
                (draft.light, draft.dark)
            });
        let schemes = [
            ("System", IconName::Monitor, Scheme::System),
            ("Light", IconName::Sun, Scheme::Light),
            ("Dark", IconName::Moon, Scheme::Dark),
        ];
        let appearance_section = section("Appearance")
            .child(subtitle("Color Scheme"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .children(schemes.into_iter().enumerate().map(
                        |(index, (label, icon, scheme))| {
                            self.scheme_card(index, label, icon, scheme, colors, theme, cx)
                        },
                    )),
            )
            .child(
                div()
                    .mt_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(subtitle("Themes"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                icon_text_button(
                                    "create-theme",
                                    IconName::Palette,
                                    "Create Theme",
                                    Variant::Secondary,
                                    None,
                                    theme,
                                    cx.listener(|this, _, window, cx| {
                                        this.open_theme_editor(None, window, cx)
                                    }),
                                )
                                .h(px(26.)),
                            )
                            .child(
                                icon_text_button(
                                    "add-theme",
                                    IconName::Plus,
                                    "Add Theme",
                                    Variant::Secondary,
                                    None,
                                    theme,
                                    cx.listener(|this, _, window, cx| this.add_theme(window, cx)),
                                )
                                .h(px(26.)),
                            ),
                    ),
            )
            .when_some(self.settings_page.theme_error.clone(), |section, error| {
                section.child(
                    div()
                        .text_color(theme.danger)
                        .text_size(px(12.))
                        .child(error),
                )
            })
            .child(
                div().flex().flex_col().gap_3().children(
                    appearance
                        .themes
                        .chunks(columns)
                        .enumerate()
                        .map(|(row, themes)| {
                            div()
                                .flex()
                                .gap_3()
                                .children(themes.iter().enumerate().map(|(col, def)| {
                                    div().flex_1().min_w_0().child(self.theme_card(
                                        row * columns + col,
                                        columns,
                                        def,
                                        (light.id == def.id, dark.id == def.id),
                                        theme,
                                        cx,
                                    ))
                                }))
                                .children((themes.len()..columns).map(|_| div().flex_1().min_w_0()))
                        }),
                ),
            )
            .children(appearance.errors.iter().cloned().map(|error| {
                div()
                    .text_color(theme.secondary_text)
                    .text_size(px(12.))
                    .child(error)
            }));
        div()
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex_col()
            .track_focus(&self.settings_page.focus)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(52.))
                    .flex_none()
                    .px_4()
                    .when(theme.glassy, |header| {
                        header.bg(theme.chrome(theme.background))
                    })
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(!self.sidebar_visible(), |row| {
                                row.child(self.sidebar_button(theme, cx))
                            })
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Settings"),
                            ),
                    )
                    .child(
                        icon_button(
                            "settings-close",
                            "Close Settings",
                            IconName::Close,
                            Variant::Secondary,
                            theme,
                            cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                        )
                        .size(px(28.))
                        .px_0(),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child({
                        let viewport = self.settings_page.settings_viewport.clone();
                        canvas(move |rect, _, _| viewport.set(Some(rect)), |_, _, _, _| {})
                            .absolute()
                            .inset_0()
                    })
                    .child(
                        div()
                            .id("settings-scroll")
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex_col()
                            .items_center()
                            .overflow_y_scroll()
                            .track_scroll(&self.settings_page.scroll)
                            .when(theme.glassy, |scroll| {
                                scroll.bg(theme.chrome(theme.background))
                            })
                            .child(appearance_section)
                            .child(self.render_interface(theme, cx))
                            .child(self.agent_section(theme, cx))
                            .child(self.editor_section(theme, cx))
                            .child(self.terminal_section(theme, window, cx))
                            .child(self.updates_section(theme, cx))
                            .child(self.mcp_section(theme, cx).pb_6()),
                    ),
            )
            .children(self.render_theme_editor(theme, window, cx))
            .children(self.render_picker(theme, cx))
    }

    fn updates_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
        section("Updates").child(settings_group(
            theme,
            vec![
                settings_row(
                    "Current Version",
                    None,
                    div()
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                    theme,
                ),
                settings_row(
                    "Include Prereleases",
                    Some("Get early versions of Vela through the updater.".into()),
                    switch(
                        "include-prereleases",
                        "Include Prereleases",
                        self.settings.include_prereleases,
                        theme,
                        cx.listener(|this, _, window, cx| {
                            let mut next = this.settings.clone();
                            next.include_prereleases = !next.include_prereleases;
                            this.save_settings(next, window, cx);
                            this.recheck_for_updates(window, cx);
                        }),
                    )
                    .debug_selector(|| "include-prereleases".into()),
                    theme,
                ),
            ],
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn scheme_card(
        &self,
        index: usize,
        label: &'static str,
        name: IconName,
        scheme: Scheme,
        colors: (Colors, Colors),
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let focus = &self.settings_page.schemes[index];
        let selected = self.settings.appearance.scheme == scheme;
        let miniature = div()
            .flex()
            .h(px(120.))
            .w_full()
            .overflow_hidden()
            .rounded(px(6.))
            .when(scheme != Scheme::Dark, |row| row.child(miniature(colors.0)))
            .when(scheme != Scheme::Light, |row| {
                row.child(miniature(colors.1))
            });
        card(("scheme", index), focus, theme)
            .flex_1()
            .aria_selected(selected)
            .border_2()
            .when(selected, |card| card.border_color(theme.accent))
            .p_2()
            .gap_2()
            .child(miniature)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_1()
                    .text_size(px(12.))
                    .child(icon(name).size(px(12.)).text_color(theme.secondary_text))
                    .child(label),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.set_scheme(scheme, window, cx)))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if let Some(next) = arrow_target(index, 1, 3, &event.keystroke.key) {
                        this.settings_page.schemes[next].focus(window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
    }

    fn theme_card(
        &self,
        index: usize,
        columns: usize,
        def: &ThemeDef,
        selected: (bool, bool),
        theme: Theme,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let theme_focus = &self.settings_page.themes[&def.id];
        let variant_button = |variant: ColorVariant| {
            let (id, name, mode, focus, selected) = match variant {
                ColorVariant::Light => (
                    "sun",
                    IconName::Sun,
                    "Light",
                    &theme_focus.light,
                    selected.0,
                ),
                ColorVariant::Dark => (
                    "moon",
                    IconName::Moon,
                    "Dark",
                    &theme_focus.dark,
                    selected.1,
                ),
            };
            let ring = selected.then_some(match (self.settings.appearance.scheme, variant) {
                (Scheme::Light, ColorVariant::Dark) | (Scheme::Dark, ColorVariant::Light) => 0.4,
                _ => 1.,
            });
            let label = format!("Use “{}” in {mode} Mode", def.name);
            let accessible_label = if selected {
                format!("{label} (selected)")
            } else {
                label.clone()
            };
            let tooltip = gpui::SharedString::from(label);
            let activate = def.id.clone();
            orb(id, *variant.colors(def), name, ring, theme)
                .role(gpui::Role::Button)
                .aria_label(accessible_label)
                .track_focus(focus)
                .tooltip(move |_, cx| cx.new(|_| Tooltip(tooltip.clone())).into())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if this.settings_page.editor.is_none() {
                        this.activate_theme(&activate, Some(variant), window, cx);
                    }
                }))
        };
        let activate = def.id.clone();
        let duplicate = def.id.clone();
        let delete = def.id.clone();
        let edit = def.id.clone();
        let focus = theme_focus.card.clone();
        let key_focus = focus.clone();
        let bounds = Rc::new(Cell::new(None));
        let layout = bounds.clone();
        card(&focus, &focus, theme)
            .w_full()
            .relative()
            .child(
                canvas(move |rect, _, _| layout.set(Some(rect)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .aria_label(def.name.clone())
            .aria_selected(selected.0 || selected.1)
            .justify_between()
            .h(px(124.))
            .p_4()
            .gap_2()
            .child(
                div()
                    .flex()
                    .justify_center()
                    .items_center()
                    .gap_6()
                    .child(variant_button(ColorVariant::Light))
                    .child(variant_button(ColorVariant::Dark)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(def.name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .flex_none()
                            .when(!def.builtin, |row| {
                                row.child(theme_action(
                                    "edit",
                                    "Edit",
                                    IconName::Pencil,
                                    theme,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_theme_editor(Some(&edit), window, cx);
                                    }),
                                ))
                            })
                            .child(theme_action(
                                "duplicate",
                                "Duplicate",
                                IconName::Copy,
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.duplicate_theme(&duplicate, window, cx);
                                }),
                            ))
                            .when(!def.builtin, |row| {
                                row.child(theme_action(
                                    "delete",
                                    "Delete",
                                    IconName::Trash,
                                    theme,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        if !this.settings_page.busy {
                                            this.open_dialog(
                                                Dialog::DeleteTheme(delete.clone()),
                                                window,
                                                cx,
                                            );
                                        }
                                    }),
                                ))
                            }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.settings_page.editor.is_none() {
                    this.activate_theme(&activate, None, window, cx);
                }
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if !key_focus.is_focused(window) {
                        return;
                    }
                    let themes = &cx.global::<Appearance>().themes;
                    if let Some(next) =
                        arrow_target(index, columns, themes.len(), &event.keystroke.key)
                    {
                        this.settings_page.themes[&themes[next].id]
                            .card
                            .focus(window, cx);
                        if let Some(rect) = bounds.get() {
                            let rows = (next / columns) as f32 - (index / columns) as f32;
                            reveal_theme_card(&this.settings_page.scroll, rect, rows);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }
                }),
            )
    }

    fn agent_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let (word, color) = self.agent_status(theme);
        let editing = self.settings_page.agent_editing;
        let status = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .text_color(theme.secondary_text)
            .child(self.agent_name())
            .when(self.settings_page.agent_probe.is_none(), |row| {
                row.child(crate::components::motion::spin(
                    icon(IconName::Spinner)
                        .size(px(12.))
                        .text_color(theme.secondary_text),
                    "settings-agent-spinner",
                ))
            })
            .when(!self.settings_page.agent_probe.is_none(), |row| {
                row.child(status_dot(color, None))
            })
            .child(div().text_color(color).child(word));

        let controls = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(status)
            .child(small_button(
                "change-agent",
                "Change…",
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| this.edit_agent_socket(window, cx)),
            ))
            .when(self.settings.agent_socket.is_some(), |row| {
                row.child(small_button(
                    "use-bitwarden",
                    "Use Bitwarden Socket",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.use_bitwarden_socket(window, cx)),
                ))
            });

        let socket_control = if editing {
            let input = self.settings_page.agent_input.clone();
            div()
                .id("agent-socket-editor")
                .flex()
                .items_center()
                .gap(px(8.))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => {
                            cx.stop_propagation();
                            this.commit_agent_socket(window, cx);
                        }
                        "escape" => {
                            cx.stop_propagation();
                            this.cancel_agent_socket(cx);
                        }
                        _ => {}
                    }
                }))
                .child(div().w(px(280.)).children(input))
                .child(small_button(
                    "save-agent-socket",
                    "Save",
                    Variant::Primary,
                    theme,
                    cx.listener(|this, _, window, cx| this.commit_agent_socket(window, cx)),
                ))
                .child(small_button(
                    "cancel-agent-socket",
                    "Cancel",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, _, cx| this.cancel_agent_socket(cx)),
                ))
                .into_any_element()
        } else {
            div()
                .font_family(crate::theme::monospace())
                .text_size(px(12.))
                .text_color(theme.secondary_text)
                .child(
                    self.effective_socket()
                        .map_or_else(|| "None".to_owned(), |path| abbreviate_path(path, 52)),
                )
                .into_any_element()
        };

        section("SSH Agent").child(settings_group(
            theme,
            vec![
                settings_row(
                    "Agent",
                    Some("Used by all servers that authenticate with an SSH key.".into()),
                    controls,
                    theme,
                ),
                settings_row("Socket", None, socket_control, theme),
            ],
        ))
    }

    fn apply_picker_value(
        &mut self,
        value: PickerValue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_settings_menu(true, window, cx);
        let mut next = self.settings.clone();
        match value {
            PickerValue::Editor(editor) => next.editor = editor,
            PickerValue::External(id) => next.external_terminal = id.map(str::to_owned),
            PickerValue::Config(source) => next.terminal_config = source,
            PickerValue::OtherEditor => {
                #[cfg(target_os = "macos")]
                self.choose_editor(window, cx);
                return;
            }
        }
        if next != self.settings {
            self.save_settings(next, window, cx);
        }
    }

    fn picker_entries(&self, picker: Picker, cx: &gpui::App) -> Vec<(&'static str, PickerValue)> {
        match picker {
            Picker::Editor => {
                let mut entries = vec![("System Default", PickerValue::Editor(None))];
                entries.extend(
                    installed_editors(&crate::dev::editor_roots())
                        .into_iter()
                        .map(|(name, path)| (name, PickerValue::Editor(Some(path)))),
                );
                if cfg!(target_os = "macos") {
                    entries.push(("Other Application…", PickerValue::OtherEditor));
                }
                entries
            }
            Picker::ExternalTerminal => {
                let mut entries = vec![("Automatic", PickerValue::External(None))];
                if let Some(terminals) = cx.try_global::<ExternalTerminals>() {
                    entries.extend(terminals.installed.iter().map(|terminal| {
                        (
                            terminal.preset.name,
                            PickerValue::External(Some(terminal.preset.id)),
                        )
                    }));
                }
                entries
            }
            Picker::TerminalConfig => imported::SOURCES
                .iter()
                .map(|&(source, label)| (label, PickerValue::Config(source)))
                .collect(),
        }
    }

    fn picker_selection(&self, picker: Picker, cx: &gpui::App) -> Option<PickerValue> {
        match picker {
            Picker::Editor => Some(PickerValue::Editor(self.settings.editor.clone())),
            Picker::ExternalTerminal => {
                let preferred = self.settings.external_terminal.as_deref();
                let id = ExternalTerminals::selected(cx).map(|terminal| terminal.preset.id);
                Some(PickerValue::External(
                    id.filter(|id| Some(*id) == preferred),
                ))
            }
            Picker::TerminalConfig => Some(PickerValue::Config(self.settings.terminal_config)),
        }
    }

    fn scroll_editor_to_bottom(&mut self, cx: &mut Context<Self>) {
        let shift = match (
            self.settings_page.settings_viewport.get(),
            Picker::Editor.bounds(&self.settings_page).get(),
        ) {
            (Some(viewport), Some(trigger)) => (viewport.bottom() - px(8.)) - trigger.bottom(),
            (Some(viewport), None) => viewport.size.height - px(88.),
            _ => return,
        };
        let offset = self.settings_page.scroll.offset();
        self.settings_page
            .scroll
            .set_offset(point(offset.x, (offset.y + shift).min(px(0.))));
        cx.notify();
    }

    fn open_settings_menu(&mut self, picker: Picker, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .settings_page
            .settings_menu
            .as_ref()
            .is_some_and(|menu| menu.picker == picker && !menu.closing)
        {
            self.close_settings_menu(true, window, cx);
            return;
        }
        let entries = self.picker_entries(picker, cx);
        let selected = self.picker_selection(picker, cx);
        let desired_height = picker_menu_height(&entries);
        let viewport = self.settings_page.settings_viewport.get();
        let trigger = picker.bounds(&self.settings_page).get();
        if let (Some(trigger), Some(viewport)) = (trigger, viewport)
            && (trigger.top() < viewport.top() || trigger.bottom() > viewport.bottom())
        {
            let shift = if trigger.top() < viewport.top() {
                viewport.top() - trigger.top()
            } else {
                viewport.bottom() - trigger.bottom() - desired_height - px(dropdown::GAP)
            };
            let offset = self.settings_page.scroll.offset();
            self.settings_page
                .scroll
                .set_offset(point(offset.x, (offset.y + shift).min(px(0.))));
        }
        let (placement, height) = trigger
            .zip(viewport)
            .and_then(|(trigger, viewport)| picker_placement(trigger, viewport, desired_height))
            .unwrap_or((Placement::Below, desired_height));
        let items = (0..entries.len())
            .map(|_| cx.focus_handle().tab_stop(true))
            .collect();
        let focus = entries
            .iter()
            .position(|(_, value)| Some(value) == selected.as_ref())
            .unwrap_or(0);
        let generation = self.settings_page.menu_generation;
        self.settings_page.menu_generation += 1;
        let menu = SettingsMenu {
            picker,
            placement,
            height,
            items,
            entries,
            scroll: ScrollHandle::new(),
            closing: false,
            restore_focus: true,
            generation,
        };
        menu.focus(focus, window, cx);
        self.settings_page.settings_menu = Some(menu);
        cx.notify();
    }

    fn update_settings_menu_placement(&mut self, picker: Picker, cx: &mut Context<Self>) {
        let trigger = picker.bounds(&self.settings_page).get();
        let viewport = self.settings_page.settings_viewport.get();
        let Some(menu) = self
            .settings_page
            .settings_menu
            .as_mut()
            .filter(|menu| menu.picker == picker && !menu.closing)
        else {
            return;
        };
        match trigger.zip(viewport).and_then(|(trigger, viewport)| {
            picker_placement(trigger, viewport, picker_menu_height(&menu.entries))
        }) {
            Some((placement, height)) if (menu.placement, menu.height) != (placement, height) => {
                menu.placement = placement;
                menu.height = height;
                cx.notify();
            }
            Some(_) => {}
            None => self.begin_settings_menu_close(false, None, cx),
        }
    }

    fn close_settings_menu(
        &mut self,
        restore_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_settings_menu_close(restore_focus, Some(window), cx);
    }

    fn begin_settings_menu_close(
        &mut self,
        restore_focus: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(menu) = self.settings_page.settings_menu.as_mut() else {
            return;
        };
        if menu.closing {
            return;
        }
        menu.closing = true;
        menu.restore_focus = restore_focus;
        let generation = menu.generation;
        let picker = menu.picker;
        if cx.reduce_motion() {
            self.settings_page.settings_menu = None;
            if restore_focus && let Some(window) = window {
                picker.trigger(&self.settings_page).focus(window, cx);
            }
            cx.notify();
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::components::motion::FAST)
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(menu) = this
                    .settings_page
                    .settings_menu
                    .as_ref()
                    .filter(|menu| menu.closing && menu.generation == generation)
                else {
                    return;
                };
                if menu.restore_focus {
                    picker.trigger(&this.settings_page).focus(window, cx);
                }
                this.settings_page.settings_menu = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn render_settings_picker(
        &self,
        picker: Picker,
        theme: Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        if picker == Picker::Editor && !crate::dev::editor_picker() {
            return div()
                .text_size(px(13.))
                .text_color(theme.secondary_text)
                .child("Default application (xdg-open)")
                .into_any_element();
        }
        let checked = self.picker_selection(picker, cx);
        let label = match checked.as_ref() {
            Some(PickerValue::Editor(path)) => {
                path.as_deref().and_then(Path::file_stem).map_or_else(
                    || "System Default".into(),
                    |name| name.to_string_lossy().into_owned(),
                )
            }
            Some(PickerValue::External(Some(_))) => ExternalTerminals::selected(cx).map_or_else(
                || "Automatic".into(),
                |terminal| terminal.preset.name.into(),
            ),
            Some(PickerValue::Config(source)) => imported::SOURCES
                .iter()
                .find(|(id, _)| id == source)
                .map_or("Automatic", |(_, label)| *label)
                .into(),
            _ => "Automatic".into(),
        };
        let bounds = picker.bounds(&self.settings_page).clone();
        let weak = cx.weak_entity();
        let trigger = div()
            .id(picker.id())
            .aria_label(picker.label())
            .role(Role::Button)
            .tab_index(0)
            .track_focus(picker.trigger(&self.settings_page))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .h(px(28.))
            .min_w(px(190.))
            .px(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme.control_border)
            .bg(theme.panel)
            .text_size(px(13.))
            .cursor_pointer()
            .hover(move |style| style.bg(theme.hover))
            .focus(move |style| style.border_color(theme.accent))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_settings_menu(picker, window, cx)
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
                    cx.stop_propagation();
                    this.open_settings_menu(picker, window, cx);
                }
            }))
            .child(label)
            .child(
                icon(IconName::ChevronsUpDown)
                    .size(px(14.))
                    .text_color(theme.secondary_text),
            );
        let wrapper = div()
            .relative()
            .flex_none()
            .child(
                canvas(
                    move |rect, _, cx| {
                        bounds.set(Some(rect));
                        let weak = weak.clone();
                        cx.defer(move |cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.update_settings_menu_placement(picker, cx);
                            });
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(trigger);
        wrapper
            .when_some(
                self.settings_page
                    .settings_menu
                    .as_ref()
                    .filter(|menu| menu.picker == picker),
                |wrapper, menu| {
                    let placement = menu.placement;
                    let closing = menu.closing;
                    let generation = menu.generation;
                    let rise = if placement == Placement::Below {
                        -4.
                    } else {
                        4.
                    };
                    let mut rows: Vec<AnyElement> = Vec::new();
                    for (index, (label, value)) in menu.entries.iter().enumerate() {
                        if matches!(value, PickerValue::OtherEditor) {
                            rows.push(
                                div()
                                    .id("editor-menu-separator")
                                    .h(px(1.))
                                    .my(px(4.))
                                    .bg(theme.border)
                                    .into_any_element(),
                            );
                        }
                        let focus = menu.items[index].clone();
                        let value = value.clone();
                        rows.push(
                            dropdown_item(
                                ("settings-menu-item", index),
                                *label,
                                checked.as_ref() == Some(&value),
                                &focus,
                                theme,
                            )
                            .flex_none()
                            .when(!closing, |row| {
                                row.on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.apply_picker_value(value.clone(), window, cx);
                                    },
                                ))
                            })
                            .into_any_element(),
                        );
                    }
                    let panel = dropdown_panel("settings-menu", theme)
                        .role(Role::Menu)
                        .key_context("SettingsMenu")
                        .max_h(menu.height)
                        .track_scroll(&menu.scroll)
                        .when(!closing, |panel| {
                            panel
                                .on_action(cx.listener(
                                    move |this, _: &dropdown::Dismiss, window, cx| {
                                        cx.stop_propagation();
                                        this.close_settings_menu(true, window, cx);
                                    },
                                ))
                                .on_key_down(cx.listener(
                                    move |this, event: &KeyDownEvent, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        match key {
                                            "tab" => this.close_settings_menu(true, window, cx),
                                            "up" | "down" | "home" | "end" | "enter" | "space" => {
                                                cx.stop_propagation();
                                                let Some(menu) =
                                                    this.settings_page.settings_menu.as_ref()
                                                else {
                                                    return;
                                                };
                                                let current = menu
                                                    .items
                                                    .iter()
                                                    .position(|handle| handle.is_focused(window))
                                                    .unwrap_or(0);
                                                let len = menu.items.len();
                                                let next = match key {
                                                    "down" => (current + 1) % len,
                                                    "up" => (current + len - 1) % len,
                                                    "home" => 0,
                                                    "end" => len - 1,
                                                    _ => {
                                                        let value = menu.entries[current].1.clone();
                                                        this.apply_picker_value(value, window, cx);
                                                        return;
                                                    }
                                                };
                                                menu.focus(next, window, cx);
                                            }
                                            _ => {}
                                        }
                                    },
                                ))
                                .on_mouse_down_out(cx.listener(
                                    move |this, event: &MouseDownEvent, window, cx| {
                                        if picker
                                            .bounds(&this.settings_page)
                                            .get()
                                            .is_some_and(|bounds| bounds.contains(&event.position))
                                        {
                                            return;
                                        }
                                        this.close_settings_menu(false, window, cx);
                                    },
                                ))
                        })
                        .children(rows);
                    let panel = if closing {
                        panel
                            .with_animation(
                                ("settings-menu-close", generation),
                                Animation::new(crate::components::motion::FAST)
                                    .with_easing(ease_out_quint()),
                                move |panel, t| panel.opacity(1. - t).relative().top(px(rise * t)),
                            )
                            .into_any_element()
                    } else if cx.reduce_motion() {
                        panel.into_any_element()
                    } else {
                        let side: usize = match placement {
                            Placement::Below => 0,
                            Placement::Above => 1,
                        };
                        crate::components::motion::appear(
                            panel,
                            ("settings-menu-open", side),
                            rise,
                            crate::components::motion::FAST,
                        )
                        .into_any_element()
                    };
                    wrapper.child(
                        deferred(
                            div()
                                .absolute()
                                .left_0()
                                .min_w_full()
                                .when(placement == Placement::Below, |menu| {
                                    menu.top(px(28. + dropdown::GAP))
                                })
                                .when(placement == Placement::Above, |menu| {
                                    menu.bottom(px(28. + dropdown::GAP))
                                })
                                .child(panel),
                        )
                        .with_priority(1),
                    )
                },
            )
            .into_any_element()
    }

    fn editor_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let editor_control = self.render_settings_picker(Picker::Editor, theme, cx);

        let copies = self.settings_page.edit_copies;
        let caption = match copies {
            None => "Calculating…".to_owned(),
            Some((0, _)) => "No copies.".to_owned(),
            Some((count, bytes)) => format!(
                "{} {} · {}. Copies with unuploaded changes are kept.",
                count,
                if count == 1 { "file" } else { "files" },
                format_copies_size(bytes)
            ),
        };
        let folder = crate::dev::edit_root().is_ok_and(|path| path.is_dir());
        let copies_control = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .children(folder.then(|| {
                small_button(
                    "show-copies",
                    "Show",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.reveal_edit_copies(window, cx)),
                )
            }))
            .child(small_button(
                "empty-copies",
                "Clear…",
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| {
                    if !this.settings_page.busy {
                        this.open_dialog(Dialog::EmptyEdits, window, cx);
                    }
                }),
            ));

        section("Editor")
            .child(settings_group(
                theme,
                vec![
                    settings_row(
                        "Open remote files with",
                        Some("Vela uploads your changes when you save in the editor.".into()),
                        editor_control,
                        theme,
                    ),
                    settings_row("Edit Copies", Some(caption.into()), copies_control, theme),
                ],
            ))
            .when_some(self.settings_error.clone(), |section, error| {
                section.child(div().text_color(theme.danger).child(error))
            })
    }

    fn terminal_section(&self, theme: Theme, window: &Window, cx: &Context<Self>) -> Div {
        let imported = cx
            .try_global::<ImportedConfig>()
            .cloned()
            .unwrap_or_default();
        let has_palette = imported.has_palette();
        let source = self.settings.terminal_colors;
        let palette = terminal_palette(source, has_palette, imported.palette, theme.terminal);
        let base_size = imported.size;
        let size = font_size::effective(base_size, cx);
        let green = gpui::rgb(palette.ansi[2]);
        let blue = gpui::rgb(palette.ansi[4]);
        let cursor = palette
            .cursor
            .map(gpui::rgb)
            .unwrap_or(gpui::rgb(palette.foreground));
        let prompt = move || {
            div()
                .flex()
                .flex_none()
                .child(div().text_color(green).child("root@ubuntu"))
                .child(":")
                .child(div().text_color(blue).child("~"))
                .child("$")
        };
        let preview = div()
            .flex()
            .flex_col()
            .mx(px(14.))
            .mt(px(12.))
            .mb(px(4.))
            .p(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(gpui::rgb(palette.background))
            .text_color(gpui::rgb(palette.foreground))
            .font_family(crate::theme::monospace())
            .text_size(px(size))
            .line_height(px(size * 1.5))
            .child(
                div()
                    .flex()
                    .child(prompt())
                    .child(div().pl(px(8.)).child("ls")),
            )
            .child(
                div()
                    .flex()
                    .child(div().text_color(blue).child("docs"))
                    .child("  ")
                    .child(div().text_color(blue).child("scripts"))
                    .child("  ")
                    .child("notes.txt"),
            )
            .child(
                div().flex().items_center().child(prompt()).child(
                    div()
                        .ml(px(8.))
                        .w(px((size * 0.6).max(6.)))
                        .h(px(size))
                        .bg(cursor),
                ),
            );

        let current = size.round() as i32;
        let step = |delta: f32| {
            move |_: &ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                if delta > 0. {
                    font_size::increase(cx, base_size);
                } else {
                    font_size::decrease(cx, base_size);
                }
            }
        };
        let cell = |id: &'static str,
                    label: &'static str,
                    aria: &'static str,
                    enabled: bool,
                    on_click: Option<ClickHandler>| {
            let mut cell = div()
                .id(id)
                .role(Role::Button)
                .aria_label(aria)
                .flex()
                .items_center()
                .justify_center()
                .h(px(24.))
                .w(px(28.))
                .text_size(px(13.))
                .line_height(px(24.));
            if enabled {
                cell = cell
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.hover))
                    .focus(move |style| style.bg(theme.selection));
            } else {
                cell = cell.text_color(theme.control_border);
            }
            if let Some(on_click) = on_click {
                cell = cell.on_click(on_click);
            }
            cell.child(label)
        };
        let stepper = div()
            .flex()
            .items_center()
            .rounded(px(6.))
            .border_1()
            .border_color(theme.control_border)
            .overflow_hidden()
            .child(cell(
                "font-smaller",
                "−",
                "Decrease Font Size",
                current > 6,
                (current > 6).then(|| {
                    Box::new(step(-1.))
                        as Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>
                }),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(24.))
                    .min_w(px(52.))
                    .px(px(8.))
                    .border_l_1()
                    .border_r_1()
                    .border_color(theme.control_border)
                    .text_size(px(13.))
                    .line_height(px(24.))
                    .child(format!("{current} pt")),
            )
            .child(cell(
                "font-larger",
                "+",
                "Increase Font Size",
                current < 48,
                (current < 48).then(|| {
                    Box::new(step(1.))
                        as Box<dyn Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static>
                }),
            ));
        let reset = crate::components::button::icon_button(
            "reset-font-size",
            "Reset Font Size to Configuration Default",
            IconName::RotateCcw,
            Variant::Secondary,
            theme,
            cx.listener(move |_, _, _, cx| font_size::reset(cx, base_size)),
        )
        .size(px(24.));
        let size_control = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(stepper)
            .when(self.settings.terminal_font_size.is_some(), |row| {
                row.child(reset)
            });
        let imported_default = imported.copy_on_select;
        let copy_value = self
            .settings
            .terminal_copy_on_select
            .unwrap_or(imported_default);
        let font_description = if cfg!(target_os = "macos") {
            "Use ⌘+, ⌘−, or ⌘0. The size is saved across launches."
        } else {
            "Use Ctrl+Shift++, Ctrl+Shift+−, or Ctrl+Shift+0. The size is saved across launches."
        };

        let appearance = cx.global::<Appearance>();
        let variant = appearance.variant_for(window);
        let effective = effective_source(source, has_palette);
        let file_name = imported
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| "configuration".into());
        let colors_description: SharedString = match effective {
            TerminalColorSource::Theme => format!(
                "Uses the terminal colors of the “{}” theme.",
                appearance.selected(variant).name,
            )
            .into(),
            TerminalColorSource::Imported => format!("Uses the colors from {file_name}.").into(),
        };
        let unavailable = (!has_palette).then_some("Imported colors unavailable");
        let sources = [TerminalColorSource::Theme, TerminalColorSource::Imported];
        let choose_source = cx.listener(move |this, index: &usize, window, cx| {
            let source = sources[*index];
            if source == this.settings.terminal_colors {
                return;
            }
            let mut next = this.settings.clone();
            next.terminal_colors = source;
            this.save_settings(next, window, cx);
        });
        let colors_control = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(4.))
            .child(segmented(
                "terminal-colors",
                "Colors",
                &[("Vela theme", None), ("Imported", unavailable)],
                usize::from(effective == TerminalColorSource::Imported),
                theme,
                move |index, window, cx| choose_source(&index, window, cx),
            ))
            .children(unavailable.map(|reason| {
                div()
                    .text_size(px(12.))
                    .text_color(theme.secondary_text)
                    .child(reason)
            }));

        let show = imported.path.clone();
        let config_control = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .children(show.map(|path| {
                small_button(
                    "show-terminal-config",
                    "Show",
                    Variant::Secondary,
                    theme,
                    cx.listener(move |_, _, window, cx| {
                        let path = path.clone();
                        let reveal = cx
                            .background_executor()
                            .spawn(async move { super::edits_view::reveal(&path) });
                        cx.spawn_in(window, async move |this, cx| {
                            if let Err(error) = reveal.await {
                                let _ = this.update_in(cx, |this, _, cx| {
                                    this.settings_error =
                                        Some(format!("Couldn't show the file: {error}"));
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                    }),
                )
            }))
            .child(small_icon_text_button(
                "reload-terminal-config",
                IconName::Refresh,
                "Reload",
                Variant::Secondary,
                theme,
                cx.listener(|this, _, window, cx| this.read_terminal_config(window, cx)),
            ));

        let external = ExternalTerminals::selected(cx).map_or_else(
            || "No supported external terminal found.".into(),
            |terminal| format!("Opens new SSH sessions in {}.", terminal.preset.name),
        );
        let config_description = imported
            .path
            .as_deref()
            .map(|path| abbreviate_path(path, 52))
            .unwrap_or_else(|| {
                if self.settings.terminal_config == TerminalConfigSource::None {
                    "Uses Vela defaults.".into()
                } else {
                    "No supported configuration file found.".into()
                }
            });
        let family = imported.resolved_family(crate::terminal::view::cached_font_names(cx));

        section("Terminal")
            .child(preview)
            .child(settings_group(
                theme,
                vec![
                    settings_row(
                        "External Terminal",
                        Some(external.into()),
                        self.render_settings_picker(Picker::ExternalTerminal, theme, cx),
                        theme,
                    ),
                    settings_row(
                        "Configuration Source",
                        None,
                        self.render_settings_picker(Picker::TerminalConfig, theme, cx),
                        theme,
                    ),
                    settings_row(
                        "Configuration File",
                        Some(config_description.into()),
                        config_control,
                        theme,
                    ),
                    settings_row(
                        "Font Size",
                        Some(font_description.into()),
                        size_control,
                        theme,
                    ),
                    settings_row(
                        "Copy on Select",
                        Some("Copies selected terminal text to the clipboard.".into()),
                        switch(
                            "terminal-copy-on-select",
                            "Copy on Select",
                            copy_value,
                            theme,
                            cx.listener(move |this, _, window, cx| {
                                let current = this
                                    .settings
                                    .terminal_copy_on_select
                                    .unwrap_or(imported_default);
                                let mut next = this.settings.clone();
                                next.terminal_copy_on_select = Some(!current);
                                this.save_settings(next, window, cx);
                            }),
                        ),
                        theme,
                    ),
                    settings_row(
                        "Font",
                        None,
                        div()
                            .text_size(px(12.))
                            .text_color(theme.secondary_text)
                            .child(family.to_owned()),
                        theme,
                    ),
                    settings_row("Colors", Some(colors_description), colors_control, theme),
                ],
            ))
            .when_some(self.settings_error.clone(), |section, error| {
                section.child(div().text_color(theme.danger).child(error))
            })
    }
}

fn picker_menu_height(entries: &[(&str, PickerValue)]) -> Pixels {
    let separator = usize::from(
        entries
            .last()
            .is_some_and(|(_, value)| *value == PickerValue::OtherEditor),
    );
    menu_height(entries.len(), separator)
}

fn picker_placement(
    trigger: Bounds<Pixels>,
    viewport: Bounds<Pixels>,
    desired: Pixels,
) -> Option<(Placement, Pixels)> {
    if let Some(placement) = place(trigger, viewport, desired) {
        return Some((placement, desired));
    }
    let above = trigger.top() - viewport.top();
    let below = viewport.bottom() - trigger.bottom();
    if above < px(0.) || below < px(0.) {
        return None;
    }
    let (placement, room) = if below >= above {
        (Placement::Below, below)
    } else {
        (Placement::Above, above)
    };
    let height = room - px(dropdown::GAP);
    (height >= menu_height(1, 0)).then_some((placement, height))
}

fn reveal_theme_card(scroll: &ScrollHandle, rect: Bounds<Pixels>, rows: f32) {
    let viewport = scroll.bounds();
    let distance = rows * (rect.size.height + px(12.));
    let top = rect.top() + distance;
    let bottom = rect.bottom() + distance;
    let shift = if top < viewport.top() {
        viewport.top() - top
    } else if bottom > viewport.bottom() {
        viewport.bottom() - bottom
    } else {
        return;
    };
    let offset = scroll.offset();
    scroll.set_offset(point(offset.x, (offset.y + shift).min(px(0.))));
}

fn names_match(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn arrow_target(index: usize, columns: usize, len: usize, key: &str) -> Option<usize> {
    let next = match key {
        "left" if !index.is_multiple_of(columns) || columns == 1 => index.checked_sub(1)?,
        "right" if !(index + 1).is_multiple_of(columns) || columns == 1 => index + 1,
        "up" => index.checked_sub(columns)?,
        "down" => index + columns,
        _ => return None,
    };
    (next < len).then_some(next)
}

pub(super) fn section(label: &'static str) -> Div {
    div()
        .flex_none()
        .w_full()
        .max_w(px(760.))
        .px_6()
        .pt_6()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .child(label),
        )
}

pub(super) fn settings_group(theme: Theme, rows: Vec<Div>) -> Div {
    let mut group = div()
        .flex()
        .flex_col()
        .w_full()
        .overflow_hidden()
        .rounded(px(10.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.content(theme.sidebar));
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            group = group.child(div().h(px(1.)).bg(theme.border));
        }
        group = group.child(row);
    }
    group
}

pub(super) fn settings_row(
    label: &'static str,
    description: Option<SharedString>,
    control: impl gpui::IntoElement,
    theme: Theme,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .px(px(14.))
        .py(px(11.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .min_w_0()
                .child(div().text_size(px(13.)).child(label))
                .children(description.map(|description| {
                    div()
                        .text_size(px(12.))
                        .text_color(theme.secondary_text)
                        .child(description)
                })),
        )
        .child(div().flex_none().child(control.into_any_element()))
}

pub(super) fn subtitle(label: &'static str) -> Div {
    div()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .child(label)
}

fn card(id: impl Into<ElementId>, focus: &FocusHandle, theme: Theme) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .tab_index(0)
        .track_focus(focus)
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(12.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.content(theme.panel))
        .focus(move |style| style.border_color(theme.accent).bg(theme.hover))
        .hover(move |style| style.bg(theme.hover))
        .cursor_pointer()
}

fn theme_action(
    id: &'static str,
    label: &'static str,
    name: IconName,
    theme: Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    button(id, "", Variant::Secondary, theme, on_click)
        .aria_label(label)
        .size(px(24.))
        .px_0()
        .child(icon(name).size(px(14.)).text_color(theme.text))
}

fn orb(
    id: &'static str,
    colors: Colors,
    name: IconName,
    ring: Option<f32>,
    theme: Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .size(px(58.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(gpui::transparent_black())
        .hover(move |style| style.bg(gpui::Hsla::from(theme.accent).opacity(0.10)))
        .focus(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .when_some(ring, |orb, alpha| {
            orb.child(
                div()
                    .absolute()
                    .inset(px(1.))
                    .rounded_full()
                    .border_2()
                    .border_color(gpui::Hsla::from(theme.accent).opacity(alpha)),
            )
        })
        .child(
            div()
                .size(px(48.))
                .rounded_full()
                .border_1()
                .border_color(rgba(colors.border))
                .bg(linear_gradient(
                    135.,
                    linear_color_stop(rgba(colors.accent), 0.),
                    linear_color_stop(rgba(colors.background), 1.),
                )),
        )
        .when(ring.is_some(), |orb| {
            orb.child(
                div()
                    .absolute()
                    .bottom(px(-2.))
                    .right(px(-2.))
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgba(colors.raised))
                    .border_1()
                    .border_color(rgba(colors.border))
                    .child(icon(name).size(px(10.)).text_color(rgba(colors.text))),
            )
        })
}

fn miniature(colors: Colors) -> Div {
    let line = |width, color| {
        div()
            .w(px(width))
            .max_w_full()
            .h(px(4.))
            .rounded_full()
            .bg(rgba(color))
    };
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .bg(rgba(colors.background))
        .border_1()
        .border_color(rgba(colors.border))
        .child(
            div()
                .w(gpui::relative(0.24))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .bg(rgba(colors.sidebar))
                .child(line(32., colors.secondary_text))
                .child(line(24., colors.control_border)),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(20.))
                        .bg(rgba(colors.raised))
                        .border_b_1()
                        .border_color(rgba(colors.border))
                        .px_2()
                        .flex()
                        .items_center()
                        .child(line(28., colors.text)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .p_2()
                        .child(line(56., colors.text))
                        .child(line(38., colors.secondary_text))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .p_1()
                                .w(px(64.))
                                .max_w_full()
                                .rounded(px(6.))
                                .bg(rgba(colors.raised))
                                .border_1()
                                .border_color(rgba(colors.border))
                                .children([colors.success, colors.warning, colors.danger].map(
                                    |color| div().size(px(6.)).rounded_full().bg(rgba(color)),
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .px_1()
                                .h(px(14.))
                                .w(px(80.))
                                .max_w_full()
                                .rounded(px(6.))
                                .bg(rgba(colors.panel))
                                .border_1()
                                .border_color(rgba(colors.control_border))
                                .child(div().size(px(5.)).rounded_full().bg(rgba(colors.accent))),
                        ),
                ),
        )
}

#[cfg(target_os = "macos")]
fn is_application(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        && path.join("Contents/Info.plist").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(cx: &mut gpui::VisualTestContext, key: &'static str) {
        cx.simulate_keystrokes(key);
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
        cx.run_until_parked();
    }

    #[test]
    fn detects_only_installed_editor_bundles_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for bundle in ["Zed.app", "Visual Studio Code.app", "Otro.app"] {
            std::fs::create_dir(root.join(bundle)).unwrap();
        }
        let found = installed_editors(&[root.to_path_buf()]);
        assert_eq!(
            found.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["Visual Studio Code", "Zed"]
        );
        assert_eq!(found[0].1, root.join("Visual Studio Code.app"));
        assert!(installed_editors(&[]).is_empty());
    }

    #[test]
    fn copy_sizes_use_a_decimal_point() {
        assert_eq!(format_copies_size(1_200_000), "1.2 MB");
        assert_eq!(format_copies_size(999), "1.0 KB");
    }

    fn serve_agent(
        socket: &Path,
    ) -> (
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let server = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 5];
                        stream.read_exact(&mut request).unwrap();
                        let answer = [12u8, 0, 0, 0, 0];
                        stream
                            .write_all(&(answer.len() as u32).to_be_bytes())
                            .unwrap();
                        stream.write_all(&answer).unwrap();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (stop, server)
    }

    #[gpui::test]
    fn committing_a_missing_socket_probes_the_new_socket(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        cx.executor().allow_parking();
        let live = dir.path().join("live.sock");
        let (stop, server) = serve_agent(&live);
        let client = vela_core::Client::new(std::env::current_exe().unwrap()).unwrap();
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(Some(client), None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            let mut next = workspace.settings.clone();
            next.agent_socket = Some(live.clone());
            workspace.save_settings(next, window, cx);
            workspace.probe_agent(window, cx);
        });
        for _ in 0..50 {
            cx.run_until_parked();
            if workspace.update(cx, |workspace, _| {
                workspace.settings_page.agent_probe.is_some()
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        workspace.update(cx, |workspace, _| {
            assert_eq!(workspace.settings_page.agent_probe, Some(Ok(0)));
        });

        let missing = dir.path().join("missing.sock");
        workspace.update_in(cx, |workspace, window, cx| {
            let input = cx.new(|cx| {
                TextInput::new(
                    &missing.to_string_lossy(),
                    "Agent Socket",
                    "/path/to/agent.sock",
                    false,
                    cx,
                )
            });
            workspace.settings_page.agent_input = Some(input);
            workspace.commit_agent_socket(window, cx);
        });
        for _ in 0..50 {
            cx.run_until_parked();
            if workspace.update(cx, |workspace, _| {
                workspace.settings_page.agent_probe.is_some()
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        workspace.update(cx, |workspace, _| {
            assert_eq!(
                workspace.settings_page.agent_probe,
                Some(Err(AgentError::Unavailable))
            );
        });
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        server.join().unwrap();
    }

    #[gpui::test]
    fn keyboard_selects_appearance_cards_and_restores_focus(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(workspace.settings_open());
            workspace.settings_page.schemes[0].focus(window, cx);
        });
        cx.simulate_keystrokes("right");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(workspace.settings_page.schemes[1].is_focused(window));
        });
        press(cx, "enter");
        assert_eq!(
            Settings::load(dir.path()).unwrap().appearance.scheme,
            Scheme::Light
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.settings_page.themes["vela"]
                .card
                .focus(window, cx);
        });
        cx.simulate_keystrokes("right");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["frambuesa"]
                    .card
                    .is_focused(window)
            );
        });
        press(cx, "space");
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "frambuesa");
        assert_eq!(settings.dark_theme, "frambuesa");
        cx.simulate_keystrokes("escape");
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(!workspace.settings_open());
            assert_eq!(window.focused(cx), Some(workspace.focus_handle()));
        });
    }

    #[gpui::test]
    fn keyboard_selects_light_and_dark_orbs_independently(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.appearance.scheme = Scheme::Dark;
        settings.save(dir.path()).unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.settings_page.themes["oceano"]
                .card
                .focus(window, cx);
        });
        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["oceano"]
                    .light
                    .is_focused(window)
            );
        });
        press(cx, "enter");
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "oceano");
        assert_eq!(settings.dark_theme, "vela");
        assert_eq!(settings.scheme, Scheme::Dark);

        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["oceano"]
                    .dark
                    .is_focused(window)
            );
        });
        press(cx, "tab");
        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["ambar"]
                    .card
                    .is_focused(window)
            );
        });
        press(cx, "tab");
        press(cx, "tab");
        workspace.update_in(cx, |workspace, window, _| {
            assert!(
                workspace.settings_page.themes["ambar"]
                    .dark
                    .is_focused(window)
            );
        });
        press(cx, "space");
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "oceano");
        assert_eq!(settings.dark_theme, "ambar");
        assert_eq!(settings.scheme, Scheme::Dark);
    }

    #[gpui::test]
    fn mcp_settings_disclosures_toggle_independently(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(|cx| cx.set_reduce_motion(true));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, cx| this.open_settings(window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, _| {
            this.settings_page.scroll.scroll_to_top_of_item(6);
            window.refresh();
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("mcp-sim-input").is_none());
        assert!(cx.debug_bounds("mcp-socket").is_none());
        let test_rule = cx.debug_bounds("mcp-test-rule").unwrap();
        cx.simulate_click(test_rule.center(), gpui::Modifiers::none());
        assert!(cx.debug_bounds("mcp-sim-input").is_some());
        assert!(cx.debug_bounds("mcp-socket").is_none());
        press(cx, "space");
        assert!(cx.debug_bounds("mcp-sim-input").is_none());

        let advanced = cx.debug_bounds("mcp-advanced-storage").unwrap();
        cx.simulate_click(advanced.center(), gpui::Modifiers::none());
        assert!(cx.debug_bounds("mcp-socket").is_some());
        assert!(cx.debug_bounds("mcp-sim-input").is_none());
        press(cx, "enter");
        assert!(cx.debug_bounds("mcp-socket").is_none());
    }

    #[gpui::test]
    fn toggling_prereleases_saves_the_setting_and_starts_a_check(cx: &mut gpui::TestAppContext) {
        use super::super::UpdateState;

        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        crate::dev::TEST_UPDATE_BASE_URL.with(|url| {
            *url.borrow_mut() = Some(format!("file://{}", dir.path().display()));
        });
        cx.update(|cx| cx.set_reduce_motion(true));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, cx| this.open_settings(window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, _| {
            this.settings_page.scroll.scroll_to_top_of_item(5);
            window.refresh();
        });
        cx.run_until_parked();

        for enabled in [true, false] {
            workspace.update(cx, |this, _| {
                this.update_check = None;
                this.set_update_state(UpdateState::Available {
                    release: super::super::tests::test_release(),
                    error: None,
                });
            });
            let toggle = cx.debug_bounds("include-prereleases").unwrap();
            cx.simulate_click(toggle.center(), gpui::Modifiers::none());
            workspace.update(cx, |this, _| {
                assert_eq!(this.updater_config().include_prereleases, enabled);
                assert!(this.update_check.is_some());
                assert!(!matches!(this.update_state, UpdateState::Available { .. }));
            });
            cx.run_until_parked();
            assert_eq!(
                Settings::load(dir.path()).unwrap().include_prereleases,
                enabled
            );
        }
        crate::dev::TEST_UPDATE_BASE_URL.with(|url| url.borrow_mut().take());
    }

    #[gpui::test]
    fn deleting_the_dark_theme_preserves_the_light_choice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut theme = vela_core::appearance::builtin_themes().remove(0);
        theme.id = "noche".into();
        theme.name = "Night".into();
        theme.builtin = false;
        save_user_theme(dir.path(), &theme).unwrap();
        let mut settings = Settings::default();
        settings.appearance.light_theme = "oceano".into();
        settings.appearance.dark_theme = theme.id.clone();
        settings.save(dir.path()).unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.remove_theme(theme.id, window, cx);
        });
        cx.run_until_parked();
        let settings = Settings::load(dir.path()).unwrap().appearance;
        assert_eq!(settings.light_theme, "oceano");
        assert_eq!(settings.dark_theme, "vela");
        assert!(load_user_themes(dir.path()).0.is_empty());
    }

    #[gpui::test]
    fn font_size_and_theme_changes_share_one_save(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_theme("oceano", Some(ColorVariant::Light), window, cx);
        });
        cx.update(|_, cx| crate::terminal::font_size::increase(cx, 11.));
        for _ in 0..3 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(600));
            cx.run_until_parked();
        }
        let saved = Settings::load(dir.path()).unwrap();
        assert_eq!(saved.appearance.light_theme, "oceano");
        assert_eq!(saved.terminal_font_size, Some(12.));
    }

    fn editor_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("apps");
        std::fs::create_dir_all(root.join("Zed.app")).unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        crate::dev::TEST_EDITOR_ROOT.with(|config| *config.borrow_mut() = Some(root));
        crate::dev::TEST_EDITOR_PICKER.set(true);
        dir
    }

    #[gpui::test]
    fn editor_menu_opens_on_enter_with_the_checked_item_focused(cx: &mut gpui::TestAppContext) {
        let _dir = editor_fixture();
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            Picker::Editor
                .trigger(&workspace.settings_page)
                .focus(window, cx);
        });
        press(cx, "enter");
        workspace.update_in(cx, |workspace, window, _| {
            let menu = workspace.settings_page.settings_menu.as_ref().unwrap();
            assert!(!menu.closing);
            assert!(menu.items[0].is_focused(window));
        });
    }

    #[gpui::test]
    fn escape_closes_the_editor_menu_and_restores_focus(cx: &mut gpui::TestAppContext) {
        let _dir = editor_fixture();
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            Picker::Editor
                .trigger(&workspace.settings_page)
                .focus(window, cx);
        });
        press(cx, "enter");
        press(cx, "escape");
        cx.executor()
            .advance_clock(crate::components::motion::FAST * 2);
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, _| {
            assert!(workspace.settings_page.settings_menu.is_none());
            assert!(
                Picker::Editor
                    .trigger(&workspace.settings_page)
                    .is_focused(window)
            );
        });
    }

    #[gpui::test]
    fn a_click_far_outside_closes_the_editor_menu(cx: &mut gpui::TestAppContext) {
        let _dir = editor_fixture();
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            Picker::Editor
                .trigger(&workspace.settings_page)
                .focus(window, cx);
        });
        press(cx, "enter");
        // Below the macOS title bar strip, whose mouse down starts a window
        // move that the test platform does not implement.
        let top = cx.update(|window, _| crate::workspace::titlebar_height(window));
        cx.simulate_mouse_down(
            point(px(4.), top + px(4.)),
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.executor()
            .advance_clock(crate::components::motion::FAST * 2);
        cx.run_until_parked();
        workspace.update(cx, |workspace, _| {
            assert!(workspace.settings_page.settings_menu.is_none());
        });
    }

    #[gpui::test]
    fn down_and_enter_select_the_focused_editor(cx: &mut gpui::TestAppContext) {
        let dir = editor_fixture();
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            Picker::Editor
                .trigger(&workspace.settings_page)
                .focus(window, cx);
        });
        press(cx, "enter");
        press(cx, "down");
        press(cx, "enter");
        let saved = Settings::load(dir.path()).unwrap();
        assert_eq!(saved.editor, Some(dir.path().join("apps/Zed.app")));
    }

    #[gpui::test]
    fn clicking_the_trigger_closes_the_editor_menu_without_reopening(
        cx: &mut gpui::TestAppContext,
    ) {
        let _dir = editor_fixture();
        cx.update(crate::workspace::bind_keys);
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("secondary-,");
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            Picker::Editor
                .trigger(&workspace.settings_page)
                .focus(window, cx);
        });
        press(cx, "enter");
        let centre = workspace.update(cx, |workspace, _| {
            Picker::Editor
                .bounds(&workspace.settings_page)
                .get()
                .unwrap()
                .center()
        });
        cx.simulate_click(centre, gpui::Modifiers::none());
        cx.executor()
            .advance_clock(crate::components::motion::FAST * 2);
        cx.run_until_parked();
        workspace.update(cx, |workspace, _| {
            assert!(workspace.settings_page.settings_menu.is_none());
        });
    }

    #[gpui::test]
    fn font_size_and_agent_socket_changes_share_one_save(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |workspace, window, cx| {
            let mut next = workspace.settings.clone();
            next.agent_socket = Some(PathBuf::from("/tmp/agente-de-prueba.sock"));
            workspace.save_settings(next, window, cx);
        });
        cx.update(|_, cx| crate::terminal::font_size::increase(cx, 11.));
        for _ in 0..3 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(600));
            cx.run_until_parked();
        }
        let saved = Settings::load(dir.path()).unwrap();
        assert_eq!(
            saved.agent_socket,
            Some(PathBuf::from("/tmp/agente-de-prueba.sock"))
        );
        assert_eq!(saved.terminal_font_size, Some(12.));
    }

    #[gpui::test]
    fn opening_and_closing_settings_passes_through_transient_states(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();

        workspace.update_in(cx, |this, window, cx| {
            this.open_settings(window, cx);
            assert!(this.settings_page.is_open());
            assert!(this.settings_page.is_opening());
            assert!(this.settings_page.is_visible());
        });
        cx.executor().advance_clock(crate::components::motion::BASE);
        cx.run_until_parked();
        workspace.update(cx, |this, _| {
            assert!(this.settings_page.is_open());
            assert!(!this.settings_page.is_opening());
        });

        workspace.update_in(cx, |this, window, cx| {
            this.close_settings(window, cx);
            assert!(!this.settings_page.is_open());
            assert!(this.settings_page.is_closing());
            assert!(this.settings_page.is_visible());
            assert_eq!(window.focused(cx), Some(this.focus_handle()));
        });
        // Reopening mid-close invalidates the fade-out timer.
        let generation =
            workspace.read_with(cx, |this, _| this.settings_page.transition_generation());
        workspace.update_in(cx, |this, window, cx| {
            this.open_settings(window, cx);
            assert!(this.settings_page.is_open());
            assert!(!this.settings_page.is_closing());
            assert_eq!(this.settings_page.transition_generation(), generation + 1);
        });
        cx.executor()
            .advance_clock(crate::components::motion::FAST * 2);
        cx.run_until_parked();
        workspace.update(cx, |this, _| {
            assert!(this.settings_page.is_open());
            assert!(!this.settings_page.is_closing());
            assert!(!this.settings_page.is_opening());
            assert!(this.settings_page.is_visible());
        });
    }

    #[gpui::test]
    fn reduce_motion_switches_settings_instantly(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|config| *config.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(|cx| cx.set_reduce_motion(true));
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        workspace.update_in(cx, |this, window, cx| {
            this.open_settings(window, cx);
            assert!(this.settings_page.is_open());
            assert!(!this.settings_page.is_opening());
            assert!(this.settings_page.is_visible());
            this.close_settings(window, cx);
            assert!(!this.settings_page.is_open());
            assert!(!this.settings_page.is_closing());
            assert!(!this.settings_page.is_visible());
        });
    }
}
