//! IM 入站命令层：监听、消息提取、共享命令（msg/new/export/stage 等）与回复。

use super::*;

// ===== IM 入站消费（命令 /here、/status…）/ 活跃槽 / 补推在途 =====

/// 确保各已启用 IM 的入站消费任务在线，使守护进程**在世期间**能收到任何入站消息（命令 / 引导 / 作答确认）。
/// 触发条件 = **daemon 存活 + 有启用 IM**（「存活即监听」，spec R1）：与工作中 agent / 在途提问 / 自动激活
/// 开关全部无关。连接随 daemon 退出而释放（serve 收尾丢弃 Router → Drop 关长连接），故无需主动断连；
/// 监听不计入保活、不阻止空闲退出。在 `serve()` 启动后台调用一次，并在受理 / 配置变更处幂等重调。
/// 各渠道只提供「连接 Router + 取原始消息观察者 + 抽取 (发送者, 文本?) + 期望发送者」这几样传输原语；
/// 通用循环与命令分派（`spawn_listener` / `handle_inbound`）一份实现，各渠道复用。幂等：可反复调用。
pub(super) async fn ensure_inbound_listeners(state: &Arc<ServerState>) {
    // 「存活即监听」：不再用「有工作中 agent」门控——只要 daemon 存活且有启用 IM 就监听，与工作中 agent /
    // 在途提问 / 自动激活开关全部无关（使任何消息在世期间都能被收到并回复）。
    // 读缓存快照（密钥已解析、config_watch 保鲜），无任何启用的 IM 渠道则无须建监听。
    let config = state.config_snapshot();
    if !any_im_enabled(&config) {
        return;
    }

    if crate::app::is_feishu_active(&config) {
        if let Some(stop) = state.inbound_listeners.claim("feishu") {
            match ensure_fs_router(state, &config.channels.feishu).await {
                Some(r) => spawn_listener(
                    state,
                    "feishu",
                    r.observe_message(),
                    extract_feishu,
                    config.channels.feishu.open_id.trim().to_string(),
                    stop,
                ),
                None => state.inbound_listeners.release("feishu", &stop),
            }
        }
    }

    if crate::app::is_dingding_active(&config) {
        if let Some(stop) = state.inbound_listeners.claim("dingding") {
            let dd = &config.channels.dingding;
            match ensure_dd_router(state, dd.client_id.trim(), dd.client_secret.trim()).await {
                Some(r) => spawn_listener(
                    state,
                    "dingding",
                    r.observe_bot(),
                    extract_dingtalk,
                    dd.user_id.trim().to_string(),
                    stop,
                ),
                None => state.inbound_listeners.release("dingding", &stop),
            }
        }
    }

    if crate::app::is_slack_active(&config) {
        if let Some(stop) = state.inbound_listeners.claim("slack") {
            match ensure_sl_router(state, &config.channels.slack).await {
                Some(r) => spawn_listener(
                    state,
                    "slack",
                    r.observe_message(),
                    extract_slack,
                    config.channels.slack.user_id.trim().to_string(),
                    stop,
                ),
                None => state.inbound_listeners.release("slack", &stop),
            }
        }
    }

    if crate::app::is_telegram_active(&config) {
        if let Some(stop) = state.inbound_listeners.claim("telegram") {
            match ensure_tg_router(state, &config.channels.telegram).await {
                Some(r) => spawn_listener(
                    state,
                    "telegram",
                    r.observe_message(),
                    extract_telegram,
                    config.channels.telegram.chat_id.trim().to_string(),
                    stop,
                ),
                None => state.inbound_listeners.release("telegram", &stop),
            }
        }
    }

    // 兜底：随 Router 重建恢复活动单选卡的按钮回调路由（无 picker 时为 no-op）。
    ensure_select_routes(state).await;
}

pub(super) fn spawn_listener(
    state: &Arc<ServerState>,
    channel_id: &'static str,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    extract: fn(&serde_json::Value) -> Option<Inbound>,
    expected_sender: String,
    stop: Arc<tokio::sync::Notify>,
) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.notified() => break,
                ev = rx.recv() => match ev {
                    Some(ev) => {
                        if let Some(inb) = extract(&ev) {
                            // 单聊机器人：仅处理期望发送者发来的消息（期望为空则不过滤）；过滤掉机器人自身回声。
                            if !expected_sender.is_empty() && inb.sender != expected_sender {
                                continue;
                            }
                            // ForceReply 的 `/msg` 作答同时会进入 Telegram 原始观察者；由 compose
                            // 精确路由独占处理，避免又被当成普通聊天消息触发 autochannel。
                            if channel_id == "telegram"
                                && is_telegram_msg_compose_reply(&state, &ev)
                            {
                                continue;
                            }
                            handle_inbound(&state, channel_id, inb.text.as_deref()).await;
                        }
                    }
                    None => break,
                },
            }
        }
        state.inbound_listeners.release(channel_id, &stop);
    });
}

fn is_telegram_msg_compose_reply(state: &Arc<ServerState>, ev: &serde_json::Value) -> bool {
    let Some(reply_mid) = ev
        .get("reply_to_message")
        .and_then(|message| message.get("message_id"))
        .and_then(|value| value.as_i64())
    else {
        return false;
    };
    if state.select.pickers.lock().unwrap().iter().any(|picker| {
        picker.channel == "telegram"
            && picker.kind == PickerKind::MsgCompose
            && picker.message_id == reply_mid.to_string()
    }) {
        return true;
    }
    let now = now_secs();
    let mut tombstones = state.select.msg_compose_tombstones.lock().unwrap();
    tombstones.retain(|_, expires_at| *expires_at > now);
    tombstones.contains_key(&format!("telegram\n{}", reply_mid))
}

/// 飞书原始消息 → `Inbound`（发送者 open_id + 文本？）；非文本时 `text=None`、非消息事件返回 None。
pub(super) fn extract_feishu(ev: &serde_json::Value) -> Option<Inbound> {
    let open_id = ev
        .get("sender")
        .and_then(|s| s.get("sender_id"))
        .and_then(|i| i.get("open_id"))
        .and_then(|v| v.as_str())?
        .to_string();
    ev.get("message")?; // 确保是一条消息事件
    let text = fs_text_and_sender(ev).map(|(_, t)| t);
    Some(Inbound {
        sender: open_id,
        text,
    })
}

/// 钉钉原始 bot 消息 → `Inbound`（senderStaffId + 文本？）；非文本时 `text=None`。
pub(super) fn extract_dingtalk(ev: &serde_json::Value) -> Option<Inbound> {
    let sender = ev
        .get("senderStaffId")
        .and_then(|v| v.as_str())?
        .to_string();
    let text = ev
        .get("text")
        .and_then(|t| t.get("content"))
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(Inbound { sender, text })
}

/// Slack 原始消息事件 → `Inbound`（user + 文本？）；非文本时 `text=None`、无发送者返回 None。
pub(super) fn extract_slack(ev: &serde_json::Value) -> Option<Inbound> {
    let user = ev.get("user").and_then(|v| v.as_str())?.to_string();
    let text = ev
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(Inbound { sender: user, text })
}

/// Telegram 原始 `message` 对象 → `Inbound`（chat id + 文本？）。Router 仅转发文本消息，
/// 故 `text` 实际恒为 `Some`；为统一签名仍按 `Option` 处理。
pub(super) fn extract_telegram(ev: &serde_json::Value) -> Option<Inbound> {
    let chat = ev
        .get("chat")
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_i64())?
        .to_string();
    let text = ev
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(Inbound { sender: chat, text })
}

/// 该渠道当前是否有「活动在途提问」（即有在途请求把本渠道挂进了协调器）。
/// 用于「普通文本退避」判定：有则交渠道会话确认/引导，观察者不重复回复（spec 协调原则）。
pub(super) fn has_active_question_on(state: &Arc<ServerState>, channel_id: &str) -> bool {
    let ask = state
        .registry
        .in_flight_entries()
        .iter()
        .any(|e| e.coordinator.has_channel(channel_id));
    ask || state
        .registry
        .in_flight_confirm_entries()
        .iter()
        .any(|entry| entry.has_live_delivery(channel_id))
}

/// 该渠道当前是否有在途单选卡（picker 未被消费）。用于 `remove_picker` 判定是否仍有单选卡残留。
pub(super) fn has_active_select_on(state: &Arc<ServerState>, channel_id: &str) -> bool {
    state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.channel == channel_id)
}

/// 该渠道是否有「仍位于会话底部」的单选卡：picker 发出后未再出现非 watch 消息（`posted_ms >=`
/// 渠道 disturb 水位）。**仅当单选卡还是最后一条消息时才抑制 watch 跟底**（免打断正在进行的单选）；
/// 一旦被其它消息淹没即放开跟底（用户定案：忘记选择的旧单选卡不该长期卡住 watch 跟底）。
pub(super) fn select_is_last_on(state: &Arc<ServerState>, channel_id: &str) -> bool {
    let disturb = state
        .watch
        .disturb
        .lock()
        .unwrap()
        .get(channel_id)
        .copied()
        .unwrap_or(0);
    state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.channel == channel_id && p.posted_ms >= disturb)
}

// ===== /watch 实时关注引擎（spec docs/specs/im-watch.md，P1 仅飞书）=====

/// 从注册表快照数组中按 session_id 找记录。
pub(super) fn find_agent_by_session<'a>(
    snapshot: &'a serde_json::Value,
    session_id: &str,
) -> Option<&'a serde_json::Value> {
    snapshot
        .as_array()?
        .iter()
        .find(|r| r.get("sessionId").and_then(|v| v.as_str()) == Some(session_id))
}

/// `/msg` 系命令的寻址公共段（spec agent-interject D9）：编号（复用 `/status` 稳定 seq）→
/// 注册表快照记录 → 校验可插话（grok 无传话通道、ended 无处送达）。失败时已回提示、返回 None。
/// `require_working`：为真时（发送场景）目标必须「工作中」，否则回提示（用户定案：只能给工作中的
/// agent 发送插话）；回显 / 撤回场景传 false（对空闲也可操作）。
pub(super) async fn resolve_msg_target(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    require_working: bool,
    config: &AppConfig,
    lang: Lang,
) -> Option<String> {
    let prefix = crate::autochannel::cmd_prefix(channel_id);
    let Some(id) = sel else {
        let text = crate::i18n::tr(lang, "autoChannel.msgUsage").replace("{p}", prefix);
        let _ = reply_channel_text(channel_id, config, &text).await;
        return None;
    };
    let snapshot = state.agents.snapshot();
    let Some(rec) = crate::autochannel::find_by_seq(&snapshot, id) else {
        let text = crate::i18n::tr(lang, "autoChannel.statusDetailNotFound")
            .replace("{id}", &id.to_string())
            .replace("{p}", prefix);
        let _ = reply_channel_text(channel_id, config, &text).await;
        return None;
    };
    if rec.get("kind").and_then(|v| v.as_str()) == Some("grok") {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "autoChannel.msgGrokUnsupported"),
        )
        .await;
        return None;
    }
    if rec.get("state").and_then(|v| v.as_str()) == Some("ended") {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "autoChannel.msgEnded"),
        )
        .await;
        return None;
    }
    if require_working && rec.get("state").and_then(|v| v.as_str()) != Some("working") {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "select.msgNoWorkingTarget"),
        )
        .await;
        return None;
    }
    rec.get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// `/msg <编号> [内容]`（spec agent-interject D2/D9）：有内容 → **追加**排队；工作中目标无内容 →
