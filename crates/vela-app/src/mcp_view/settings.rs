use vela_mcp::{EvalContext, PolicyEngine, config::HeadlessAskBehavior};

use super::*;
use crate::{
    components::{segmented::segmented, switch::switch_with_state},
    file_metadata::format_size,
    mcp_model::{decision_label, profile_summary, profile_usage, rollback_limits, simulation_tone},
    workspace::settings_view::{section, settings_group, settings_row, subtitle},
};

const MIB: u64 = 1024 * 1024;

impl Workspace {
    pub(in crate::workspace) fn mcp_section(&self, theme: Theme, cx: &Context<Self>) -> Div {
        let summary = if self.mcp.scanning {
            "Checking for agents…".into()
        } else if self.mcp.scan_error.is_some() {
            "Couldn’t check for agents".into()
        } else if let Some(scan) = &self.mcp.scan {
            scan.connected_summary()
        } else {
            "No agents configured".into()
        };
        let connect = settings_row(
            "Connected Agents",
            Some(summary.into()),
            small_button(
                "connect-agents",
                "Connect Agents…",
                Variant::Primary,
                theme,
                cx.listener(|this, _, window, cx| this.open_mcp_harnesses(window, cx)),
            )
            .focus(|style| style)
            .focus_visible(move |style| style.border_color(theme.accent)),
            theme,
        );
        let mut panel = section("MCP").children(self.mcp_error(theme, cx));
        let Some(config) = self.mcp.config() else {
            return panel
                .child(settings_group(theme, vec![connect]))
                .when(self.mcp.loading, |panel| {
                    panel.child(muted("Loading mcp.toml…", theme))
                });
        };
        let enabled = config.enabled;
        let busy = self.mcp.busy() || self.mcp.conflict;
        let unavailable = busy.then_some("Unavailable");
        let top = settings_row(
            "Local MCP Server",
            Some(
                "Allow AI agents to work on authorized servers without sharing keys or passwords."
                    .into(),
            ),
            switch_with_state(
                "mcp-enabled",
                "Local MCP Server",
                enabled,
                theme,
                (!busy).then_some(cx.listener(move |this, _, window, cx| {
                    this.save_mcp(window, cx, false, |config| config.enabled = !enabled)
                })),
            )
            .focus(|style| style)
            .focus_visible(move |style| style.border_color(theme.text)),
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
        let details = vec![
            settings_row(
                "When Vela Is Closed",
                Some("Choose how to handle requests that require approval.".into()),
                segmented(
                    "mcp-headless",
                    "When Vela Is Closed",
                    &[("Deny", unavailable), ("Open Vela", unavailable)],
                    usize::from(config.headless_ask == HeadlessAskBehavior::OpenVela),
                    theme,
                    on_segment(cx, |this, index, window, cx| {
                        this.save_mcp(window, cx, false, |config| {
                            config.headless_ask = if index == 0 {
                                HeadlessAskBehavior::Deny
                            } else {
                                HeadlessAskBehavior::OpenVela
                            };
                        })
                    }),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.accent)),
                theme,
            ),
            settings_row(
                "Approval Timeout",
                Some(if timeout_index < timing.len() {
                    "Deny requests when the time runs out.".into()
                } else {
                    format!(
                        "Currently {timeout} seconds. Requests are denied when the time runs out."
                    )
                    .into()
                }),
                segmented(
                    "mcp-timeout",
                    "Approval Timeout",
                    &[
                        ("1 min", unavailable),
                        ("3 min", unavailable),
                        ("5 min", unavailable),
                    ],
                    timeout_index,
                    theme,
                    on_segment(cx, move |this, index, window, cx| {
                        this.save_mcp(window, cx, false, |config| {
                            config.approval_timeout_secs = timing[index]
                        })
                    }),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.accent)),
                theme,
            ),
            settings_row(
                "Hide Server Addresses",
                Some("Show agents only server IDs, aliases, and notes.".into()),
                switch_with_state(
                    "mcp-hide",
                    "Hide Server Addresses",
                    hidden,
                    theme,
                    (!busy).then_some(cx.listener(move |this, _, window, cx| {
                        this.save_mcp(window, cx, false, |config| {
                            config.hide_server_identity = !hidden
                        })
                    })),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.text)),
                theme,
            ),
            settings_row(
                "Share Active Folder",
                Some("Share the currently open server and folder with agents.".into()),
                switch_with_state(
                    "mcp-context",
                    "Share Active Folder",
                    share,
                    theme,
                    (!busy).then_some(cx.listener(move |this, _, window, cx| {
                        this.save_mcp(window, cx, false, |config| config.share_ui_context = !share)
                    })),
                )
                .focus(|style| style)
                .focus_visible(move |style| style.border_color(theme.text)),
                theme,
            ),
        ];
        let dimmed = |element: Div| element.when(!enabled, |element| element.opacity(0.88));
        panel = panel
            .child(settings_group(theme, vec![top, connect]))
            .when(busy, |panel| {
                panel.child(muted(
                    if self.mcp.conflict {
                        "Reload the configuration to make changes."
                    } else if self.mcp.loading {
                        "Loading mcp.toml…"
                    } else if self.mcp.saving {
                        "Saving changes…"
                    } else {
                        "An MCP operation is in progress."
                    },
                    theme,
                ))
            });
        panel
            .child(dimmed(
                stack()
                    .pt_3()
                    .child(subtitle("Behavior & Privacy"))
                    .child(settings_group(theme, details)),
            ))
            .child(dimmed(self.profiles_section(config, theme, cx)))
            .child(dimmed(self.advanced_section(config, theme, cx)))
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
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_1()
                                    .child(heading(profile.name.clone()).min_w_0().truncate())
                                    .child(badge(
                                        if readonly {
                                            "Built-in".into()
                                        } else if count == 1 {
                                            "1 server".into()
                                        } else {
                                            format!("{count} servers")
                                        },
                                        Tone::Neutral,
                                        theme,
                                    )),
                            )
                            .child(ellipsis(
                                format!("mcp-profile-description-{profile_id}"),
                                if profile.description.is_empty() {
                                    profile_summary(&profile).join(" · ")
                                } else {
                                    profile.description.clone()
                                },
                                theme,
                            )),
                    )
                    .child(small(
                        format!("edit-mcp-profile-{profile_id}"),
                        if readonly { "View" } else { "Edit" },
                        theme,
                        cx.listener(move |this, _, window, cx| {
                            this.open_mcp_profile(profile_id.clone(), false, window, cx)
                        }),
                    )),
            );
        }
        let expanded = self.mcp.simulator_expanded;
        rows.push(
            div()
                .child(disclosure(
                    "mcp-test-rule",
                    "Test a Rule",
                    None,
                    expanded,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.mcp.simulator_expanded = !this.mcp.simulator_expanded;
                        if !this.mcp.simulator_expanded && this.mcp.select == Some("mcp-sim-target")
                        {
                            this.clear_mcp_select();
                        }
                        cx.notify();
                    }),
                ))
                .when(expanded, |row| {
                    row.child(self.simulator_row(config, theme, cx))
                }),
        );
        stack()
            .pt_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(subtitle("Permission Profiles"))
                    .child(small(
                        "new-mcp-profile",
                        "New Profile…",
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
                    format!("Profile · {}", profile.name),
                )
            })
            .chain(self.servers.iter().map(|server| {
                (
                    format!("s:{}", server.id),
                    format!("Server · {}", server.display_name()),
                )
            }))
            .collect();
        let key = as_key(&self.mcp.simulate_target);
        let label = choices
            .iter()
            .find(|(id, _)| *id == key)
            .map_or("Choose a Profile or Server", |(_, label)| label.as_str());
        let mut row = stack().px(px(14.)).py_2().child(self.mcp_select(
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
                    .debug_selector(|| "mcp-sim-input".into())
                    .flex()
                    .gap_2()
                    .items_center()
                    .key_context("McpSimulator")
                    .on_action(cx.listener(|this, _: &Simulate, _, cx| this.run_simulation(cx)))
                    .child(div().flex_1().min_w_0().child(input.clone()))
                    .child(small(
                        "mcp-simulate",
                        "Test",
                        theme,
                        cx.listener(|this, _, _, cx| this.run_simulation(cx)),
                    )),
            );
        }
        match &self.mcp.simulation {
            Some(Ok(decision)) => row.child(muted(decision.reason.clone(), theme)).child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .flex_1()
                            .min_w_0()
                            .gap_x_1()
                            .child(muted("Matched rule:", theme))
                            .child(technical(decision.matched_rule.clone(), theme)),
                    )
                    .child(badge(
                        decision_label(decision.outcome),
                        simulation_tone(decision),
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
        if presets::resolve_profile(config, profile).is_none() {
            self.mcp.simulation = Some(Err("The selected profile no longer exists.".into()));
            cx.notify();
            return;
        }
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
            Err("Enter a tool and its arguments, for example fs_read /srv/app.log.".into())
        } else {
            Ok(decision)
        });
        cx.notify();
    }

    fn advanced_section(&self, config: &McpConfig, theme: Theme, cx: &Context<Self>) -> Div {
        let limits = &config.rollback;
        let stored = self
            .mcp
            .usage
            .as_ref()
            .and_then(|result| result.as_ref().ok());
        let usage = match &self.mcp.usage {
            Some(Ok(usage)) => format!(
                "{} / {}",
                format_backup_size(usage.total_bytes),
                format_backup_size(limits.max_total_bytes)
            ),
            Some(Err(_)) => "Usage unavailable".into(),
            None => "Checking usage…".into(),
        };
        let summary = if limits.enabled {
            format!("Rollback on · {usage}")
        } else {
            "Rollback off".into()
        };
        let expanded = self.mcp.advanced_expanded;
        stack().pt_3().child(settings_group(
            theme,
            vec![
                div()
                    .child(disclosure(
                        "mcp-advanced-storage",
                        "Advanced & Storage",
                        (!expanded).then_some(summary),
                        expanded,
                        theme,
                        cx.listener(|this, _, _, cx| {
                            this.mcp.advanced_expanded = !this.mcp.advanced_expanded;
                            cx.notify();
                        }),
                    ))
                    .when(expanded, |group| {
                        let detail = stored.map_or_else(
                            || usage.clone(),
                            |stored| {
                                let copies = if stored.count == 1 {
                                    "1 backup".into()
                                } else {
                                    format!("{} backups", stored.count)
                                };
                                format!("{copies} · {usage}")
                            },
                        );
                        group
                            .child(div().h(px(1.)).bg(theme.border))
                            .child(self.rollback_row(limits.enabled, detail, theme, cx))
                            .child(div().h(px(1.)).bg(theme.border))
                            .child(
                                stack()
                                    .gap_1()
                                    .px(px(14.))
                                    .py_2()
                                    .child(heading("Local Socket"))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                ellipsis(
                                                    "mcp-socket",
                                                    vela_mcp::ipc::socket_path(None)
                                                        .display()
                                                        .to_string(),
                                                    theme,
                                                )
                                                .font_family(crate::theme::monospace())
                                                .debug_selector(|| "mcp-socket".into()),
                                            )
                                            .child(
                                                technical("stdio", theme)
                                                    .flex_none()
                                                    .border_l_1()
                                                    .border_color(theme.border)
                                                    .pl_2(),
                                            ),
                                    ),
                            )
                    }),
            ],
        ))
    }

    fn rollback_row(&self, enabled: bool, usage: String, theme: Theme, cx: &Context<Self>) -> Div {
        let busy = self.mcp.busy() || self.mcp.conflict;
        let actions = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .px(px(14.))
            .pb_2()
            .child(small(
                "mcp-backup-limits",
                "Limits…",
                theme,
                cx.listener(|this, _, window, cx| this.open_rollback_limits(window, cx)),
            ))
            .child(small(
                "mcp-backup-open",
                "Open Folder",
                theme,
                cx.listener(|this, _, window, cx| this.rollback_action(false, window, cx)),
            ))
            .child(
                small(
                    "mcp-backup-clear",
                    "Clear…",
                    theme,
                    cx.listener(|this, _, window, cx| {
                        this.mcp.confirmation = Some(Confirmation::Clear);
                        this.start_mcp(window, cx);
                    }),
                )
                .text_color(theme.danger),
            );
        stack()
            .gap_0()
            .child(settings_row(
                "Rollback Backups",
                Some(usage.into()),
                switch_with_state(
                    "mcp-backup",
                    "Rollback Backups",
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
            ))
            .child(actions)
            .children(
                self.mcp
                    .usage
                    .as_ref()
                    .and_then(|result| result.as_ref().err())
                    .map(|error| {
                        muted(error.clone(), theme)
                            .px(px(14.))
                            .pb_2()
                            .text_color(theme.danger)
                    }),
            )
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
            input(cx, "Maximum per File (MiB)", &file, "25", false),
            input(cx, "Total Limit (MiB)", &total, "100", false),
            input(cx, "Keep Backups for (Hours)", &hours, "24", false),
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
                    "Maximum per File (MiB)",
                    "Total Limit (MiB)",
                    "Keep Backups for (Hours)",
                ]
                .into_iter()
                .zip(fields)
                .map(|(label, field)| stack().gap_1().child(heading(label)).child(field.clone())),
            )
            .when(!valid, |body| {
                body.child(
                    muted(
                        "Enter positive whole numbers. The per-file limit must fit within the total limit.",
                        theme,
                    )
                    .text_color(theme.danger),
                )
            })
            .children(self.mcp_error(theme, cx));
        (
            "Rollback Limits".into(),
            body.into_any_element(),
            String::new(),
            Some("Save"),
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
                    this.mcp.usage = Some(Err(format!(
                        "Couldn’t complete the backup operation. {error}"
                    )));
                    cx.notify();
                } else {
                    this.refresh_mcp_usage(window, cx);
                }
            });
        })
        .detach();
    }
}

fn format_backup_size(bytes: u64) -> String {
    if bytes < MIB {
        return format_size(bytes);
    }
    let value = format!("{:.1}", bytes as f64 / MIB as f64);
    format!("{} MiB", value.strip_suffix(".0").unwrap_or(&value))
}

fn disclosure(
    id: &'static str,
    label: &'static str,
    summary: Option<String>,
    expanded: bool,
    theme: Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Button)
        .aria_expanded(expanded)
        .tab_index(0)
        .flex()
        .items_center()
        .gap_2()
        .px(px(14.))
        .py_2()
        .border_1()
        .border_color(gpui::transparent_black())
        .rounded(px(8.))
        .hover(move |style| style.bg(theme.hover))
        .focus_visible(move |style| style.border_color(theme.accent))
        .cursor_pointer()
        .child(
            icon(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(px(14.))
            .text_color(theme.secondary_text),
        )
        .child(heading(label).flex_none())
        .children(summary.map(|summary| muted(summary, theme).ml_auto().min_w_0().truncate()))
        .on_click(on_click)
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
