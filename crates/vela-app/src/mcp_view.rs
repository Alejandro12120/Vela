#[path = "mcp_view/harnesses.rs"]
mod harnesses;
#[path = "mcp_view/permissions.rs"]
mod permissions;
#[path = "mcp_view/settings.rs"]
mod settings;
pub(super) use settings::bind;

use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, Div, ElementId, Entity, FocusHandle,
    Focusable, FontWeight, IntoElement, SharedString, Stateful, Window, div, prelude::*, px,
};
use vela_mcp::{
    McpConfig, PermissionProfile, ROLLBACK_DIR_NAME, RollbackStore, ServerMcpPolicy,
    config::ConfigSnapshot, harness::HarnessId, presets, rollback::RollbackUsage,
};

use super::{Theme, Variant, Workspace, button};
use crate::{
    components::{
        button::{small_button, small_icon_button},
        dropdown, modal,
        text_input::TextInput,
    },
    context_menu::Tooltip,
    icons::{IconName, icon},
    mcp_model::{Change, HarnessScan, PendingChanges, Tone},
};

pub(super) struct McpView {
    snapshot: Option<ConfigSnapshot>,
    loading: bool,
    saving: bool,
    config_error: Option<String>,
    conflict: bool,
    usage: Option<Result<RollbackUsage, String>>,
    scan: Option<HarnessScan>,
    scanning: bool,
    scan_error: Option<String>,
    pending: PendingChanges,
    diff: Option<(HarnessId, Option<String>)>,
    diff_scroll: gpui::ScrollHandle,
    manual_preview: Option<HarnessId>,
    results: HashMap<HarnessId, (String, Tone)>,
    applying: bool,
    copied: Option<u64>,
    copy_generation: u64,
    dialog: Option<View>,
    confirmation: Option<Confirmation>,
    focus: FocusHandle,
    scroll: gpui::ScrollHandle,
    last: FocusHandle,
    previous_focus: Option<FocusHandle>,
    select: Option<&'static str>,
    choices: Vec<FocusHandle>,
    select_trigger: Option<FocusHandle>,
    simulator: Option<Entity<TextInput>>,
    simulate_target: Target,
    simulation: Option<Result<(String, vela_mcp::PolicyDecision), String>>,
}

#[derive(Clone)]
enum Target {
    Profile(String),
    Server(String),
}

enum View {
    Harnesses,
    Server(Box<ServerDraft>),
    ChoosePreset,
    Profile(Box<ProfileDraft>),
    Limits([Entity<TextInput>; 3]),
}

enum Confirmation {
    Replace(HarnessId),
    Discard,
    DeleteProfile(String),
    Clear,
}

struct ServerDraft {
    id: String,
    initial: ServerMcpPolicy,
    policy: ServerMcpPolicy,
    notes: Entity<TextInput>,
    rules: RuleInputs,
}

struct ProfileDraft {
    readonly: bool,
    initial: Option<PermissionProfile>,
    profile: PermissionProfile,
    name: Entity<TextInput>,
    rules: RuleInputs,
}

struct RuleInputs {
    fields: HashMap<&'static str, Entity<TextInput>>,
    error: Option<(&'static str, &'static str)>,
}

impl McpView {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        Self {
            snapshot: None,
            loading: false,
            saving: false,
            config_error: None,
            conflict: false,
            usage: None,
            scan: None,
            scanning: false,
            scan_error: None,
            pending: PendingChanges::default(),
            diff: None,
            diff_scroll: gpui::ScrollHandle::new(),
            manual_preview: None,
            results: HashMap::new(),
            applying: false,
            copied: None,
            copy_generation: 0,
            dialog: None,
            confirmation: None,
            focus: cx.focus_handle(),
            scroll: gpui::ScrollHandle::new(),
            last: cx.focus_handle().tab_stop(true),
            previous_focus: None,
            select: None,
            choices: Vec::new(),
            select_trigger: None,
            simulator: None,
            simulate_target: Target::Profile("readonly".into()),
            simulation: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.dialog.is_some() || self.confirmation.is_some()
    }

    fn config(&self) -> Option<&McpConfig> {
        self.snapshot.as_ref().map(|snapshot| &snapshot.config)
    }