/// 打开一次性输入卡；空闲目标无内容仍回显当前待送达全文。
pub(super) async fn handle_msg_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    content: Option<String>,
    config: &AppConfig,
    lang: Lang,
) {
    // `/msg` 插话＝在该渠道主动参与 → 设为活跃槽（用户决策）。
    activate_channel_on_action(state, channel_id, config, lang).await;
    match (sel, content) {
        // 显式编号 + 内容 → 发送（收紧为仅工作中）。
        (Some(_), Some(content)) => {
            let Some(sid) = resolve_msg_target(state, channel_id, sel, true, config, lang).await
            else {
                return;
            };
            let text = deliver_msg(state, channel_id, &sid, &content, lang);
            let _ = reply_channel_text(channel_id, config, &text).await;
        }
        // 显式编号无内容：工作中 → 输入卡；空闲 → 沿用待送达回显。
        (Some(_), None) => {
            let Some(sid) = resolve_msg_target(state, channel_id, sel, false, config, lang).await
            else {
                return;
            };
            let snapshot = state.agents.snapshot();
            if is_working_non_grok(&snapshot, &sid) {
                open_msg_compose_or_reply(state, channel_id, config, &sid, lang).await;
            } else {
                let text = msg_echo_text(state, &sid, lang);
                let _ = reply_channel_text(channel_id, config, &text).await;
            }
        }
        // 无编号 + 内容 → 自动选择（关注恰 1 个且工作中直发；否则弹选择卡）。
        (None, Some(content)) => {
            handle_msg_auto(state, channel_id, content, config, lang).await;
        }
        // 无编号无内容：唯一关注目标可发则直开输入卡，否则先选目标。
        (None, None) => {
            handle_msg_compose_auto(state, channel_id, config, lang).await;
        }
    }
}

async fn open_msg_compose_or_reply(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    session_id: &str,
    lang: Lang,
) {
    if send_msg_compose_card(state, channel_id, config, session_id, lang).await {
        return;
    }
    let snapshot = state.agents.snapshot();
    let id = find_agent_by_session(&snapshot, session_id)
        .and_then(|rec| rec.get("seq").and_then(|value| value.as_u64()))
        .unwrap_or(0);
    let text = crate::i18n::tr(lang, "msgCard.openFailed")
        .replace("{p}", crate::autochannel::cmd_prefix(channel_id))
        .replace("{id}", &id.to_string());
    let _ = reply_channel_text(channel_id, config, &text).await;
}

async fn handle_msg_compose_auto(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let watching = watching_sessions(state, channel_id);
    if watching.len() == 1 {
        if let Some(session_id) = watching.iter().next() {
            if is_working_non_grok(&snapshot, session_id) {
                open_msg_compose_or_reply(state, channel_id, config, session_id, lang).await;
                return;
            }
        }
    }
    let options = crate::select::msg_options(&snapshot, &watching, now_secs(), lang);
    if options.is_empty() {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "select.msgNoWorking"),
        )
        .await;
        return;
    }
    if !send_agent_picker(
        state,
        channel_id,
        config,
        PickerKind::Msg,
        crate::select::title_msg(lang),
        options,
        None,
        lang,
    )
    .await
    {
        let text = msg_usage_hint(state, channel_id, lang);
        let _ = reply_channel_text(channel_id, config, &text).await;
    }
}

/// 追加一条插话并回执文案（`n==0` ⇒ 恰有 hook 挂起等待 → 立即送达）。发送三路径共用
/// （显式编号 / 无编号直发 / 单选卡点「发送」）。`channel_id`＝来源渠道：排队时登记，供消息被
/// agent 消费后回推「已阅读」回执（D9）。
pub(super) fn deliver_msg(
    state: &Arc<ServerState>,
    channel_id: &str,
    session_id: &str,
    content: &str,
    lang: Lang,
) -> String {
    let n = state
        .interject
        .append(session_id, content, Vec::new(), Some(channel_id));
    state.interject.persist();
    broadcast_agents_state(state);
    if n == 0 {
        crate::i18n::tr(lang, "autoChannel.msgDeliveredNow").to_string()
    } else {
        crate::i18n::tr(lang, "autoChannel.msgQueued").replace("{n}", &n.to_string())
    }
}

/// 排队插话被消费后，给各来源渠道回推一条「已阅读」回执（编号按当前快照现算）。仅在有待回执
/// 渠道时才 spawn（罕见），不拖慢 hook 热路径。
pub(super) fn spawn_read_receipts(
    state: &Arc<ServerState>,
    session_id: &str,
    channels: Vec<String>,
) {
    if channels.is_empty() {
        return;
    }
    let state = state.clone();
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        let lang = Lang::current();
        let snapshot = state.agents.snapshot();
        let seq = find_agent_by_session(&snapshot, &session_id)
            .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let text =
            crate::i18n::tr(lang, "autoChannel.msgReadReceipt").replace("{id}", &seq.to_string());
        let config = state.config_snapshot();
        for ch in channels {
            let _ = reply_channel_text(&ch, &config, &text).await;
        }
    });
}

/// `/msg <编号>`（无内容）回显该 agent 待送达全文（无则「暂无待送达」）。
pub(super) fn msg_echo_text(state: &Arc<ServerState>, session_id: &str, lang: Lang) -> String {
    let full = state.interject.full_text(session_id);
    if full.is_empty() {
        crate::i18n::tr(lang, "autoChannel.msgNone").to_string()
    } else {
        let n = state.interject.pending_count(session_id);
        format!(
            "{}\n{}",
            crate::i18n::tr(lang, "autoChannel.msgEchoHeader").replace("{n}", &n.to_string()),
            full
        )
    }
}

/// 快照中该 session 是否「工作中·非 grok」（插话可发的前提；直发短路判定用）。
pub(super) fn is_working_non_grok(snapshot: &serde_json::Value, session_id: &str) -> bool {
    find_agent_by_session(snapshot, session_id)
        .map(|r| {
            r.get("state").and_then(|v| v.as_str()) == Some("working")
                && r.get("kind").and_then(|v| v.as_str()) != Some("grok")
        })
        .unwrap_or(false)
}

/// 工作中·非 grok 的 agent 列表行（`[编号] 类型 — 标题（项目）`；用法提示 / 兜底文本用）。
pub(super) fn working_agent_lines(snapshot: &serde_json::Value, lang: Lang) -> Vec<String> {
    let empty = Vec::new();
    snapshot
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter(|r| {
            r.get("state").and_then(|v| v.as_str()) == Some("working")
                && r.get("kind").and_then(|v| v.as_str()) != Some("grok")
        })
        .map(|r| {
            let seq = r.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
            format!(
                "[{}] {}",
                seq,
                crate::autochannel::kind_title_project(r, lang)
            )
        })
        .collect()
}

/// `/msg`（无编号无内容）增强用法提示：用法示例 + 当前工作中 agent 列表（带编号，可直接
/// `/msg <编号> <内容>` 定向）。无工作中 → 附一行「当前没有工作中的 Agent」。
pub(super) fn msg_usage_hint(state: &Arc<ServerState>, channel_id: &str, lang: Lang) -> String {
    let prefix = crate::autochannel::cmd_prefix(channel_id);
    let mut out = crate::i18n::tr(lang, "autoChannel.msgUsage").replace("{p}", prefix);
    let snapshot = state.agents.snapshot();
    let lines = working_agent_lines(&snapshot, lang);
    out.push_str("\n\n");
    if lines.is_empty() {
        out.push_str(crate::i18n::tr(lang, "select.msgNoWorking"));
    } else {
        out.push_str(&lines.join("\n"));
    }
    out
}

/// `/msg <内容>`（无编号）：本渠道关注恰 1 个且该 agent 工作中·非 grok → 直发；否则弹选择卡
/// （列工作中·非 grok，每行「发送」按钮）；无可发对象 → 提示、不弹卡。
pub(super) async fn handle_msg_auto(
    state: &Arc<ServerState>,
    channel_id: &str,
    content: String,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let watching = watching_sessions(state, channel_id);
    // 直发条件（用户定案）：只有明确关注恰 1 个、且它工作中·非 grok 时才直发，避免发错。
    if watching.len() == 1 {
        if let Some(sid) = watching.iter().next().cloned() {
            if is_working_non_grok(&snapshot, &sid) {
                let text = deliver_msg(state, channel_id, &sid, &content, lang);
                let _ = reply_channel_text(channel_id, config, &text).await;
                return;
            }
        }
    }
    // 否则弹选择卡（列工作中·非 grok）；关注中的仍带「· 关注中」徽标。
    let opts = crate::select::msg_options(&snapshot, &watching, now_secs(), lang);
    if opts.is_empty() {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "select.msgNoWorking"),
        )
        .await;
        return;
    }
    let sent = send_agent_picker(
        state,
        channel_id,
        config,
        PickerKind::Msg,
        crate::select::title_msg(lang),
        opts,
        Some(content),
        lang,
    )
    .await;
    if !sent {
        // 发卡失败（非支持渠道 / API 失败）：回工作中列表兜底，用户可 `/msg <编号> <内容>` 定向。
        let text = msg_usage_hint(state, channel_id, lang);
        let _ = reply_channel_text(channel_id, config, &text).await;
    }
}

/// `/msg-clear <编号>`（`/撤回`）：清空该 agent 的待送达插话 + 回执。
pub(super) async fn handle_msg_clear_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    config: &AppConfig,
    lang: Lang,
) {
    // `/msg-clear` 撤回插话＝在该渠道操作 → 设为活跃槽（用户决策）。
    activate_channel_on_action(state, channel_id, config, lang).await;
    let Some(sid) = resolve_msg_target(state, channel_id, sel, false, config, lang).await else {
        return;
    };
    let text = if state.interject.clear(&sid) {
        state.interject.persist();
        broadcast_agents_state(state);
        crate::i18n::tr(lang, "autoChannel.msgCleared")
    } else {
        crate::i18n::tr(lang, "autoChannel.msgNone")
    };
    let _ = reply_channel_text(channel_id, config, text).await;
}

