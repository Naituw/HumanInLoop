//! Cross-channel select/picker delivery, routing, and callback dispatch.

use super::*;

// ===== 通用「单选卡」子系统（spec docs/specs/im-select-card.md）=====

/// 登记一条单选卡台账（顺带按 TTL + 每渠道软上限清理旧卡）。
pub(super) fn register_picker(state: &Arc<ServerState>, entry: PickerEntry) {
    let now = now_secs();
    let is_msg_compose = entry.kind == PickerKind::MsgCompose;
    let mut pickers = state.select.pickers.lock().unwrap();
    // TTL 兜底清理（全渠道）。
    // MsgCompose 有独立的可见过期收尾，不能在这里静默丢弃。
    pickers.retain(|p| {
        p.kind == PickerKind::MsgCompose
            || now.saturating_sub(p.created_at) < SELECT_PICKER_TTL_SECS
    });
    let channel = entry.channel.clone();
    pickers.push(entry);
    // 每渠道软上限：超出丢最旧（本渠道最靠前的条目）。
    while pickers.iter().filter(|p| p.channel == channel).count() > SELECT_MAX_PICKERS_PER_CHANNEL {
        if let Some(pos) = pickers
            .iter()
            .position(|p| p.channel == channel && p.kind != PickerKind::MsgCompose)
        {
            pickers.remove(pos);
        } else {
            break;
        }
    }
    drop(pickers);
    if is_msg_compose {
        persist_msg_compose_recovery(state);
    }
}

fn persist_msg_compose_recovery(state: &Arc<ServerState>) {
    let items: Vec<crate::msg_card::MsgComposeRecovery> = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .filter(|picker| picker.kind == PickerKind::MsgCompose)
        .filter_map(|picker| {
            let payload = crate::msg_card::decode_payload(picker.payload.as_deref())?;
            Some(crate::msg_card::MsgComposeRecovery {
                channel: picker.channel.clone(),
                message_id: picker.message_id.clone(),
                session_id: payload.session_id,
                expires_at: payload.expires_at,
            })
        })
        .collect();
    crate::msg_card::save_recovery(&items);
}

fn msg_compose_payload(session_id: &str, recovered: bool) -> Option<String> {
    crate::msg_card::encode_payload(&crate::msg_card::MsgComposePayload {
        session_id: session_id.to_string(),
        expires_at: now_secs().saturating_add(SELECT_PICKER_TTL_SECS),
        recovered,
    })
}

/// 发送一张 `/msg` 一次性输入卡并登记最小恢复记录。
pub(super) async fn send_msg_compose_card(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    session_id: &str,
    lang: Lang,
) -> bool {
    let snapshot = state.agents.snapshot();
    let Some(rec) = find_agent_by_session(&snapshot, session_id) else {
        return false;
    };
    if !is_working_non_grok(&snapshot, session_id) {
        return false;
    }
    let view = crate::msg_card::build_view(
        rec,
        state.interject.pending_count(session_id),
        &state.interject.full_text(session_id),
        None,
        lang,
    );
    let message_id = match channel_id {
        "feishu" => {
            let Ok(client) = crate::feishu::client::FeishuClient::new(&config.channels.feishu)
            else {
                return false;
            };
            let card = crate::feishu::card::build_msg_compose_card(&view, None);
            match client.send_card(&card).await {
                Ok(mid) => mid,
                Err(err) => {
                    log(&format!("msg compose: send feishu card failed: {}", err));
                    return false;
                }
            }
        }
        "dingding" => {
            let Ok(client) =
                crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
            else {
                return false;
            };
            let otid = format!("msg-{}", uuid::Uuid::new_v4());
            let map = crate::dingtalk::card::build_card_param_map(
                &view.title,
                &crate::msg_card::escape_markdown(&view.plain_body()),
                &[],
                false,
                false,
                "",
            );
            if let Err(err) = client
                .create_and_deliver_card(
                    &otid,
                    crate::channels::dingding::effective_template_id(&config.channels.dingding),
                    map,
                    crate::dingtalk::card::build_card_private_map(),
                )
                .await
            {
                log(&format!("msg compose: send dingtalk card failed: {}", err));
                return false;
            }
            otid
        }
        "telegram" => {
            let tg = &config.channels.telegram;
            let Ok(client) = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            ) else {
                return false;
            };
            let html = render_msg_compose_telegram(&view);
            let force_reply = serde_json::json!({
                "force_reply": true,
                "selective": true,
                "input_field_placeholder": view.input_placeholder,
            });
            match client
                .send_message(&html, Some("HTML"), Some(force_reply))
                .await
            {
                Ok(mid) if mid != 0 => mid.to_string(),
                Ok(_) => return false,
                Err(err) => {
                    log(&format!(
                        "msg compose: send telegram prompt failed: {}",
                        err
                    ));
                    return false;
                }
            }
        }
        "slack" => {
            let Ok(client) = crate::slack::client::SlackClient::new(&config.channels.slack) else {
                return false;
            };
            let Ok(dm) = client.open_dm().await else {
                return false;
            };
            let nonce = uuid::Uuid::new_v4().to_string();
            let blocks = crate::slack::blockkit::build_msg_compose_card(&view, &nonce, None);
            match client.post_message(&dm, Some(&blocks), &view.title).await {
                Ok(ts) => ts,
                Err(err) => {
                    log(&format!("msg compose: send slack card failed: {}", err));
                    return false;
                }
            }
        }
        _ => return false,
    };
    let Some(payload) = msg_compose_payload(session_id, false) else {
        return false;
    };
    register_picker(
        state,
        PickerEntry {
            channel: channel_id.to_string(),
            message_id,
            kind: PickerKind::MsgCompose,
            title: view.title,
            options: Vec::new(),
            payload: Some(payload),
            created_at: now_secs(),
            posted_ms: now_ms(),
        },
    );
    state.select.route_refresh.notify_one();
    true
}

fn render_msg_compose_telegram(view: &crate::msg_card::MsgComposeView) -> String {
    let esc = crate::telegram::markdown::escape_html;
    format!("<b>{}</b>\n\n{}", esc(&view.title), esc(&view.plain_body()))
}

/// 发一张单选卡到某渠道，返回消息 id（MVP 仅飞书；其它渠道 None → 调用方回文本兜底）。
pub(super) async fn send_select_card(
    channel_id: &str,
    config: &AppConfig,
    view: &crate::select::SelectView,
) -> Option<String> {
    match channel_id {
        "feishu" => {
            let client = crate::feishu::client::FeishuClient::new(&config.channels.feishu).ok()?;
            let card = crate::feishu::card::build_select_card(view);
            client.send_card(&card).await.ok()
        }
        "dingding" => {
            // 钉钉：模板 + 变量。消息 id = 自铸 outTrackId（与 watch 卡同规，天然可编辑）。
            let client =
                crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding).ok()?;
            let otid = format!("select-{}", uuid::Uuid::new_v4());
            let map = crate::dingtalk::select::build_select_param_map(view, Lang::current());
            client
                .create_and_deliver_card(
                    &otid,
                    crate::dingtalk::select::DEFAULT_SELECT_CARD_TEMPLATE_ID,
                    map,
                    serde_json::json!({}),
                )
                .await
                .ok()?;
            Some(otid)
        }
        "telegram" => {
            let tg = &config.channels.telegram;
            let client = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            )
            .ok()?;
            let html = crate::telegram::select::render_select_html(view);
            let markup = crate::telegram::select::inline_keyboard(view, Lang::current());
            client
                .send_message(&html, Some("HTML"), Some(markup))
                .await
                .ok()
                .map(|mid| mid.to_string())
        }
        "slack" => {
            let client = crate::slack::client::SlackClient::new(&config.channels.slack).ok()?;
            let dm = client.open_dm().await.ok()?;
            let (blocks, fallback) =
                crate::slack::select::build_select_blocks(view, Lang::current());
            client
                .post_message(&dm, Some(&blocks), &fallback)
                .await
                .ok()
        }
        _ => None,
    }
}

/// 组装并发一张 agent 单选卡：空选项 / 非支持渠道（send 失败）→ 返回 false（调用方回文本兜底）。
/// `payload` 仅 `PickerKind::Msg` 用（待发送内容随卡登记，点「发送」时投递）。
#[allow(clippy::too_many_arguments)] // args mirror the picker card fields
pub(super) async fn send_agent_picker(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    kind: PickerKind,
    title: String,
    options: Vec<crate::select::SelectOption>,
    payload: Option<String>,
    lang: Lang,
) -> bool {
    if options.is_empty() {
        return false;
    }
    let action = match kind {
        PickerKind::TaskWorkspace => crate::select::SelectAction::TaskWorkspace,
        PickerKind::TaskAgent => crate::select::SelectAction::TaskAgent,
        PickerKind::TaskPermission => crate::select::SelectAction::TaskPermission,
        PickerKind::TaskInputSource => crate::select::SelectAction::TaskInputSource,
        PickerKind::ForkSource => crate::select::SelectAction::Fork,
        PickerKind::ForkPermission => crate::select::SelectAction::TaskPermission,
        PickerKind::Watch => crate::select::SelectAction::Watch,
        PickerKind::Status => crate::select::SelectAction::Status,
        PickerKind::Unwatch => crate::select::SelectAction::Unwatch,
        PickerKind::Msg if payload.is_none() => crate::select::SelectAction::MsgTarget,
        PickerKind::Msg => crate::select::SelectAction::Msg,
        PickerKind::MsgCompose => return false,
        PickerKind::Diff => crate::select::SelectAction::Diff,
        PickerKind::Stage => crate::select::SelectAction::Stage,
        PickerKind::Transcript => crate::select::SelectAction::Transcript,
        PickerKind::Todo => crate::select::SelectAction::Todo,
        PickerKind::TodoRm => crate::select::SelectAction::TodoRm,
        PickerKind::TodoRmEntry => crate::select::SelectAction::TodoRmEntry,
        PickerKind::TodoAuto => crate::select::SelectAction::TodoAuto,
        PickerKind::TodoAutoEntry => crate::select::SelectAction::TodoAutoEntry,
        // 管理卡不经单选卡通道发送（见 todo.rs::send_todo_manage / register_todo_manage）。
        PickerKind::TodoManage => return false,
        PickerKind::Yolo => crate::select::SelectAction::Yolo,
    };
    let view = crate::select::build_view(title, options, action, lang);
    let session_ids: Vec<String> = view.options.iter().map(|o| o.id.clone()).collect();
    let Some(mid) = send_select_card(channel_id, config, &view).await else {
        return false;
    };
    register_picker(
        state,
        PickerEntry {
            channel: channel_id.to_string(),
            message_id: mid,
            kind,
            title: view.title.clone(),
            options: session_ids,
            payload,
            created_at: now_secs(),
            posted_ms: now_ms(),
        },
    );
    state.select.route_refresh.notify_one();
    true
}

