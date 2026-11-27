use vela_mcp::{Decision, ServerAvailability, config::ExecMode, path::valid_scope_pattern};

use super::*;
use crate::{
    components::{radio::radio_dot, segmented::segmented},
    mcp_model::{availability_allowed, exec_label, profile_usage, valid_notes},
    workspace::settings_view::{settings_group, settings_row},
};

type DialogContent = (SharedString, AnyElement, String, Option<&'static str>, bool);

#[derive(Clone, Copy)]
enum Rule {
    RemoteAllow,
    RemoteDeny,
    LocalAllow,
    CommandAllow,
    CommandDeny,
}

impl Rule {
    const ALL: [Self; 5] = [
        Self::RemoteAllow,
        Self::RemoteDeny,
        Self::LocalAllow,
        Self::CommandAllow,
        Self::CommandDeny,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::RemoteAllow => "Rutas remotas permitidas",
            Self::RemoteDeny => "Rutas remotas prohibidas (Deny siempre)",
            Self::LocalAllow => "Rutas locales para transferencias",
            Self::CommandAllow => "Comandos permitidos",
            Self::CommandDeny => "Comandos prohibidos",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::RemoteAllow => "/srv/proyecto/**",
            Self::RemoteDeny => "**/.secrets/**",
            Self::LocalAllow => "~/proyectos/**",
            Self::CommandAllow => "systemctl status *",
            Self::CommandDeny => "systemctl stop *",
        }
    }

    fn validate(self, value: &str) -> Result<(), &'static str> {
        let valid = match self {
            Self::CommandAllow | Self::CommandDeny => {
                vela_mcp::command::tokenize(value).is_ok_and(|tokens| !tokens.is_empty())
                    && !value.chars().any(char::is_control)
            }
            _ => valid_scope_pattern(value, matches!(self, Self::LocalAllow)),
        };
        if valid {
            Ok(())
        } else {
            Err(match self {
                Self::LocalAllow => {
                    "Usa una ruta absoluta, ~/ o un patrón **/ sin segmentos . ni ..."
                }
                Self::CommandAllow | Self::CommandDeny => {
                    "Escribe un patrón de comando con las comillas cerradas."
                }
                _ => "Usa una ruta absoluta o un patrón **/ sin segmentos . ni ...",
            })
        }
    }

    fn server(self, policy: &mut ServerMcpPolicy) -> &mut Vec<String> {
        match self {
            Self::RemoteAllow => &mut policy.allowed_remote_paths,
            Self::RemoteDeny => &mut policy.denied_remote_paths,
            Self::LocalAllow => &mut policy.allowed_local_paths,
            Self::CommandAllow => &mut policy.allowed_commands,
            Self::CommandDeny => &mut policy.denied_commands,
        }
    }

    fn profile(self, profile: &mut PermissionProfile) -> Option<&mut Vec<String>> {
        Some(match self {
            Self::RemoteAllow => &mut profile.default_allowed_remote_paths,
            Self::RemoteDeny => &mut profile.default_denied_remote_paths,
            Self::LocalAllow => return None,
            Self::CommandAllow => &mut profile.allowed_commands,
            Self::CommandDeny => &mut profile.denied_commands,
        })
    }
}

impl RuleInputs {
    fn new(cx: &mut Context<Workspace>) -> Self {
        Self {
            fields: Rule::ALL
                .into_iter()
                .map(|rule| {
                    (
                        rule.label(),
                        input(cx, rule.label(), "", rule.placeholder(), true),
                    )
                })
                .collect(),
            error: None,
        }
    }

    fn empty(&self, cx: &App) -> bool {
        self.fields
            .values()
            .all(|field| field.read(cx).value().trim().is_empty())
    }
}

fn optional_text(input: &Entity<TextInput>, cx: &App) -> Option<String> {
    let value = input.read(cx).value();
    (!value.trim().is_empty()).then(|| value.to_owned())
}

const DECISIONS: [Decision; 3] = [Decision::Allow, Decision::Ask, Decision::Deny];