pub(super) async fn start_new_task_flow(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    // `/new` always makes its source IM the active slot, independently of auto-activation.
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
            Lang::Zh => "当前版本仅支持 macOS 系统终端 Terminal.app。",
            Lang::En => "This version currently requires macOS Terminal.app.",
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    let (workspaces, readiness) = tokio::task::spawn_blocking(|| {
        (
            crate::agents::workspaces::refresh(),
            crate::integrations::agent_launch::all_readiness(),
        )
    })
    .await
    .unwrap_or_default();
    if workspaces.is_empty() {
        let text = match lang {
            Lang::Zh => {
                "没有找到可用工作目录。请先在电脑上的 Agent 中打开一个项目，或在设置中添加目录。"
            }
            Lang::En => {
                "No workspace is available. Open a project in a local Agent or add one in Settings."
            }
        };
        let _ = reply_channel_text(channel_id, config, text).await;
        return;
    }
    let diagnostics = readiness
        .iter()
        .flat_map(|item| item.diagnostics.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let ready: Vec<_> = readiness.into_iter().filter(|item| item.ready).collect();
    if ready.is_empty() {
        let title = match lang {
            Lang::Zh => "没有已就绪的 Agent。",
            Lang::En => "No Agent is ready.",
        };
        let _ = reply_channel_text(channel_id, config, &format!("{title}\n{diagnostics}")).await;
        return;
    }
    let options = task_workspace_options(workspaces, true, lang);
    let sent = send_agent_picker(
        state,
        channel_id,
        config,
        PickerKind::TaskWorkspace,
        crate::select::title_task_workspace(lang),
        options,
        Some(
            serde_json::to_string(&TaskPickerPayload {
                workspace: String::new(),
                kind: String::new(),
            })
            .unwrap_or_default(),
        ),
        lang,
    )
    .await;
    if !sent {
        let _ = reply_channel_text(channel_id, config, "Failed to send workspace picker").await;
    }
}

pub(super) fn task_workspace_options(
    workspaces: Vec<crate::agents::workspaces::Workspace>,
    recent_only: bool,
    lang: Lang,
) -> Vec<crate::select::SelectOption> {
    let total = workspaces.len();
    let visible = if recent_only { total.min(5) } else { total };
    let mut options: Vec<_> = workspaces
        .into_iter()
        .take(visible)
        .map(|workspace| {
            let parent = std::path::Path::new(&workspace.path)
                .parent()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default();
            let home = crate::paths::home().to_string_lossy().to_string();
            let parent = parent
                .strip_prefix(&home)
                .map(|rest| format!("~{rest}"))
                .unwrap_or(parent);
            crate::select::SelectOption {
                id: workspace.path.clone(),
                dot: None,
                seq: None,
                primary: if workspace.pinned {
                    format!("★ {}", workspace.label)
                } else {
                    workspace.label
                },
                badge: None,
                elapsed: None,
                secondary: (!parent.is_empty()).then_some(parent),
            }
        })
        .collect();
    if recent_only && total > visible {
        options.push(crate::select::SelectOption {
            id: crate::select::MORE_OPTION_ID.to_string(),
            dot: None,
            seq: None,
            primary: match lang {
                Lang::Zh => "显示更多工作目录",
                Lang::En => "Show more workspaces",
            }
            .into(),
            badge: None,
            elapsed: None,
            secondary: Some(match lang {
                Lang::Zh => format!("还有 {} 个", total - visible),
                Lang::En => format!("{} more", total - visible),
            }),
        });
    }
    options
}

pub(super) async fn continue_task_picker(
    state: &Arc<ServerState>,
    channel_id: &str,
    picker: &PickerEntry,
    selected_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    if matches!(
        picker.kind,
        PickerKind::ForkSource | PickerKind::ForkPermission
    ) {
        continue_fork_picker(state, channel_id, picker, selected_id, config, lang).await;
        return;
    }
    if picker.kind == PickerKind::TaskInputSource {
        let Some(source): Option<TaskInputSourcePayload> = picker
            .payload
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
        else {
            return;
        };
        if selected_id == TASK_MANUAL_ACTION_ID {
            start_task_input_form(
                state,
                channel_id,
                source.task,
                source.permission,
                source.project,
                Vec::new(),
                None,
                None,
                config,
                lang,
            )
            .await;
            return;
        }
        let Some(todo_id) = selected_id.strip_prefix(TASK_TODO_ACTION_PREFIX) else {
            return;
        };
        let Some(todo) = source.todos.iter().find(|todo| todo.id == todo_id).cloned() else {
            return;
        };
        start_task_input_form(
            state,
            channel_id,
            source.task,
            source.permission,
            source.project,
            Vec::new(),
            None,
            Some(todo),
            config,
            lang,
        )
        .await;
        return;
    }
    let mut payload: TaskPickerPayload = picker
        .payload
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or(TaskPickerPayload {
            workspace: String::new(),
            kind: String::new(),
        });
    match picker.kind {
        PickerKind::TaskWorkspace => {
            if selected_id == crate::select::MORE_OPTION_ID {
                let workspaces = crate::agents::workspaces::list()
                    .into_iter()
                    .filter(|workspace| {
                        !workspace.hidden && std::path::Path::new(&workspace.path).is_dir()
                    })
                    .collect();
                let options = task_workspace_options(workspaces, false, lang);
                let _ = send_agent_picker(
                    state,
                    channel_id,
                    config,
                    PickerKind::TaskWorkspace,
                    crate::select::title_task_workspace(lang),
                    options,
                    picker.payload.clone(),
                    lang,
                )
                .await;
                return;
            }
            if !std::path::Path::new(selected_id).is_dir() {
                let _ = reply_channel_text(channel_id, config, "Workspace is no longer available")
                    .await;
                return;
            }
            payload.workspace = selected_id.to_string();
            let options = crate::integrations::agent_launch::all_readiness()
                .into_iter()
                .filter(|item| item.ready)
                .map(|item| crate::select::SelectOption {
                    id: item.kind.as_str().to_string(),
                    dot: None,
                    seq: None,
                    primary: item.label,
                    badge: None,
                    elapsed: None,
                    secondary: Some(format!(
                        "{} · {}",
                        item.integration_mode,
                        item.executable.unwrap_or_default()
                    )),
                })
                .collect();
            let _ = send_agent_picker(
                state,
                channel_id,
                config,
                PickerKind::TaskAgent,
                crate::select::title_task_agent(lang),
                options,
                Some(serde_json::to_string(&payload).unwrap_or_default()),
                lang,
            )
            .await;
        }
        PickerKind::TaskAgent => {
            let Some(kind) = AgentKind::parse(selected_id) else {
                return;
            };
            if !crate::integrations::agent_launch::readiness(kind).ready {
                let _ = reply_channel_text(channel_id, config, "Agent is no longer ready").await;
                return;
            }
            payload.kind = kind.as_str().to_string();
            match config.agent_tasks.permission_prompt {
                crate::config::AgentTaskPermission::Ask => {
                    let options = vec![
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
                                    Lang::En => {
                                        "Auto-approve operations and bypass sandbox restrictions"
                                    }
                                }
                                .into(),
                            ),
                        },
                    ];
                    let _ = send_agent_picker(
                        state,
                        channel_id,
                        config,
                        PickerKind::TaskPermission,
                        crate::select::title_task_permission(lang),
                        options,
                        Some(serde_json::to_string(&payload).unwrap_or_default()),
                        lang,
                    )
                    .await;
                }
                crate::config::AgentTaskPermission::AgentDefault => {
                    start_task_input(
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
                    start_task_input(
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
        PickerKind::TaskPermission => {
            let permission = match selected_id {
                "agent-default" => {
                    crate::integrations::agent_launch::LaunchPermission::AgentDefault
                }
                "yolo" => crate::integrations::agent_launch::LaunchPermission::Yolo,
                _ => return,
            };
            start_task_input(state, channel_id, payload, permission, config, lang).await;
        }
        PickerKind::TaskInputSource => {}
        _ => {}
    }
}

pub(super) async fn start_task_input(
    state: &Arc<ServerState>,
    channel_id: &str,
    payload: TaskPickerPayload,
    permission: crate::integrations::agent_launch::LaunchPermission,
    config: &AppConfig,
    lang: Lang,
) {
    let project = crate::project::detect_from(std::path::Path::new(&payload.workspace));
    let mut todos = crate::todos::list(&project);
    let overflow_note = crate::todos::overflow_note(todos.len(), lang);
    todos.truncate(crate::todos::MAX_OPTION_TODOS);
    if channel_id == "telegram" && !todos.is_empty() {
        let mut options = Vec::with_capacity(todos.len() + 1);
        options.push(crate::select::SelectOption {
            id: TASK_MANUAL_ACTION_ID.into(),
            dot: None,
            seq: None,
            primary: match lang {
                Lang::Zh => "输入新任务",
                Lang::En => "Enter a new task",
            }
            .into(),
            badge: None,
            elapsed: None,
            secondary: Some(
                match lang {
                    Lang::Zh => "回复 ForceReply 输入任务",
                    Lang::En => "Reply to a ForceReply prompt",
                }
                .into(),
            ),
        });
        options.extend(
            todos
                .iter()
                .enumerate()
                .map(|(index, todo)| crate::select::SelectOption {
                    id: format!("{TASK_TODO_ACTION_PREFIX}{}", todo.id),
                    dot: None,
                    seq: Some((index + 1) as u64),
                    primary: task_todo_label(todo, lang),
                    badge: None,
                    elapsed: None,
                    secondary: None,
                }),
        );
        let title = match overflow_note.as_deref() {
            Some(note) => format!("{} · {note}", crate::select::title_task_input_source(lang)),
            None => crate::select::title_task_input_source(lang),
        };
        let source = TaskInputSourcePayload {
            task: payload,
            permission,
            project,
            todos,
            overflow_note,
        };
        let sent = send_agent_picker(
            state,
            channel_id,
            config,
            PickerKind::TaskInputSource,
            title,
            options,
            Some(serde_json::to_string(&source).unwrap_or_default()),
            lang,
        )
        .await;
        if !sent {
            let _ =
                reply_channel_text(channel_id, config, "Failed to send task source picker").await;
        }
        return;
    }
    start_task_input_form(
        state,
        channel_id,
        payload,
        permission,
        project,
        todos,
        overflow_note,
        None,
        config,
        lang,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn start_task_input_form(
    state: &Arc<ServerState>,
    channel_id: &str,
    payload: TaskPickerPayload,
    permission: crate::integrations::agent_launch::LaunchPermission,
    todo_project: String,
    todos: Vec<crate::todos::TodoEntry>,
    overflow_note: Option<String>,
    selected_todo: Option<crate::todos::TodoEntry>,
    config: &AppConfig,
    lang: Lang,
) {
    use crate::models::{
        ConfirmChoice, ConfirmDetail, ConfirmField, ConfirmFieldKind, ConfirmInput,
        ConfirmPresentation, ConfirmSpec,
    };
    let Some(kind) = AgentKind::parse(&payload.kind) else {
        return;
    };
    let todo_confirmation = selected_todo.is_some();
    let has_todo_choices = !todos.is_empty();
    let permission_label = match permission {
        crate::integrations::agent_launch::LaunchPermission::AgentDefault => match lang {
            Lang::Zh => "Agent 默认",
            Lang::En => "Agent default",
        },
        crate::integrations::agent_launch::LaunchPermission::Yolo => "YOLO",
    };
    let title = match (todo_confirmation, lang) {
        (true, Lang::Zh) => "确认 TODO 任务",
        (true, Lang::En) => "Confirm the TODO task",
        (false, Lang::Zh) => "输入新任务",
        (false, Lang::En) => "Enter the new task",
    };
    let workspace_label = crate::autochannel::project_name(&payload.workspace)
        .unwrap_or_else(|| payload.workspace.clone());
    let mut task_prompt = match (selected_todo.as_ref(), lang) {
        (Some(todo), Lang::Zh) => format!(
            "**将启动以下 TODO；可直接启动，或回复本卡补充说明。**\n\n{}{}\n\n- **Agent：** {}\n- **工作目录：** {}\n- **权限模式：** {}\n\n提交后将在 Mac 上通过新 Terminal 窗口启动 Agent。",
            if todo.auto { "⚡ " } else { "" },
            todo.text,
            kind.label(),
            workspace_label,
            permission_label
        ),
        (Some(todo), Lang::En) => format!(
            "**Start this TODO directly, or reply with extra instructions first.**\n\n{}{}\n\n- **Agent:** {}\n- **Workspace:** {}\n- **Permission mode:** {}\n\nSubmitting starts the Agent on your Mac in a new Terminal window.",
            if todo.auto { "⚡ " } else { "" },
            todo.text,
            kind.label(),
            workspace_label,
            permission_label
        ),
        (None, Lang::Zh) => format!(
            "**{}**\n\n- **Agent：** {}\n- **工作目录：** {}\n- **权限模式：** {}\n\n提交后将在 Mac 上通过新 Terminal 窗口启动 Agent。",
            if has_todo_choices {
                "请输入需要 Agent 执行的任务，或选择一个项目 TODO。"
            } else {
                "请输入需要 Agent 执行的任务。"
            },
            kind.label(), workspace_label, permission_label
        ),
        (None, Lang::En) => format!(
            "**{}**\n\n- **Agent:** {}\n- **Workspace:** {}\n- **Permission mode:** {}\n\nSubmitting starts the Agent on your Mac in a new Terminal window.",
            if has_todo_choices {
                "Enter a task for the Agent, or select a project TODO."
            } else {
                "Enter the task for the Agent to perform."
            },
            kind.label(), workspace_label, permission_label
        ),
    };
    if !todo_confirmation {
        if let Some(note) = overflow_note.as_deref() {
            task_prompt.push_str("\n\n");
            task_prompt.push_str(note);
        }
    }
    let mut choices = vec![ConfirmChoice {
        id: TASK_MANUAL_ACTION_ID.into(),
        label: match (has_todo_choices, lang) {
            (true, Lang::Zh) => "直接输入新任务",
            (true, Lang::En) => "Enter a new task",
            (false, Lang::Zh) => "启动任务",
            (false, Lang::En) => "Start task",
        }
        .into(),
        description: String::new(),
        role: crate::confirm::ActionRole::Primary,
        variant: None,
    }];
    if !todo_confirmation {
        choices.extend(todos.iter().map(|todo| ConfirmChoice {
            id: format!("{TASK_TODO_ACTION_PREFIX}{}", todo.id),
            label: task_todo_label(todo, lang),
            description: String::new(),
            role: crate::confirm::ActionRole::Default,
            variant: None,
        }));
    }
    choices.push(ConfirmChoice {
        id: "cancel".into(),
        label: match lang {
            Lang::Zh => "取消",
            Lang::En => "Cancel",
        }
        .into(),
        description: String::new(),
        role: crate::confirm::ActionRole::Destructive,
        variant: None,
    });
    let spec = ConfirmSpec {
        title: title.into(),
        context: vec![
            ConfirmField {
                id: "agent".into(),
                label: "Agent".into(),
                value: kind.label().into(),
                kind: ConfirmFieldKind::Text,
            },
            ConfirmField {
                id: "workspace".into(),
                label: match lang {
                    Lang::Zh => "工作目录",
                    Lang::En => "Workspace",
                }
                .into(),
                value: payload.workspace.clone(),
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
            summary: task_prompt,
            body_md: String::new(),
        },
        choices,
        presentation: ConfirmPresentation::SingleSelectSubmit {
            input: Some(ConfirmInput {
                id: "task".into(),
                visible_when_action_id: TASK_MANUAL_ACTION_ID.into(),
                always_visible: todo_confirmation || has_todo_choices,
                required: !todo_confirmation,
                prefix_chars_by_action_id: todos
                    .iter()
                    .map(|todo| {
                        (
                            format!("{TASK_TODO_ACTION_PREFIX}{}", todo.id),
                            todo.text.trim().chars().count(),
                        )
                    })
                    .chain(selected_todo.iter().map(|todo| {
                        (
                            TASK_MANUAL_ACTION_ID.to_string(),
                            todo.text.trim().chars().count(),
                        )
                    }))
                    .collect(),
                label: match (todo_confirmation || has_todo_choices, lang) {
                    (true, Lang::Zh) => "任务描述或补充说明",
                    (true, Lang::En) => "Task or extra instructions",
                    (false, Lang::Zh) => "任务描述",
                    (false, Lang::En) => "Task",
                }
                .into(),
                placeholder: match (todo_confirmation, lang) {
                    (true, Lang::Zh) => "可选：补充对该 TODO 的要求",
                    (true, Lang::En) => "Optional: add instructions for this TODO",
                    (false, Lang::Zh) => "输入新任务，或选择 TODO 后补充要求（最多 3000 字）",
                    (false, Lang::En) => {
                        "Enter a task, or add instructions after selecting a TODO (up to 3000 characters)"
                    }
                }
                .into(),
                max_chars: 3000,
            }),
            submit_label: match lang {
                Lang::Zh => "启动任务",
                Lang::En => "Start task",
            }
            .into(),
            default_action_id: Some(TASK_MANUAL_ACTION_ID.into()),
        },
        dismiss_action_id: "cancel".into(),
    };
    let Ok((entry, mut outcome)) = request::create_internal_confirm(
        spec,
        channel_id,
        lang.code(),
        &payload.workspace,
        kind.as_str(),
        Duration::from_secs(30 * 60),
    ) else {
        let _ = reply_channel_text(channel_id, config, "Failed to create task input").await;
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
        let _ = reply_channel_text(channel_id, config, "Task input channel is unavailable").await;
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
        let todo = selected_todo.or_else(|| {
            result
                .action_id
                .strip_prefix(TASK_TODO_ACTION_PREFIX)
                .and_then(|todo_id| todos.iter().find(|todo| todo.id == todo_id).cloned())
        });
        let task = match resolve_task_input(todo.as_ref(), result.comment.as_deref()) {
            Ok(task) => task,
            Err(error) => {
                let _ = reply_channel_text(&channel, &config, &error).await;
                return;
            }
        };
        let source = crate::integrations::agent_launch::LaunchSource {
            channel: channel.clone(),
            target: task_source_target(&config, &channel),
        };
        let delivery_request_id = uuid::Uuid::new_v4().to_string();
        let delivery = if let Some(todo) = todo.as_ref() {
            let expected: Vec<_> = todo.attachments.iter().map(|a| a.snapshot()).collect();
            crate::todos::prepare_delivery_consistent(
                &todo_project,
                &todo.id,
                &expected,
                &delivery_request_id,
            )
        } else {
            crate::todo_attachments::TodoDelivery::default()
        };
        let launch = match crate::integrations::agent_launch::create_record_with_files(
            source,
            std::path::Path::new(&payload.workspace),
            kind,
            permission,
            &task,
            &delivery.files,
            &delivery.warnings,
        ) {
            Ok(record) => {
                register_pending_launch_watch(&state, &record, &channel, &config, lang);
                match crate::integrations::agent_launch::open_terminal(&record) {
                    Ok(()) => {
                        if let Some(todo) = todo.as_ref() {
                            let _ =
                                crate::todos::take(&todo_project, std::slice::from_ref(&todo.id));
                        }
                        Ok(record)
                    }
                    Err(error) => {
                        crate::todo_attachments::cleanup_delivery(&delivery_request_id);
                        state
                            .pending_launches
                            .lock()
                            .unwrap()
                            .retain(|item| item.id != record.id);
                        Err(error)
                    }
                }
            }
            Err(error) => {
                crate::todo_attachments::cleanup_delivery(&delivery_request_id);
                Err(error)
            }
        };
        let text = match launch {
            Ok(_) => match lang {
                Lang::Zh => "已在电脑上打开新的 Terminal.app 窗口并启动任务。",
                Lang::En => "Opened a new Terminal.app window and started the task.",
            }
            .to_string(),
            Err(error) => format!("Failed to launch task: {error:#}"),
        };
        let _ = reply_channel_text(&channel, &config, &text).await;
        state.watch.notify.notify_one();
    });
}

fn resolve_task_input(
    todo: Option<&crate::todos::TodoEntry>,
    input: Option<&str>,
) -> Result<String, String> {
    let input = input.unwrap_or_default().trim();
    let task = match todo {
        Some(todo) if input.is_empty() => todo.text.trim().to_string(),
        Some(todo) => format!("{}\n\n{input}", todo.text.trim()),
        None => input.to_string(),
    };
    if task.is_empty() {
        return Err("Task must not be empty".into());
    }
    if task.contains('\0') {
        return Err("Task must not contain NUL characters".into());
    }
    if task.chars().count() > 3000 {
        return Err("Task exceeds 3000 characters".into());
    }
    Ok(task)
}

pub(super) fn task_source_target(config: &AppConfig, channel_id: &str) -> String {
    match channel_id {
        "feishu" => config.channels.feishu.open_id.clone(),
        "dingding" => config.channels.dingding.user_id.clone(),
        "telegram" => config.channels.telegram.chat_id.clone(),
        "slack" => config.channels.slack.user_id.clone(),
        _ => String::new(),
    }
}

/// 统一入站分派（与渠道无关），spec R3/R4：
/// - `/status`：始终回状态文本（开关开且因此切槽时附激活回执）。
/// - `/here`：开关开时激活+补推+回执；开关关时改回**引导文案**（不再静默忽略）。
/// - `/help` 与未知 `/命令`：回**动态引导文案**（命令永不被卡片当答案，安全回）。
/// - 普通文本：该渠道**有活动在途提问**时退避（交渠道会话确认/引导，避免重复回复）；
///   否则开关开按现状切槽（切换则回激活回执），未切换/未开则回引导（liveness）。
/// - 非文本消息（`text=None`）：无活动在途提问时回引导（有则交会话确认附件）。
pub(super) async fn handle_inbound(state: &Arc<ServerState>, channel_id: &str, text: Option<&str>) {
    use crate::autochannel::{classify, help_view, Command, Parsed};
    let lang = Lang::current();
    let config = state.config_snapshot();
    let auto = config.channels.auto_activation;
    // `/watch` 渠道门控（spec docs/specs/im-watch.md）：决定 help 是否列 watch 命令。
    let watch_cmd = crate::watch::channel_supported(channel_id);
    // 命令展示前缀：Slack 客户端拦截 `/` 输入，提示用 `!`；其余渠道 `/`。
    let prefix = crate::autochannel::cmd_prefix(channel_id);
    // 任何用户入站消息都会把 watch 卡顶上去（机器人的文本回执紧随其后，同属一次扰动）。
    mark_watch_disturbed(state, channel_id);

    let Some(text) = text else {
        // 非文本消息（图片/文件）：有活动提问 → 交渠道会话确认；否则回引导（liveness）。
        if !has_active_question_on(state, channel_id) {
            let _ = reply_channel_help(
                channel_id,
                &config,
                &help_view(auto, false, watch_cmd, prefix, lang),
                lang,
            )
            .await;
        }
        return;
    };

    match classify(text) {
        Parsed::Command(Command::New { has_args }) => {
            if has_args {
                let prefix = crate::autochannel::cmd_prefix(channel_id);
                let text = match lang {
                    Lang::Zh => format!("用法：{prefix}new（任务内容请在后续输入卡中填写）"),
                    Lang::En => {
                        format!("Usage: {prefix}new (enter the task in the following form)")
                    }
                };
                let _ = reply_channel_text(channel_id, &config, &text).await;
            } else {
                start_new_task_flow(state, channel_id, &config, lang).await;
            }
        }
        Parsed::Command(Command::Fork {
            sel,
            has_invalid_args,
        }) => {
            if has_invalid_args {
                let prefix = crate::autochannel::cmd_prefix(channel_id);
                let text = match lang {
                    Lang::Zh => format!("用法：{prefix}fork [编号]（分支指令请在后续输入卡中填写）"),
                    Lang::En => format!(
                        "Usage: {prefix}fork [number] (enter branch instructions in the following form)"
                    ),
                };
                let _ = reply_channel_text(channel_id, &config, &text).await;
            } else {
                start_fork_flow(state, channel_id, sel, &config, lang).await;
            }
        }
        Parsed::Command(Command::Status(sel)) => {
            // 状态查询是独立功能：始终响应。仅当开关开、且本次因 /status 切了活跃槽时附激活回执。
            let (switched, n) = if auto {
                set_active_channel(state, channel_id).await
            } else {
                (false, 0)
            };
            let snapshot = state.agents.snapshot();
            match sel {
                // /status <编号>：单个 agent 的当前活动详情（直达，不弹卡）。
                Some(id) => {
                    let mut body = String::new();
                    if switched {
                        body.push_str(&crate::autochannel::activated_receipt(n, lang));
                        body.push_str("\n\n");
                    }
                    body.push_str(&crate::autochannel::status_detail_text(
                        &snapshot, id, prefix, lang,
                    ));
                    let _ = reply_channel_text(channel_id, &config, &body).await;
                }
                // /status（无参）：切槽回执（若有）作独立文本，随后推「选择要查看的 Agent」单选卡；
                // 无 agent / 非飞书 → 回既有工作中/空闲文本列表兜底。
                None => {
                    if switched {
                        let _ = reply_channel_text(
                            channel_id,
                            &config,
                            &crate::autochannel::activated_receipt(n, lang),
                        )
                        .await;
                    }
                    let opts = crate::select::agent_options(
                        &snapshot,
                        &std::collections::HashSet::new(),
                        now_secs(),
                        lang,
                    );
                    let sent = send_agent_picker(
                        state,
                        channel_id,
                        &config,
                        PickerKind::Status,
                        crate::select::title_status(lang),
                        opts,
                        None,
                        lang,
                    )
                    .await;
                    if !sent {
                        let _ = reply_channel_text(
                            channel_id,
                            &config,
                            &crate::autochannel::status_text(&snapshot, lang),
                        )
                        .await;
                    }
                }
            }
        }
        Parsed::Command(Command::Here) => {
            if !auto {
                // 关态无「活跃槽」概念：回引导（替代旧的静默忽略）。
                let has_q = has_active_question_on(state, channel_id);
                let _ = reply_channel_help(
                    channel_id,
                    &config,
                    &help_view(auto, has_q, watch_cmd, prefix, lang),
                    lang,
                )
                .await;
                return;
            }
            // 激活 + 补推（在 set_active_channel 内完成）；/here 始终回执（即便已是当前槽，n=0）。
            let (_switched, n) = set_active_channel(state, channel_id).await;
            let _ = reply_channel_text(
                channel_id,
                &config,
                &crate::autochannel::activated_receipt(n, lang),
            )
            .await;
        }
        // /watch、/unwatch：实时关注（P1 仅飞书；其余渠道回「暂仅支持飞书」提示）。
        Parsed::Command(Command::Watch(sel)) => {
            // `/watch` 属「在该渠道操作」→ 设为活跃槽（用户决策；配合 D2 让离开时自动结束 watch）。
            activate_channel_on_action(state, channel_id, &config, lang).await;
            match sel {
                // /watch <编号>：直达关注（不弹卡）。
                Some(_) => handle_watch_cmd(state, channel_id, sel, &config, lang).await,
                // /watch（无参）：推「选择要关注的 Agent」单选卡（仅工作中；已关注者带
                // 「· 关注中」徽标，点它＝换新卡）。无工作中 agent → 回文本列表兜底。
                None => {
                    let snapshot = state.agents.snapshot();
                    let watching = watching_sessions(state, channel_id);
                    let opts = crate::select::watch_options(&snapshot, &watching, now_secs(), lang);
                    let sent = send_agent_picker(
                        state,
                        channel_id,
                        &config,
                        PickerKind::Watch,
                        crate::select::title_watch(lang),
                        opts,
                        None,
                        lang,
                    )
                    .await;
                    if !sent {
                        handle_watch_cmd(state, channel_id, None, &config, lang).await;
                    }
                }
            }
        }
        Parsed::Command(Command::Unwatch(sel)) => {
            use crate::autochannel::WatchSel;
            // 仅「无参且本渠道有多个关注」时弹卡；0/1/编号/all 一律直达（行为不变）。
            let multi = matches!(sel, WatchSel::Auto)
                && state
                    .watch
                    .subs
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.channel == channel_id)
                    .count()
                    >= 2;
            let sent = if multi {
                let snapshot = state.agents.snapshot();
                let opts = unwatch_options(state, channel_id, &snapshot, lang);
                send_agent_picker(
                    state,
                    channel_id,
                    &config,
                    PickerKind::Unwatch,
                    crate::select::title_unwatch(lang),
                    opts,
                    None,
                    lang,
                )
                .await
            } else {
                false
            };
            if !sent {
                handle_unwatch_cmd(state, channel_id, sel, &config, lang).await;
            }
        }
        // /msg、/msg-clear：插话（spec agent-interject D9；与 /status 同门控，始终响应）。
        Parsed::Command(Command::Msg(sel, content)) => {
            handle_msg_cmd(state, channel_id, sel, content, &config, lang).await;
        }
        Parsed::Command(Command::MsgClear(sel)) => {
            handle_msg_clear_cmd(state, channel_id, sel, &config, lang).await;
        }
        // /diff · /stage · /transcript（spec im-diff-stage-transcript）。
        Parsed::Command(Command::Diff(sel)) => {
            handle_export_cmd(state, channel_id, sel, PickerKind::Diff, &config, lang).await;
        }
        Parsed::Command(Command::Stage(sel)) => {
            handle_export_cmd(state, channel_id, sel, PickerKind::Stage, &config, lang).await;
        }
        Parsed::Command(Command::Transcript(sel)) => {
            handle_export_cmd(
                state,
                channel_id,
                sel,
                PickerKind::Transcript,
                &config,
                lang,
            )
            .await;
        }
        // /todo · /todo-rm：项目待办（spec todo-whats-next D8；与 /status 同门控）。
        Parsed::Command(Command::Todo(sel, content)) => {
            handle_todo_cmd(state, channel_id, sel, content, &config, lang).await;
        }
        Parsed::Command(Command::TodoRm(sel)) => {
            handle_todo_rm_cmd(state, channel_id, sel, &config, lang).await;
        }
        Parsed::Command(Command::TodoAuto(sel, content)) => {
            handle_todo_auto_cmd(state, channel_id, sel, content, &config, lang).await;
        }
        // /yolo：Codex YOLO 模式管理（spec codex-permission-remember D53）。
        Parsed::Command(Command::Yolo(sel)) => {
            activate_channel_on_action(state, channel_id, &config, lang).await;
            handle_yolo_cmd(state, channel_id, sel, &config, lang).await;
        }
        Parsed::Command(Command::Help) | Parsed::UnknownCommand => {
            let has_q = has_active_question_on(state, channel_id);
            let _ = reply_channel_help(
                channel_id,
                &config,
                &help_view(auto, has_q, watch_cmd, prefix, lang),
                lang,
            )
            .await;
        }
        Parsed::Text => {
            let has_q = has_active_question_on(state, channel_id);
            if auto {
                let (switched, n) = set_active_channel(state, channel_id).await;
                if switched {
                    let _ = reply_channel_text(
                        channel_id,
                        &config,
                        &crate::autochannel::activated_receipt(n, lang),
                    )
                    .await;
                    return;
                }
            }
            if has_q {
                return;
            }
            let _ = reply_channel_help(
                channel_id,
                &config,
                &help_view(auto, false, watch_cmd, prefix, lang),
                lang,
            )
            .await;
        }
    }
}

/// 把活跃槽切到 `new_id`（IM id 或 "popup"）。统一入口：「在哪个渠道说话 / 作答就用哪个」。
/// 切换时：持久化 → 给**旧**渠道（若为 IM）发反激活提示 → 把**在途未答**问题补推给**新**渠道
/// （若为 IM）。补推是「渠道激活」的固有行为，与触发方式无关（`/here`、普通消息、`/status`、作答切槽均同）。
/// 返回 `(是否切换, 补推条数)`；新渠道激活回执文案由调用方按场景发送（弹窗无需）。
pub(super) async fn set_active_channel(state: &Arc<ServerState>, new_id: &str) -> (bool, usize) {
    let prev = {
        let mut guard = state.active_channel.lock().unwrap();
        if guard.as_deref() == Some(new_id) {
            return (false, 0);
        }
        let prev = guard.take();
        *guard = Some(new_id.to_string());
        prev
    };
    crate::autochannel::save_active(Some(new_id));
    log(&format!("auto-channel: active slot -> {}", new_id));
    let cfg = state.config_snapshot();
    // 旧渠道反激活提示（仅真实 IM；"popup" / None 无收件端，跳过）。
    if let Some(old) = prev {
        if old != "popup" && old != new_id {
            let _ = reply_channel_text(
                &old,
                &cfg,
                &crate::autochannel::deactivated_receipt(new_id, Lang::current()),
            )
            .await;
            // 反激活提示可在无该渠道入站时发出（如在别的渠道作答切槽）→ 单独记扰动。
            mark_watch_disturbed(state, &old);
            // 「按需发送」子开关：活跃槽从某 IM 切走时自动结束该渠道的全部 watch（D1/D2，
            // spec docs/specs/im-auto-end-watch.md）。卡片定格「已切换到 {new} · 自动结束关注」，
            // 不额外发文字（D4，反激活提示已发）。
            if cfg.channels.auto_activation && cfg.channels.auto_end_watch {
                let targets: Vec<WatchEntry> = state
                    .watch
                    .subs
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.channel == old)
                    .cloned()
                    .collect();
                let final_kind = crate::watch::FinalKind::AutoStopped(
                    crate::autochannel::channel_label(new_id, Lang::current()),
                );
                finalize_and_drop_watches(state, &old, &targets, final_kind, &cfg, Lang::current())
                    .await;
            }
        }
    }
    // 激活即补推在途（仅真实 IM；弹窗无卡片概念）。
    let backfilled = if new_id != "popup" {
        backfill_inflight(state, new_id, &cfg).await
    } else {
        0
    };
    if backfilled > 0 {
        mark_watch_disturbed(state, new_id); // 补推的提问卡也是「非 watch」消息。
    }
    (true, backfilled)
}

/// 「在该渠道操作即激活」的统一入口：`auto_activation` 开时把活跃槽切到本渠道；真正切换了
/// 就回一条激活回执（与 `/here`、`/status`、普通文本一致）。用于 `/watch`、`/msg`、`/msg-clear`、
/// `/diff`/`/stage`/`/transcript` 及单选卡点选——这些本属「在渠道上说话」，理应设为活跃槽。
pub(super) async fn activate_channel_on_action(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    if !config.channels.auto_activation {
        return;
    }
    let (switched, n) = set_active_channel(state, channel_id).await;
    if switched {
        let _ = reply_channel_text(
            channel_id,
            config,
            &crate::autochannel::activated_receipt(n, lang),
        )
        .await;
    }
}

/// 把所有「在途未答」问题补推为 `channel_id` 的卡片（已挂接该渠道的请求跳过，避免重发）。返回补推数。
pub(super) async fn backfill_inflight(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
) -> usize {
    let mut n = 0;
    for entry in state.registry.in_flight_entries() {
        if entry.coordinator.has_channel(channel_id) {
            continue;
        }
        if let Some(ch) = build_im_channel(channel_id, config, state).await {
            entry.coordinator.register(ch.clone());
            let origin = super::im_conversation_origin(&entry, None);
            ch.start(entry.request(), &origin, entry.coordinator.clone());
            n += 1;
        }
    }
    for entry in state.registry.in_flight_confirm_entries() {
        if entry.coordinator.is_terminal() || entry.has_delivery(channel_id) {
            continue;
        }
        let popup_available = popup_should_dispatch(config, has_display());
        let eligible = confirm_im_candidates(&entry, state, config, popup_available);
        if !eligible.contains(&channel_id) {
            continue;
        }
        entry.start_delivery(channel_id);
        attach_confirm_im_channels(&entry, state, config, &[channel_id]).await;
        n += 1;
    }
    n
}

/// 为补推构造一个挂共享 Router 的渠道实例（各渠道仅此处差异：取对应 Router + 构造对应 Channel）。
pub(super) async fn build_im_channel(
    channel_id: &str,
    config: &AppConfig,
    state: &Arc<ServerState>,
) -> Option<Arc<dyn Channel>> {
    let ch: Arc<dyn Channel> = match channel_id {
        "feishu" => {
            let router = ensure_fs_router(state, &config.channels.feishu).await?;
            Arc::new(FeishuChannel::shared(
                config.channels.feishu.clone(),
                router,
            ))
        }
        "dingding" => {
            let dd = &config.channels.dingding;
            let router =
                ensure_dd_router(state, dd.client_id.trim(), dd.client_secret.trim()).await?;
            Arc::new(DingTalkChannel::shared(dd.clone(), router))
        }
        "slack" => {
            let router = ensure_sl_router(state, &config.channels.slack).await?;
            Arc::new(SlackChannel::shared(config.channels.slack.clone(), router))
        }
        "telegram" => {
            let router = ensure_tg_router(state, &config.channels.telegram).await?;
            Arc::new(TelegramChannel::shared(
                config.channels.telegram.clone(),
                router,
            ))
        }
        _ => return None,
    };
    Some(ch)
}

/// 向某渠道回一条纯文本（回执 / 状态）。各渠道仅此处差异：用对应 OpenAPI client 发文本。
pub(super) async fn reply_channel_text(
    channel_id: &str,
    config: &AppConfig,
    text: &str,
) -> Result<(), String> {
    match channel_id {
        "feishu" => {
            let client = crate::feishu::client::FeishuClient::new(&config.channels.feishu)
                .map_err(|e| e.to_string())?;
            client
                .send_text(text)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        "dingding" => {
            let client = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
                .map_err(|e| e.to_string())?;
            client.send_oto_text(text).await.map_err(|e| e.to_string())
        }
        "slack" => {
            let client = crate::slack::client::SlackClient::new(&config.channels.slack)
                .map_err(|e| e.to_string())?;
            let channel = client.open_dm().await.map_err(|e| e.to_string())?;
            client
                .post_text(&channel, text)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        "telegram" => {
            let tg = &config.channels.telegram;
            let client = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            )
            .map_err(|e| e.to_string())?;
            client
                .send_message(text, None, None)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        _ => Err(format!("reply unsupported for channel: {}", channel_id)),
    }
}

/// Send structured help using each channel's richest native message type, then retry once as
/// grouped plain text if the rich payload is rejected.
pub(super) async fn reply_channel_help(
    channel_id: &str,
    config: &AppConfig,
    view: &crate::autochannel::HelpView,
    lang: Lang,
) -> Result<(), String> {
    let plain = crate::autochannel::render_help_plain(view, lang);
    match channel_id {
        "feishu" => {
            let client = crate::feishu::client::FeishuClient::new(&config.channels.feishu)
                .map_err(|e| e.to_string())?;
            let card = crate::feishu::card::build_help_card(view, lang);
            match client.send_card(&card).await {
                Ok(_) => Ok(()),
                Err(_) => client
                    .send_text(&plain)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            }
        }
        "dingding" => {
            let client = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
                .map_err(|e| e.to_string())?;
            let markdown = crate::dingtalk::help::render(view, lang);
            match client.send_oto_markdown(&view.title, &markdown).await {
                Ok(()) => Ok(()),
                Err(_) => client
                    .send_oto_text(&plain)
                    .await
                    .map_err(|e| e.to_string()),
            }
        }
        "slack" => {
            let client = crate::slack::client::SlackClient::new(&config.channels.slack)
                .map_err(|e| e.to_string())?;
            let channel = client.open_dm().await.map_err(|e| e.to_string())?;
            let blocks = crate::slack::blockkit::build_help_blocks(view, lang);
            match client.post_message(&channel, Some(&blocks), &plain).await {
                Ok(_) => Ok(()),
                Err(_) => client
                    .post_text(&channel, &plain)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            }
        }
        "telegram" => {
            let tg = &config.channels.telegram;
            let client = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            )
            .map_err(|e| e.to_string())?;
            let html = crate::telegram::help::render(view, lang);
            match client.send_message(&html, Some("HTML"), None).await {
                Ok(_) => Ok(()),
                Err(_) => client
                    .send_message(&plain, None, None)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            }
        }
        _ => Err(format!(
            "help reply unsupported for channel: {}",
            channel_id
        )),
    }
}

// ── /yolo（spec codex-permission-remember D53）──

/// `/yolo`：列出开着 YOLO 的会话（带「关闭」按钮的选择卡）。`off <编号>` 按 Agent 编号直
/// 关；`off` 缺省编号且恰一个开着 → 直关，多个 → 弹同一张选择卡。卡片发送失败（渠道不支
/// 持）→ 文本列表兜底。
pub(super) async fn handle_yolo_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: crate::autochannel::YoloSel,
    config: &AppConfig,
    lang: Lang,
) {
    use crate::autochannel::YoloSel;
    let ids = tokio::task::spawn_blocking(crate::permission_rules::yolo_session_ids)
        .await
        .unwrap_or_default();
    if ids.is_empty() {
        let _ =
            reply_channel_text(channel_id, config, crate::i18n::tr(lang, "select.yoloNone")).await;
        return;
    }
    // 直达目标：`off <编号>` 用 agent 快照解析；`off` 缺省且唯一 → 该会话。
    // 编号解析不到（agent 已结束 / 编号漂移）→ 落到选择卡，点选兜底。
    let direct = match sel {
        YoloSel::Off(Some(seq)) => {
            let snapshot = state.agents.snapshot();
            crate::autochannel::find_by_seq(&snapshot, seq)
                .and_then(|rec| rec.get("sessionId").and_then(|v| v.as_str()))
                .map(str::to_string)
        }
        YoloSel::Off(None) if ids.len() == 1 => Some(ids[0].clone()),
        _ => None,
    };
    if let Some(session_id) = direct {
        let sid = session_id.clone();
        let removed =
            tokio::task::spawn_blocking(move || crate::permission_rules::disable_yolo(&sid))
                .await
                .unwrap_or(false);
        let text = if removed {
            log(&format!(
                "yolo turned off via {channel_id} for session {session_id}"
            ));
            crate::i18n::tr(lang, "select.yoloOffDone")
                .replace("{name}", &yolo_session_label(state, &session_id))
        } else {
            crate::i18n::tr(lang, "select.yoloOffGone").to_string()
        };
        let _ = reply_channel_text(channel_id, config, &text).await;
        return;
    }
    let opts = yolo_options(state, &ids, lang);
    let sent = send_agent_picker(
        state,
        channel_id,
        config,
        PickerKind::Yolo,
        crate::select::title_yolo(lang),
        opts,
        None,
        lang,
    )
    .await;
    if !sent {
        // 渠道不支持卡片：文本列表 + 用法提示兜底。
        let mut text = crate::select::title_yolo(lang);
        for session_id in &ids {
            text.push_str(&format!("\n• {}", yolo_session_label(state, session_id)));
        }
        let prefix = crate::autochannel::cmd_prefix(channel_id);
        text.push_str("\n\n");
        text.push_str(&crate::i18n::tr(lang, "select.yoloOffHint").replace("{p}", prefix));
        let _ = reply_channel_text(channel_id, config, &text).await;
    }
}

// ── /diff · /stage · /transcript（spec im-diff-stage-transcript）──

pub(super) async fn handle_export_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    kind: PickerKind,
    config: &AppConfig,
    lang: Lang,
) {
    activate_channel_on_action(state, channel_id, config, lang).await;
    match sel {
        Some(n) => {
            let snapshot = state.agents.snapshot();
            let Some(rec) = crate::autochannel::find_by_seq(&snapshot, n) else {
                let prefix = crate::autochannel::cmd_prefix(channel_id);
                let text = crate::i18n::tr(lang, "export.notFound")
                    .replace("{n}", &n.to_string())
                    .replace("{p}", prefix);
                let _ = reply_channel_text(channel_id, config, &text).await;
                return;
            };
            let sid = rec
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if sid.is_empty() {
                let _ =
                    reply_channel_text(channel_id, config, crate::i18n::tr(lang, "export.noCwd"))
                        .await;
                return;
            }
            match kind {
                PickerKind::Diff => run_diff(state, channel_id, &sid, config, lang).await,
                PickerKind::Transcript => {
                    run_transcript(state, channel_id, &sid, config, lang).await
                }
                PickerKind::Stage => run_stage_confirm(state, channel_id, &sid, config, lang).await,
                _ => {}
            }
        }
        None => {
            let snapshot = state.agents.snapshot();
            let opts = crate::select::agent_options(
                &snapshot,
                &std::collections::HashSet::new(),
                now_secs(),
                lang,
            );
            let title = match kind {
                PickerKind::Diff => crate::select::title_diff(lang),
                PickerKind::Stage => crate::select::title_stage(lang),
                PickerKind::Transcript => crate::select::title_transcript(lang),
                _ => String::new(),
            };
            let sent =
                send_agent_picker(state, channel_id, config, kind, title, opts, None, lang).await;
            if !sent {
                let _ = reply_channel_text(
                    channel_id,
                    config,
                    &crate::autochannel::status_text(&snapshot, lang),
                )
                .await;
            }
        }
    }
}

pub(super) fn agent_export_meta(
    snapshot: &serde_json::Value,
    session_id: &str,
) -> Option<(u64, String, String, String, Option<String>)> {
    let rec = find_agent_by_session(snapshot, session_id)?;
    let seq = rec.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
    let kind = rec
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = rec
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cwd = rec
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let project = cwd
        .as_deref()
        .and_then(crate::autochannel::project_name)
        .unwrap_or_else(|| "project".into());
    Some((seq, kind, title, project, cwd))
}

pub(super) async fn run_diff(
    state: &Arc<ServerState>,
    channel_id: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let Some((seq, kind, _title, project, cwd)) = agent_export_meta(&snapshot, session_id) else {
        let _ = reply_channel_text(channel_id, config, crate::i18n::tr(lang, "export.noCwd")).await;
        return;
    };
    let Some(cwd) = cwd else {
        let _ = reply_channel_text(channel_id, config, crate::i18n::tr(lang, "export.noCwd")).await;
        return;
    };
    let Some(root) = crate::gitutil::find_git_root(std::path::Path::new(&cwd)) else {
        let text = crate::i18n::tr(lang, "export.notGit").replace("{path}", &cwd);
        let _ = reply_channel_text(channel_id, config, &text).await;
        return;
    };
    let model = match crate::gitutil::build_diff_model(&root) {
        Ok(m) => m,
        Err(e) => {
            let _ = reply_channel_text(channel_id, config, &e).await;
            return;
        }
    };
    if model.total_paths == 0 {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "export.noUnstaged"),
        )
        .await;
        return;
    }
    let meta = format!("Diff · [{seq}] {kind} · {project}");
    // 用户定案：直接发附件，不附摘要消息头。
    let (bytes, name) = match channel_id {
        "feishu" => {
            let diff = crate::export::render_diff_file(&model);
            (
                diff.into_bytes(),
                crate::export::diff_filename(seq, &project, "diff"),
            )
        }
        "dingding" | "slack" => match crate::export::render_diff_docx(&model, &meta) {
            Ok(b) => (b, crate::export::diff_filename(seq, &project, "docx")),
            Err(e) => {
                let t = crate::i18n::tr(lang, "export.sendFailed").replace("{err}", &e);
                let _ = reply_channel_text(channel_id, config, &t).await;
                return;
            }
        },
        _ => {
            // telegram (and any other)
            let html = crate::export::render_diff_html(&model, &meta);
            (
                html.into_bytes(),
                crate::export::diff_filename(seq, &project, "html"),
            )
        }
    };
    if let Err(e) = reply_channel_file(channel_id, config, &name, &bytes).await {
        let t = crate::i18n::tr(lang, "export.sendFailed").replace("{err}", &e);
        let _ = reply_channel_text(channel_id, config, &t).await;
    }
}