    fn busy(&self) -> bool {
        self.loading || self.saving || self.applying
    }
}

impl Workspace {
    pub(super) fn load_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else { return };
        if self.mcp.loading || self.mcp.saving {
            return;
        }
        self.mcp.loading = true;
        self.mcp.config_error = None;
        let task = cx
            .background_executor()
            .spawn(async move { ConfigSnapshot::load(&dir) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.mcp.loading = false;
                this.mcp.conflict = false;
                match result {
                    Ok(snapshot) => {
                        this.mcp.snapshot = Some(snapshot);
                        this.mcp.simulator.get_or_insert_with(|| {
                            input(
                                cx,
                                "Simulador de reglas",
                                "",
                                "Prueba: fs_read /var/log/nginx/error.log o exec sudo nginx -t",
                                true,
                            )
                        });
                        if let Some(View::Server(draft)) = &this.mcp.dialog {
                            let id = draft.id.clone();
                            this.make_server_draft(id, cx);
                        }
                    }
                    Err(error) => {
                        this.mcp.snapshot = None;
                        this.mcp.config_error = Some(format!("No se pudo leer mcp.toml. {error}"));
                    }
                }
                this.refresh_mcp_usage(window, cx);
                this.align_mcp_preview(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_mcp(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        close_after: bool,
        change: impl FnOnce(&mut McpConfig),
    ) {
        let (Some(dir), Some(snapshot)) = (self.dir.clone(), self.mcp.snapshot.clone()) else {
            return;
        };
        if self.mcp.busy() || self.mcp.conflict {
            return;
        }
        let mut config = snapshot.config.clone();
        change(&mut config);
        if config == snapshot.config {
            if close_after {
                self.finish_mcp(window, cx);
            }
            return;
        }
        self.mcp.saving = true;
        self.mcp.config_error = None;
        let task = cx
            .background_executor()
            .spawn(async move { snapshot.save(&dir, &config) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.mcp.saving = false;
                match result {
                    Ok(snapshot) => {
                        this.mcp.snapshot = Some(snapshot);
                        this.mcp.simulation = None;
                        if close_after {
                            this.finish_mcp(window, cx);
                        }
                        this.refresh_mcp_usage(window, cx);
                    }
                    Err(error) => {
                        this.mcp.conflict = error.kind() == io::ErrorKind::AlreadyExists;
                        this.mcp.config_error = Some(if this.mcp.conflict {
                            "La configuración cambió fuera de esta ventana".into()
                        } else {
                            format!("No se pudo guardar mcp.toml. {error}")
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn refresh_mcp_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(dir), Some(config)) = (&self.dir, self.mcp.config()) else {
            return;
        };
        let path = dir.join(ROLLBACK_DIR_NAME);
        let config = config.rollback.clone();
        let task = cx.background_executor().spawn(async move {
            if !path.try_exists()? {
                return Ok(RollbackUsage {
                    count: 0,
                    total_bytes: 0,
                    max_total_bytes: config.max_total_bytes,
                });
            }
            RollbackStore::new(path, config)?.usage(SystemTime::now())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result: io::Result<_> = task.await;
            let _ = this.update(cx, |this, cx| {
                this.mcp.usage = Some(result.map_err(|error| error.to_string()));
                cx.notify();
            });
        })
        .detach();
    }

    fn start_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mcp.previous_focus.is_none() {
            self.mcp.previous_focus = window.focused(cx).or_else(|| Some(self.focus.clone()));
        }
        self.clear_mcp_select();
        self.mcp.scroll = gpui::ScrollHandle::new();
        self.mcp.focus.focus(window, cx);
        cx.notify();
    }

    fn finish_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mcp.dialog = None;
        self.mcp.confirmation = None;
        self.mcp.pending = PendingChanges::default();
        self.mcp.diff = None;
        self.mcp.manual_preview = None;
        self.clear_mcp_select();
        if let Some(focus) = self.mcp.previous_focus.take() {
            focus.focus(window, cx);
        }
        self.show_next_prompt(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mcp.select.is_some() {
            if let Some(focus) = self.mcp.select_trigger.take() {
                focus.focus(window, cx);
            }
            self.clear_mcp_select();
        } else if self.mcp.saving || self.mcp.applying {
            return;
        } else if self.mcp.confirmation.take().is_some() {
            if self.mcp.dialog.is_none() {
                self.finish_mcp(window, cx);
            } else {
                self.mcp.focus.focus(window, cx);
            }
        } else if self.mcp_has_draft_changes(cx) {
            self.mcp.confirmation = Some(Confirmation::Discard);
            self.mcp.focus.focus(window, cx);
        } else {
            self.finish_mcp(window, cx);
        }
        cx.notify();
    }

    pub(super) fn confirm_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mcp.saving || self.mcp.applying || self.mcp.select.is_some() {
            return;
        }
        match self.mcp.confirmation.take() {
            Some(Confirmation::Replace(id)) => {
                self.toggle_harness_change(id, Change::Replace, cx);
                self.mcp.focus.focus(window, cx);
            }
            Some(Confirmation::Discard) => self.finish_mcp(window, cx),
            Some(Confirmation::DeleteProfile(id)) => self.delete_mcp_profile(id, window, cx),
            Some(Confirmation::Clear) => self.rollback_action(true, window, cx),
            None => match self.mcp.dialog {
                Some(View::Harnesses) => self.apply_mcp_harnesses(window, cx),
                Some(View::Server(_) | View::Profile(_)) => self.submit_permissions(window, cx),
                Some(View::Limits(_)) => self.save_rollback_limits(window, cx),
                _ => {}
            },
        }
        cx.notify();
    }

    pub(super) fn dev_open_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = crate::dev::mcp_view();
        if request.as_deref() == Some("harnesses") {
            if !self.settings_open() {
                self.open_settings(window, cx);
            }
            self.open_mcp_harnesses(window, cx);
        } else if let Some(name) = request
            .as_deref()
            .and_then(|request| request.strip_prefix("server:"))
            && let Some(server) = self
                .servers
                .iter()
                .find(|server| server.display_name() == name)
        {
            let id = server.id.clone();
            self.select(&id);
            self.open_mcp_server(id, window, cx);
        }
    }

    pub(super) fn mcp_header(&self, id: &str, theme: Theme, cx: &Context<Self>) -> Div {
        let label = self
            .mcp
            .config()
            .and_then(|config| {
                let policy = config.servers.get(id)?;
                (config.enabled && policy.availability != vela_mcp::ServerAvailability::Disabled)
                    .then(|| presets::resolve_profile(config, &policy.profile_id))
                    .flatten()
                    .map(|profile| profile.name)
            })
            .unwrap_or_else(|| "Desactivado".into());
        let id = id.to_owned();
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(badge(format!("MCP: {label}"), Tone::Neutral, theme).max_w(px(128.)))
            .child(
                small_icon_button(
                    "open-server-mcp",
                    "Acceso MCP…",
                    IconName::Plug,
                    Variant::Secondary,
                    theme,
                    cx.listener(move |this, _, window, cx| {
                        this.open_mcp_server(id.clone(), window, cx)
                    }),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.accent)),
            )
    }

    pub(super) fn render_mcp_dialog(&self, theme: Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let (title, body, hint, action, enabled) = if let Some(confirmation) =
            &self.mcp.confirmation
        {
            let (title, message, action) = match confirmation {
                Confirmation::Replace(id) => (
                    "¿Reemplazar la entrada vela?",
                    format!(
                        "La entrada actual de {} se sustituirá al pulsar Aplicar. Vela guardará una copia del archivo.",
                        id.display_name()
                    ),
                    "Marcar reemplazo",
                ),
                Confirmation::Discard => (
                    "¿Descartar los cambios?",
                    "Se perderán los cambios de esta ventana.".into(),
                    "Descartar",
                ),
                Confirmation::DeleteProfile(_) => (
                    "¿Borrar este perfil?",
                    "El perfil se eliminará de mcp.toml.".into(),
                    "Borrar",
                ),
                Confirmation::Clear => (
                    "¿Vaciar las copias de rollback?",
                    "Las copias guardadas se borrarán de forma permanente.".into(),
                    "Vaciar",
                ),
            };
            (
                title.into(),
                muted(message, theme).into_any_element(),
                String::new(),
                Some(action),
                !self.mcp.busy(),
            )
        } else {
            match self.mcp.dialog.as_ref()? {
                View::Harnesses => {
                    let count = self.mcp.pending.len();
                    let hint = if self.mcp.applying {
                        "Aplicando…".into()
                    } else if count == 0 {
                        "Sin cambios pendientes.".into()
                    } else if count == 1 {
                        "1 cambio pendiente".into()
                    } else {
                        format!("{count} cambios pendientes")
                    };
                    (
                        "Conectar agentes".into(),
                        self.harnesses_body(theme, cx).into_any_element(),
                        hint,
                        Some("Aplicar"),
                        self.mcp
                            .pending
                            .can_apply(self.mcp.scanning || self.mcp.applying),
                    )
                }
                View::Server(draft) => self.server_dialog(draft, theme, cx),
                View::ChoosePreset => (
                    "Nuevo perfil".into(),
                    self.preset_choices(theme, cx).into_any_element(),
                    String::new(),
                    None,
                    false,
                ),
                View::Profile(draft) => self.profile_dialog(draft, theme, cx),
                View::Limits(fields) => self.limits_dialog(fields, theme, cx),
            }
        };
        let cancel = button(
            "mcp-cancel",
            if action.is_none() {
                "Cerrar"
            } else {
                "Cancelar"
            },
            Variant::Secondary,
            theme,
            cx.listener(|this, _, window, cx| this.cancel_mcp(window, cx)),
        )
        .focus(|style| style)
        .focus_visible(move |style| style.border_color(theme.accent))
        .when(!enabled, |button| button.track_focus(&self.mcp.last));
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_2()
            .child(muted(hint, theme))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(cancel)
                    .children(action.map(|label| {
                        if enabled {
                            button(
                                "mcp-confirm",
                                label,
                                Variant::Primary,
                                theme,
                                cx.listener(|this, _, window, cx| this.confirm_mcp(window, cx)),
                            )
                            .track_focus(&self.mcp.last)
                            .focus(|style| style)
                            .focus_visible(move |style| style.border_color(theme.accent))
                            .into_any_element()
                        } else {
                            disabled_button(label, theme).h(px(30.)).into_any_element()
                        }
                    })),
            );
        let width = if self.mcp.confirmation.is_some()
            || matches!(self.mcp.dialog, Some(View::Limits(_) | View::ChoosePreset))
        {
            448.
        } else {
            592.
        };
        Some(
            modal::overlay_with_width(
                title,
                None,
                body,
                footer.into_any_element(),
                &self.mcp.focus,
                &self.mcp.last,
                theme,
                true,
                px(width),
                "McpDialog",
                Some(&self.mcp.scroll),
                cx,
            )
            .id("mcp-overlay")
            .on_action(cx.listener(|this, _: &settings::PageUp, window, _| {
                this.scroll_mcp_page(window, -1.);
            }))
            .on_action(cx.listener(|this, _: &settings::PageDown, window, _| {
                this.scroll_mcp_page(window, 1.);
            }))
            .into_any_element(),
        )
    }

    fn scroll_mcp_page(&self, window: &mut Window, direction: f32) {
        let scroll = &self.mcp.scroll;
        let offset = scroll.offset();
        scroll.set_offset(gpui::point(
            offset.x,
            (offset.y - scroll.bounds().size.height * direction * 0.8).min(px(0.)),
        ));
        window.refresh();
    }

    fn mcp_error(&self, theme: Theme, cx: &Context<Self>) -> Option<Div> {
        let message = self.mcp.config_error.clone()?;
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_color(theme.danger)
                        .text_size(px(12.))
                        .child(message),
                )
                .child(small_button(
                    "reload-mcp",
                    "Recargar",
                    Variant::Secondary,
                    theme,
                    cx.listener(|this, _, window, cx| this.reload_mcp_draft(window, cx)),
                )),
        )
    }

    fn reload_mcp_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(View::Profile(_) | View::Limits(_)) = self.mcp.dialog {
            self.finish_mcp(window, cx);
        }
        self.load_mcp(window, cx);
    }