macro_rules! profile_operations {
    ($apply:ident, $($arg:expr),*) => {
        $apply!($($arg),*;
            fs_list => "Listar", fs_stat => "Ver atributos", fs_read => "Leer",
            fs_write => "Escribir", fs_patch => "Editar", fs_mkdir => "Crear carpetas",
            fs_move => "Mover", fs_delete => "Borrar", fs_chmod => "Cambiar permisos",
            transfer_upload => "Subir archivos", transfer_download => "Descargar archivos"
        )
    }
}

macro_rules! render_operations {
    ($this:expr, $profile:expr, $readonly:expr, $theme:expr, $cx:expr; $($field:ident => $label:literal),*) => {
        vec![$(settings_row($label, None,
            if $readonly {
                muted(crate::mcp_model::decision_label($profile.$field), $theme).into_any_element()
            } else {
                segmented(stringify!($field), $label, &[("Allow", None), ("Ask", None), ("Deny", None)],
                    DECISIONS.iter().position(|mode| *mode == $profile.$field).unwrap(), $theme,
                    on_segment($cx, |this, index, _, cx| {
                        if let Some(View::Profile(draft)) = &mut this.mcp.dialog {
                            draft.profile.$field = DECISIONS[index];
                            cx.notify();
                        }
                    })).focus(|style| style).focus_visible(move |style| style.border_color($theme.accent)).into_any_element()
            }, $theme)),*]
    }
}

impl Workspace {
    pub(in crate::workspace) fn open_mcp_server(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() || self.mcp.is_open() || self.mcp.saving {
            return;
        }
        self.make_server_draft(id, cx);
        self.start_mcp(window, cx);
        self.load_mcp(window, cx);
    }

    pub(super) fn make_server_draft(&mut self, id: String, cx: &mut Context<Self>) {
        let policy = self
            .mcp
            .config()
            .and_then(|config| config.servers.get(&id))
            .cloned()
            .unwrap_or_default();
        let notes = input(
            cx,
            "Notas para el agente",
            policy.notes.as_deref().unwrap_or(""),
            "Contexto visible en list_servers",
            false,
        );
        self.mcp.dialog = Some(View::Server(Box::new(ServerDraft {
            id,
            initial: policy.clone(),
            policy,
            notes,
            rules: RuleInputs::new(cx),
        })));
    }

    pub(super) fn open_mcp_profile(
        &mut self,
        id: String,
        duplicate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.mcp.config() else {
            return;
        };
        let Some(mut profile) = (if duplicate {
            presets::builtin_profile(&id)
        } else {
            presets::resolve_profile(config, &id)
        }) else {
            return;
        };
        let initial = if duplicate {
            let mut index = 1;
            loop {
                let id = format!("{}-copia-{index}", profile.id);
                if presets::resolve_profile(config, &id).is_none() {
                    profile.id = id;
                    break;
                }
                index += 1;
            }
            profile.name.push_str(" · copia");
            profile.builtin = false;
            None
        } else {
            Some(profile.clone())
        };
        let name = input(cx, "Nombre del perfil", &profile.name, "Nombre", false);
        self.clear_mcp_select();
        self.mcp.dialog = Some(View::Profile(Box::new(ProfileDraft {
            initial,
            profile,
            name,
            rules: RuleInputs::new(cx),
        })));
        self.start_mcp(window, cx);
    }

    pub(super) fn mcp_has_draft_changes(&self, cx: &App) -> bool {
        match self.mcp.dialog.as_ref() {
            Some(View::Server(draft)) => {
                draft.policy != draft.initial
                    || optional_text(&draft.notes, cx) != draft.initial.notes
                    || !draft.rules.empty(cx)
            }
            Some(View::Profile(draft)) if !self.profile_readonly(draft) => {
                draft.initial.as_ref() != Some(&draft.profile)
                    || draft.name.read(cx).value() != draft.profile.name
                    || !draft.rules.empty(cx)
            }
            _ => false,
        }
    }