pub(super) async fn run_transcript(
    state: &Arc<ServerState>,
    channel_id: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let Some((seq, kind_s, title, project, _)) = agent_export_meta(&snapshot, session_id) else {
        let _ = reply_channel_text(channel_id, config, crate::i18n::tr(lang, "export.noCwd")).await;
        return;
    };
    let Some(akind) = crate::agents::AgentKind::parse(&kind_s) else {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "export.noTranscript"),
        )
        .await;
        return;
    };
    let doc = match crate::agents::transcript_full::load_events(akind, session_id) {
        Ok(d) => d,
        Err(_) => {
            let _ = reply_channel_text(
                channel_id,
                config,
                crate::i18n::tr(lang, "export.noTranscript"),
            )
            .await;
            return;
        }
    };
    let meta = format!(
        "Transcript · [{seq}] {kind_s} · {}",
        if title.is_empty() { &project } else { &title }
    );
    // 用户定案：直接发附件，不附摘要消息头。
    let slug_src = if title.is_empty() { &project } else { &title };
    let (bytes, name) = match channel_id {
        "feishu" => {
            let md = crate::export::render_transcript_md(&doc, &meta);
            (
                md.into_bytes(),
                crate::export::transcript_filename(seq, slug_src, "md"),
            )
        }
        "dingding" | "slack" => match crate::export::render_transcript_docx(&doc, &meta) {
            Ok(b) => (b, crate::export::transcript_filename(seq, slug_src, "docx")),
            Err(e) => {
                let t = crate::i18n::tr(lang, "export.sendFailed").replace("{err}", &e);
                let _ = reply_channel_text(channel_id, config, &t).await;
                return;
            }
        },
        _ => {
            let html = crate::export::render_transcript_html(&doc, &meta);
            (
                html.into_bytes(),
                crate::export::transcript_filename(seq, slug_src, "html"),
            )
        }
    };
    if let Err(e) = reply_channel_file(channel_id, config, &name, &bytes).await {
        let t = crate::i18n::tr(lang, "export.sendFailed").replace("{err}", &e);
        let _ = reply_channel_text(channel_id, config, &t).await;
    }
}