#[allow(clippy::too_many_arguments)] // args mirror the select-card callback context
pub(super) async fn select_pick_task_flow(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    selected_id: &str,
    picker: &PickerEntry,
    config: &AppConfig,
    lang: Lang,
    ack: Option<crate::confirm::transport::FsAck>,
) {
    let title = match picker.kind {
        PickerKind::TaskWorkspace => crate::select::title_task_workspace(lang),
        PickerKind::TaskAgent => crate::select::title_task_agent(lang),
        PickerKind::TaskPermission => crate::select::title_task_permission(lang),
        PickerKind::TaskInputSource => crate::select::title_task_input_source(lang),
        PickerKind::ForkSource => crate::select::title_fork(lang),
        PickerKind::ForkPermission => crate::select::title_task_permission(lang),
        _ => String::new(),
    };
    let label = match picker.kind {
        PickerKind::TaskWorkspace => {
            crate::autochannel::project_name(selected_id).unwrap_or_else(|| selected_id.to_string())
        }
        PickerKind::TaskAgent => AgentKind::parse(selected_id)
            .map(|kind| kind.label().to_string())
            .unwrap_or_else(|| selected_id.to_string()),
        PickerKind::TaskPermission if selected_id == "agent-default" => match lang {
            Lang::Zh => "Agent 默认",
            Lang::En => "Agent default",
        }
        .into(),
        PickerKind::TaskPermission => "YOLO".into(),
        PickerKind::TaskInputSource => task_input_source_label(picker, selected_id, lang),
        PickerKind::ForkSource => picker
            .options
            .iter()
            .position(|id| id == selected_id)
            .map(|_| selected_id.chars().take(8).collect())
            .unwrap_or_else(|| selected_id.to_string()),
        PickerKind::ForkPermission if selected_id == "agent-default" => match lang {
            Lang::Zh => "Agent 默认",
            Lang::En => "Agent default",
        }
        .into(),
        PickerKind::ForkPermission => "YOLO".into(),
        _ => selected_id.to_string(),
    };
    if channel_id == "feishu" {
        if let Some(ack) = ack {
            let card = crate::feishu::card::build_select_final_card(&title, &label);
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        }
    } else if channel_id == "dingding" {
        dd_finalize_select_card(config, mid, &label).await;
    } else {
        finalize_select_card_edit(channel_id, config, mid, &title, &label).await;
    }
    remove_picker(state, channel_id, mid);
    state.select.route_refresh.notify_one();
    continue_task_picker(state, channel_id, picker, selected_id, config, lang).await;
}

fn task_input_source_label(picker: &PickerEntry, selected_id: &str, lang: Lang) -> String {
    if selected_id == TASK_MANUAL_ACTION_ID {
        return match lang {
            Lang::Zh => "输入新任务",
            Lang::En => "Enter a new task",
        }
        .into();
    }
    let todo_id = selected_id
        .strip_prefix(TASK_TODO_ACTION_PREFIX)
        .unwrap_or_default();
    picker
        .payload
        .as_deref()
        .and_then(|value| serde_json::from_str::<TaskInputSourcePayload>(value).ok())
        .and_then(|payload| payload.todos.into_iter().find(|todo| todo.id == todo_id))
        .map(|todo| task_todo_label(&todo, lang))
        .unwrap_or_else(|| selected_id.to_string())
}

/// `/status` 详情（按 session_id 定位，避免 seq 漂移）。找不到 → notFound 提示。
pub(super) fn status_detail_by_session(
    snapshot: &serde_json::Value,
    session_id: &str,
    channel_id: &str,
    lang: Lang,
) -> String {
    let prefix = crate::autochannel::cmd_prefix(channel_id);
    let seq = snapshot.as_array().and_then(|l| {
        l.iter()
            .find(|r| r.get("sessionId").and_then(|v| v.as_str()) == Some(session_id))
            .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
    });
    match seq {
        Some(id) => crate::autochannel::status_detail_text(snapshot, id, prefix, lang),
        None => crate::i18n::tr(lang, "autoChannel.statusDetailNotFound")
            .replace("{id}", "?")
            .replace("{p}", prefix),
    }
}

/// 从单选卡台账移除某条卡。移除即视为「单选完结」：清零本渠道全部 watch 订阅的跟底节流
/// （`last_move_ms=0`）并唤醒引擎——单选期间被抑制的跟底在下一次内容变化时立即重发到会话底部
/// （用户定案，与「提问完结」一致；此处覆盖到钉钉，补上提问路径遗漏 dingding 的口径差）。
pub(super) fn remove_picker(state: &Arc<ServerState>, channel_id: &str, message_id: &str) {
    let removed_kind = {
        let mut pickers = state.select.pickers.lock().unwrap();
        pickers
            .iter()
            .position(|p| p.channel == channel_id && p.message_id == message_id)
            .map(|pos| pickers.remove(pos).kind)
    };
    let Some(removed_kind) = removed_kind else {
        return;
    };
    after_picker_removed(state, channel_id, message_id, removed_kind);
}

fn take_msg_compose_picker(
    state: &Arc<ServerState>,
    channel_id: &str,
    message_id: &str,
) -> Option<PickerEntry> {
    let removed = {
        let mut pickers = state.select.pickers.lock().unwrap();
        let pos = pickers.iter().position(|picker| {
            picker.channel == channel_id
                && picker.message_id == message_id
                && picker.kind == PickerKind::MsgCompose
        })?;
        pickers.remove(pos)
    };
    after_picker_removed(state, channel_id, message_id, removed.kind);
    Some(removed)
}

fn after_picker_removed(
    state: &Arc<ServerState>,
    channel_id: &str,
    message_id: &str,
    kind: PickerKind,
) {
    if kind == PickerKind::MsgCompose {
        let now = now_secs();
        let mut tombstones = state.select.msg_compose_tombstones.lock().unwrap();
        tombstones.retain(|_, expires_at| *expires_at > now);
        // Router/observer 的调度差通常只有毫秒；保留一分钟足以封住重复消费，又不会长期吞旧回复。
        tombstones.insert(
            format!("{}\n{}", channel_id, message_id),
            now.saturating_add(60),
        );
        drop(tombstones);
        persist_msg_compose_recovery(state);
        state.select.route_refresh.notify_one();
    }
    // 本渠道若已无其它在途单选卡，放开跟底：清零节流 + 唤醒引擎。
    if !has_active_select_on(state, channel_id) {
        let mut cleared = false;
        for s in state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter_mut()
            .filter(|s| s.channel == channel_id)
        {
            s.last_move_ms = 0;
            cleared = true;
        }
        if cleared {
            state.watch.notify.notify_one();
        }
    }
}

/// graceful 关停前把所有活动单选/确认卡就地定格为「已失效」终态（第 15 轮定案）：
/// 台账不持久化（spec im-select-card D7），重启后旧卡点击本会静默无响应——退出前主动
/// 去掉按钮/表单并留「请重新发送命令」提示。best-effort（渠道不可用/更新失败仅记日志），
/// 调用方需自行限时以免拖住关停。
pub(super) async fn finalize_all_select_cards(state: &Arc<ServerState>) {
    let pickers: Vec<PickerEntry> = std::mem::take(&mut *state.select.pickers.lock().unwrap());
    let confirms: Vec<ConfirmEntry> = std::mem::take(&mut *state.select.confirms.lock().unwrap());
    if pickers.is_empty() && confirms.is_empty() {
        return;
    }
    let lang = Lang::current();
    let config = state.config_snapshot();
    let label = crate::i18n::tr(lang, "select.expiredCard");
    for p in &pickers {
        let picker_label = if p.kind == PickerKind::MsgCompose {
            msg_compose_expired_label(&p.channel, lang)
        } else {
            label.to_string()
        };
        match p.channel.as_str() {
            "feishu" => {
                let card = crate::feishu::card::build_select_final_card(&p.title, &picker_label);
                if let Ok(client) =
                    crate::feishu::client::FeishuClient::new(&config.channels.feishu)
                {
                    if let Err(err) = client.patch_card(&p.message_id, &card).await {
                        log(&format!("select: expire feishu card failed: {}", err));
                    }
                }
            }
            "dingding" => {
                if matches!(p.kind, PickerKind::TodoManage | PickerKind::MsgCompose) {
                    // 管理卡走提问卡模板：置私有 `submitted=true` 关表单 + 公有终态文案。
                    if let Ok(client) =
                        crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
                    {
                        if let Err(err) = client
                            .update_card_private(
                                &p.message_id,
                                serde_json::json!({ "submit_status": picker_label }),
                                serde_json::json!({ "submitted": "true" }),
                            )
                            .await
                        {
                            log(&format!(
                                "select: expire dingtalk manage card failed: {}",
                                err
                            ));
                        }
                    }
                } else {
                    dd_finalize_select_card(&config, &p.message_id, label).await;
                }
            }
            "telegram" if p.kind == PickerKind::MsgCompose => {
                let tg = &config.channels.telegram;
                if let (Ok(client), Ok(mid)) = (
                    crate::telegram::TelegramClient::new(
                        tg.bot_token.clone(),
                        tg.chat_id.clone(),
                        tg.api_base_url.clone(),
                    ),
                    p.message_id.parse::<i64>(),
                ) {
                    let _ = client.send_message(&picker_label, None, None).await;
                    let _ = client.delete_message(mid).await;
                }
            }
            _ => {
                finalize_select_card_edit(
                    &p.channel,
                    &config,
                    &p.message_id,
                    &p.title,
                    &picker_label,
                )
                .await
            }
        }
    }
    for c in &confirms {
        let fv = crate::confirm::ConfirmFinalView {
            title: c.view.title.clone(),
            body: label.to_string(),
            label: crate::confirm::transport::truncate_for_label(label),
        };
        crate::confirm::transport::finalize(&c.channel, &config, &c.message_id, &fv, None).await;
    }
}