    pub(super) fn close_mcp_settings_popup(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.mcp.select.is_none() {
            return false;
        }
        self.cancel_mcp(window, cx);
        true
    }

    fn clear_mcp_select(&mut self) {
        self.mcp.select = None;
        self.mcp.select_trigger = None;
        self.mcp.choices.clear();
    }

    #[allow(clippy::too_many_arguments)]
    fn mcp_select(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        choices: Vec<(String, String)>,
        selected: &str,
        theme: Theme,
        cx: &Context<Self>,
        choose: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let count = choices.len();
        let trigger = button(
            id,
            label,
            Variant::Secondary,
            theme,
            cx.listener(move |this, _, window, cx| {
                if this.mcp.select.take() == Some(id) {
                    this.clear_mcp_select();
                } else {
                    this.mcp.select = Some(id);
                    this.mcp.select_trigger = window.focused(cx);
                    this.mcp.choices = (0..count)
                        .map(|_| cx.focus_handle().tab_stop(true))
                        .collect();
                    if let Some(first) = this.mcp.choices.first() {
                        first.focus(window, cx);
                    }
                }
                cx.notify();
            }),
        )
        .w_full()
        .justify_between()
        .min_w_0()
        .focus(|style| style)
        .focus_visible(move |style| style.border_color(theme.accent))
        .child(
            icon(IconName::ChevronDown)
                .size(px(14.))
                .text_color(theme.secondary_text),
        );
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(trigger)
            .when(self.mcp.select == Some(id), |row| {
                row.child(
                    dropdown::dropdown_panel(format!("{id}-options"), theme)
                        .min_w_0()
                        .on_key_down(|event, window, cx| match event.keystroke.key.as_str() {
                            "down" => {
                                cx.stop_propagation();
                                window.focus_next(cx);
                            }
                            "up" => {
                                cx.stop_propagation();
                                window.focus_prev(cx);
                            }
                            _ => {}
                        })
                        .children(choices.into_iter().zip(&self.mcp.choices).enumerate().map(
                            |(index, ((value, label), focus))| {
                                let choose = choose.clone();
                                dropdown::dropdown_item(
                                    (id, index),
                                    label,
                                    value == selected,
                                    focus,
                                    theme,
                                )
                                .flex_none()
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if let Some(focus) = this.mcp.select_trigger.take() {
                                            focus.focus(window, cx);
                                        }
                                        this.clear_mcp_select();
                                        choose(this, value.clone(), window, cx);
                                        cx.notify();
                                    },
                                ))
                            },
                        )),
                )
            })
    }
}