pub(super) async fn run_stage_confirm(
    state: &Arc<ServerState>,
    channel_id: &str,
    session_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    let snapshot = state.agents.snapshot();
    let Some((_seq, _kind, _title, project, cwd)) = agent_export_meta(&snapshot, session_id) else {
        let _ = reply_channel_text(channel_id, config, crate::i18n::tr(lang, "export.noCwd")).await;
        return;
    };
    let Some(cwd) = cwd else {
        let _ = reply_channel_text(channel_id, config, crate::i18n::tr(lang, "export.noCwd")).await;
        return;
    };
    let Some(root) = crate::gitutil::find_git_root(std::path::Path::new(&cwd)) else {
        let text = crate::i18n::tr(lang, "export.notGit").replace("{path}", &cwd);
        let _ = reply_channel_text(channel_id, config, &text).await;
        return;
    };
    let preview = match crate::gitutil::preview_stage(&root) {
        Ok(p) => p,
        Err(e) => {
            let _ = reply_channel_text(channel_id, config, &e).await;
            return;
        }
    };
    if preview.paths.is_empty() {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "export.noUnstaged"),
        )
        .await;
        return;
    }
    let view =
        crate::confirm::stage_confirm_view(lang, &project, &preview.paths, preview.paths.len());
    let Some(mid) = crate::confirm::transport::send(channel_id, config, &view).await else {
        let _ = reply_channel_text(channel_id, config, "Failed to send confirm card").await;
        return;
    };
    {
        let mut cs = state.select.confirms.lock().unwrap();
        let now = now_secs();
        cs.retain(|c| now.saturating_sub(c.created_at) < SELECT_PICKER_TTL_SECS);
        cs.push(ConfirmEntry {
            channel: channel_id.to_string(),
            message_id: mid,
            session_id: session_id.to_string(),
            git_root: root,
            paths_fp: crate::gitutil::paths_fingerprint(&preview.paths),
            view,
            created_at: now,
        });
    }
    state.select.route_refresh.notify_one();
}

