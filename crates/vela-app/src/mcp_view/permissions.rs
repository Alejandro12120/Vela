use vela_mcp::{Decision, ServerAvailability, config::ExecMode, path::valid_scope_pattern};

use super::*;
use crate::{
    components::{radio::radio_dot, segmented::segmented},
    mcp_model::{
        availability_allowed, decision_label, exec_label, permission_save_hint, profile_usage,
        server_permission_issue, valid_notes,
    },
    workspace::settings_view::settings_group,
};

type DialogContent = (SharedString, AnyElement, String, Option<&'static str>, bool);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FolderScope {
    EntireServer,
    SelectedFolders,
    Ask,
}

impl FolderScope {
    const OPTIONS: [Self; 3] = [Self::EntireServer, Self::SelectedFolders, Self::Ask];
    const LABELS: [(&str, Option<&str>); 3] = [
        ("Todo el servidor", None),
        ("Solo carpetas", None),
        ("Preguntar", None),
    ];
}

impl ServerDraft {
    fn folder_scope(&self) -> FolderScope {
        match self.policy.allowed_remote_paths.as_slice() {
            [root] if root == "/**" => FolderScope::EntireServer,
            [] if !self.editing_folder_scope => FolderScope::Ask,
            _ => FolderScope::SelectedFolders,
        }
    }

    fn set_folder_scope(&mut self, scope: FolderScope) {
        let previous = self.folder_scope();
        self.editing_folder_scope = scope == FolderScope::SelectedFolders;
        let paths = &mut self.policy.allowed_remote_paths;
        match scope {
            FolderScope::EntireServer => *paths = vec!["/**".into()],
            FolderScope::Ask => paths.clear(),
            FolderScope::SelectedFolders if previous == FolderScope::EntireServer => paths.clear(),
            FolderScope::SelectedFolders => {}
        }
    }

    fn advanced_count(&self, cx: &App) -> usize {
        let policy = &self.policy;
        [
            policy.exec_mode.is_some(),
            policy.sudo.is_some(),
            policy.hide_server_identity.is_some(),
            optional_text(&self.notes, cx).is_some(),
        ]
        .into_iter()
        .filter(|custom| *custom)
        .count()
            + policy.allowed_commands.len()
            + policy.denied_commands.len()
            + policy.denied_remote_paths.len()
            + policy.allowed_local_paths.len()
            + policy.scoped_rules.len()
    }
}

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
            Self::RemoteAllow => "Permitidas",
            Self::RemoteDeny => "Prohibidas siempre",
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

    fn clear(&mut self, rule: Rule, window: &mut Window, cx: &mut Context<Workspace>) {
        let label = rule.label();
        self.fields[label].update(cx, |field, cx| field.set_value("", window, cx));
        if self.error.is_some_and(|(field, _)| field == label) {
            self.error = None;
        }
    }

    fn clear_command_inputs(&mut self, window: &mut Window, cx: &mut Context<Workspace>) {
        self.clear(Rule::CommandAllow, window, cx);
        self.clear(Rule::CommandDeny, window, cx);
    }
}

fn optional_text(input: &Entity<TextInput>, cx: &App) -> Option<String> {
    let value = input.read(cx).value();
    (!value.trim().is_empty()).then(|| value.to_owned())
}

const DECISIONS: [Decision; 3] = [Decision::Allow, Decision::Ask, Decision::Deny];