/// 启动恢复与运行时 TTL 共用的过期收尾。只有远端卡片成功定格后才删除恢复记录；失败项留待
/// 下一轮重试，且回调处理仍会拒绝发送。
pub(super) async fn finalize_expired_msg_compose_cards(state: &Arc<ServerState>) {
    let now = now_secs();
    let expired: Vec<PickerEntry> = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .filter(|picker| picker.kind == PickerKind::MsgCompose)
        .filter(|picker| {
            crate::msg_card::decode_payload(picker.payload.as_deref())
                .map(|payload| payload.recovered || now >= payload.expires_at)
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    if expired.is_empty() {
        return;
    }
    let config = state.config_snapshot();
    let lang = Lang::current();
    for picker in expired {
        let label = msg_compose_expired_label(&picker.channel, lang);
        let finalized = match picker.channel.as_str() {
            "feishu" => match crate::feishu::client::FeishuClient::new(&config.channels.feishu) {
                Ok(client) => {
                    let card = crate::feishu::card::build_select_final_card(&picker.title, &label);
                    client.patch_card(&picker.message_id, &card).await.is_ok()
                }
                Err(_) => false,
            },
            "dingding" => {
                match crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) {
                    Ok(client) => client
                        .update_card_private(
                            &picker.message_id,
                            serde_json::json!({ "submit_status": label }),
                            serde_json::json!({ "submitted": "true" }),
                        )
                        .await
                        .is_ok(),
                    Err(_) => false,
                }
            }
            "telegram" => {
                let tg = &config.channels.telegram;
                match (
                    crate::telegram::TelegramClient::new(
                        tg.bot_token.clone(),
                        tg.chat_id.clone(),
                        tg.api_base_url.clone(),
                    ),
                    picker.message_id.parse::<i64>(),
                ) {
                    (Ok(client), Ok(mid)) => {
                        let notified = client.send_message(&label, None, None).await.is_ok();
                        if let Err(err) = client.delete_message(mid).await {
                            log(&format!(
                                "msg compose: delete expired telegram prompt failed: {}",
                                err
                            ));
                        }
                        notified
                    }
                    _ => false,
                }
            }
            "slack" => match crate::slack::client::SlackClient::new(&config.channels.slack) {
                Ok(client) => match client.open_dm().await {
                    Ok(dm) => {
                        let (blocks, fallback) =
                            crate::slack::select::build_select_final_blocks(&picker.title, &label);
                        client
                            .update_message(&dm, &picker.message_id, Some(&blocks), &fallback)
                            .await
                            .is_ok()
                    }
                    Err(_) => false,
                },
                Err(_) => false,
            },
            _ => true,
        };
        if finalized {
            remove_picker(state, &picker.channel, &picker.message_id);
        }
    }
    state.select.route_refresh.notify_one();
}

/// 幂等确保各渠道的单选卡回调路由任务在位（撤掉已无 picker 的渠道路由）。飞书 / 钉钉 / TG / Slack。
/// Confirm 卡 message_id 一并纳入（与 pickers 共享路由任务）。
pub(super) async fn ensure_select_routes(state: &Arc<ServerState>) {
    let mut desired: HashMap<String, Vec<String>> = HashMap::new();
    for p in state.select.pickers.lock().unwrap().iter() {
        desired
            .entry(p.channel.clone())
            .or_default()
            .push(p.message_id.clone());
    }
    for c in state.select.confirms.lock().unwrap().iter() {
        desired
            .entry(c.channel.clone())
            .or_default()
            .push(c.message_id.clone());
    }
    for mids in desired.values_mut() {
        mids.sort();
        mids.dedup();
    }
    {
        let mut routes = state.select.routes.lock().unwrap();
        routes.retain(|ch, h| {
            if desired.contains_key(ch) {
                true
            } else {
                h.stop.notify_waiters();
                false
            }
        });
    }
    let config = state.config_snapshot();
    for (ch, mids) in desired {
        ensure_select_route_for(state, &config, &ch, mids).await;
    }
}

/// 幂等确保单一渠道的单选卡回调路由任务在位（飞书 / 钉钉 / TG / Slack；复用 watch 的路由句柄类型）。
/// 飞书走「回调同步回卡」(oneshot Option)；钉钉先空 ACK、卡片变化经 OpenAPI；TG/Slack 就地编辑
/// （见 `handle_select_dd_action` / `handle_select_tg_action` / `handle_select_slack_action`）。
pub(super) async fn ensure_select_route_for(
    state: &Arc<ServerState>,
    config: &AppConfig,
    channel_id: &str,
    mids: Vec<String>,
) {
    let msg_compose: HashMap<String, crate::msg_card::MsgComposePayload> = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .filter(|picker| picker.channel == channel_id && picker.kind == PickerKind::MsgCompose)
        .filter_map(|picker| {
            Some((
                picker.message_id.clone(),
                crate::msg_card::decode_payload(picker.payload.as_deref())?,
            ))
        })
        .collect();
    // 取该渠道的共享 Router（渠道不可用则跳过；picker 仍在，渠道恢复后下一拍补挂）。
    let router: WatchChannelRouter = match channel_id {
        "feishu" => {
            if !crate::app::is_feishu_active(config) {
                return;
            }
            match ensure_fs_router(state, &config.channels.feishu).await {
                Some(r) => WatchChannelRouter::Feishu(r),
                None => return,
            }
        }
        "dingding" => {
            if !crate::app::is_dingding_active(config) {
                return;
            }
            let dd = &config.channels.dingding;
            match ensure_dd_router(state, dd.client_id.trim(), dd.client_secret.trim()).await {
                Some(r) => WatchChannelRouter::DingTalk(r),
                None => return,
            }
        }
        "telegram" => {
            if !crate::app::is_telegram_active(config) {
                return;
            }
            match ensure_tg_router(state, &config.channels.telegram).await {
                Some(r) => WatchChannelRouter::Telegram(r),
                None => return,
            }
        }
        "slack" => {
            if !crate::app::is_slack_active(config) {
                return;
            }
            match ensure_sl_router(state, &config.channels.slack).await {
                Some(r) => WatchChannelRouter::Slack(r),
                None => return,
            }
        }
        _ => return,
    };
    // 现任务仍绑定同一存活 Router 且卡集合未变 → 无事可做。
    {
        let routes = state.select.routes.lock().unwrap();
        if let Some(h) = routes.get(channel_id) {
            if h.router.is_same_alive(&router) && h.mids == mids {
                return;
            }
        }
    }
    let stop = Arc::new(tokio::sync::Notify::new());
    let stop2 = stop.clone();
    let st = state.clone();
    let router_ref: WatchRouterRef = match &router {
        WatchChannelRouter::Feishu(r) => {
            let mut routed = r.register();
            for mid in &mids {
                routed.set_active(Some(mid), "");
            }
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::feishu::router::FsInbound::Card { data, ack }) => {
                                handle_select_card_action(&st, "feishu", &data, ack).await;
                            }
                            Some(_) => {} // 未认领聊天消息，不会到达；防御性忽略。
                            None => break, // Router 断开：下一拍 ensure 重建。
                        },
                    }
                }
            });
            WatchRouterRef::Feishu(Arc::downgrade(r))
        }
        WatchChannelRouter::DingTalk(r) => {
            let mut routed = r.register();
            for mid in &mids {
                routed.set_active(Some(mid), "");
            }
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::dingtalk::router::DdInbound::Card { data, ack }) => {
                                // 先 ACK 满足 3 秒回包（钉钉无「回调同步回卡」，卡片变化走 OpenAPI）。
                                // 提问模板的「提交」（待办管理卡）须回成功裁决（空包会显示「请求失败」）；
                                // 其余（单选/确认按钮）空包即可。
                                let _ = ack.send(if crate::dingtalk::card::is_submit(&data) {
                                    crate::dingtalk::card::submit_ack_success()
                                } else {
                                    serde_json::json!({})
                                });
                                handle_select_dd_action(&st, &data).await;
                            }
                            Some(_) => {} // 未认领聊天消息，不会到达；防御性忽略。
                            None => break, // Router 断开：下一拍 ensure 重建。
                        },
                    }
                }
            });
            WatchRouterRef::DingTalk(Arc::downgrade(r))
        }
        WatchChannelRouter::Telegram(r) => {
            let routed = r.register();
            for mid in &mids {
                if let Ok(m) = mid.parse::<i64>() {
                    if let Some(payload) = msg_compose.get(mid) {
                        if payload.recovered {
                            // 重启恢复的旧 ForceReply 只保留精确 reply 路由，不抢松散文字。
                            routed.set_card_route(m);
                        } else if let Ok(chat_id) = config.channels.telegram.chat_id.parse::<i64>()
                        {
                            routed.set_active(chat_id, m);
                        }
                    } else {
                        // 普通单选只认领 callback，不抢提问卡答案。
                        routed.set_card_route(m);
                    }
                }
            }
            let mut routed = routed;
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::telegram::router::TgInbound::Callback(cb)) => {
                                handle_select_tg_action(&st, &cb).await;
                            }
                            Some(crate::telegram::router::TgInbound::Text { text, message_id, reply_to_message_id }) => {
                                handle_msg_compose_tg_text(
                                    &st,
                                    &text,
                                    message_id,
                                    reply_to_message_id,
                                )
                                .await;
                            }
                            None => break,
                        },
                    }
                }
            });
            WatchRouterRef::Telegram(Arc::downgrade(r))
        }
        WatchChannelRouter::Slack(r) => {
            let mut routed = r.register();
            for mid in &mids {
                // user_id 传空 → 只认领卡片交互（message_ts），不认领聊天消息。
                routed.set_active(Some(mid), "");
            }
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::slack::router::SlInbound::Interactive(payload)) => {
                                handle_select_slack_action(&st, &payload).await;
                            }
                            Some(_) => {}
                            None => break,
                        },
                    }
                }
            });
            WatchRouterRef::Slack(Arc::downgrade(r))
        }
    };
    if let Some(old) = state.select.routes.lock().unwrap().insert(
        channel_id.to_string(),
        WatchRouteHandle {
            stop,
            router: router_ref,
            mids,
        },
    ) {
        old.stop.notify_waiters();
    }
}