/// 钉钉 stage 确认（专用确认模板双按钮）：成功返回 true。
pub(super) async fn handle_stage_dd_submit(
    state: &Arc<ServerState>,
    data: &serde_json::Value,
) -> bool {
    let Some((otid, slot)) = crate::dingtalk::confirm::parse_confirm_action(data) else {
        return false;
    };
    let has = {
        let cs = state.select.confirms.lock().unwrap();
        cs.iter()
            .any(|c| c.channel == "dingding" && c.message_id == otid)
    };
    if !has {
        return false;
    }
    handle_confirm_action(state, "dingding", &otid, slot, None).await;
    true
}

pub(super) async fn handle_confirm_action(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    slot: crate::confirm::ConfirmSlot,
    ack: Option<crate::confirm::transport::FsAck>,
) {
    let lang = Lang::current();
    let config = state.config_snapshot();
    let entry = {
        let mut cs = state.select.confirms.lock().unwrap();
        let pos = cs
            .iter()
            .position(|c| c.channel == channel_id && c.message_id == mid);
        pos.map(|i| cs.remove(i))
    };
    let Some(entry) = entry else {
        if let Some(ack) = ack {
            let _ = ack.send(None);
        }
        return;
    };
    let action_id = entry.view.action_id_for_slot(slot).to_string();
    if action_id == crate::confirm::STAGE_CANCEL_ACTION_ID {
        let text = crate::i18n::tr(lang, "confirm.stageCancelled").to_string();
        let fv = crate::confirm::ConfirmFinalView {
            title: entry.view.title.clone(),
            body: text.clone(),
            label: crate::confirm::transport::truncate_for_label(&text),
        };
        crate::confirm::transport::finalize(channel_id, &config, mid, &fv, ack).await;
        state.select.route_refresh.notify_one();
        return;
    }
    if action_id != crate::confirm::STAGE_CONFIRM_ACTION_ID {
        if let Some(ack) = ack {
            let _ = ack.send(None);
        }
        return;
    }
    // Re-check paths fingerprint.
    let preview = match crate::gitutil::preview_stage(&entry.git_root) {
        Ok(p) => p,
        Err(e) => {
            let text = crate::i18n::tr(lang, "confirm.stageFailed").replace("{err}", &e);
            let fv = crate::confirm::ConfirmFinalView {
                title: entry.view.title.clone(),
                body: text.clone(),
                label: crate::confirm::transport::truncate_for_label(&text),
            };
            crate::confirm::transport::finalize(channel_id, &config, mid, &fv, ack).await;
            state.select.route_refresh.notify_one();
            return;
        }
    };
    let fp = crate::gitutil::paths_fingerprint(&preview.paths);
    if fp != entry.paths_fp {
        let text = crate::i18n::tr(lang, "confirm.stageChanged").to_string();
        let fv = crate::confirm::ConfirmFinalView {
            title: entry.view.title.clone(),
            body: text.clone(),
            label: crate::confirm::transport::truncate_for_label(&text),
        };
        crate::confirm::transport::finalize(channel_id, &config, mid, &fv, ack).await;
        let _ = reply_channel_text(channel_id, &config, &text).await;
        state.select.route_refresh.notify_one();
        return;
    }
    match crate::gitutil::stage_all(&entry.git_root) {
        Ok(r) => {
            let text = crate::i18n::tr(lang, "confirm.stageDone")
                .replace("{n}", &r.paths.len().to_string());
            let fv = crate::confirm::ConfirmFinalView {
                title: entry.view.title.clone(),
                body: text.clone(),
                label: crate::confirm::transport::truncate_for_label(&text),
            };
            crate::confirm::transport::finalize(channel_id, &config, mid, &fv, ack).await;
            let show: Vec<&str> = r
                .paths
                .iter()
                .take(crate::confirm::STAGE_LIST_MAX)
                .map(|s| s.as_str())
                .collect();
            let mut detail = text.clone();
            if !show.is_empty() {
                detail.push('\n');
                detail.push_str(&show.join("\n"));
                if r.paths.len() > show.len() {
                    detail.push_str(&format!("\n… +{}", r.paths.len() - show.len()));
                }
            }
            let _ = reply_channel_text(channel_id, &config, &detail).await;
        }
        Err(e) => {
            let text = crate::i18n::tr(lang, "confirm.stageFailed").replace("{err}", &e);
            let fv = crate::confirm::ConfirmFinalView {
                title: entry.view.title.clone(),
                body: text.clone(),
                label: crate::confirm::transport::truncate_for_label(&text),
            };
            crate::confirm::transport::finalize(channel_id, &config, mid, &fv, ack).await;
            let _ = reply_channel_text(channel_id, &config, &text).await;
        }
    }
    state.select.route_refresh.notify_one();
}

