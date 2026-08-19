//! Native Agent session fork flow shared by IM commands and watch-card actions in the runtime.

use super::*;

pub(super) async fn start_fork_flow(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    config: &AppConfig,
    lang: Lang,
) {
    let _ = set_active_channel(state, channel_id).await;
    if !config.agent_tasks.enabled {
        let text = match lang {
            Lang::Zh => "尚未开启「从 IM 创建 Agent 任务」。请先在设置中开启。",
            Lang::En => "IM Agent task creation is disabled. Enable it in Settings first.",
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    if config.general.daemon_lifecycle != crate::config::DaemonLifecycleMode::KeepAlive
        || !crate::integrations::login_item::daemon_is_installed()
        || crate::integrations::login_item::daemon_needs_update()
    {
        let text = match lang {
            Lang::Zh => "功能尚未就绪：请在设置中重新保存此功能，以启用 Daemon 保活与登录项。",
            Lang::En => "This feature is not ready. Save it again in Settings to enable daemon keepalive and its login item.",
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    if !crate::integrations::agent_launch::terminal_available() {
        let text = match lang {
            Lang::Zh => "没有找到受支持的系统终端（macOS Terminal.app 或 Windows Terminal）。",
            Lang::En => {
                "No supported system terminal was found (macOS Terminal.app or Windows Terminal)."
            }
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    let readiness =
        tokio::task::spawn_blocking(crate::integrations::agent_launch::all_fork_readiness)
            .await
            .unwrap_or_default();
    let ready_kinds: std::collections::HashSet<_> = readiness
        .iter()
        .filter(|item| item.ready)
        .map(|item| item.kind)
        .collect();
    let snapshot = state.agents.snapshot();
    let options = crate::select::fork_options(&snapshot, &ready_kinds, now_secs(), lang);
    if let Some(seq) = sel {
        let Some(option) = options.iter().find(|option| option.seq == Some(seq)) else {
            let diagnostics = readiness
                .iter()
                .flat_map(|item| item.diagnostics.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let text = match lang {
                Lang::Zh => format!("Agent #{seq} 当前不可分叉。"),
                Lang::En => format!("Agent #{seq} cannot be forked right now."),
            };
            let text = if diagnostics.is_empty() {
                text
            } else {
                format!("{text}\n{diagnostics}")
            };
            let _ = reply_channel_text(channel_id, config, &text).await;
            return;
        };
        start_fork_source(state, channel_id, &option.id, config, lang).await;
        return;
    }
    if options.is_empty() {
        let diagnostics = readiness
            .iter()
            .flat_map(|item| item.diagnostics.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let text = match lang {
            Lang::Zh => "当前没有可分叉的工作中或空闲 Agent。",
            Lang::En => "No working or idle Agent can be forked right now.",
        };
        let text = if diagnostics.is_empty() {
            text.to_string()
        } else {
            format!("{text}\n{diagnostics}")
        };
        let _ = reply_channel_text(channel_id, config, &text).await;
        return;
    }
    let sent = send_agent_picker(
        state,
        channel_id,
        config,
        PickerKind::ForkSource,
        crate::select::title_fork(lang),
        options,
        None,
        lang,
    )
    .await;
    if !sent {
        let _ = reply_channel_text(channel_id, config, "Failed to send fork picker").await;
    }
}

pub(super) async fn start_fork_source(
    state: &Arc<ServerState>,
    channel_id: &str,
    source_session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    if !config.agent_tasks.enabled {
        let text = match lang {
            Lang::Zh => "尚未开启「从 IM 创建 Agent 任务」。请先在设置中开启。",
            Lang::En => "IM Agent task creation is disabled. Enable it in Settings first.",
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    if config.general.daemon_lifecycle != crate::config::DaemonLifecycleMode::KeepAlive
        || !crate::integrations::login_item::daemon_is_installed()
        || crate::integrations::login_item::daemon_needs_update()
    {
        let text = match lang {
            Lang::Zh => "功能尚未就绪：请在设置中重新保存此功能，以启用 Daemon 保活与登录项。",
            Lang::En => "This feature is not ready. Save it again in Settings to enable daemon keepalive and its login item.",
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    if !crate::integrations::agent_launch::terminal_available() {
        let text = match lang {
            Lang::Zh => "没有找到受支持的系统终端（macOS Terminal.app 或 Windows Terminal）。",
            Lang::En => {
                "No supported system terminal was found (macOS Terminal.app or Windows Terminal)."
            }
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    let snapshot = state.agents.snapshot();
    let Some(record) = snapshot.as_array().and_then(|items| {
        items.iter().find(|record| {
            record.get("sessionId").and_then(serde_json::Value::as_str) == Some(source_session_id)
                && matches!(
                    record.get("state").and_then(serde_json::Value::as_str),
                    Some("working" | "idle")
                )
        })
    }) else {
        let _ = reply_channel_text(channel_id, config, "Source session is no longer active").await;
        return;
    };
    let Some(kind) = record
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .and_then(AgentKind::parse)
    else {
        return;
    };
    let Some(cwd) = record
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|cwd| std::path::Path::new(cwd).is_dir())
    else {
        let _ = reply_channel_text(channel_id, config, "Source workspace is unavailable").await;
        return;
    };
    let readiness = tokio::task::spawn_blocking(move || {
        crate::integrations::agent_launch::fork_readiness(kind)
    })
    .await
    .ok();
    if !readiness.is_some_and(|item| item.ready)
        || crate::agents::transcript_full::transcript_mtime(kind, source_session_id).is_none()
    {
        let _ = reply_channel_text(channel_id, config, "Source session cannot be forked").await;
        return;
    }
    let payload = ForkPickerPayload {
        source_session_id: source_session_id.to_string(),
        source_seq: record
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        cwd,
        kind,
        title: record
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    if kind == AgentKind::Pi {
        start_fork_input(
            state,
            channel_id,
            payload,
            crate::integrations::agent_launch::LaunchPermission::AgentDefault,
            config,
            lang,
        )
        .await;
        return;
    }
    match config.agent_tasks.permission_prompt {
        crate::config::AgentTaskPermission::Ask => {
            let _ = send_agent_picker(
                state,
                channel_id,
                config,
                PickerKind::ForkPermission,
                crate::select::title_task_permission(lang),
                fork_permission_options(lang),
                Some(serde_json::to_string(&payload).unwrap_or_default()),
                lang,
            )
            .await;
        }
        crate::config::AgentTaskPermission::AgentDefault => {
            start_fork_input(
                state,
                channel_id,
                payload,
                crate::integrations::agent_launch::LaunchPermission::AgentDefault,
                config,
                lang,
            )
            .await;
        }
        crate::config::AgentTaskPermission::Yolo => {
            start_fork_input(
                state,
                channel_id,
                payload,
                crate::integrations::agent_launch::LaunchPermission::Yolo,
                config,
                lang,
            )
            .await;
        }
    }
}

fn fork_permission_options(lang: Lang) -> Vec<crate::select::SelectOption> {
    vec![
        crate::select::SelectOption {
            id: "agent-default".into(),
            dot: None,
            seq: None,
            primary: match lang {
                Lang::Zh => "Agent 默认",
                Lang::En => "Agent default",
            }
            .into(),
            badge: None,
            elapsed: None,
            secondary: Some(
                match lang {
                    Lang::Zh => "不附加权限覆盖参数",
                    Lang::En => "Do not override Agent permissions",
                }
                .into(),
            ),
        },
        crate::select::SelectOption {
            id: "yolo".into(),
            dot: None,
            seq: None,
            primary: "YOLO".into(),
            badge: Some(
                match lang {
                    Lang::Zh => "危险",
                    Lang::En => "Danger",
                }
                .into(),
            ),
            elapsed: None,
            secondary: Some(
                match lang {
                    Lang::Zh => "自动批准操作并绕过沙箱限制",
                    Lang::En => "Auto-approve operations and bypass sandbox restrictions",
                }
                .into(),
            ),
        },
    ]
}

pub(super) async fn continue_fork_picker(
    state: &Arc<ServerState>,
    channel_id: &str,
    picker: &PickerEntry,
    selected_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    match picker.kind {
        PickerKind::ForkSource => {
            start_fork_source(state, channel_id, selected_id, config, lang).await;
        }
        PickerKind::ForkPermission => {
            let Some(payload) = picker
                .payload
                .as_deref()
                .and_then(|value| serde_json::from_str::<ForkPickerPayload>(value).ok())
            else {
                return;
            };
            let permission = match selected_id {
                "agent-default" => {
                    crate::integrations::agent_launch::LaunchPermission::AgentDefault
                }
                "yolo" => crate::integrations::agent_launch::LaunchPermission::Yolo,
                _ => return,
            };
            start_fork_input(state, channel_id, payload, permission, config, lang).await;
        }
        _ => {}
    }
}

async fn start_fork_input(
    state: &Arc<ServerState>,
    channel_id: &str,
    payload: ForkPickerPayload,
    permission: crate::integrations::agent_launch::LaunchPermission,
    config: &AppConfig,
    lang: Lang,
) {
    use crate::models::{
        ConfirmChoice, ConfirmDetail, ConfirmField, ConfirmFieldKind, ConfirmInput,
        ConfirmPresentation, ConfirmSpec,
    };
    let permission_label = match permission {
        crate::integrations::agent_launch::LaunchPermission::AgentDefault => match lang {
            Lang::Zh => "Agent 默认",
            Lang::En => "Agent default",
        },
        crate::integrations::agent_launch::LaunchPermission::Yolo => "YOLO",
    };
    let project = crate::project::display_name(&payload.cwd);
    let spec = ConfirmSpec {
        title: format!("Fork Agent #{}", payload.source_seq),
        context: vec![
            ConfirmField {
                id: "agent".into(),
                label: "Agent".into(),
                value: payload.kind.label().into(),
                kind: ConfirmFieldKind::Text,
            },
            ConfirmField {
                id: "workspace".into(),
                label: match lang {
                    Lang::Zh => "工作目录",
                    Lang::En => "Workspace",
                }
                .into(),
                value: payload.cwd.clone(),
                kind: ConfirmFieldKind::Path,
            },
            ConfirmField {
                id: "permission".into(),
                label: match lang {
                    Lang::Zh => "权限",
                    Lang::En => "Permission",
                }
                .into(),
                value: permission_label.into(),
                kind: ConfirmFieldKind::Text,
            },
        ],
        detail: ConfirmDetail {
            summary: match lang {
                Lang::Zh => format!(
                    "**原会话将继续运行。**\n\n从 {} · {} 的已持久化上下文创建新分支。",
                    payload.kind.label(),
                    project
                ),
                Lang::En => format!(
                    "**The source session will keep running.**\n\nCreate a new branch from the persisted context of {} · {}.",
                    payload.kind.label(),
                    project
                ),
            },
            body_md: String::new(),
        },
        choices: vec![
            ConfirmChoice {
                id: "start".into(),
                label: match lang {
                    Lang::Zh => "启动分支",
                    Lang::En => "Start fork",
                }
                .into(),
                description: String::new(),
                role: crate::confirm::ActionRole::Primary,
                variant: None,
            },
            ConfirmChoice {
                id: "cancel".into(),
                label: match lang {
                    Lang::Zh => "取消",
                    Lang::En => "Cancel",
                }
                .into(),
                description: String::new(),
                role: crate::confirm::ActionRole::Destructive,
                variant: None,
            },
        ],
        presentation: ConfirmPresentation::SingleSelectSubmit {
            input: Some(ConfirmInput {
                id: "task".into(),
                visible_when_action_id: "start".into(),
                always_visible: true,
                required: true,
                prefix_chars_by_action_id: std::collections::BTreeMap::new(),
                label: match lang {
                    Lang::Zh => "新分支接下来做什么",
                    Lang::En => "What should the new branch do?",
                }
                .into(),
                placeholder: match lang {
                    Lang::Zh => "输入分支指令（最多 3000 字）",
                    Lang::En => "Enter branch instructions (up to 3000 characters)",
                }
                .into(),
                max_chars: 3000,
            }),
            submit_label: match lang {
                Lang::Zh => "启动分支",
                Lang::En => "Start fork",
            }
            .into(),
            default_action_id: Some("start".into()),
        },
        dismiss_action_id: "cancel".into(),
    };
    let Ok((entry, mut outcome)) = request::create_internal_confirm(
        spec,
        channel_id,
        lang.code(),
        &payload.cwd,
        payload.kind.as_str(),
        Duration::from_secs(30 * 60),
    ) else {
        let _ = reply_channel_text(channel_id, config, "Failed to create fork input").await;
        return;
    };
    let started = match channel_id {
        "feishu" => ensure_fs_router(state, &config.channels.feishu)
            .await
            .map(|router| {
                crate::channels::confirm::start_feishu(
                    entry.clone(),
                    config.channels.feishu.clone(),
                    router,
                );
            }),
        "dingding" => ensure_dd_router(
            state,
            config.channels.dingding.client_id.trim(),
            config.channels.dingding.client_secret.trim(),
        )
        .await
        .map(|router| {
            crate::channels::confirm::start_dingtalk(
                entry.clone(),
                config.channels.dingding.clone(),
                router,
            );
        }),
        "telegram" => ensure_tg_router(state, &config.channels.telegram)
            .await
            .map(|router| {
                crate::channels::confirm::start_telegram(
                    entry.clone(),
                    config.channels.telegram.clone(),
                    router,
                );
            }),
        "slack" => ensure_sl_router(state, &config.channels.slack)
            .await
            .map(|router| {
                crate::channels::confirm::start_slack(
                    entry.clone(),
                    config.channels.slack.clone(),
                    router,
                );
            }),
        _ => None,
    }
    .is_some();
    if !started {
        entry
            .coordinator
            .fallback(ConfirmFallbackReason::NoAvailableChannel);
        let _ = reply_channel_text(channel_id, config, "Fork input channel is unavailable").await;
        return;
    }
    let state = state.clone();
    let config = config.clone();
    let channel = channel_id.to_string();
    tokio::spawn(async move {
        let Some(ConfirmOutcome::Final(result)) = outcome.recv().await else {
            return;
        };
        if result.action_id == "cancel" {
            return;
        }
        let task = result.comment.unwrap_or_default().trim().to_string();
        if task.is_empty() || task.contains('\0') || task.chars().count() > 3000 {
            let _ = reply_channel_text(&channel, &config, "Fork instruction is invalid").await;
            return;
        }
        let source = crate::integrations::agent_launch::LaunchSource {
            channel: channel.clone(),
            target: task_source_target(&config, &channel),
        };
        let launch = crate::integrations::agent_launch::create_fork_record(
            source,
            std::path::Path::new(&payload.cwd),
            payload.kind,
            permission,
            &payload.source_session_id,
            &task,
        )
        .and_then(|record| {
            register_pending_launch_watch(&state, &record, &channel, &config, lang);
            crate::integrations::agent_launch::open_terminal(&record)
                .inspect_err(|_| {
                    state
                        .pending_launches
                        .lock()
                        .unwrap()
                        .retain(|item| item.id != record.id);
                })
                .map(|_| record)
        });
        let text = match launch {
            Ok(_) => match lang {
                Lang::Zh => format!(
                    "已请求分叉 #{}；原会话继续运行，正在等待新会话注册。",
                    payload.source_seq
                ),
                Lang::En => format!(
                    "Requested fork of #{}; the source continues while the new session registers.",
                    payload.source_seq
                ),
            },
            Err(error) => format!("Failed to launch fork: {error:#}"),
        };
        let _ = reply_channel_text(&channel, &config, &text).await;
        state.watch.notify.notify_one();
    });
}