/// 处理飞书单选卡 / 确认卡点击。
/// 过期 / 越界 / 无台账 → 空 ACK（静默，D7）。
pub(super) async fn handle_select_card_action(
    state: &Arc<ServerState>,
    channel_id: &str,
    data: &serde_json::Value,
    ack: crate::feishu::router::CardAck,
) {
    // Stage 双按钮确认卡。
    if let Some((mid, slot)) = crate::feishu::card::parse_confirm_action(data) {
        handle_confirm_action(state, channel_id, &mid, slot, Some(ack)).await;
        return;
    }
    if let Some(submit) = crate::feishu::card::parse_card_submit(data, &[]) {
        let is_msg_compose = state.select.pickers.lock().unwrap().iter().any(|picker| {
            picker.channel == channel_id
                && picker.message_id == submit.message_id
                && picker.kind == PickerKind::MsgCompose
        });
        if is_msg_compose {
            handle_fs_msg_compose_submit(state, channel_id, submit, ack).await;
            return;
        }
    }
    let Some((mid, idx)) = crate::feishu::card::parse_select_action(data) else {
        // 非单选点击：可能是普通 / 自动待办卡的表单提交；否则空 ACK。
        fs_todo_manage_submit(state, data, ack).await;
        return;
    };
    let picker = {
        let pickers = state.select.pickers.lock().unwrap();
        pickers
            .iter()
            .find(|p| p.channel == channel_id && p.message_id == mid)
            .cloned()
    };
    let Some(picker) = picker else {
        let _ = ack.send(None); // 已过期 / 被清理：静默（D7）。
        return;
    };
    let Some(session_id) = picker.options.get(idx).cloned() else {
        let _ = ack.send(None);
        return;
    };
    let lang = Lang::current();
    let config = state.config_snapshot();
    if session_id == crate::select::MORE_OPTION_ID
        && matches!(
            picker.kind,
            PickerKind::Todo | PickerKind::TodoRm | PickerKind::TodoAuto
        )
    {
        select_pick_todo_more(state, channel_id, &mid, &picker, &config, lang, Some(ack)).await;
        activate_channel_on_action(state, channel_id, &config, lang).await;
        return;
    }
    match picker.kind {
        PickerKind::TaskWorkspace
        | PickerKind::TaskAgent
        | PickerKind::TaskPermission
        | PickerKind::TaskInputSource
        | PickerKind::ForkSource
        | PickerKind::ForkPermission => {
            select_pick_task_flow(
                state,
                channel_id,
                &mid,
                &session_id,
                &picker,
                &config,
                lang,
                Some(ack),
            )
            .await;
        }
        PickerKind::Watch => {
            // 先完成就地变身（含卡片 ACK），再激活——避免激活的补推/回执拖慢同步 ACK。
            select_pick_watch(state, channel_id, &mid, &session_id, &config, lang, ack).await;
            // 卡片点『关注』＝在该渠道操作 → 设为活跃槽（与 /watch 一致，用户决策）。
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        PickerKind::Status => {
            // 单选卡不动：先空 ACK，再回纯文本详情（可继续点其它 agent）。
            let _ = ack.send(None);
            let snapshot = state.agents.snapshot();
            let text = status_detail_by_session(&snapshot, &session_id, channel_id, lang);
            let _ = reply_channel_text(channel_id, &config, &text).await;
            // 卡片点『查看』＝在该渠道操作 → 设为活跃槽（补齐与 /status 文本命令的一致性）。
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        PickerKind::Unwatch => {
            select_pick_unwatch(state, channel_id, &mid, &session_id, &config, lang, ack).await;
        }
        PickerKind::Msg => {
            if let Some(content) = picker.payload.as_deref() {
                select_pick_msg(state, channel_id, &mid, &session_id, content, lang, ack).await;
            } else {
                fs_select_pick_msg_compose(
                    state,
                    channel_id,
                    &mid,
                    &session_id,
                    &config,
                    lang,
                    ack,
                )
                .await;
            }
            // 卡片点『发送』＝在该渠道操作 → 设为活跃槽（与 /msg 一致，用户决策）。
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        PickerKind::MsgCompose => {
            let _ = ack.send(None);
        }
        PickerKind::Diff | PickerKind::Stage | PickerKind::Transcript => {
            select_pick_export(
                state,
                channel_id,
                &mid,
                &session_id,
                picker.kind,
                &config,
                lang,
                Some(ack),
            )
            .await;
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        // /todo · /todo-rm（spec todo-whats-next D8）。
        PickerKind::Todo => {
            fs_select_pick_todo(
                state,
                &mid,
                &session_id,
                picker.payload.as_deref(),
                lang,
                ack,
            )
            .await;
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        PickerKind::TodoRm => {
            fs_select_pick_todo_rm(state, &mid, &session_id, lang, ack).await;
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        PickerKind::TodoRmEntry => {
            fs_select_pick_todo_rm_entry(state, &mid, &session_id, &picker, lang, ack).await;
        }
        PickerKind::TodoAuto => {
            fs_select_pick_todo_auto(
                state,
                &mid,
                &session_id,
                picker.payload.as_deref(),
                lang,
                ack,
            )
            .await;
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
        PickerKind::TodoAutoEntry => {
            fs_select_pick_todo_auto_entry(state, &mid, &session_id, &picker, lang, ack).await;
        }
        // 管理卡无行按钮（options 恒空，上方取选项即已短路）；防御性空 ACK。
        PickerKind::TodoManage => {
            let _ = ack.send(None);
        }
        PickerKind::Yolo => {
            select_pick_yolo(
                state,
                channel_id,
                &mid,
                &session_id,
                &config,
                lang,
                Some(ack),
            )
            .await;
            activate_channel_on_action(state, channel_id, &config, lang).await;
        }
    }
}

async fn handle_fs_msg_compose_submit(
    state: &Arc<ServerState>,
    channel_id: &str,
    submit: crate::feishu::card::CardSubmit,
    ack: crate::feishu::router::CardAck,
) {
    let picker = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .find(|picker| {
            picker.channel == channel_id
                && picker.message_id == submit.message_id
                && picker.kind == PickerKind::MsgCompose
        })
        .cloned();
    let Some(picker) = picker else {
        let _ = ack.send(None);
        return;
    };
    let Some(payload) = crate::msg_card::decode_payload(picker.payload.as_deref()) else {
        let _ = ack.send(None);
        return;
    };
    let lang = Lang::current();
    let config = state.config_snapshot();
    if payload.recovered || now_secs() >= payload.expires_at {
        let label = msg_compose_expired_label(channel_id, lang);
        let card = crate::feishu::card::build_select_final_card(&picker.title, &label);
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, &submit.message_id);
        return;
    }
    let snapshot = state.agents.snapshot();
    let Some(rec) = find_agent_by_session(&snapshot, &payload.session_id) else {
        let card = crate::feishu::card::build_select_final_card(
            &picker.title,
            crate::i18n::tr(lang, "select.msgTargetGone"),
        );
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, &submit.message_id);
        return;
    };
    if !is_working_non_grok(&snapshot, &payload.session_id) {
        let card = crate::feishu::card::build_select_final_card(
            &picker.title,
            crate::i18n::tr(lang, "select.msgTargetGone"),
        );
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, &submit.message_id);
        return;
    }
    let content = match crate::msg_card::validate_input(submit.user_input.as_deref(), lang) {
        Ok(content) => content,
        Err(error) => {
            let view = crate::msg_card::build_view(
                rec,
                state.interject.pending_count(&payload.session_id),
                &state.interject.full_text(&payload.session_id),
                Some(error),
                lang,
            );
            let card =
                crate::feishu::card::build_msg_compose_card(&view, submit.user_input.as_deref());
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
            return;
        }
    };
    if take_msg_compose_picker(state, channel_id, &submit.message_id).is_none() {
        let _ = ack.send(None);
        return;
    }
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, &payload.session_id);
    let label = msg_pick_deliver(state, channel_id, &payload.session_id, rec, &content, lang);
    let card = crate::feishu::card::build_select_final_card(&picker.title, &label);
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    state.select.route_refresh.notify_one();
    activate_channel_on_action(state, channel_id, &config, lang).await;
}

fn msg_compose_expired_label(channel_id: &str, lang: Lang) -> String {
    crate::i18n::tr(lang, "msgCard.expired")
        .replace("{p}", crate::autochannel::cmd_prefix(channel_id))
}

/// 单选卡点选「发送」（飞书就地定格）：校验目标工作中·非 grok → 投递 + 定格「已发送给 [编号]」；
/// 目标已漂移（不在工作中 / 已结束 / 消失）→ 定格「已不在工作中，未发送」。定格文案随卡回（ack）。
#[allow(clippy::too_many_arguments)]
pub(super) async fn select_pick_msg(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    content: &str,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let label = msg_pick_deliver(state, channel_id, session_id, rec, content, lang);
    let card =
        crate::feishu::card::build_select_final_card(&crate::select::title_msg(lang), &label);
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    remove_picker(state, channel_id, mid);
}

fn morph_picker_to_msg_compose(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    title: String,
) -> bool {
    let Some(payload) = msg_compose_payload(session_id, false) else {
        return false;
    };
    let changed = {
        let mut pickers = state.select.pickers.lock().unwrap();
        let Some(picker) = pickers
            .iter_mut()
            .find(|picker| picker.channel == channel_id && picker.message_id == mid)
        else {
            return false;
        };
        picker.kind = PickerKind::MsgCompose;
        picker.title = title;
        picker.options.clear();
        picker.payload = Some(payload);
        picker.created_at = now_secs();
        true
    };
    if changed {
        persist_msg_compose_recovery(state);
        state.select.route_refresh.notify_one();
    }
    changed
}

#[allow(clippy::too_many_arguments)]
async fn fs_select_pick_msg_compose(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    _config: &AppConfig,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    let snapshot = state.agents.snapshot();
    let Some(rec) = find_agent_by_session(&snapshot, session_id) else {
        let card = crate::feishu::card::build_select_final_card(
            &crate::select::title_msg(lang),
            crate::i18n::tr(lang, "select.msgTargetGone"),
        );
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, mid);
        return;
    };
    if !is_working_non_grok(&snapshot, session_id) {
        let card = crate::feishu::card::build_select_final_card(
            &crate::select::title_msg(lang),
            crate::i18n::tr(lang, "select.msgTargetGone"),
        );
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, mid);
        return;
    }
    let view = crate::msg_card::build_view(
        rec,
        state.interject.pending_count(session_id),
        &state.interject.full_text(session_id),
        None,
        lang,
    );
    let card = crate::feishu::card::build_msg_compose_card(&view, None);
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    morph_picker_to_msg_compose(state, channel_id, mid, session_id, view.title);
}

/// 单选卡「发送」的共享收尾：目标仍工作中·非 grok → 投递并返回「已发送给 [编号] · 回执」定格文案；
/// 否则返回「已不在工作中，未发送」。渲染层各渠道自行把该文案落进定格卡。
pub(super) fn msg_pick_deliver(
    state: &Arc<ServerState>,
    channel_id: &str,
    session_id: &str,
    rec: Option<&serde_json::Value>,
    content: &str,
    lang: Lang,
) -> String {
    let ok = rec
        .map(|r| {
            r.get("state").and_then(|v| v.as_str()) == Some("working")
                && r.get("kind").and_then(|v| v.as_str()) != Some("grok")
        })
        .unwrap_or(false);
    if !ok {
        return crate::i18n::tr(lang, "select.msgTargetGone").to_string();
    }
    let seq = rec
        .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let delivered = deliver_msg(state, channel_id, session_id, content, lang);
    let note = if delivered == crate::i18n::tr(lang, "autoChannel.msgDeliveredNow") {
        delivered
    } else {
        crate::i18n::tr(lang, "msgCard.queuedShort").to_string()
    };
    crate::i18n::tr(lang, "select.msgSentCard")
        .replace("{id}", &seq.to_string())
        .replace("{note}", &note)
}

/// 单选卡点选「watch」：就地把这张卡编辑成实时 watch 卡（经 oneshot 同步回卡）。
#[allow(clippy::too_many_arguments)]
pub(super) async fn select_pick_watch(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&session_id.to_string());
    let seq = rec
        .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let frame = crate::watch::build_frame(seq, rec, waiting);
    let ended = frame.phase == crate::watch::WatchPhase::Ended;
    if ended {
        // 已结束/消失：就地定格终态卡、不订阅、消费掉 picker。
        let card = crate::feishu::card::build_watch_card(&crate::watch::card_view(
            &frame,
            crate::watch::CardMode::Final(crate::watch::FinalKind::Ended),
            now,
            lang,
            None,
        ));
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, mid);
        // 不在此重挂 select 路由（避免 recv-loop 递归 → 非 Send）：残留的 mid 认领无害（卡已定格无按钮），
        // 下次 send_agent_picker / 监听重建时统一收敛。
        return;
    }
    // 上限校验（本渠道；已在关注同一 session＝换新卡，不计新增）。
    let already = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.channel == channel_id && s.session_id == session_id);
    if !already {
        let count = state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.channel == channel_id && !s.rewatchable)
            .count();
        if count >= crate::watch::MAX_WATCHES {
            let _ = ack.send(None);
            let text = crate::i18n::tr(lang, "watch.limit")
                .replace("{n}", &crate::watch::MAX_WATCHES.to_string())
                .replace("{p}", crate::autochannel::cmd_prefix(channel_id));
            let _ = reply_channel_text(channel_id, config, &text).await;
            return;
        }
    }
    // 就地回一张实时 watch 卡（这条单选卡消息随即变成 watch 卡）。
    let card = crate::feishu::card::build_watch_card(&crate::watch::card_view(
        &frame,
        crate::watch::CardMode::Active,
        now,
        lang,
        None,
    ));
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    // 登记订阅（含换新卡收尾）+ 消费 picker + 让 watch 立即认领本消息（`ensure_watch_routes` 不会递归
    // 回 select）。select 侧不在此重挂（避免 recv-loop 递归 → 非 Send）：本 mid 已被 watch 认领覆盖，
    // 残留的 select 认领无害，下次 send_agent_picker / 监听重建时收敛。
    register_watch_at(
        state, channel_id, session_id, seq, mid, &frame, false, config, lang,
    )
    .await;
    remove_picker(state, channel_id, mid);
    ensure_watch_routes(state).await;
}