pub(super) async fn reply_channel_file(
    channel_id: &str,
    config: &AppConfig,
    file_name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let dir = crate::paths::request_temp_dir(&format!("export-{}", now_ms()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(file_name);
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let path_str = path.to_string_lossy().to_string();
    let result = match channel_id {
        "feishu" => {
            let client = crate::feishu::client::FeishuClient::new(&config.channels.feishu)
                .map_err(|e| e.to_string())?;
            let key = client
                .upload_file(&path_str, file_name)
                .await
                .map_err(|e| e.to_string())?;
            client
                .send_file(&key)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        "telegram" => {
            let tg = &config.channels.telegram;
            let client = crate::telegram::TelegramClient::new(
                tg.bot_token.clone(),
                tg.chat_id.clone(),
                tg.api_base_url.clone(),
            )
            .map_err(|e| e.to_string())?;
            client
                .send_document(&path_str, file_name)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        "slack" => {
            let client = crate::slack::client::SlackClient::new(&config.channels.slack)
                .map_err(|e| e.to_string())?;
            let dm = client.open_dm().await.map_err(|e| e.to_string())?;
            client
                .upload_file(&dm, &path_str, file_name)
                .await
                .map_err(|e| e.to_string())
        }
        "dingding" => {
            let client = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
                .map_err(|e| e.to_string())?;
            let media_id = client
                .upload_media(&path_str, "file")
                .await
                .map_err(|e| e.to_string())?;
            let ext = std::path::Path::new(file_name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("docx");
            client
                .send_oto_file(&media_id, file_name, ext)
                .await
                .map_err(|e| e.to_string())
        }
        _ => Err(format!("file send unsupported for channel: {}", channel_id)),
    };
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    result
}

/// 从飞书 im.message.receive_v1 的 event 取 (发送者 open_id, 文本)。非文本消息返回 None。
pub(super) fn fs_text_and_sender(event: &serde_json::Value) -> Option<(String, String)> {
    let open_id = event
        .get("sender")
        .and_then(|s| s.get("sender_id"))
        .and_then(|i| i.get("open_id"))
        .and_then(|v| v.as_str())?
        .to_string();
    let message = event.get("message")?;
    if message.get("message_type").and_then(|v| v.as_str()) != Some("text") {
        return None;
    }
    let content_str = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let content: serde_json::Value = serde_json::from_str(content_str).ok()?;
    let text = content.get("text").and_then(|v| v.as_str())?.to_string();
    Some((open_id, text))
}

#[cfg(test)]
mod task_input_tests {
    use super::*;

    fn todo(text: &str) -> crate::todos::TodoEntry {
        crate::todos::TodoEntry {
            id: "todo-1".into(),
            text: text.into(),
            created_at_ms: 1,
            agent_kind: None,
            auto: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn task_input_uses_manual_text_or_todo_snapshot_with_supplement() {
        assert_eq!(
            resolve_task_input(None, Some("  do work  ")).unwrap(),
            "do work"
        );
        assert_eq!(
            resolve_task_input(Some(&todo("fix login")), None).unwrap(),
            "fix login"
        );
        assert_eq!(
            resolve_task_input(Some(&todo("fix login")), Some("  add regression test  ")).unwrap(),
            "fix login\n\nadd regression test"
        );
    }

    #[test]
    fn task_input_validates_the_final_combined_task() {
        assert!(resolve_task_input(None, Some("  ")).is_err());
        assert!(resolve_task_input(None, Some("bad\0task")).is_err());
        let long = "x".repeat(2999);
        assert!(resolve_task_input(Some(&todo(&long)), Some("more")).is_err());
    }

    #[test]
    fn task_todo_choices_share_the_whats_next_prefix() {
        let mut entry = todo("fix login");
        assert_eq!(task_todo_label(&entry, Lang::En), "Run todo: fix login");
        entry.auto = true;
        assert_eq!(task_todo_label(&entry, Lang::Zh), "执行待办：⚡ fix login");
        entry
            .attachments
            .push(crate::todo_attachments::TodoAttachment {
                id: "attachment-1".into(),
                name: "brief.md".into(),
                size: 1,
                is_image: false,
                source_path: "/tmp/brief.md".into(),
                path: "/tmp/brief.md".into(),
                storage: crate::todo_attachments::TodoAttachmentStorage::Reference,
                thumbnail_path: None,
                source_modified_ms: None,
            });
        assert_eq!(
            task_todo_label(&entry, Lang::Zh),
            "执行待办：⚡ fix login 【1 个附件】"
        );
    }
}

#[cfg(test)]
mod help_transport_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length: ")
                    .or_else(|| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(bytes[header_end..header_end + content_length].to_vec()).unwrap()
    }

    #[tokio::test]
    async fn telegram_help_retries_plain_text_after_rich_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let server = tokio::spawn(async move {
            for response in [
                r#"{"ok":false,"description":"bad html"}"#,
                r#"{"ok":true,"result":{"message_id":1}}"#,
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                captured.lock().unwrap().push(request);
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                );
                stream.write_all(wire.as_bytes()).await.unwrap();
            }
        });

        let mut config = AppConfig::default();
        config.channels.telegram.bot_token = "test-token".into();
        config.channels.telegram.chat_id = "123".into();
        config.channels.telegram.api_base_url = format!("http://{address}");
        let view = crate::autochannel::help_view(true, false, true, "/", Lang::Zh);
        reply_channel_help("telegram", &config, &view, Lang::Zh)
            .await
            .unwrap();
        server.await.unwrap();

        let bodies = requests.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        let rich: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(rich["parse_mode"], "HTML");
        assert!(rich["text"].as_str().unwrap().contains("<code>"));
        let plain: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
        assert!(plain.get("parse_mode").is_none());
        assert!(plain["text"].as_str().unwrap().contains("• /status [编号]"));
        assert!(!plain["text"].as_str().unwrap().contains("<code>"));
    }
}