fn on_segment(
    cx: &Context<Workspace>,
    callback: impl Fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>) + 'static,
) -> impl Fn(usize, &mut Window, &mut App) + 'static {
    let listener =
        cx.listener(move |this, index: &usize, window, cx| callback(this, *index, window, cx));
    move |index, window, cx| listener(&index, window, cx)
}

fn input(
    cx: &mut Context<Workspace>,
    label: &'static str,
    value: &str,
    placeholder: &'static str,
    mono: bool,
) -> Entity<TextInput> {
    let entity = cx.new(|cx| {
        let input = TextInput::new(value, label, placeholder, false, cx).keyboard_focus_only();
        if mono { input.monospace() } else { input }
    });
    cx.observe(&entity, |_, _, cx| cx.notify()).detach();
    entity
}

fn muted(text: impl Into<SharedString>, theme: Theme) -> Div {
    div()
        .min_w_0()
        .text_size(px(12.))
        .text_color(theme.secondary_text)
        .child(text.into())
}

fn technical(text: impl Into<SharedString>, theme: Theme) -> Div {
    muted(text, theme).font_family(crate::theme::monospace())
}

fn technical_sentence(
    before: &'static str,
    code: &'static str,
    after: &'static str,
    theme: Theme,
) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_x_1()
        .child(muted(before, theme).flex_none().whitespace_nowrap())
        .child(
            div()
                .flex()
                .flex_none()
                .items_baseline()
                .whitespace_nowrap()
                .child(technical(code, theme).flex_none())
                .child(muted(after, theme).flex_none()),
        )
}