/// 单选卡点选「unwatch」：取消该关注（旧卡定格）+ 回文本确认 + 就地刷新单选卡（移除该项）。
#[allow(clippy::too_many_arguments)]
pub(super) async fn select_pick_unwatch(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    let now = now_secs();
    // 找到该 session 在本渠道的订阅（可能已被别处取消/结束 → 视为已不在关注，只刷新卡）。
    let entry = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.channel == channel_id && s.session_id == session_id)
        .cloned();
    if let Some(entry) = entry {
        // 旧 watch 卡定格「已取消关注」（可重新关注）。
        if let Some(client) = watch_client(state, channel_id, config).await {
            let snapshot = state.agents.snapshot();
            let waiting = state
                .registry
                .in_flight_agent_session_ids()
                .contains(&entry.session_id);
            let frame = crate::watch::build_frame(
                entry.seq,
                find_agent_by_session(&snapshot, &entry.session_id),
                waiting,
            );
            if let Err(err) = client
                .edit(
                    &entry.message_id,
                    &frame,
                    crate::watch::CardMode::Final(crate::watch::FinalKind::Cancelled),
                    now,
                    lang,
                    Some(&entry.session_id),
                )
                .await
            {
                log(&format!("select: finalize unwatch card failed: {}", err));
            }
        }
        {
            let mut subs = state.watch.subs.lock().unwrap();
            if let Some(s) = subs.iter_mut().find(|s| s.message_id == entry.message_id) {
                s.rewatchable = true;
            }
        }
        persist_watch_subs(state);
        state.watch.notify.notify_one();
        ensure_watch_routes(state).await;
        let text =
            crate::i18n::tr(lang, "watch.unwatchDone").replace("{id}", &entry.seq.to_string());
        let _ = reply_channel_text(channel_id, config, &text).await;
    }
    // 就地刷新单选卡：剩余订阅 → 新卡；空 → 定格「已全部取消关注」并消费 picker。
    let snapshot = state.agents.snapshot();
    let options = unwatch_options(state, channel_id, &snapshot, lang);
    if options.is_empty() {
        let card = crate::feishu::card::build_select_final_card(
            &crate::select::title_unwatch(lang),
            crate::i18n::tr(lang, "select.unwatchAllDoneCard"),
        );
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, channel_id, mid);
        // 不在此重挂 select 路由（同 select_pick_watch 理由）：卡已定格无按钮，残留认领无害。
    } else {
        let view = crate::select::build_view(
            crate::select::title_unwatch(lang),
            options,
            crate::select::SelectAction::Unwatch,
            lang,
        );
        let new_ids: Vec<String> = view.options.iter().map(|o| o.id.clone()).collect();
        let card = crate::feishu::card::build_select_card(&view);
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        // 更新 picker 的选项快照（下标对齐新卡）。
        if let Some(p) = state
            .select
            .pickers
            .lock()
            .unwrap()
            .iter_mut()
            .find(|p| p.channel == channel_id && p.message_id == mid)
        {
            p.options = new_ids;
        }
    }
}

// ===== /yolo（spec codex-permission-remember D53）=====

/// `/yolo` 选择卡的会话显示名：对话标题 → 项目名 → 缩短的 session id（与管理面板同口径）。
pub(super) fn yolo_session_label(state: &Arc<ServerState>, session_id: &str) -> String {
    if let Some((title, project)) = state.agents.session_display(session_id) {
        if !title.is_empty() {
            return title;
        }
        if !project.is_empty() {
            return project;
        }
    }
    if session_id.len() > 12 {
        format!("{}…", &session_id[..8])
    } else {
        session_id.to_string()
    }
}

/// `/yolo` 选择卡选项：`ids` 已按最近使用倒序（[`yolo_session_ids`] 的顺序）。
/// 展示口径与其余 agent 卡一致（见 [`crate::select::yolo_option_by_session`]）。
pub(super) fn yolo_options(
    state: &Arc<ServerState>,
    ids: &[String],
    lang: Lang,
) -> Vec<crate::select::SelectOption> {
    let snapshot = state.agents.snapshot();
    let now = now_secs();
    ids.iter()
        .map(|session_id| {
            crate::select::yolo_option_by_session(
                &snapshot,
                session_id,
                state.agents.session_display(session_id),
                now,
                lang,
            )
        })
        .collect()
}

/// 单选卡点选「关闭」（/yolo）：只删该会话的 Yolo 规则 → 本卡定格终态文案。三路复用：
/// 飞书经 ack 同步回卡，钉钉走 OpenAPI 终态，TG/Slack 就地编辑。规则已不在（别处关了 /
/// 过期）→ 定格「已不在开启状态」。
pub(super) async fn select_pick_yolo(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
    ack: Option<crate::confirm::transport::FsAck>,
) {
    let sid = session_id.to_string();
    let removed = tokio::task::spawn_blocking(move || crate::permission_rules::disable_yolo(&sid))
        .await
        .unwrap_or(false);
    let label = if removed {
        log(&format!(
            "yolo turned off via {channel_id} for session {session_id}"
        ));
        crate::i18n::tr(lang, "select.yoloOffDone")
            .replace("{name}", &yolo_session_label(state, session_id))
    } else {
        crate::i18n::tr(lang, "select.yoloOffGone").to_string()
    };
    let title = crate::select::title_yolo(lang);
    if channel_id == "feishu" {
        if let Some(ack) = ack {
            let card = crate::feishu::card::build_select_final_card(&title, &label);
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        }
    } else if channel_id == "dingding" {
        dd_finalize_select_card(config, mid, &label).await;
    } else {
        finalize_select_card_edit(channel_id, config, mid, &title, &label).await;
    }
    remove_picker(state, channel_id, mid);
}

// ===== 钉钉单选卡点选（无「回调同步回卡」：空 ACK 已在路由任务发出，卡片变化走 OpenAPI）=====

