use vela_mcp::{EvalContext, PolicyEngine, config::HeadlessAskBehavior};

use super::*;
use crate::{
    components::{segmented::segmented, switch::switch_with_state},
    mcp_model::{decision_label, decision_tone, profile_summary, profile_usage, rollback_limits},
    workspace::settings_view::{section, settings_group, settings_row, subtitle},
};

const MIB: u64 = 1024 * 1024;

impl Workspace {
    pub(in crate::workspace) fn mcp_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let summary = if self.mcp.scanning {
            "Detectando…".to_owned()
        } else if let Some(scan) = &self.mcp.scan {
            scan.summary()
        } else {
            "Detección no disponible".into()
        };
        let connect = settings_row(
            "Harnesses instalados",
            Some(summary.into()),
            small_button(
                "connect-agents",
                "Conectar agentes…",
                Variant::Primary,
                theme,
                cx.listener(|this, _, window, cx| this.open_mcp_harnesses(window, cx)),
            )
            .focus(|style| style)
            .focus_visible(move |style| style.border_color(theme.accent)),
            theme,
        );
        let mut panel = section("Agentes (MCP)").children(self.mcp_error(theme, cx));
        let Some(config) = self.mcp.config() else {
            return panel
                .child(settings_group(theme, vec![connect]))
                .when(self.mcp.loading, |panel| {
                    panel.child(muted("Cargando mcp.toml…", theme))
                });
        };
        let enabled = config.enabled;
        let busy = self.mcp.busy() || self.mcp.conflict;
        let unavailable = busy.then_some("No disponible");
        let top = settings_row(
            "Servidor MCP local",
            Some("Permite a agentes acceder a servidores autorizados sin entregar nunca claves ni contraseñas.".into()),
            switch_with_state("mcp-enabled", "Servidor MCP local", enabled, theme,
                (!busy).then_some(cx.listener(move |this, _, window, cx| this.save_mcp(window, cx, false, |config| config.enabled = !enabled))))
                .focus(|style| style).focus_visible(move |style| style.border_color(theme.text)),
            theme,
        );
        let hidden = config.hide_server_identity;
        let share = config.share_ui_context;
        let timeout = config.approval_timeout_secs;
        let timing = [60, 180, 300];
        let timeout_index = timing
            .iter()
            .position(|&seconds| seconds == timeout)
            .unwrap_or(timing.len());
        let details = [
            connect,
            settings_row(
                "Con Vela cerrada (headless)",
                Some("Qué hacer con las acciones que requieren aprobación.".into()),
                segmented("mcp-headless", "Con Vela cerrada", &[("Denegar", unavailable), ("Abrir Vela", unavailable)],
                    usize::from(config.headless_ask == HeadlessAskBehavior::OpenVela), theme,
                    on_segment(cx, |this, index, window, cx| this.save_mcp(window, cx, false, |config| {
                        config.headless_ask = if index == 0 { HeadlessAskBehavior::Deny } else { HeadlessAskBehavior::OpenVela };
                    }))).focus(|style| style).focus_visible(move |style| style.border_color(theme.accent)),
                theme,
            ),
            settings_row(
                "Ocultar dirección y usuario a los agentes",
                Some("El agente ve el ID, el alias y las notas de los servidores autorizados.".into()),
                switch_with_state("mcp-hide", "Ocultar dirección y usuario", hidden, theme,
                    (!busy).then_some(cx.listener(move |this, _, window, cx| this.save_mcp(window, cx, false, |config| config.hide_server_identity = !hidden))))
                    .focus(|style| style).focus_visible(move |style| style.border_color(theme.text)),
                theme,
            ),
            settings_row(
                "Compartir carpeta abierta",
                Some("Comparte con el agente el servidor y la carpeta que tienes abiertos en Vela. Estará disponible con el puente de la ventana (M6).".into()),
                switch_with_state("mcp-context", "Compartir carpeta abierta", share, theme,
                    (!busy).then_some(cx.listener(move |this, _, window, cx| this.save_mcp(window, cx, false, |config| config.share_ui_context = !share))))
                    .focus(|style| style).focus_visible(move |style| style.border_color(theme.text)),
                theme,
            ),
            settings_row(
                "Espera máxima de aprobación",
                Some(if timeout_index < timing.len() { "Al agotarse la espera, se deniega la acción.".into() }
                    else { format!("Ajuste actual: {timeout} s. Al agotarse la espera, se deniega la acción.").into() }),
                segmented("mcp-timeout", "Espera máxima de aprobación", &[("1 min", unavailable), ("3 min", unavailable), ("5 min", unavailable)],
                    timeout_index, theme, on_segment(cx, move |this, index, window, cx|
                        this.save_mcp(window, cx, false, |config| config.approval_timeout_secs = timing[index])))
                    .focus(|style| style).focus_visible(move |style| style.border_color(theme.accent)),
                theme,
            ),
        ];
        let socket = ellipsis(
            "mcp-socket",
            format!("{} · stdio", vela_mcp::ipc::socket_path(None).display()),
            theme,
        )
        .font_family(crate::theme::monospace());
        let rows = [top, div().px(px(14.)).py_2().child(socket)]
            .into_iter()
            .chain(details)
            .collect();
        panel = panel
            .child(settings_group(theme, rows))
            .when(busy, |panel| {
                panel.child(muted(
                    if self.mcp.conflict {
                        "Recarga la configuración para poder editarla."
                    } else if self.mcp.loading {
                        "Cargando mcp.toml…"
                    } else if self.mcp.saving {
                        "Guardando cambios…"
                    } else {
                        "Hay una operación MCP en curso."
                    },
                    theme,
                ))
            });
        panel
            .child(self.profiles_section(config, theme, cx))
            .child(self.rollback_section(config, theme, cx))
    }

    fn profiles_section(&self, config: &McpConfig, theme: Theme, cx: &Context<Self>) -> Div {
        let mut rows = Vec::new();
        for profile in presets::all_profiles(config) {
            let readonly = !config
                .profiles
                .iter()
                .any(|candidate| candidate.id == profile.id);
            let count = profile_usage(config, &profile.id);
            let profile_id = profile.id.clone();
            rows.push(
                div()
                    .px(px(14.))
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        stack()
                            .gap_0()
                            .flex_1()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_1()
                                    .child(heading(profile.name.clone()))
                                    .child(badge(
                                        if readonly {
                                            "Predefinido".into()
                                        } else if count == 1 {
                                            "1 servidor".into()
                                        } else {
                                            format!("{count} servidores")
                                        },
                                        Tone::Neutral,
                                        theme,
                                    )),
                            )
                            .child(muted(profile_summary(&profile), theme)),
                    )
                    .child(small(
                        format!("edit-mcp-profile-{profile_id}"),
                        if readonly { "Ver" } else { "Editar" },
                        theme,
                        cx.listener(move |this, _, window, cx| {
                            this.open_mcp_profile(profile_id.clone(), false, window, cx)
                        }),
                    )),
            );
        }
        rows.push(self.simulator_row(config, theme, cx));
        stack()
            .pt_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(subtitle("Perfiles de permisos"))
                    .child(small(
                        "new-mcp-profile",
                        "Nuevo perfil…",
                        theme,
                        cx.listener(|this, _, window, cx| {
                            if !this.mcp.is_open() {
                                this.mcp.dialog = Some(View::ChoosePreset);
                                this.start_mcp(window, cx);
                            }
                        }),
                    )),
            )
            .child(settings_group(theme, rows))
    }

    fn simulator_row(&self, config: &McpConfig, theme: Theme, cx: &Context<Self>) -> Div {
        let as_key = |target: &Target| match target {
            Target::Profile(id) => format!("p:{id}"),
            Target::Server(id) => format!("s:{id}"),
        };
        let choices: Vec<_> = presets::all_profiles(config)
            .into_iter()
            .map(|profile| {
                (
                    format!("p:{}", profile.id),
                    format!("Perfil · {}", profile.name),
                )
            })
            .chain(self.servers.iter().map(|server| {
                (
                    format!("s:{}", server.id),
                    format!("Servidor · {}", server.display_name()),
                )
            }))
            .collect();
        let key = as_key(&self.mcp.simulate_target);
        let label = choices
            .iter()
            .find(|(id, _)| *id == key)
            .map_or("Elegir perfil o servidor", |(_, label)| label.as_str());
        let mut row = stack()
            .px(px(14.))
            .py_2()
            .child(subtitle("Simulador de reglas"))
            .child(self.mcp_select(
                "mcp-sim-target",
                label.to_owned(),
                choices,
                &key,
                theme,
                cx,
                |this, key, _, _| {
                    if let Some((kind, id)) = key.split_once(':') {
                        this.mcp.simulate_target = if kind == "s" {
                            Target::Server(id.into())
                        } else {
                            Target::Profile(id.into())
                        };
                        this.mcp.simulation = None;
                    }
                },
            ));
        if let Some(input) = &self.mcp.simulator {
            row = row.child(
                div()
                    .id("mcp-sim-input")
                    .flex()
                    .gap_2()
                    .items_center()
                    .key_context("McpSimulator")
                    .on_action(cx.listener(|this, _: &Simulate, _, cx| this.run_simulation(cx)))
                    .child(div().flex_1().min_w_0().child(input.clone()))
                    .child(small(
                        "mcp-simulate",
                        "Probar",
                        theme,
                        cx.listener(|this, _, _, cx| this.run_simulation(cx)),
                    )),
            );
        }
        match &self.mcp.simulation {
            Some(Ok((profile, decision))) => row.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        stack()
                            .flex_1()
                            .min_w_0()
                            .gap_0()
                            .child(muted(format!("Perfil: {profile}"), theme))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_x_1()
                                    .child(muted("Regla aplicada:", theme))
                                    .child(technical(decision.matched_rule.clone(), theme)),
                            ),
                    )
                    .child(badge(
                        decision_label(decision.outcome),
                        decision_tone(decision.outcome),
                        theme,
                    )),
            ),
            Some(Err(error)) => row.child(muted(error.clone(), theme).text_color(theme.danger)),
            None => row,
        }
    }

    fn run_simulation(&mut self, cx: &mut Context<Self>) {
        let (Some(config), Some(input)) = (self.mcp.config(), &self.mcp.simulator) else {
            return;
        };
        let (target, profile, server) = match &self.mcp.simulate_target {
            Target::Profile(id) => (id, id.as_str(), false),
            Target::Server(id) => (
                id,
                config
                    .servers
                    .get(id)
                    .map_or("readonly", |policy| policy.profile_id.as_str()),
                true,
            ),
        };
        let Some(profile) = presets::resolve_profile(config, profile) else {
            self.mcp.simulation = Some(Err("El perfil seleccionado ya no existe.".into()));
            cx.notify();
            return;
        };
        let context = EvalContext {
            home_dir: std::env::var_os("HOME").map(PathBuf::from),
            vela_config_dir: self.dir.clone(),
            ..EvalContext::default()
        };
        let decision = PolicyEngine::new(config.clone()).simulate_target(
            target,
            server,
            input.read(cx).value(),
            &context,
        );
        self.mcp.simulation = Some(if decision.matched_rule == "unknown_simulated_input" {
            Err("Formato esperado: fs_read /var/log/nginx/error.log o exec sudo nginx -t".into())
        } else {
            Ok((profile.name, decision))
        });
        cx.notify();
    }

    fn rollback_section(&self, config: &McpConfig, theme: Theme, cx: &Context<Self>) -> Div {
        let bytes = |count| format!("{:.1}", count as f64 / MIB as f64).replace('.', ",");
        let limits = &config.rollback;
        let enabled = limits.enabled;
        let busy = self.mcp.busy() || self.mcp.conflict;
        let usage = self
            .mcp
            .usage
            .as_ref()
            .and_then(|usage| usage.as_ref().ok());
        let total = usage.map_or(0, |usage| usage.total_bytes);
        let percent = (100. * total as f32 / limits.max_total_bytes.max(1) as f32).min(100.);
        let gauge = div()
            .flex()
            .items_center()
            .gap_2()
            .px(px(14.))
            .pb_2()
            .child(
                div()
                    .flex_1()
                    .h(px(4.))
                    .rounded_full()
                    .bg(theme.control_border)
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(percent / 100.))
                            .rounded_full()
                            .bg(theme.accent),
                    ),
            )
            .child(muted(
                usage.map_or_else(
                    || "Calculando…".into(),
                    |usage| {
                        let copies = if usage.count == 1 {
                            "1 copia guardada".into()
                        } else {
                            format!("{} copias guardadas", usage.count)
                        };
                        format!(
                            "{copies} · {} MB de {} MB",
                            bytes(total),
                            bytes(limits.max_total_bytes)
                        )
                    },
                ),
                theme,
            ));
        let actions = div()
            .flex()
            .gap_1()
            .px(px(14.))
            .pb_2()
            .child(small(
                "mcp-backup-limits",
                "Límites…",
                theme,
                cx.listener(|this, _, window, cx| this.open_rollback_limits(window, cx)),
            ))
            .child(small(
                "mcp-backup-open",
                "Mostrar",
                theme,
                cx.listener(|this, _, window, cx| this.rollback_action(false, window, cx)),
            ))
            .child(
                small(
                    "mcp-backup-clear",
                    "Vaciar…",
                    theme,
                    cx.listener(|this, _, window, cx| {
                        this.mcp.confirmation = Some(Confirmation::Clear);
                        this.start_mcp(window, cx);
                    }),
                )
                .text_color(theme.danger),
            );
        let rows = vec![
            settings_row(
                "Respaldo automático antes de editar o borrar",
                Some(
                    format!(
                        "Máx. {} MB por archivo · Cuota {} MB · Caducidad {} h",
                        bytes(limits.max_file_bytes),
                        bytes(limits.max_total_bytes),
                        limits.retention_hours
                    )
                    .into(),
                ),
                switch_with_state(
                    "mcp-backup",
                    "Respaldo automático",
                    enabled,
                    theme,
                    (!busy).then_some(cx.listener(move |this, _, window, cx| {
                        this.save_mcp(window, cx, false, |config| {
                            config.rollback.enabled = !enabled
                        });
                    })),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.text)),
                theme,
            )
            .pb_2(),
            stack().child(gauge).child(actions).children(
                self.mcp
                    .usage
                    .as_ref()
                    .and_then(|result| result.as_ref().err())
                    .map(|error| {
                        muted(error.clone(), theme)
                            .px_3()
                            .pb_2()
                            .text_color(theme.danger)
                    }),
            ),
        ];
        stack()
            .pt_4()
            .child(subtitle("Copias de rollback"))
            .child(settings_group(theme, rows))
    }

    fn open_rollback_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.mcp.config() else {
            return;
        };
        let [file, total, hours] = [
            (config.rollback.max_file_bytes / MIB).to_string(),
            (config.rollback.max_total_bytes / MIB).to_string(),
            config.rollback.retention_hours.to_string(),
        ];
        self.mcp.dialog = Some(View::Limits([
            input(cx, "Máximo por archivo (MB)", &file, "25", false),
            input(cx, "Cuota total (MB)", &total, "100", false),
            input(cx, "Caducidad (horas)", &hours, "24", false),
        ]));
        self.start_mcp(window, cx);
    }

    pub(super) fn limits_dialog(
        &self,
        fields: &[Entity<TextInput>; 3],
        theme: Theme,
        cx: &Context<Self>,
    ) -> (SharedString, AnyElement, String, Option<&'static str>, bool) {
        let valid = self.current_limits(fields, cx).is_some();
        let body = stack()
            .children(
                [
                    "Máximo por archivo (MB)",
                    "Cuota total (MB)",
                    "Caducidad (horas)",
                ]
                .into_iter()
                .zip(fields)
                .map(|(label, field)| stack().gap_1().child(heading(label)).child(field.clone())),
            )
            .when(!valid, |body| {
                body.child(
                    muted(
                        "Usa enteros positivos. El límite por archivo debe caber en la cuota.",
                        theme,
                    )
                    .text_color(theme.danger),
                )
            })
            .children(self.mcp_error(theme, cx));
        (
            "Límites de rollback".into(),
            body.into_any_element(),
            String::new(),
            Some("Guardar"),
            valid && !self.mcp.busy() && !self.mcp.conflict,
        )
    }

    fn current_limits(
        &self,
        fields: &[Entity<TextInput>; 3],
        cx: &Context<Self>,
    ) -> Option<vela_mcp::config::RollbackConfig> {
        let [file, total, hours] = fields.each_ref().map(|field| field.read(cx).value());
        rollback_limits(file, total, hours, self.mcp.config()?.rollback.enabled)
    }

    pub(super) fn save_rollback_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(View::Limits(fields)) = &self.mcp.dialog
            && let Some(limits) = self.current_limits(fields, cx)
        {
            self.save_mcp(window, cx, true, |config| config.rollback = limits);
        }
    }

    pub(super) fn rollback_action(
        &mut self,
        clear: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(dir), Some(config)) = (&self.dir, self.mcp.config()) else {
            return;
        };
        let path = dir.join(ROLLBACK_DIR_NAME);
        let config = config.rollback.clone();
        if clear {
            self.finish_mcp(window, cx);
        }
        let task = cx.background_executor().spawn(async move {
            let store = RollbackStore::new(path, config)?;
            if clear {
                store.clear().map(|_| ())
            } else {
                crate::dev::open_default(store.dir())
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = result {
                    this.mcp.usage =
                        Some(Err(format!("No se pudo completar la operación. {error}")));
                    cx.notify();
                } else {
                    this.refresh_mcp_usage(window, cx);
                }
            });
        })
        .detach();
    }
}

gpui::actions!(mcp_settings, [Simulate, PageUp, PageDown]);

pub(in crate::workspace) fn bind(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("enter", Simulate, Some("McpSimulator > TextInput")),
        KeyBinding::new("enter", modal::Confirm, Some("McpDialog > TextInput")),
        KeyBinding::new("tab", modal::Next, Some("McpDialog")),
        KeyBinding::new("shift-tab", modal::Previous, Some("McpDialog")),
        KeyBinding::new("escape", modal::Cancel, Some("McpDialog")),
        KeyBinding::new("pageup", PageUp, Some("McpDialog")),
        KeyBinding::new("pagedown", PageDown, Some("McpDialog")),
    ]);
}