fn heading(text: impl Into<SharedString>) -> Div {
    div().font_weight(FontWeight::MEDIUM).child(text.into())
}

fn tone_color(tone: Tone, theme: Theme) -> gpui::Rgba {
    match tone {
        Tone::Neutral => theme.secondary_text,
        Tone::Success => theme.success,
        Tone::Warning => theme.warning,
        Tone::Danger => theme.danger,
    }
}

fn badge(text: impl Into<SharedString>, tone: Tone, theme: Theme) -> Div {
    div()
        .flex_none()
        .rounded(px(5.))
        .px_1()
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(if tone == Tone::Neutral {
            theme.text
        } else {
            tone_color(tone, theme)
        })
        .bg(match tone {
            Tone::Neutral => theme.selection,
            Tone::Success => theme.hover,
            Tone::Warning => theme.warning_bg,
            Tone::Danger => theme.danger_bg,
        })
        .truncate()
        .child(text.into())
}

fn ellipsis(id: impl Into<ElementId>, text: String, theme: Theme) -> Stateful<Div> {
    let text: SharedString = text.into();
    div()
        .id(id)
        .min_w_0()
        .truncate()
        .text_size(px(12.))
        .text_color(theme.secondary_text)
        .tooltip({
            let text = text.clone();
            move |_, cx| cx.new(|_| Tooltip(text.clone())).into()
        })
        .child(text)
}