/// 处理钉钉单选卡点击：解析 `(outTrackId, sid)` → 找 picker → 按 kind 分派。
/// 过期 / sid 空 / 不属于本卡 → 静默（D7；空 ACK 已由路由任务发出）。
pub(super) async fn handle_select_dd_action(state: &Arc<ServerState>, data: &serde_json::Value) {
    // Stage 确认卡（提问模板）：提交后按选项 0=暂存 / 1=取消。
    if handle_stage_dd_submit(state, data).await {
        return;
    }
    if handle_msg_compose_dd_submit(state, data).await {
        return;
    }
    // 待办管理卡「新增」提交（提问卡模板复用，spec todo-whats-next D8）。
    if handle_todo_dd_submit(state, data).await {
        return;
    }
    let Some((otid, session_id)) = crate::dingtalk::select::parse_select_action(data) else {
        return;
    };
    let picker = {
        let pickers = state.select.pickers.lock().unwrap();
        pickers
            .iter()
            .find(|p| p.channel == "dingding" && p.message_id == otid)
            .cloned()
    };
    let Some(picker) = picker else {
        return; // 已过期 / 被清理：静默（D7）。
    };
    // 路由靠 param 回传的 session_id（不用会漂移的编号）；空 / 不属于本卡 → 无效（模板未绑定或已变）。
    if session_id.is_empty() || !picker.options.contains(&session_id) {
        return;
    }
    let lang = Lang::current();
    let config = state.config_snapshot();
    if session_id == crate::select::MORE_OPTION_ID
        && matches!(
            picker.kind,
            PickerKind::Todo | PickerKind::TodoRm | PickerKind::TodoAuto
        )
    {
        select_pick_todo_more(state, "dingding", &otid, &picker, &config, lang, None).await;
        activate_channel_on_action(state, "dingding", &config, lang).await;
        return;
    }
    match picker.kind {
        PickerKind::TaskWorkspace
        | PickerKind::TaskAgent
        | PickerKind::TaskPermission
        | PickerKind::TaskInputSource
        | PickerKind::ForkSource
        | PickerKind::ForkPermission => {
            select_pick_task_flow(
                state,
                "dingding",
                &otid,
                &session_id,
                &picker,
                &config,
                lang,
                None,
            )
            .await;
        }
        PickerKind::Watch => {
            dd_select_pick_watch(state, &otid, &session_id, &config, lang).await;
            // 卡片点『关注』＝在该渠道操作 → 设为活跃槽（与 /watch 一致，用户决策）。
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        PickerKind::Status => {
            // 单选卡不动：回纯文本详情（可继续点其它 agent）。
            let snapshot = state.agents.snapshot();
            let text = status_detail_by_session(&snapshot, &session_id, "dingding", lang);
            let _ = reply_channel_text("dingding", &config, &text).await;
            // 卡片点『查看』＝在该渠道操作 → 设为活跃槽（补齐与 /status 文本命令的一致性）。
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        PickerKind::Unwatch => {
            dd_select_pick_unwatch(state, &otid, &session_id, &config, lang).await;
        }
        PickerKind::Msg => {
            if let Some(content) = picker.payload.as_deref() {
                dd_select_pick_msg(state, &otid, &session_id, content, &config, lang).await;
            } else {
                select_pick_msg_compose_sequential(
                    state,
                    "dingding",
                    &otid,
                    &session_id,
                    &config,
                    lang,
                )
                .await;
            }
            // 卡片点『发送』＝在该渠道操作 → 设为活跃槽（与 /msg 一致，用户决策）。
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        PickerKind::MsgCompose => {}
        PickerKind::Diff | PickerKind::Stage | PickerKind::Transcript => {
            select_pick_export(
                state,
                "dingding",
                &otid,
                &session_id,
                picker.kind,
                &config,
                lang,
                None,
            )
            .await;
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        // /todo · /todo-rm（spec todo-whats-next D8）。
        PickerKind::Todo => {
            dd_select_pick_todo(
                state,
                &otid,
                &session_id,
                picker.payload.as_deref(),
                &config,
                lang,
            )
            .await;
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        PickerKind::TodoRm => {
            dd_select_pick_todo_rm(state, &otid, &session_id, &config, lang).await;
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        PickerKind::TodoRmEntry => {
            dd_select_pick_todo_rm_entry(state, &otid, &session_id, &picker, &config, lang).await;
        }
        PickerKind::TodoAuto => {
            dd_select_pick_todo_auto(
                state,
                &otid,
                &session_id,
                picker.payload.as_deref(),
                &config,
                lang,
            )
            .await;
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
        PickerKind::TodoAutoEntry => {
            dd_select_pick_todo_auto_entry(state, &otid, &session_id, &picker, &config, lang).await;
        }
        // 管理卡提交已在上方 handle_todo_dd_submit 处理；行按钮不存在。
        PickerKind::TodoManage => {}
        PickerKind::Yolo => {
            select_pick_yolo(state, "dingding", &otid, &session_id, &config, lang, None).await;
            activate_channel_on_action(state, "dingding", &config, lang).await;
        }
    }
}

async fn handle_msg_compose_dd_submit(state: &Arc<ServerState>, data: &serde_json::Value) -> bool {
    let Some(submit) = crate::dingtalk::card::parse_card_submit(data) else {
        return false;
    };
    let picker = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .find(|picker| {
            picker.channel == "dingding"
                && picker.message_id == submit.out_track_id
                && picker.kind == PickerKind::MsgCompose
        })
        .cloned();
    let Some(picker) = picker else {
        return false;
    };
    let Some(payload) = crate::msg_card::decode_payload(picker.payload.as_deref()) else {
        return true;
    };
    let lang = Lang::current();
    let config = state.config_snapshot();
    let Ok(client) = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) else {
        return true;
    };
    if payload.recovered || now_secs() >= payload.expires_at {
        let _ = client
            .update_card_private(
                &submit.out_track_id,
                serde_json::json!({ "submit_status": msg_compose_expired_label("dingding", lang) }),
                serde_json::json!({ "submitted": "true" }),
            )
            .await;
        remove_picker(state, "dingding", &submit.out_track_id);
        return true;
    }
    let snapshot = state.agents.snapshot();
    let Some(rec) = find_agent_by_session(&snapshot, &payload.session_id) else {
        let _ = client
            .update_card_private(
                &submit.out_track_id,
                serde_json::json!({ "submit_status": crate::i18n::tr(lang, "select.msgTargetGone") }),
                serde_json::json!({ "submitted": "true" }),
            )
            .await;
        remove_picker(state, "dingding", &submit.out_track_id);
        return true;
    };
    if !is_working_non_grok(&snapshot, &payload.session_id) {
        let _ = client
            .update_card_private(
                &submit.out_track_id,
                serde_json::json!({ "submit_status": crate::i18n::tr(lang, "select.msgTargetGone") }),
                serde_json::json!({ "submitted": "true" }),
            )
            .await;
        remove_picker(state, "dingding", &submit.out_track_id);
        return true;
    }
    let content = match crate::msg_card::validate_input(submit.user_input.as_deref(), lang) {
        Ok(content) => content,
        Err(error) => {
            let view = crate::msg_card::build_view(
                rec,
                state.interject.pending_count(&payload.session_id),
                &state.interject.full_text(&payload.session_id),
                Some(error),
                lang,
            );
            let map = crate::dingtalk::card::build_card_param_map(
                &view.title,
                &crate::msg_card::escape_markdown(&view.plain_body()),
                &[],
                false,
                false,
                "",
            );
            let _ = client
                .update_card_private(
                    &submit.out_track_id,
                    map,
                    serde_json::json!({
                        "submitted": "false",
                        "private_input": submit.user_input.unwrap_or_default(),
                    }),
                )
                .await;
            return true;
        }
    };
    if take_msg_compose_picker(state, "dingding", &submit.out_track_id).is_none() {
        return true;
    }
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, &payload.session_id);
    let label = msg_pick_deliver(state, "dingding", &payload.session_id, rec, &content, lang);
    let _ = client
        .update_card_private(
            &submit.out_track_id,
            serde_json::json!({ "submit_status": label }),
            serde_json::json!({ "submitted": "true" }),
        )
        .await;
    state.select.route_refresh.notify_one();
    activate_channel_on_action(state, "dingding", &config, lang).await;
    true
}

/// 钉钉单选卡点选「发送」：投递（若目标仍工作中·非 grok）+ 单选卡定格（OpenAPI 更新）。
pub(super) async fn dd_select_pick_msg(
    state: &Arc<ServerState>,
    otid: &str,
    session_id: &str,
    content: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let label = msg_pick_deliver(state, "dingding", session_id, rec, content, lang);
    dd_finalize_select_card(config, otid, &label).await;
    remove_picker(state, "dingding", otid);
}

/// 钉钉单选卡点选「watch」：钉钉不能就地变身（模板固定），故**另发一张新的实时 watch 卡** +
/// 把单选卡定格「已选择 [n]」。已在关注同一 session ＝换新卡（`register_watch_at` 定格旧卡）。
pub(super) async fn dd_select_pick_watch(
    state: &Arc<ServerState>,
    otid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&session_id.to_string());
    let seq = rec
        .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let frame = crate::watch::build_frame(seq, rec, waiting);
    let ended = frame.phase == crate::watch::WatchPhase::Ended;
    // 上限校验（本渠道；已在关注同一 session＝换新卡，不计新增；已结束不订阅、不计数）。
    let already = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.channel == "dingding" && s.session_id == session_id);
    if !ended && !already {
        let count = state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.channel == "dingding" && !s.rewatchable)
            .count();
        if count >= crate::watch::MAX_WATCHES {
            let text = crate::i18n::tr(lang, "watch.limit")
                .replace("{n}", &crate::watch::MAX_WATCHES.to_string())
                .replace("{p}", crate::autochannel::cmd_prefix("dingding"));
            let _ = reply_channel_text("dingding", config, &text).await;
            return; // 单选卡不动，可另选。
        }
    }
    // 另发一张实时 watch 卡（活动态活卡 / 已结束则终态卡）。
    let Some(client) = watch_client(state, "dingding", config).await else {
        return;
    };
    let mode = if ended {
        crate::watch::CardMode::Final(crate::watch::FinalKind::Ended)
    } else {
        crate::watch::CardMode::Active
    };
    let new_mid = match client.send(&frame, mode, now, lang).await {
        Ok(m) => m,
        Err(err) => {
            log(&format!("select: send dingtalk watch card failed: {}", err));
            return;
        }
    };
    // 登记订阅（含换新卡：本渠道同 session 旧卡定格 Replaced）+ 让 watch 引擎认领新卡按钮。
    register_watch_at(
        state, "dingding", session_id, seq, &new_mid, &frame, ended, config, lang,
    )
    .await;
    ensure_watch_routes(state).await;
    // 单选卡定格「已选择 [n]」并消费 picker。
    let label = crate::i18n::tr(lang, "select.pickedCard").replace("{id}", &seq.to_string());
    dd_finalize_select_card(config, otid, &label).await;
    remove_picker(state, "dingding", otid);
}

/// 钉钉单选卡点选「unwatch」：取消该关注（旧 watch 卡定格）+ 回文本确认 + 就地刷新单选卡
/// （经 OpenAPI 更新 loop；取到 0 则定格「已全部取消关注」）。
pub(super) async fn dd_select_pick_unwatch(
    state: &Arc<ServerState>,
    otid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let now = now_secs();
    let entry = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.channel == "dingding" && s.session_id == session_id)
        .cloned();
    if let Some(entry) = entry {
        if let Some(client) = watch_client(state, "dingding", config).await {
            let snapshot = state.agents.snapshot();
            let waiting = state
                .registry
                .in_flight_agent_session_ids()
                .contains(&entry.session_id);
            let frame = crate::watch::build_frame(
                entry.seq,
                find_agent_by_session(&snapshot, &entry.session_id),
                waiting,
            );
            if let Err(err) = client
                .edit(
                    &entry.message_id,
                    &frame,
                    crate::watch::CardMode::Final(crate::watch::FinalKind::Cancelled),
                    now,
                    lang,
                    Some(&entry.session_id),
                )
                .await
            {
                log(&format!(
                    "select: finalize dingtalk unwatch card failed: {}",
                    err
                ));
            }
        }
        {
            let mut subs = state.watch.subs.lock().unwrap();
            if let Some(s) = subs.iter_mut().find(|s| s.message_id == entry.message_id) {
                s.rewatchable = true;
            }
        }
        persist_watch_subs(state);
        state.watch.notify.notify_one();
        ensure_watch_routes(state).await;
        let text =
            crate::i18n::tr(lang, "watch.unwatchDone").replace("{id}", &entry.seq.to_string());
        let _ = reply_channel_text("dingding", config, &text).await;
    }
    // 就地刷新单选卡：剩余订阅 → 更新 loop；空 → 定格「已全部取消关注」并消费 picker。
    let snapshot = state.agents.snapshot();
    let options = unwatch_options(state, "dingding", &snapshot, lang);
    if options.is_empty() {
        dd_finalize_select_card(
            config,
            otid,
            crate::i18n::tr(lang, "select.unwatchAllDoneCard"),
        )
        .await;
        remove_picker(state, "dingding", otid);
    } else {
        let view = crate::select::build_view(
            crate::select::title_unwatch(lang),
            options,
            crate::select::SelectAction::Unwatch,
            lang,
        );
        let new_ids: Vec<String> = view.options.iter().map(|o| o.id.clone()).collect();
        if let Ok(client) = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
        {
            let map = crate::dingtalk::select::build_select_param_map(&view, lang);
            if let Err(err) = client
                .update_card_private(otid, map, serde_json::json!({}))
                .await
            {
                log(&format!(
                    "select: refresh dingtalk unwatch card failed: {}",
                    err
                ));
            }
        }
        if let Some(p) = state
            .select
            .pickers
            .lock()
            .unwrap()
            .iter_mut()
            .find(|p| p.channel == "dingding" && p.message_id == otid)
        {
            p.options = new_ids;
        }
    }
}

/// 定格一张钉钉单选卡（按 key 更新公有 `finalized=true` + `final_label`）：隐藏循环、显示定格文案。
pub(super) async fn dd_finalize_select_card(config: &AppConfig, otid: &str, final_label: &str) {
    let _ = try_dd_finalize_select_card(config, otid, final_label).await;
}

async fn try_dd_finalize_select_card(config: &AppConfig, otid: &str, final_label: &str) -> bool {
    if let Ok(client) = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) {
        let map = crate::dingtalk::select::build_select_final_param_map(final_label);
        if let Err(err) = client
            .update_card_private(otid, map, serde_json::json!({}))
            .await
        {
            log(&format!(
                "select: finalize dingtalk select card failed: {}",
                err
            ));
            return false;
        }
        return true;
    }
    false
}

// ===== Telegram / Slack 单选卡点选（可就地编辑：点 watch → 本消息变身为实时 watch 卡）=====

/// 处理 Telegram 单选卡 / 确认卡点击：应答消除转圈 → 解析 → 分派。
pub(super) async fn handle_select_tg_action(state: &Arc<ServerState>, cb: &serde_json::Value) {
    let data = cb.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let Some(mid) = cb
        .get("message")
        .and_then(|m| m.get("message_id"))
        .and_then(|v| v.as_i64())
    else {
        return;
    };
    let config = state.config_snapshot();
    // 应答 callback（消除客户端转圈，best-effort）。
    if let Some(id) = cb.get("id").and_then(|i| i.as_str()) {
        let tg = &config.channels.telegram;
        if let Ok(c) = crate::telegram::TelegramClient::new(
            tg.bot_token.clone(),
            tg.chat_id.clone(),
            tg.api_base_url.clone(),
        ) {
            c.answer_callback_query(id).await;
        }
    }
    if let Some(slot) = crate::telegram::confirm::parse_confirm_action(data) {
        handle_confirm_action(state, "telegram", &mid.to_string(), slot, None).await;
        return;
    }
    let Some(idx) = crate::telegram::select::parse_select_action(data) else {
        return;
    };
    dispatch_select_pick(state, "telegram", &mid.to_string(), idx, &config).await;
}

async fn handle_msg_compose_tg_text(
    state: &Arc<ServerState>,
    text: &str,
    message_id: i64,
    reply_to_message_id: Option<i64>,
) {
    let Some(prompt_mid) = reply_to_message_id else {
        return;
    };
    let prompt_mid = prompt_mid.to_string();
    let picker = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .find(|picker| {
            picker.channel == "telegram"
                && picker.message_id == prompt_mid
                && picker.kind == PickerKind::MsgCompose
        })
        .cloned();
    let Some(picker) = picker else { return };
    let Some(payload) = crate::msg_card::decode_payload(picker.payload.as_deref()) else {
        return;
    };
    let lang = Lang::current();
    let config = state.config_snapshot();
    let tg = &config.channels.telegram;
    let Ok(client) = crate::telegram::TelegramClient::new(
        tg.bot_token.clone(),
        tg.chat_id.clone(),
        tg.api_base_url.clone(),
    ) else {
        return;
    };
    let Ok(mid) = prompt_mid.parse::<i64>() else {
        return;
    };
    if payload.recovered || now_secs() >= payload.expires_at {
        let label = msg_compose_expired_label("telegram", lang);
        finalize_msg_compose_telegram(&client, mid, message_id, &label).await;
        remove_picker(state, "telegram", &prompt_mid);
        return;
    }
    let snapshot = state.agents.snapshot();
    if !is_working_non_grok(&snapshot, &payload.session_id) {
        finalize_msg_compose_telegram(
            &client,
            mid,
            message_id,
            crate::i18n::tr(lang, "select.msgTargetGone"),
        )
        .await;
        remove_picker(state, "telegram", &prompt_mid);
        return;
    }
    let content = match crate::msg_card::validate_input(Some(text), lang) {
        Ok(content) => content,
        Err(error) => {
            // ForceReply 无法通过 editMessageText 重新唤起，保留原 prompt 并回一条校验提示。
            let _ = client.send_reply_message(message_id, &error).await;
            return;
        }
    };
    if take_msg_compose_picker(state, "telegram", &prompt_mid).is_none() {
        return;
    }
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, &payload.session_id);
    let label = msg_pick_deliver(state, "telegram", &payload.session_id, rec, &content, lang);
    finalize_msg_compose_telegram(&client, mid, message_id, &label).await;
    state.select.route_refresh.notify_one();
    activate_channel_on_action(state, "telegram", &config, lang).await;
}