macro_rules! render_operations {
    ($this:expr, $profile:expr, $readonly:expr, $theme:expr, $cx:expr; $($field:ident => $label:literal),*) => {
        vec![$(permission_row($label, Some(technical(stringify!($field), $theme)),
            if $readonly {
                muted(decision_label($profile.$field), $theme).into_any_element()
            } else {
                segmented(stringify!($field), $label, &DECISIONS.map(|decision| (decision_label(decision), None)),
                    DECISIONS.iter().position(|mode| *mode == $profile.$field).unwrap(), $theme,
                    on_segment($cx, |this, index, _, cx| {
                        if let Some(View::Profile(draft)) = &mut this.mcp.dialog {
                            draft.profile.$field = DECISIONS[index];
                            cx.notify();
                        }
                    })).focus(|style| style).focus_visible(move |style| style.border_color($theme.accent)).into_any_element()
            })),*]
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
            "Escribe una nota",
            false,
        );
        self.mcp.dialog = Some(View::Server(Box::new(ServerDraft {
            id,
            initial: policy.clone(),
            editing_folder_scope: !policy.allowed_remote_paths.is_empty(),
            advanced: false,
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
        let readonly = !duplicate && !config.profiles.iter().any(|own| own.id == id);
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
            readonly,
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
            Some(View::Profile(draft)) if !draft.readonly => {
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
            && self.server_save_hint(draft, cx).is_none()
        {
            let mut policy = draft.policy.clone();
            policy.notes = optional_text(&draft.notes, cx);
            let id = draft.id.clone();
            self.save_mcp(window, cx, true, move |config| {
                config.servers.insert(id, policy);
            });
        } else if let Some(View::Profile(draft)) = &self.mcp.dialog
            && self.profile_save_hint(draft, cx).is_none()
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

    fn permissions_busy_hint(&self) -> Option<&'static str> {
        if self.mcp.conflict {
            Some("Recarga la configuración para poder guardar.")
        } else if self.mcp.saving {
            Some("Guardando cambios…")
        } else if self.mcp.loading {
            Some("Cargando mcp.toml…")
        } else if self.mcp.busy() {
            Some("Hay una operación MCP en curso.")
        } else {
            None
        }
    }

    fn server_save_hint(&self, draft: &ServerDraft, cx: &App) -> Option<&'static str> {
        let blocked = self.permissions_busy_hint().or_else(|| {
            let Some(config) = self.mcp.config() else {
                return Some("La configuración MCP no está disponible.");
            };
            let auth = self
                .servers
                .iter()
                .find(|server| server.id == draft.id)
                .map(|server| &server.auth);
            server_permission_issue(
                config,
                &draft.policy,
                auth,
                draft.notes.read(cx).value(),
                draft.initial.notes.as_deref(),
            )
        });
        permission_save_hint(
            blocked,
            !draft.rules.empty(cx),
            draft.policy != draft.initial || optional_text(&draft.notes, cx) != draft.initial.notes,
        )
    }

    fn profile_save_hint(&self, draft: &ProfileDraft, cx: &App) -> Option<&'static str> {
        let name = draft.name.read(cx).value();
        let blocked = self.permissions_busy_hint().or_else(|| {
            if self.mcp.config().is_none() {
                Some("La configuración MCP no está disponible.")
            } else if draft.readonly {
                Some("Este perfil es predefinido.")
            } else if draft.initial.is_some()
                && self.mcp.config().is_some_and(|config| {
                    !config
                        .profiles
                        .iter()
                        .any(|profile| profile.id == draft.profile.id)
                })
            {
                Some("El perfil ya no existe.")
            } else if name.trim().is_empty() || !valid_notes(name) {
                Some("Escribe un nombre para el perfil sin caracteres de control.")
            } else {
                None
            }
        });
        permission_save_hint(
            blocked,
            !draft.rules.empty(cx),
            self.mcp_has_draft_changes(cx),
        )
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
                self.server_save_hint(draft, cx)
                    .unwrap_or("Guardado en mcp.toml")
                    .into(),
                Some("Guardar"),
                false,
            );
        };
        let policy = &draft.policy;
        let profile = presets::resolve_profile(config, &policy.profile_id);
        let profile_name = profile
            .as_ref()
            .map_or("Perfil no encontrado", |profile| profile.name.as_str());
        let folders = draft.folder_scope();
        let inherited_folders = profile.as_ref().map_or(&[][..], |profile| {
            profile.default_allowed_remote_paths.as_slice()
        });
        let mut permissions = vec![
            permission_row(
                "Perfil",
                None,
                self.mcp_select(
                    "mcp-server-profile",
                    profile_name.to_owned(),
                    presets::all_profiles(config)
                        .into_iter()
                        .map(|profile| (profile.id, profile.name))
                        .collect(),
                    &policy.profile_id,
                    theme,
                    cx,
                    |this, id, window, cx| {
                        let inherited = this
                            .mcp
                            .config()
                            .and_then(|config| presets::resolve_profile(config, &id))
                            .map(|profile| profile.exec_mode);
                        if let Some(View::Server(draft)) = &mut this.mcp.dialog {
                            draft.policy.profile_id = id;
                            if draft.policy.exec_mode.or(inherited).unwrap_or_default()
                                == ExecMode::Disabled
                            {
                                draft.rules.clear_command_inputs(window, cx);
                            }
                        }
                    },
                ),
            )
            .when_some(
                profile
                    .as_ref()
                    .filter(|profile| !profile.description.is_empty()),
                |row, profile| {
                    stack()
                        .gap_0()
                        .child(row.pb_1())
                        .child(div().px_3().pb_2().child(ellipsis(
                            "mcp-profile-description",
                            profile.description.clone(),
                            theme,
                        )))
                },
            ),
            permission_row(
                "Ámbito de carpetas",
                None,
                segmented(
                    "mcp-folder-scope",
                    "Ámbito de carpetas",
                    &FolderScope::LABELS,
                    FolderScope::OPTIONS
                        .iter()
                        .position(|scope| *scope == folders)
                        .unwrap(),
                    theme,
                    on_segment(cx, |this, index, window, cx| {
                        if let Some(View::Server(draft)) = &mut this.mcp.dialog {
                            let scope = FolderScope::OPTIONS[index];
                            draft.set_folder_scope(scope);
                            if scope != FolderScope::SelectedFolders {
                                draft.rules.clear(Rule::RemoteAllow, window, cx);
                            }
                            cx.notify();
                        }
                    }),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.accent)),
            ),
        ];
        if folders == FolderScope::SelectedFolders {
            permissions.push(self.rule_list(
                Rule::RemoteAllow,
                &policy.allowed_remote_paths,
                &[],
                Some(&draft.rules),
                theme,
                cx,
            ));
        }
        if folders != FolderScope::EntireServer && !inherited_folders.is_empty() {
            permissions.push(div().px_3().py_2().child(ellipsis(
                "mcp-inherited-folders",
                format!(
                    "Carpetas heredadas del perfil · {}",
                    inherited_folders.join(" · ")
                ),
                theme,
            )));
        } else if folders == FolderScope::Ask {
            permissions.push(div().px_3().py_2().child(muted(
                "Sin carpetas permitidas: toda operación pedirá aprobación.",
                theme,
            )));
        }

        let advanced_count = draft.advanced_count(cx);
        let body = stack()
            .gap_3()
            .children(self.mcp_error(theme, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .justify_between()
                    .child(
                        ellipsis(
                            "mcp-endpoint",
                            format!("{}@{}:{}", server.user, server.host, server.port),
                            theme,
                        )
                        .font_family(crate::theme::monospace())
                        .flex_1(),
                    )
                    .child(badge(profile_name.to_owned(), Tone::Neutral, theme).max_w(px(210.))),
            )
            .child(self.availability_rows(policy.availability, &server.auth, theme, cx))
            .child(permission_group("Permisos", None, permissions, theme))
            .child(
                div()
                    .id("mcp-advanced-toggle")
                    .debug_selector(|| "mcp-advanced-toggle".to_owned())
                    .role(gpui::Role::Button)
                    .aria_expanded(draft.advanced)
                    .tab_index(0)
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(28.))
                    .px_1()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .text_size(px(13.))
                    .text_color(theme.secondary_text)
                    .cursor_pointer()
                    .hover(move |style| style.text_color(theme.text).bg(theme.hover))
                    .focus_visible(move |style| style.border_color(theme.accent))
                    .child(
                        icon(if draft.advanced {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(14.))
                        .text_color(theme.secondary_text),
                    )
                    .child(heading("Ajustes avanzados"))
                    .when(advanced_count > 0, |row| {
                        row.child(badge(advanced_count.to_string(), Tone::Neutral, theme))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(View::Server(draft)) = &mut this.mcp.dialog {
                            draft.advanced = !draft.advanced;
                            cx.notify();
                        }
                    })),
            )
            .when(draft.advanced, |body| {
                body.child(self.server_advanced(draft, profile.as_ref(), config, theme, cx))
            });
        let hint = self.server_save_hint(draft, cx);
        (
            title,
            body.into_any_element(),
            hint.unwrap_or("Guardado en mcp.toml").into(),
            Some("Guardar"),
            hint.is_none(),
        )
    }

    fn server_advanced(
        &self,
        draft: &ServerDraft,
        profile: Option<&PermissionProfile>,
        config: &McpConfig,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Div {
        let policy = &draft.policy;
        let inherited_exec = profile.map(|profile| profile.exec_mode);
        let mut commands = vec![
            self.exec_control((policy.exec_mode, inherited_exec), false, false, theme, cx),
            self.sudo_control(
                (policy.sudo, profile.map(|profile| profile.sudo)),
                false,
                false,
                theme,
                cx,
            ),
        ];
        if policy.exec_mode.or(inherited_exec).unwrap_or_default() != ExecMode::Disabled {
            for (rule, own, inherited) in [
                (
                    Rule::CommandAllow,
                    &policy.allowed_commands,
                    profile.map(|profile| &profile.allowed_commands),
                ),
                (
                    Rule::CommandDeny,
                    &policy.denied_commands,
                    profile.map(|profile| &profile.denied_commands),
                ),
            ] {
                commands.push(
                    self.rule_list(rule, own, &[], Some(&draft.rules), theme, cx)
                        .when_some(inherited.filter(|rules| !rules.is_empty()), |row, rules| {
                            row.child(ellipsis(
                                format!("{}-inherited", rule.label()),
                                format!("Del perfil · {}", rules.join(" · ")),
                                theme,
                            ))
                        }),
                );
            }
        }

        let mut paths = vec![div().px_3().py_2().child(muted(
            "Protegidas siempre: .env, .ssh, claves privadas y /etc/shadow.",
            theme,
        ))];
        if let Some(profile) = profile {
            let inherited = profile
                .default_denied_remote_paths
                .iter()
                .filter(|path| !presets::CRITICAL_DENIED_PATHS.contains(&path.as_str()))
                .map(String::as_str)
                .collect::<Vec<_>>();
            if !inherited.is_empty() {
                paths.push(div().px_3().py_2().child(ellipsis(
                    "mcp-inherited-denied-paths",
                    format!("Prohibidas por el perfil · {}", inherited.join(" · ")),
                    theme,
                )));
            }
        }
        paths.extend([
            self.rule_list(
                Rule::RemoteDeny,
                &policy.denied_remote_paths,
                &[],
                Some(&draft.rules),
                theme,
                cx,
            ),
            self.rule_list(
                Rule::LocalAllow,
                &policy.allowed_local_paths,
                &[],
                Some(&draft.rules),
                theme,
                cx,
            ),
        ]);

        stack()
            .gap_3()
            .child(permission_group("Comandos", None, commands, theme))
            .child(permission_group(
                "Rutas protegidas y locales",
                None,
                paths,
                theme,
            ))
            .child(permission_group(
                "Privacidad y notas",
                None,
                vec![
                    permission_row(
                        "Ocultar host y usuario",
                        Some(muted(
                            format!(
                                "Ajuste global {}.",
                                if config.hide_server_identity {
                                    "activo"
                                } else {
                                    "inactivo"
                                }
                            ),
                            theme,
                        )),
                        segmented(
                            "mcp-server-hide",
                            "Ocultar host y usuario",
                            &[("Heredar", None), ("Sí", None), ("No", None)],
                            match policy.hide_server_identity {
                                None => 0,
                                Some(true) => 1,
                                Some(false) => 2,
                            },
                            theme,
                            on_segment(cx, |this, index, _, cx| {
                                if let Some(View::Server(draft)) = &mut this.mcp.dialog {
                                    draft.policy.hide_server_identity =
                                        [None, Some(true), Some(false)][index];
                                    cx.notify();
                                }
                            }),
                        )
                        .focus(|style| style)
                        .focus_visible(move |style| style.border_color(theme.accent)),
                    ),
                    permission_row(
                        "Notas para el agente",
                        Some(technical_sentence(
                            "Contexto breve visible en",
                            "list_servers",
                            ".",
                            theme,
                        )),
                        draft.notes.clone(),
                    ),
                ],
                theme,
            ))
            .when(!policy.scoped_rules.is_empty(), |body| {
                let rows = policy
                    .scoped_rules
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| {
                        let label = rule
                            .scope
                            .strip_prefix("exact:")
                            .and_then(|scope| {
                                serde_json::from_str::<(
                                        Vec<String>,
                                        Option<String>,
                                        Option<String>,
                                    )>(scope)
                                    .ok()
                            })
                            .map_or_else(
                                || rule.scope.clone(),
                                |(paths, command, parameter)| {
                                    command.unwrap_or_else(|| {
                                        paths
                                            .into_iter()
                                            .chain(parameter)
                                            .collect::<Vec<_>>()
                                            .join(" · ")
                                    })
                                },
                            );
                        permission_row(
                            rule.operation.clone(),
                            Some(technical(label, theme)),
                            small_button(
                                ("mcp-remove-scoped", index),
                                "Quitar",
                                Variant::Secondary,
                                theme,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(View::Server(draft)) = &mut this.mcp.dialog
                                        && index < draft.policy.scoped_rules.len()
                                    {
                                        draft.policy.scoped_rules.remove(index);
                                        cx.notify();
                                    }
                                }),
                            ),
                        )
                    })
                    .collect();
                body.child(permission_group(
                    "Reglas guardadas en aprobaciones",
                    Some("Cada regla se aplica a una operación y un destino concretos."),
                    rows,
                    theme,
                ))
            })
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
            (Disabled, "Desactivado", "No aparece en"),
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
                    .child(stack().min_w_0().flex_1().gap_0().child(heading(label)).child(if !enabled {
                        muted(
                            "Requiere autenticación con clave SSH. Este servidor usa contraseña.",
                            theme,
                        )
                    } else if mode == Disabled {
                        technical_sentence(description, "list_servers", ".", theme)
                    } else {
                        muted(description, theme)
                    }));
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
            .child(heading("Acceso"))
            .child(settings_group(theme, rows))
    }

    pub(super) fn profile_dialog(
        &self,
        draft: &ProfileDraft,
        theme: Theme,
        cx: &Context<Self>,
    ) -> DialogContent {
        let readonly = draft.readonly;
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
        let mut commands = vec![
            self.exec_control((Some(profile.exec_mode), None), true, readonly, theme, cx),
            self.sudo_control((Some(profile.sudo), None), true, readonly, theme, cx),
            self.rule_list(
                Rule::CommandAllow,
                &profile.allowed_commands,
                &[],
                rules,
                theme,
                cx,
            ),
            self.rule_list(
                Rule::CommandDeny,
                &profile.denied_commands,
                &[],
                rules,
                theme,
                cx,
            ),
        ];
        commands.extend(render_operations!(self, profile, readonly, theme, cx;
            transfer_upload => "Subir archivos", transfer_download => "Descargar archivos"
        ));
        let body = stack()
            .gap_3()
            .children(self.mcp_error(theme, cx))
            .when(readonly, |body| {
                body.child(badge("Predefinido", Tone::Neutral, theme).self_start())
            })
            .when(!readonly, |body| {
                body.child(
                    stack()
                        .gap_1()
                        .child(heading("Nombre del perfil"))
                        .child(draft.name.clone()),
                )
            })
            .child(permission_group(
                "Operaciones de archivos",
                None,
                render_operations!(self, profile, readonly, theme, cx;
                    fs_list => "Listar", fs_stat => "Ver atributos", fs_read => "Leer",
                    fs_write => "Escribir", fs_patch => "Editar", fs_mkdir => "Crear carpetas",
                    fs_move => "Mover", fs_delete => "Borrar", fs_chmod => "Cambiar permisos"
                ),
                theme,
            ))
            .child(permission_group(
                "Rutas remotas",
                None,
                vec![
                    self.rule_list(
                        Rule::RemoteAllow,
                        &profile.default_allowed_remote_paths,
                        &[],
                        rules,
                        theme,
                        cx,
                    ),
                    self.rule_list(
                        Rule::RemoteDeny,
                        &profile.default_denied_remote_paths,
                        &presets::critical_denied_paths(),
                        rules,
                        theme,
                        cx,
                    ),
                ],
                theme,
            ))
            .child(permission_group(
                "Comandos y transferencias",
                None,
                commands,
                theme,
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
        let hint = self.profile_save_hint(draft, cx);
        (
            title.into(),
            body.into_any_element(),
            if readonly {
                String::new()
            } else {
                hint.unwrap_or("Guardado en mcp.toml").into()
            },
            (!readonly).then_some("Guardar"),
            hint.is_none(),
        )
    }

    fn exec_control(
        &self,
        (current, inherited): (Option<ExecMode>, Option<ExecMode>),
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
        let effective = current.or(inherited);
        let value = if current.is_none() && inherited.is_some() {
            format!("Según perfil · {}", label(effective))
        } else {
            label(current).to_owned()
        };
        let control = if readonly {
            muted(value, theme)
        } else {
            let choices = values
                .iter()
                .enumerate()
                .map(|(index, &mode)| (index.to_string(), label(mode).into()))
                .collect();
            self.mcp_select(
                "mcp-exec",
                value,
                choices,
                &key,
                theme,
                cx,
                move |this, choice, window, cx| {
                    let value = values[choice.parse::<usize>().expect("choice index")];
                    match &mut this.mcp.dialog {
                        Some(View::Server(draft)) => {
                            draft.policy.exec_mode = value;
                            if value.or(inherited).unwrap_or_default() == ExecMode::Disabled {
                                draft.rules.clear_command_inputs(window, cx);
                            }
                        }
                        Some(View::Profile(draft)) => {
                            draft.profile.exec_mode = value.unwrap_or_default()
                        }
                        _ => {}
                    }
                },
            )
        };
        permission_row("SSH exec", None, control)
    }

    fn sudo_control(
        &self,
        (current, inherited): (Option<Decision>, Option<Decision>),
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
        let effective = current.or(inherited);
        let value = if current.is_none() && inherited.is_some() {
            format!("Según perfil · {}", label(effective))
        } else {
            label(current).to_owned()
        };
        let control = if readonly {
            muted(value, theme)
        } else {
            let choices = values
                .iter()
                .enumerate()
                .map(|(index, &mode)| (index.to_string(), label(mode).into()))
                .collect();
            self.mcp_select(
                "mcp-sudo",
                value,
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
        };
        permission_row("Elevación (sudo)", None, control)
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
        let row = stack()
            .px_3()
            .py_2()
            .gap_1()
            .child(div().text_size(px(13.)).child(label))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(inherited.iter().enumerate().map(|(index, value)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1()
                            .min_w_0()
                            .max_w_full()
                            .rounded(px(5.))
                            .bg(theme.hover)
                            .child(
                                icon(IconName::Lock)
                                    .size(px(11.))
                                    .text_color(theme.secondary_text),
                            )
                            .child(
                                ellipsis((label, index), value.clone(), theme)
                                    .font_family(crate::theme::monospace())
                                    .aria_description("Regla heredada; no se puede quitar aquí."),
                            )
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
                            .border_color(theme.control_border)
                            .bg(theme.selection)
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
                                    .border_color(gpui::transparent_black())
                                    .bg(gpui::transparent_black())
                                    .text_color(theme.secondary_text)
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
                .child(
                    small(
                        format!("{label}-add"),
                        "Añadir",
                        theme,
                        cx.listener(move |this, _, window, cx| this.add_mcp_rule(rule, window, cx)),
                    )
                    .debug_selector(move || format!("{label}-add")),
                ),
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
        let readonly = matches!(&self.mcp.dialog, Some(View::Profile(draft)) if draft.readonly);
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

fn permission_row(
    label: impl Into<SharedString>,
    description: Option<Div>,
    control: impl IntoElement,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .child(
            stack()
                .w(gpui::relative(0.38))
                .flex_none()
                .gap_0()
                .child(div().text_size(px(13.)).child(label.into()))
                .children(description),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .justify_end()
                .child(control),
        )
}

fn permission_group(
    title: &'static str,
    detail: Option<&'static str>,
    rows: Vec<Div>,
    theme: Theme,
) -> Div {
    stack()
        .pt_1()
        .gap_2()
        .child(
            stack()
                .gap_0()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(detail.map(|detail| muted(detail, theme))),
        )
        .child(settings_group(theme, rows))
}