fn disabled_button(label: &'static str, theme: Theme) -> Stateful<Div> {
    div()
        .id(label)
        .role(gpui::Role::Button)
        .aria_description("No disponible")
        .flex_none()
        .flex()
        .items_center()
        .h(px(24.))
        .px_2()
        .rounded(px(6.))
        .border_1()
        .border_color(theme.border)
        .text_color(theme.secondary_text)
        .opacity(0.65)
        .child(label)
}

fn small(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    small_button(id, label, Variant::Secondary, theme, on_click)
        .focus(|style| style)
        .focus_visible(move |style| style.border_color(theme.accent))
}

fn stack() -> Div {
    div().min_w_0().flex().flex_col().gap_2()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use vela_core::{Auth, config::Server};

    #[gpui::test]
    fn harness_modal_opens_with_apply_disabled_and_closes_with_escape(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        crate::dev::TEST_CONFIG.with(|path| *path.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        let (view, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        view.update_in(cx, |this, window, cx| {
            this.open_mcp_harnesses(window, cx);
            assert!(matches!(this.mcp.dialog, Some(View::Harnesses)));
            assert!(!this.mcp.pending.can_apply(false));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        view.update(cx, |this, _| assert!(!this.mcp.is_open()));
    }

    #[gpui::test]
    fn server_sheet_requires_changes_and_confirms_discard(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        vela_core::config::save(
            dir.path(),
            vec![Server {
                id: "fixture".into(),
                name: "Fixture".into(),
                host: "example.test".into(),
                user: "deploy".into(),
                port: 22,
                auth: Auth::Password,
                initial_remote_path: None,
                pinned: false,
            }],
        )
        .unwrap();
        crate::dev::TEST_CONFIG.with(|path| *path.borrow_mut() = Some(dir.path().to_owned()));
        cx.update(crate::workspace::bind_keys);
        let (view, cx) =
            cx.add_window_view(|window, cx| Workspace::new(None, None, None, window, cx));
        cx.run_until_parked();
        view.update_in(cx, |this, window, cx| {
            this.open_mcp_server("fixture".into(), window, cx)
        });
        cx.run_until_parked();
        view.update(cx, |this, cx| {
            let Some(View::Server(draft)) = &mut this.mcp.dialog else {
                panic!("no server view")
            };
            assert_eq!(draft.policy, ServerMcpPolicy::default());
            assert!(!this.mcp_has_draft_changes(cx));
        });
        cx.simulate_keystrokes("escape");
        view.update_in(cx, |this, window, cx| {
            assert!(!this.mcp.is_open());
            this.open_mcp_server("fixture".into(), window, cx);
        });
        cx.run_until_parked();
        view.update(cx, |this, cx| {
            let Some(View::Server(draft)) = &mut this.mcp.dialog else {
                panic!("no server view")
            };
            draft.policy.hide_server_identity = Some(false);
            cx.notify();
        });
        cx.simulate_keystrokes("escape");
        view.update_in(cx, |this, window, cx| {
            assert!(matches!(this.mcp.confirmation, Some(Confirmation::Discard)));
            this.confirm_mcp(window, cx);
            assert!(!this.mcp.is_open());
        });
        assert!(!dir.path().join(vela_mcp::config::CONFIG_FILE_NAME).exists());
    }
}