async fn finalize_msg_compose_telegram(
    client: &crate::telegram::TelegramClient,
    prompt_message_id: i64,
    reply_message_id: i64,
    label: &str,
) {
    if let Err(err) = client.delete_message(prompt_message_id).await {
        log(&format!(
            "msg compose: delete telegram ForceReply prompt failed: {}",
            err
        ));
    }
    if let Err(err) = client.send_reply_message(reply_message_id, label).await {
        log(&format!(
            "msg compose: send telegram terminal reply failed: {}",
            err
        ));
    }
}

/// 处理 Slack 单选卡 / 确认卡点击（ack 已在 ws 层完成）。
pub(super) async fn handle_select_slack_action(
    state: &Arc<ServerState>,
    payload: &serde_json::Value,
) {
    let config = state.config_snapshot();
    if let Some((ts, slot)) = crate::slack::confirm::parse_confirm_action(payload) {
        handle_confirm_action(state, "slack", &ts, slot, None).await;
        return;
    }
    if let Some(submit) = crate::slack::blockkit::parse_submit(payload, &[]) {
        let is_msg_compose = state.select.pickers.lock().unwrap().iter().any(|picker| {
            picker.channel == "slack"
                && picker.message_id == submit.message_ts
                && picker.kind == PickerKind::MsgCompose
        });
        if is_msg_compose {
            handle_msg_compose_slack_submit(state, submit, &config).await;
            return;
        }
    }
    let Some((ts, idx)) = crate::slack::select::parse_select_action(payload) else {
        return;
    };
    dispatch_select_pick(state, "slack", &ts, idx, &config).await;
}

async fn handle_msg_compose_slack_submit(
    state: &Arc<ServerState>,
    submit: crate::slack::blockkit::CardSubmit,
    config: &AppConfig,
) {
    let picker = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .find(|picker| {
            picker.channel == "slack"
                && picker.message_id == submit.message_ts
                && picker.kind == PickerKind::MsgCompose
        })
        .cloned();
    let Some(picker) = picker else { return };
    let Some(payload) = crate::msg_card::decode_payload(picker.payload.as_deref()) else {
        return;
    };
    let lang = Lang::current();
    let Ok(client) = crate::slack::client::SlackClient::new(&config.channels.slack) else {
        return;
    };
    let Ok(dm) = client.open_dm().await else {
        return;
    };
    if payload.recovered || now_secs() >= payload.expires_at {
        let label = msg_compose_expired_label("slack", lang);
        let (blocks, fallback) =
            crate::slack::select::build_select_final_blocks(&picker.title, &label);
        let _ = client
            .update_message(&dm, &submit.message_ts, Some(&blocks), &fallback)
            .await;
        remove_picker(state, "slack", &submit.message_ts);
        return;
    }
    let snapshot = state.agents.snapshot();
    let Some(rec) = find_agent_by_session(&snapshot, &payload.session_id) else {
        let label = crate::i18n::tr(lang, "select.msgTargetGone");
        let (blocks, fallback) =
            crate::slack::select::build_select_final_blocks(&picker.title, label);
        let _ = client
            .update_message(&dm, &submit.message_ts, Some(&blocks), &fallback)
            .await;
        remove_picker(state, "slack", &submit.message_ts);
        return;
    };
    if !is_working_non_grok(&snapshot, &payload.session_id) {
        let label = crate::i18n::tr(lang, "select.msgTargetGone");
        let (blocks, fallback) =
            crate::slack::select::build_select_final_blocks(&picker.title, label);
        let _ = client
            .update_message(&dm, &submit.message_ts, Some(&blocks), &fallback)
            .await;
        remove_picker(state, "slack", &submit.message_ts);
        return;
    }
    let content = match crate::msg_card::validate_input(submit.user_input.as_deref(), lang) {
        Ok(content) => content,
        Err(error) => {
            let view = crate::msg_card::build_view(
                rec,
                state.interject.pending_count(&payload.session_id),
                &state.interject.full_text(&payload.session_id),
                Some(error),
                lang,
            );
            let nonce = uuid::Uuid::new_v4().to_string();
            let blocks = crate::slack::blockkit::build_msg_compose_card(
                &view,
                &nonce,
                submit.user_input.as_deref(),
            );
            let _ = client
                .update_message(&dm, &submit.message_ts, Some(&blocks), &view.title)
                .await;
            return;
        }
    };
    if take_msg_compose_picker(state, "slack", &submit.message_ts).is_none() {
        return;
    }
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, &payload.session_id);
    let label = msg_pick_deliver(state, "slack", &payload.session_id, rec, &content, lang);
    let (blocks, fallback) = crate::slack::select::build_select_final_blocks(&picker.title, &label);
    let _ = client
        .update_message(&dm, &submit.message_ts, Some(&blocks), &fallback)
        .await;
    state.select.route_refresh.notify_one();
    activate_channel_on_action(state, "slack", config, lang).await;
}