    pub(super) fn submit_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rule = match self.mcp.dialog.as_ref() {
            Some(View::Server(draft)) => Some(&draft.rules),
            Some(View::Profile(draft)) => Some(&draft.rules),
            _ => None,
        }
        .and_then(|rules| {
            Rule::ALL.into_iter().find(|rule| {
                rules.fields[rule.label()]
                    .focus_handle(cx)
                    .is_focused(window)
            })
        });
        if let Some(rule) = rule {
            self.add_mcp_rule(rule, window, cx);
            return;
        }
        if let Some(View::Server(draft)) = &self.mcp.dialog
            && self.server_can_save(draft, cx)
        {
            let mut policy = draft.policy.clone();
            policy.notes = optional_text(&draft.notes, cx);
            let id = draft.id.clone();
            self.save_mcp(window, cx, true, move |config| {
                config.servers.insert(id, policy);
            });
        } else if let Some(View::Profile(draft)) = &self.mcp.dialog
            && self.profile_can_save(draft, cx)
        {
            let mut profile = draft.profile.clone();
            profile.name = draft.name.read(cx).value().trim().to_owned();
            self.save_mcp(window, cx, true, move |config| {
                if let Some(existing) = config
                    .profiles
                    .iter_mut()
                    .find(|existing| existing.id == profile.id)
                {
                    *existing = profile;
                } else {
                    config.profiles.push(profile);
                }
            });
        }
    }

    fn server_can_save(&self, draft: &ServerDraft, cx: &App) -> bool {
        let Some(config) = self.mcp.config() else {
            return false;
        };
        let auth = self
            .servers
            .iter()
            .find(|server| server.id == draft.id)
            .map(|server| &server.auth);
        !self.mcp.busy()
            && !self.mcp.conflict
            && auth.is_some_and(|auth| availability_allowed(draft.policy.availability, auth))
            && presets::resolve_profile(config, &draft.policy.profile_id).is_some()
            && (valid_notes(draft.notes.read(cx).value())
                || optional_text(&draft.notes, cx) == draft.initial.notes)
            && draft.rules.empty(cx)
            && (draft.policy != draft.initial
                || optional_text(&draft.notes, cx) != draft.initial.notes)
    }

    fn profile_readonly(&self, draft: &ProfileDraft) -> bool {
        draft.initial.is_some()
            && self.mcp.config().is_some_and(|config| {
                !config
                    .profiles
                    .iter()
                    .any(|profile| profile.id == draft.profile.id)
            })
    }

    fn profile_can_save(&self, draft: &ProfileDraft, cx: &App) -> bool {
        let name = draft.name.read(cx).value();
        !self.mcp.busy()
            && !self.mcp.conflict
            && !self.profile_readonly(draft)
            && !name.trim().is_empty()
            && valid_notes(name)
            && draft.rules.empty(cx)
            && self.mcp_has_draft_changes(cx)
    }

    pub(super) fn delete_mcp_profile(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .mcp
            .config()
            .is_some_and(|config| profile_usage(config, &id) == 0)
        {
            self.save_mcp(window, cx, true, move |config| {
                config.profiles.retain(|profile| profile.id != id)
            });
        }
    }

    pub(super) fn preset_choices(&self, theme: Theme, cx: &Context<Self>) -> Div {
        stack()
            .child(muted(
                "Elige un perfil de fábrica para copiar sus reglas.",
                theme,
            ))
            .children(presets::builtin_profiles().into_iter().map(|profile| {
                small(
                    format!("duplicate-profile-{}", profile.id),
                    profile.name,
                    theme,
                    cx.listener(move |this, _, window, cx| {
                        this.open_mcp_profile(profile.id.clone(), true, window, cx);
                    }),
                )
                .justify_start()
            }))
    }

    pub(super) fn server_dialog(
        &self,
        draft: &ServerDraft,
        theme: Theme,
        cx: &Context<Self>,
    ) -> DialogContent {
        let name = self.servers.iter().find(|server| server.id == draft.id);
        let title = format!(
            "{} · Acceso MCP",
            name.map_or("Servidor", |server| server.display_name())
        )
        .into();
        let (Some(server), Some(config)) = (name, self.mcp.config()) else {
            let body = stack()
                .children(self.mcp_error(theme, cx))
                .when(self.mcp.loading, |body| {
                    body.child(muted("Cargando mcp.toml…", theme))
                });
            return (
                title,
                body.into_any_element(),
                "Guardado en mcp.toml".into(),
                Some("Guardar"),
                false,
            );
        };
        let policy = &draft.policy;
        let profile = presets::resolve_profile(config, &policy.profile_id);
        let profile_name = profile
            .as_ref()
            .map_or("Perfil no encontrado", |profile| profile.name.as_str());
        let body = stack().gap_3().children(self.mcp_error(theme, cx))
            .child(div().flex().items_start().gap_2().justify_between()
                .child(stack().gap_0().flex_1()
                    .child(ellipsis("mcp-endpoint", format!("{}@{}:{}", server.user, server.host, server.port), theme).font_family(crate::theme::monospace()))
                    .when(policy.hides_identity(config), |heading| heading.child(muted(policy.alias.as_ref().map_or_else(
                        || format!("Oculto a los agentes · ID {}", server.id),
                        |alias| format!("Oculto a los agentes (alias {alias})"),
                    ), theme))))
                .child(badge(profile_name.to_owned(), Tone::Neutral, theme).max_w(px(210.))))
            .child(self.availability_rows(policy.availability, &server.auth, theme, cx))
            .child(settings_group(theme, vec![
                stack().px_3().py_2().gap_1().child(heading("Perfil de permisos"))
                    .child(self.mcp_select("mcp-server-profile", profile_name.to_owned(),
                        presets::all_profiles(config).into_iter().map(|profile| (profile.id, profile.name)).collect(),
                        &policy.profile_id, theme, cx, |this, id, _, _| {
                            if let Some(View::Server(draft)) = &mut this.mcp.dialog { draft.policy.profile_id = id; }
                        })),
                stack().px_3().py_2().gap_1().child(heading("Notas para el agente")).child(draft.notes.clone()),
                settings_row("Ocultar host y usuario",
                    Some(format!("Ajuste global {}.", if config.hide_server_identity { "activo" } else { "inactivo" }).into()),
                    segmented("mcp-server-hide", "Ocultar host y usuario", &[("Heredar", None), ("Sí", None), ("No", None)],
                        match policy.hide_server_identity { None => 0, Some(true) => 1, Some(false) => 2 }, theme,
                        on_segment(cx, |this, index, _, cx| {
                            if let Some(View::Server(draft)) = &mut this.mcp.dialog {
                                draft.policy.hide_server_identity = [None, Some(true), Some(false)][index];
                                cx.notify();
                            }
                        })).focus(|style| style).focus_visible(move |style| style.border_color(theme.accent)), theme)
            ]))
            .child(heading("Rutas remotas"))
            .child(muted("Los patrones permitidos delimitan el ámbito de las operaciones que permite el perfil.", theme))
            .child(self.rule_list(Rule::RemoteAllow, &policy.allowed_remote_paths, profile.as_ref().map_or(&[], |profile| &profile.default_allowed_remote_paths), Some(&draft.rules), theme, cx))
            .child(self.rule_list(Rule::RemoteDeny, &policy.denied_remote_paths, &profile.as_ref().map_or_else(presets::critical_denied_paths, PermissionProfile::denied_paths), Some(&draft.rules), theme, cx))
            .child(heading("Comandos y transferencias"))
            .child(self.exec_control(policy.exec_mode, false, false, theme, cx))
            .child(self.sudo_control(policy.sudo, false, false, theme, cx))
            .child(self.rule_list(Rule::CommandAllow, &policy.allowed_commands, profile.as_ref().map_or(&[], |profile| &profile.allowed_commands), Some(&draft.rules), theme, cx))
            .child(self.rule_list(Rule::CommandDeny, &policy.denied_commands, profile.as_ref().map_or(&[], |profile| &profile.denied_commands), Some(&draft.rules), theme, cx))
            .child(self.rule_list(Rule::LocalAllow, &policy.allowed_local_paths, &[], Some(&draft.rules), theme, cx));
        (
            title,
            body.into_any_element(),
            "Guardado en mcp.toml".into(),
            Some("Guardar"),
            self.server_can_save(draft, cx),
        )
    }

    fn availability_rows(
        &self,
        selected: ServerAvailability,
        auth: &vela_core::Auth,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Div {
        use ServerAvailability::*;
        let options = [
            (Disabled, "Desactivado", "No aparece en list_servers."),
            (
                ConnectedOnly,
                "Solo si ya está conectado",
                "Limita el acceso a una sesión abierta en Vela.",
            ),
            (
                OnDemandAsk,
                "Bajo demanda con aprobación",
                "Solicita aprobación para conectar.",
            ),
            (
                Headless,
                "Desatendido (headless)",
                "Conecta con clave SSH, sin elevación desatendida.",
            ),
        ];
        let rows = options
            .into_iter()
            .enumerate()
            .map(|(i, (mode, label, description))| {
                let enabled = availability_allowed(mode, auth);
                let row = div()
                    .id(("mcp-access-mode", i))
                    .role(gpui::Role::RadioButton)
                    .aria_label(label)
                    .aria_selected(selected == mode)
                    .when(!enabled, |row| row.aria_description("No disponible"))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .when(selected == mode, |row| row.bg(theme.selection))
                    .child(radio_dot(selected == mode, theme))
                    .child(stack().gap_0().child(heading(label)).child(muted(
                        if enabled {
                            description
                        } else {
                            "Requiere autenticación con clave SSH. Este servidor usa contraseña."
                        },
                        theme,
                    )));
                let row = if enabled {
                    row.tab_index(0)
                        .cursor_pointer()
                        .hover(move |style| style.bg(theme.hover))
                        .focus(|style| style)
                        .focus_visible(move |style| style.border_color(theme.accent))
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(View::Server(draft)) = &mut this.mcp.dialog {
                                draft.policy.availability = mode;
                                cx.notify();
                            }
                        }))
                } else {
                    row.opacity(0.65)
                };
                div().child(row)
            })
            .collect();
        stack()
            .gap_1()
            .child(heading("Disponibilidad para agentes"))
            .child(settings_group(theme, rows))
    }

    pub(super) fn profile_dialog(
        &self,
        draft: &ProfileDraft,
        theme: Theme,
        cx: &Context<Self>,
    ) -> DialogContent {
        let readonly = self.profile_readonly(draft);
        let profile = &draft.profile;
        let rules = (!readonly).then_some(&draft.rules);
        let title = if readonly {
            profile.name.clone()
        } else if draft.initial.is_none() {
            "Nuevo perfil".into()
        } else {
            "Editar perfil".into()
        };
        let count = self
            .mcp
            .config()
            .map_or(0, |config| profile_usage(config, &profile.id));
        let body = stack()
            .gap_3()
            .children(self.mcp_error(theme, cx))
            .when(readonly, |body| {
                body.child(badge("Preset", Tone::Neutral, theme).self_start())
            })
            .when(!readonly, |body| {
                body.child(
                    stack()
                        .gap_1()
                        .child(heading("Nombre del perfil"))
                        .child(draft.name.clone()),
                )
            })
            .child(heading("Operaciones de archivos y transferencias"))
            .child(settings_group(
                theme,
                profile_operations!(render_operations, self, profile, readonly, theme, cx),
            ))
            .child(heading("Rutas remotas"))
            .child(self.rule_list(
                Rule::RemoteAllow,
                &profile.default_allowed_remote_paths,
                &[],
                rules,
                theme,
                cx,
            ))
            .child(self.rule_list(
                Rule::RemoteDeny,
                &profile.default_denied_remote_paths,
                &presets::critical_denied_paths(),
                rules,
                theme,
                cx,
            ))
            .child(heading("Comandos"))
            .child(self.exec_control(Some(profile.exec_mode), true, readonly, theme, cx))
            .child(self.sudo_control(Some(profile.sudo), true, readonly, theme, cx))
            .child(self.rule_list(
                Rule::CommandAllow,
                &profile.allowed_commands,
                &[],
                rules,
                theme,
                cx,
            ))
            .child(self.rule_list(
                Rule::CommandDeny,
                &profile.denied_commands,
                &[],
                rules,
                theme,
                cx,
            ))
            .when(!readonly && draft.initial.is_some(), |body| {
                let id = profile.id.clone();
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(if count == 0 && !self.mcp.conflict {
                            small(
                                "delete-profile",
                                "Borrar perfil…",
                                theme,
                                cx.listener(move |this, _, window, cx| {
                                    this.mcp.confirmation =
                                        Some(Confirmation::DeleteProfile(id.clone()));
                                    this.start_mcp(window, cx);
                                }),
                            )
                            .text_color(theme.danger)
                            .into_any_element()
                        } else {
                            disabled_button("Borrar perfil…", theme).into_any_element()
                        })
                        .when(count > 0, |row| {
                            row.child(muted("Este perfil se usa en al menos un servidor.", theme))
                        }),
                )
            });
        (
            title.into(),
            body.into_any_element(),
            if readonly {
                String::new()
            } else {
                "Guardado en mcp.toml".into()
            },
            (!readonly).then_some("Guardar"),
            self.profile_can_save(draft, cx),
        )
    }

    fn exec_control(
        &self,
        current: Option<ExecMode>,
        profile: bool,
        readonly: bool,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Div {
        let modes = [
            Some(ExecMode::Disabled),
            Some(ExecMode::AskAll),
            Some(ExecMode::Allowlist),
            Some(ExecMode::AllowExceptDenylist),
        ];
        let values = if profile {
            modes.to_vec()
        } else {
            std::iter::once(None).chain(modes).collect()
        };
        let label = |mode: Option<ExecMode>| mode.map_or("Según perfil", exec_label);
        let key = values
            .iter()
            .position(|mode| *mode == current)
            .unwrap_or(0)
            .to_string();
        stack()
            .gap_1()
            .child(heading("SSH exec"))
            .child(if readonly {
                muted(label(current), theme)
            } else {
                let choices = values
                    .iter()
                    .enumerate()
                    .map(|(index, &mode)| (index.to_string(), label(mode).into()))
                    .collect();
                self.mcp_select(
                    "mcp-exec",
                    label(current),
                    choices,
                    &key,
                    theme,
                    cx,
                    move |this, choice, _, _| {
                        let value = values[choice.parse::<usize>().expect("choice index")];
                        match &mut this.mcp.dialog {
                            Some(View::Server(draft)) => draft.policy.exec_mode = value,
                            Some(View::Profile(draft)) => {
                                draft.profile.exec_mode = value.unwrap_or_default()
                            }
                            _ => {}
                        }
                    },
                )
            })
    }

    fn sudo_control(
        &self,
        current: Option<Decision>,
        profile: bool,
        readonly: bool,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Div {
        let standard = if profile {
            vec![Some(Decision::Deny), Some(Decision::Ask)]
        } else {
            vec![None, Some(Decision::Deny), Some(Decision::Ask)]
        };
        let values = standard
            .into_iter()
            .chain((current == Some(Decision::Allow)).then_some(Some(Decision::Allow)))
            .collect::<Vec<_>>();
        let label = |decision| match decision {
            None => "Según perfil",
            Some(Decision::Ask) => "Preguntar",
            Some(Decision::Deny) => "Denegar",
            Some(Decision::Allow) => "Permitir (configuración existente)",
        };
        let key = values
            .iter()
            .position(|decision| *decision == current)
            .unwrap_or(0)
            .to_string();
        stack()
            .gap_1()
            .child(heading("Elevación (sudo)"))
            .child(if readonly {
                muted(label(current), theme)
            } else {
                let choices = values
                    .iter()
                    .enumerate()
                    .map(|(index, &mode)| (index.to_string(), label(mode).into()))
                    .collect();
                self.mcp_select(
                    "mcp-sudo",
                    label(current),
                    choices,
                    &key,
                    theme,
                    cx,
                    move |this, choice, _, _| {
                        let value = values[choice.parse::<usize>().expect("choice index")];
                        match &mut this.mcp.dialog {
                            Some(View::Server(draft)) => draft.policy.sudo = value,
                            Some(View::Profile(draft)) => {
                                draft.profile.sudo = value.unwrap_or(Decision::Deny)
                            }
                            _ => {}
                        }
                    },
                )
            })
    }

    fn rule_list(
        &self,
        rule: Rule,
        own: &[String],
        inherited: &[String],
        fields: Option<&RuleInputs>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Div {
        let label = rule.label();
        let row = stack().gap_1().child(heading(label)).child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(inherited.iter().enumerate().map(|(index, value)| {
                    ellipsis((label, index), value.clone(), theme)
                        .font_family(crate::theme::monospace())
                        .max_w_full()
                        .px_1()
                        .rounded(px(5.))
                        .bg(theme.hover)
                        .aria_description("Regla heredada")
                }))
                .children(own.iter().enumerate().map(|(index, value)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .max_w_full()
                        .px_1()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            ellipsis((label, inherited.len() + index), value.clone(), theme)
                                .font_family(crate::theme::monospace())
                                .text_color(theme.text),
                        )
                        .when(fields.is_some(), |chip| {
                            chip.child(
                                small_icon_button(
                                    format!("{label}-remove-{index}"),
                                    format!("Quitar {value}"),
                                    IconName::Close,
                                    Variant::Secondary,
                                    theme,
                                    cx.listener(move |this, _, _, cx| {
                                        this.update_rules(rule, |list| {
                                            list.remove(index);
                                        });
                                        cx.notify();
                                    }),
                                )
                                .size(px(18.))
                                .focus(|style| style)
                                .focus_visible(move |style| style.border_color(theme.accent)),
                            )
                        })
                })),
        );
        let Some(fields) = fields else { return row };
        row.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(fields.fields[label].clone()))
                .child(small(
                    format!("{label}-add"),
                    "Añadir",
                    theme,
                    cx.listener(move |this, _, window, cx| this.add_mcp_rule(rule, window, cx)),
                )),
        )
        .when_some(
            fields.error.filter(|(field, _)| *field == label),
            |row, (_, error)| row.child(muted(error, theme).text_color(theme.danger)),
        )
    }

    fn update_rules(&mut self, rule: Rule, apply: impl FnOnce(&mut Vec<String>)) {
        match &mut self.mcp.dialog {
            Some(View::Server(draft)) => apply(rule.server(&mut draft.policy)),
            Some(View::Profile(draft)) => {
                if let Some(values) = rule.profile(&mut draft.profile) {
                    apply(values);
                }
            }
            _ => {}
        }
    }

    fn add_mcp_rule(&mut self, rule: Rule, window: &mut Window, cx: &mut Context<Self>) {
        let readonly =
            matches!(&self.mcp.dialog, Some(View::Profile(draft)) if self.profile_readonly(draft));
        if readonly || self.mcp.busy() {
            return;
        }
        let fields = match &mut self.mcp.dialog {
            Some(View::Server(draft)) => &mut draft.rules,
            Some(View::Profile(draft)) => &mut draft.rules,
            _ => return,
        };
        let field = fields.fields[rule.label()].clone();
        let value = field.read(cx).value().trim().to_owned();
        if value.is_empty() {
            return;
        }
        if let Err(error) = rule.validate(&value) {
            fields.error = Some((rule.label(), error));
        } else {
            fields.error = None;
            field.update(cx, |field, cx| field.set_value("", window, cx));
            self.update_rules(rule, |list| {
                if !list.contains(&value) {
                    list.push(value);
                }
            });
        }
        field.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
}