/// TG/Slack 共用的下标分派：找 picker → 按下标取 session_id → 按 kind 处理。
/// 过期 / 越界 / 无 picker → 静默（D7）。
pub(super) async fn dispatch_select_pick(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    idx: usize,
    config: &AppConfig,
) {
    let picker = {
        let pickers = state.select.pickers.lock().unwrap();
        pickers
            .iter()
            .find(|p| p.channel == channel_id && p.message_id == mid)
            .cloned()
    };
    let Some(picker) = picker else {
        return; // 已过期 / 被清理：静默（D7）。
    };
    let Some(session_id) = picker.options.get(idx).cloned() else {
        return;
    };
    let lang = Lang::current();
    if session_id == crate::select::MORE_OPTION_ID
        && matches!(
            picker.kind,
            PickerKind::Todo | PickerKind::TodoRm | PickerKind::TodoAuto
        )
    {
        select_pick_todo_more(state, channel_id, mid, &picker, config, lang, None).await;
        activate_channel_on_action(state, channel_id, config, lang).await;
        return;
    }
    match picker.kind {
        PickerKind::TaskWorkspace
        | PickerKind::TaskAgent
        | PickerKind::TaskPermission
        | PickerKind::TaskInputSource
        | PickerKind::ForkSource
        | PickerKind::ForkPermission => {
            select_pick_task_flow(
                state,
                channel_id,
                mid,
                &session_id,
                &picker,
                config,
                lang,
                None,
            )
            .await;
        }
        PickerKind::Watch => {
            select_pick_watch_inplace(state, channel_id, mid, &session_id, config, lang).await;
            // 卡片点『关注』＝在该渠道操作 → 设为活跃槽（与 /watch 一致，用户决策）。
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        PickerKind::Status => {
            let snapshot = state.agents.snapshot();
            let text = status_detail_by_session(&snapshot, &session_id, channel_id, lang);
            let _ = reply_channel_text(channel_id, config, &text).await;
            // 卡片点『查看』＝在该渠道操作 → 设为活跃槽（补齐与 /status 文本命令的一致性）。
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        PickerKind::Unwatch => {
            select_pick_unwatch_inplace(state, channel_id, mid, &session_id, config, lang).await;
        }
        PickerKind::Msg => {
            if let Some(content) = picker.payload.as_deref() {
                select_pick_msg_inplace(state, channel_id, mid, &session_id, content, config, lang)
                    .await;
            } else if channel_id == "slack" {
                slack_select_pick_msg_compose(state, mid, &session_id, config, lang).await;
            } else {
                select_pick_msg_compose_sequential(
                    state,
                    channel_id,
                    mid,
                    &session_id,
                    config,
                    lang,
                )
                .await;
            }
            // 卡片点『发送』＝在该渠道操作 → 设为活跃槽（与 /msg 一致，用户决策）。
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        PickerKind::MsgCompose => {}
        PickerKind::Diff | PickerKind::Stage | PickerKind::Transcript => {
            select_pick_export(
                state,
                channel_id,
                mid,
                &session_id,
                picker.kind,
                config,
                lang,
                None,
            )
            .await;
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        // /todo · /todo-rm（spec todo-whats-next D8）。
        PickerKind::Todo => {
            select_pick_todo_text(
                state,
                channel_id,
                mid,
                &session_id,
                picker.payload.as_deref(),
                config,
                lang,
            )
            .await;
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        PickerKind::TodoRm => {
            select_pick_todo_rm_inplace(state, channel_id, mid, &session_id, config, lang).await;
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        PickerKind::TodoRmEntry => {
            select_pick_todo_rm_entry_inplace(
                state,
                channel_id,
                mid,
                &session_id,
                &picker,
                config,
                lang,
            )
            .await;
        }
        PickerKind::TodoAuto => {
            select_pick_todo_auto_inplace(
                state,
                channel_id,
                mid,
                &session_id,
                picker.payload.as_deref(),
                config,
                lang,
            )
            .await;
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
        PickerKind::TodoAutoEntry => {
            select_pick_todo_auto_entry_inplace(
                state,
                channel_id,
                mid,
                &session_id,
                &picker,
                config,
                lang,
            )
            .await;
        }
        // 管理卡在 TG/Slack 上是纯文本形态，不会有卡片回调。
        PickerKind::TodoManage => {}
        PickerKind::Yolo => {
            select_pick_yolo(state, channel_id, mid, &session_id, config, lang, None).await;
            activate_channel_on_action(state, channel_id, config, lang).await;
        }
    }
}

/// 单选卡点选「发送」（TG/Slack 就地定格）：投递（若目标仍工作中·非 grok）+ 定格本单选卡。
pub(super) async fn select_pick_msg_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    content: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let label = msg_pick_deliver(state, channel_id, session_id, rec, content, lang);
    finalize_select_card_edit(
        channel_id,
        config,
        mid,
        &crate::select::title_msg(lang),
        &label,
    )
    .await;
    remove_picker(state, channel_id, mid);
}

async fn slack_select_pick_msg_compose(
    state: &Arc<ServerState>,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let Some(rec) = find_agent_by_session(&snapshot, session_id) else {
        finalize_select_card_edit(
            "slack",
            config,
            mid,
            &crate::select::title_msg(lang),
            crate::i18n::tr(lang, "select.msgTargetGone"),
        )
        .await;
        remove_picker(state, "slack", mid);
        return;
    };
    if !is_working_non_grok(&snapshot, session_id) {
        finalize_select_card_edit(
            "slack",
            config,
            mid,
            &crate::select::title_msg(lang),
            crate::i18n::tr(lang, "select.msgTargetGone"),
        )
        .await;
        remove_picker(state, "slack", mid);
        return;
    }
    let view = crate::msg_card::build_view(
        rec,
        state.interject.pending_count(session_id),
        &state.interject.full_text(session_id),
        None,
        lang,
    );
    let Ok(client) = crate::slack::client::SlackClient::new(&config.channels.slack) else {
        return;
    };
    let Ok(dm) = client.open_dm().await else {
        return;
    };
    let nonce = uuid::Uuid::new_v4().to_string();
    let blocks = crate::slack::blockkit::build_msg_compose_card(&view, &nonce, None);
    if let Err(err) = client
        .update_message(&dm, mid, Some(&blocks), &view.title)
        .await
    {
        log(&format!("msg compose: morph slack card failed: {}", err));
        return;
    }
    morph_picker_to_msg_compose(state, "slack", mid, session_id, view.title);
}

async fn select_pick_msg_compose_sequential(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let seq = rec
        .and_then(|item| item.get("seq").and_then(|value| value.as_u64()))
        .unwrap_or(0);
    let working = is_working_non_grok(&snapshot, session_id);
    let label = if working {
        crate::i18n::tr(lang, "select.pickedCard").replace("{id}", &seq.to_string())
    } else {
        crate::i18n::tr(lang, "select.msgTargetGone").to_string()
    };
    let finalized = if channel_id == "dingding" {
        try_dd_finalize_select_card(config, mid, &label).await
    } else {
        try_finalize_select_card_edit(
            channel_id,
            config,
            mid,
            &crate::select::title_msg(lang),
            &label,
        )
        .await
    };
    if !finalized {
        return;
    }
    remove_picker(state, channel_id, mid);
    if !working {
        return;
    }
    // 钉钉 / Telegram 必须先让选择卡终态化，再投放输入卡，保证任一时刻只有一个活动载体。
    if send_msg_compose_card(state, channel_id, config, session_id, lang).await {
        return;
    }
    let snapshot = state.agents.snapshot();
    let failed = if is_working_non_grok(&snapshot, session_id) {
        crate::i18n::tr(lang, "msgCard.openFailed")
            .replace("{p}", crate::autochannel::cmd_prefix(channel_id))
            .replace("{id}", &seq.to_string())
    } else {
        crate::i18n::tr(lang, "select.msgTargetGone").to_string()
    };
    if channel_id == "dingding" {
        dd_finalize_select_card(config, mid, &failed).await;
    } else {
        finalize_select_card_edit(
            channel_id,
            config,
            mid,
            &crate::select::title_msg(lang),
            &failed,
        )
        .await;
    }
    let _ = reply_channel_text(channel_id, config, &failed).await;
}

/// 单选卡点选「watch」（TG/Slack 可就地编辑）：把本消息编辑成实时 watch 卡（`WatchClient::edit`），
/// 登记订阅（含换新卡收尾）+ 消费 picker + 让 watch 引擎认领本消息。已结束则定格终态卡、不订阅。
pub(super) async fn select_pick_watch_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, session_id);
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&session_id.to_string());
    let seq = rec
        .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let frame = crate::watch::build_frame(seq, rec, waiting);
    let ended = frame.phase == crate::watch::WatchPhase::Ended;
    if ended {
        // 已结束/消失：就地把本消息编辑成终态卡、不订阅、消费掉 picker。
        if let Some(client) = watch_client(state, channel_id, config).await {
            if let Err(err) = client
                .edit(
                    mid,
                    &frame,
                    crate::watch::CardMode::Final(crate::watch::FinalKind::Ended),
                    now,
                    lang,
                    None,
                )
                .await
            {
                log(&format!(
                    "select: transform to ended watch card failed ({}): {}",
                    channel_id, err
                ));
                return;
            }
        }
        remove_picker(state, channel_id, mid);
        return;
    }
    // 上限校验（本渠道；已在关注同一 session＝换新卡，不计新增）。
    let already = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.channel == channel_id && s.session_id == session_id);
    if !already {
        let count = state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.channel == channel_id && !s.rewatchable)
            .count();
        if count >= crate::watch::MAX_WATCHES {
            let text = crate::i18n::tr(lang, "watch.limit")
                .replace("{n}", &crate::watch::MAX_WATCHES.to_string())
                .replace("{p}", crate::autochannel::cmd_prefix(channel_id));
            let _ = reply_channel_text(channel_id, config, &text).await;
            return; // 单选卡不动，可另选。
        }
    }
    // 就地把这条单选卡消息编辑成实时 watch 卡。
    let Some(client) = watch_client(state, channel_id, config).await else {
        return;
    };
    if let Err(err) = client
        .edit(mid, &frame, crate::watch::CardMode::Active, now, lang, None)
        .await
    {
        log(&format!(
            "select: transform select card to watch card failed ({}): {}",
            channel_id, err
        ));
        return;
    }
    register_watch_at(
        state, channel_id, session_id, seq, mid, &frame, false, config, lang,
    )
    .await;
    remove_picker(state, channel_id, mid);
    ensure_watch_routes(state).await;
}

/// 单选卡点选「unwatch」（TG/Slack）：取消该关注（旧 watch 卡定格）+ 文本确认 + 就地刷新本单选卡
/// （移除该项；取到 0 则定格「已全部取消关注」）。
pub(super) async fn select_pick_unwatch_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let now = now_secs();
    let entry = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.channel == channel_id && s.session_id == session_id)
        .cloned();
    if let Some(entry) = entry {
        if let Some(client) = watch_client(state, channel_id, config).await {
            let snapshot = state.agents.snapshot();
            let waiting = state
                .registry
                .in_flight_agent_session_ids()
                .contains(&entry.session_id);
            let frame = crate::watch::build_frame(
                entry.seq,
                find_agent_by_session(&snapshot, &entry.session_id),
                waiting,
            );
            if let Err(err) = client
                .edit(
                    &entry.message_id,
                    &frame,
                    crate::watch::CardMode::Final(crate::watch::FinalKind::Cancelled),
                    now,
                    lang,
                    Some(&entry.session_id),
                )
                .await
            {
                log(&format!(
                    "select: finalize unwatch card failed ({}): {}",
                    channel_id, err
                ));
            }
        }
        {
            let mut subs = state.watch.subs.lock().unwrap();
            if let Some(s) = subs.iter_mut().find(|s| s.message_id == entry.message_id) {
                s.rewatchable = true;
            }
        }
        persist_watch_subs(state);
        state.watch.notify.notify_one();
        ensure_watch_routes(state).await;
        let text =
            crate::i18n::tr(lang, "watch.unwatchDone").replace("{id}", &entry.seq.to_string());
        let _ = reply_channel_text(channel_id, config, &text).await;
    }
    // 就地刷新单选卡：剩余订阅 → 新卡；空 → 定格「已全部取消关注」并消费 picker。
    let snapshot = state.agents.snapshot();
    let options = unwatch_options(state, channel_id, &snapshot, lang);
    if options.is_empty() {
        finalize_select_card_edit(
            channel_id,
            config,
            mid,
            &crate::select::title_unwatch(lang),
            crate::i18n::tr(lang, "select.unwatchAllDoneCard"),
        )
        .await;
        remove_picker(state, channel_id, mid);
    } else {
        let view = crate::select::build_view(
            crate::select::title_unwatch(lang),
            options,
            crate::select::SelectAction::Unwatch,
            lang,
        );
        let new_ids: Vec<String> = view.options.iter().map(|o| o.id.clone()).collect();
        refresh_select_card_edit(channel_id, config, mid, &view, lang).await;
        if let Some(p) = state
            .select
            .pickers
            .lock()
            .unwrap()
            .iter_mut()
            .find(|p| p.channel == channel_id && p.message_id == mid)
        {
            p.options = new_ids;
        }
    }
}

/// 就地把 TG/Slack 单选卡编辑为新的一版单选卡（`/unwatch` 移除该项后刷新）。
pub(super) async fn refresh_select_card_edit(
    channel_id: &str,
    config: &AppConfig,
    mid: &str,
    view: &crate::select::SelectView,
    lang: Lang,
) {
    match channel_id {
        "telegram" => {
            let Ok(m) = mid.parse::<i64>() else { return };
            let tg = &config.channels.telegram;
            if let Ok(c) = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            ) {
                let html = crate::telegram::select::render_select_html(view);
                let markup = crate::telegram::select::inline_keyboard(view, lang);
                if let Err(err) = c
                    .edit_message_text(m, &html, Some("HTML"), Some(markup))
                    .await
                {
                    log(&format!(
                        "select: refresh telegram select card failed: {}",
                        err
                    ));
                }
            }
        }
        "slack" => {
            if let Ok(c) = crate::slack::client::SlackClient::new(&config.channels.slack) {
                if let Ok(dm) = c.open_dm().await {
                    let (blocks, fallback) = crate::slack::select::build_select_blocks(view, lang);
                    if let Err(err) = c.update_message(&dm, mid, Some(&blocks), &fallback).await {
                        log(&format!(
                            "select: refresh slack select card failed: {}",
                            err
                        ));
                    }
                }
            }
        }
        _ => {}
    }
}

/// 就地把 TG/Slack 单选卡定格为无按钮终态（标题 + 定格文案）。
pub(super) async fn finalize_select_card_edit(
    channel_id: &str,
    config: &AppConfig,
    mid: &str,
    title: &str,
    final_label: &str,
) {
    let _ = try_finalize_select_card_edit(channel_id, config, mid, title, final_label).await;
}

async fn try_finalize_select_card_edit(
    channel_id: &str,
    config: &AppConfig,
    mid: &str,
    title: &str,
    final_label: &str,
) -> bool {
    match channel_id {
        "telegram" => {
            let Ok(m) = mid.parse::<i64>() else {
                return false;
            };
            let tg = &config.channels.telegram;
            if let Ok(c) = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            ) {
                let html = crate::telegram::select::render_select_final_html(title, final_label);
                if let Err(err) = c.edit_message_text(m, &html, Some("HTML"), None).await {
                    log(&format!(
                        "select: finalize telegram select card failed: {}",
                        err
                    ));
                    return false;
                }
                return true;
            }
            false
        }
        "slack" => {
            if let Ok(c) = crate::slack::client::SlackClient::new(&config.channels.slack) {
                if let Ok(dm) = c.open_dm().await {
                    let (blocks, fallback) =
                        crate::slack::select::build_select_final_blocks(title, final_label);
                    if let Err(err) = c.update_message(&dm, mid, Some(&blocks), &fallback).await {
                        log(&format!(
                            "select: finalize slack select card failed: {}",
                            err
                        ));
                        return false;
                    }
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn select_pick_export(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    session_id: &str,
    kind: PickerKind,
    config: &AppConfig,
    lang: Lang,
    ack: Option<crate::feishu::router::CardAck>,
) {
    let snapshot = state.agents.snapshot();
    let seq = find_agent_by_session(&snapshot, session_id)
        .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let label_key = match kind {
        PickerKind::Diff => "select.diffDoneCard",
        PickerKind::Stage => "select.stageOpenedCard",
        PickerKind::Transcript => "select.transcriptDoneCard",
        _ => "select.diffDoneCard",
    };
    let title = match kind {
        PickerKind::Diff => crate::select::title_diff(lang),
        PickerKind::Stage => crate::select::title_stage(lang),
        PickerKind::Transcript => crate::select::title_transcript(lang),
        _ => String::new(),
    };
    let label = crate::i18n::tr(lang, label_key).replace("{id}", &seq.to_string());
    if channel_id == "feishu" {
        if let Some(ack) = ack {
            let card = crate::feishu::card::build_select_final_card(&title, &label);
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        }
    } else if channel_id == "dingding" {
        dd_finalize_select_card(config, mid, &label).await;
    } else {
        finalize_select_card_edit(channel_id, config, mid, &title, &label).await;
    }
    remove_picker(state, channel_id, mid);
    match kind {
        PickerKind::Diff => run_diff(state, channel_id, session_id, config, lang).await,
        PickerKind::Transcript => run_transcript(state, channel_id, session_id, config, lang).await,
        PickerKind::Stage => run_stage_confirm(state, channel_id, session_id, config, lang).await,
        _ => {}
    }
}
