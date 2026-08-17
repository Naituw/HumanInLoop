//! Watch persistence, tick refresh, routing, channel callbacks, and automatic subscriptions.

use super::*;

/// 标记某渠道出现一条「非 watch」消息（用户入站消息 / 机器人文本回执 / 提问会话）。
/// 这是跟底判定的**淹没信号**：发出时刻早于该时刻的 watch 卡已被顶上去，下一次内容变化时
/// 跟底重发（watch 卡自身的发送/编辑不经此处，watch 卡之间互不影响、无级联）。
pub(super) fn mark_watch_disturbed(state: &Arc<ServerState>, channel_id: &str) {
    if !crate::watch::channel_supported(channel_id) {
        return;
    }
    state
        .watch
        .disturb
        .lock()
        .unwrap()
        .insert(channel_id.to_string(), now_ms());
}

/// 把当前订阅持久化到 `~/.askhuman/state/watch.json`（daemon 重启后恢复、继续编辑同卡）。
pub(super) fn persist_watch_subs(state: &Arc<ServerState>) {
    let items: Vec<crate::watch::PersistedWatch> = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .map(|s| crate::watch::PersistedWatch {
            channel: s.channel.clone(),
            session_id: s.session_id.clone(),
            message_id: s.message_id.clone(),
            created_at: s.created_at,
            rewatchable: s.rewatchable,
        })
        .collect();
    crate::watch::save(&items);
}

/// watch 引擎入口：恢复持久化订阅（按 session 重解析展示编号——`seq` 不跨重启保留），
/// 然后进入「Notify 即醒 / 自适应 tick」循环：有「工作中」订阅 2s 一拍、只有处于空闲宽限期的
/// 订阅 10s 一拍、无订阅纯等 Notify（零空转）。
pub(super) async fn watch_restore_and_run(state: Arc<ServerState>) {
    let persisted = crate::watch::load();
    if !persisted.is_empty() {
        let snapshot = state.agents.snapshot();
        let mut subs: Vec<WatchEntry> = Vec::new();
        for p in persisted {
            if !crate::watch::channel_supported(&p.channel) || p.message_id.is_empty() {
                continue;
            }
            // 记录已彻底消失 → seq=0 占位；首拍会渲染终态并自动退订。
            let seq = find_agent_by_session(&snapshot, &p.session_id)
                .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
                .unwrap_or(0);
            subs.push(WatchEntry {
                channel: p.channel,
                session_id: p.session_id,
                message_id: p.message_id,
                seq,
                created_at: p.created_at,
                last_sig: String::new(),
                last_edit_ms: 0,
                fails: 0,
                working: false,
                // 重启后 disturb 从 0 起算，恢复的卡先视为未淹没；一有新扰动即可跟底（不节流）。
                sent_at_ms: p.created_at.saturating_mul(1000),
                last_move_ms: 0,
                rewatchable: p.rewatchable,
            });
        }
        if !subs.is_empty() {
            log(&format!("watch: restored {} subscription(s)", subs.len()));
            *state.watch.subs.lock().unwrap() = subs;
            ensure_watch_routes(&state).await;
        }
    }
    loop {
        // IM 订阅与 GUI 焦点订阅共用同一引擎节奏：任一侧「工作中」→ 2s，只有空闲侧 → 10s，
        // 两侧皆无 → 纯等 Notify。
        let subs_wait = {
            let subs = state.watch.subs.lock().unwrap();
            let has_active = subs.iter().any(|s| !s.rewatchable);
            if !has_active {
                None
            } else if subs.iter().any(|s| !s.rewatchable && s.working) {
                Some(Duration::from_secs(2))
            } else {
                Some(Duration::from_secs(10))
            }
        };
        let focus_wait = {
            let focus = state.gui_focus.lock().unwrap();
            if focus.is_empty() {
                None
            } else if focus.iter().any(|f| f.working) {
                Some(Duration::from_secs(2))
            } else {
                Some(Duration::from_secs(10))
            }
        };
        let wait = match (subs_wait, focus_wait) {
            (None, None) => None,
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (Some(a), Some(b)) => Some(a.min(b)),
        };
        match wait {
            None => state.watch.notify.notified().await,
            Some(d) => {
                tokio::select! {
                    _ = state.watch.notify.notified() => {}
                    _ = tokio::time::sleep(d) => {}
                }
            }
        }
        watch_tick(&state).await;
    }
}

/// 引擎一拍：对每个订阅重算帧，**签名变化才**编辑卡片（帧是全量的，丢帧无损）；
/// agent 结束 → 立即终态定格 + 自动退订；Idle → 保留活动卡，持续满宽限期才定格退订；连续失败
/// ≥5 退订。按渠道分组：每渠道各建一次传输客户端、各取各的淹没水位与在途提问。末尾幂等确保
/// 回调路由在位。
pub(super) async fn watch_tick(state: &Arc<ServerState>) {
    // 「等待回答」集合变化 → 重推 AgentsState（waitingRequestId 徽标即时；提问创建/完结
    // 本就 notify 引擎，此处集中比对，免散布广播调用点）。
    waiting_badge_tick(state);
    // GUI 控制台焦点会话帧（spec gui-agent-console C8）：与 IM 订阅同拍、同签名门控。
    gui_focus_tick(state);

    let all_entries: Vec<WatchEntry> = state.watch.subs.lock().unwrap().clone();
    // 活跃 entry：引擎只驱动非 rewatchable 的订阅（rewatchable 保留仅供回调路由）。
    let entries: Vec<&WatchEntry> = all_entries.iter().filter(|e| !e.rewatchable).collect();
    if entries.is_empty() {
        ensure_watch_routes(state).await;
        return;
    }
    let config = state.config_snapshot();
    // 语言从快照解析（`Lang::current()` 每次读盘，2s 一拍的热路径不值得）。
    let lang = Lang::resolve(&config.general.language);
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let waiting = state.registry.in_flight_agent_session_ids();
    let mut channels: Vec<String> = entries.iter().map(|e| e.channel.clone()).collect();
    channels.sort();
    channels.dedup();
    let mut changed = false;
    for ch in channels {
        // 渠道不可用（配置被关/失效）→ 本拍跳过该渠道订阅，下一拍重试。
        let Some(client) = watch_client(state, &ch, &config).await else {
            continue;
        };
        // 跟底判定的渠道量：淹没水位线 + 是否有在途提问 + 单选卡是否仍在会话底部（二者期间均抑制
        // 跟底，只就地编辑，不打断问答 / 单选交互）。单选卡抑制**仅在它还是最后一条消息时**生效——
        // 被其它消息淹没（含用户忘记选择后又发了别的）即放开跟底（用户定案）。
        let disturb = state
            .watch
            .disturb
            .lock()
            .unwrap()
            .get(&ch)
            .copied()
            .unwrap_or(0);
        let ask_active = has_active_question_on(state, &ch);
        let select_active = select_is_last_on(state, &ch);
        for e in entries.iter().filter(|e| e.channel == ch) {
            let rec = find_agent_by_session(&snapshot, &e.session_id);
            let frame = crate::watch::build_frame(e.seq, rec, waiting.contains(&e.session_id));
            let final_kind = crate::watch::automatic_final_kind(&frame, rec, now);
            let finalize = final_kind.is_some();
            let sig = crate::watch::signature(&frame);
            if !finalize && sig == e.last_sig {
                continue; // 内容没变，不编辑。
            }
            // 每卡最短编辑间隔按渠道（终态豁免：定格必须落地）；漏掉的变化下一拍补上。
            if !finalize && now_ms().saturating_sub(e.last_edit_ms) < client.min_edit_interval_ms()
            {
                continue;
            }
            let mode = final_kind
                .map(crate::watch::CardMode::Final)
                .unwrap_or(crate::watch::CardMode::Active);
            // 跟底：卡已被非 watch 消息淹没 + 无在途提问 + 无在途单选卡 + 30s 节流
            // （`last_move_ms == 0` 豁免：答复 / 单选完结、重启恢复）→ 发新卡到会话底部，
            // 旧卡定格「已移至最新卡片」。
            let buried = disturb > e.sent_at_ms;
            let move_ok = buried
                && !ask_active
                && !select_active
                && (e.last_move_ms == 0
                    || now_ms().saturating_sub(e.last_move_ms) >= WATCH_MOVE_THROTTLE_MS);
            if move_ok {
                match client.send(&frame, mode, now, lang).await {
                    Ok(new_mid) => {
                        // 旧卡定格（best-effort：失败只记日志，新卡已接管）。
                        if let Err(err) = client
                            .edit(
                                &e.message_id,
                                &frame,
                                crate::watch::CardMode::Final(crate::watch::FinalKind::Moved),
                                now,
                                lang,
                                None,
                            )
                            .await
                        {
                            log(&format!("watch: finalize moved card failed: {}", err));
                        }
                        let mut subs = state.watch.subs.lock().unwrap();
                        if finalize {
                            // 新卡即终态卡（ended / Idle grace expired）：定格已随发送完成 → 退订。
                            subs.retain(|s| s.message_id != e.message_id);
                        } else if let Some(s) =
                            subs.iter_mut().find(|s| s.message_id == e.message_id)
                        {
                            s.message_id = new_mid;
                            s.sent_at_ms = now_ms();
                            s.last_move_ms = now_ms();
                            s.last_sig = sig;
                            s.last_edit_ms = now_ms();
                            s.fails = 0;
                            s.working = frame.phase == crate::watch::WatchPhase::Working;
                        }
                        changed = true; // message_id 变了：持久化 + 路由重建。
                    }
                    Err(err) => {
                        // 发送失败与编辑失败同流：计失败数、下一拍重试（帧全量，丢帧无损）。
                        log(&format!("watch: move card failed: {}", err));
                        let mut subs = state.watch.subs.lock().unwrap();
                        let mut drop_it = false;
                        if let Some(s) = subs.iter_mut().find(|s| s.message_id == e.message_id) {
                            s.fails += 1;
                            drop_it = s.fails >= 5;
                        }
                        if drop_it {
                            log("watch: too many consecutive failures; unsubscribed");
                            subs.retain(|s| s.message_id != e.message_id);
                            changed = true;
                        }
                    }
                }
                continue;
            }
            match client
                .edit(&e.message_id, &frame, mode, now, lang, None)
                .await
            {
                Ok(()) => {
                    let mut subs = state.watch.subs.lock().unwrap();
                    if finalize {
                        // 定格成功（ended / Idle grace expired）→ 自动退订。
                        subs.retain(|s| s.message_id != e.message_id);
                        changed = true;
                    } else if let Some(s) = subs.iter_mut().find(|s| s.message_id == e.message_id) {
                        s.last_sig = sig;
                        s.last_edit_ms = now_ms();
                        s.fails = 0;
                        s.working = frame.phase == crate::watch::WatchPhase::Working;
                    }
                }
                Err(err) => {
                    log(&format!("watch: patch card failed: {}", err));
                    let mut subs = state.watch.subs.lock().unwrap();
                    let mut drop_it = false;
                    if let Some(s) = subs.iter_mut().find(|s| s.message_id == e.message_id) {
                        s.fails += 1;
                        drop_it = s.fails >= 5;
                    }
                    if drop_it {
                        log("watch: too many consecutive failures; unsubscribed");
                        subs.retain(|s| s.message_id != e.message_id);
                        changed = true;
                    }
                }
            }
        }
    }
    // rewatchable entry TTL 清理。
    {
        let now = now_secs();
        let mut subs = state.watch.subs.lock().unwrap();
        let before = subs.len();
        subs.retain(|s| !s.rewatchable || now.saturating_sub(s.created_at) < REWATCHABLE_TTL_SECS);
        if subs.len() != before {
            changed = true;
        }
    }
    if changed {
        persist_watch_subs(state);
    }
    ensure_watch_routes(state).await;
}

/// 「等待回答」集合变化检测：与上次广播时比对（排序 session id 列表），变化即重推
/// AgentsState 快照（`waitingRequestId` 注入在 `agents_snapshot_for_gui`）。
fn waiting_badge_tick(state: &Arc<ServerState>) {
    let mut waiting = state.registry.in_flight_agent_session_ids();
    waiting.sort();
    {
        let mut last = state.last_waiting.lock().unwrap();
        if *last == waiting {
            return;
        }
        *last = waiting;
    }
    broadcast_agents_state(state);
}

/// GUI 控制台焦点会话一拍：对每个焦点订阅重算 Watch 帧，签名变化才推 `AgentDetail`
/// （本地 IPC 无需编辑间隔）。发送失败（订阅端已消亡）即移除表项。
fn gui_focus_tick(state: &Arc<ServerState>) {
    let targets: Vec<(usize, String, String)> = {
        let focus = state.gui_focus.lock().unwrap();
        focus
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.session_id.clone(), f.last_sig.clone()))
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let snapshot = state.agents.snapshot();
    let waiting = state.registry.in_flight_agent_session_ids();
    for (idx, session_id, last_sig) in targets {
        let rec = find_agent_by_session(&snapshot, &session_id);
        let seq = rec
            .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        let frame = crate::watch::build_frame(seq, rec, waiting.contains(&session_id));
        let sig = crate::watch::signature(&frame);
        if sig == last_sig {
            continue;
        }
        let detail = frame_detail_json(&session_id, &frame);
        let mut focus = state.gui_focus.lock().unwrap();
        // 表可能已被并发更新（重设焦点 / 断开）：按下标 + session 双重校验后就地更新。
        let Some(f) = focus.get_mut(idx).filter(|f| f.session_id == session_id) else {
            continue;
        };
        if f.tx.send(ServerMsg::AgentDetail { detail }).is_err() {
            focus.remove(idx);
            continue;
        }
        f.last_sig = sig;
        f.working = frame.phase == crate::watch::WatchPhase::Working;
    }
}

/// Watch 帧 → 控制台 detail JSON（tagged，spec gui-agent-console R5）。工具类别以结构化
/// kind 下发（run/read/write/other），本地化由前端 i18n 完成；步/待办状态用与前端约定的
/// 字符串枚举。
fn frame_detail_json(session_id: &str, f: &crate::watch::WatchFrame) -> serde_json::Value {
    use crate::agents::activity::{StepState, TodoState, ToolLabel};
    use crate::watch::WatchPhase;
    let steps: Vec<serde_json::Value> = f
        .steps
        .iter()
        .map(|s| {
            let (kind, name) = match &s.tool.label {
                ToolLabel::Run => ("run", None),
                ToolLabel::Read => ("read", None),
                ToolLabel::Write => ("write", None),
                ToolLabel::Other(n) => ("other", Some(n.clone())),
            };
            serde_json::json!({
                "kind": kind,
                "name": name,
                "object": s.tool.object,
                "state": match s.state {
                    StepState::Running => "running",
                    StepState::Done => "done",
                    StepState::Failed => "failed",
                },
            })
        })
        .collect();
    let todos: Vec<serde_json::Value> = f
        .todos
        .iter()
        .map(|t| {
            serde_json::json!({
                "content": t.content,
                "state": match t.state {
                    TodoState::InProgress => "inProgress",
                    TodoState::Completed => "completed",
                    TodoState::Pending | TodoState::Cancelled => "pending",
                },
            })
        })
        .collect();
    serde_json::json!({
        "type": "watchFrame",
        "sessionId": session_id,
        "seq": f.seq,
        "kindLabel": f.kind_label,
        "phase": match f.phase {
            WatchPhase::Working => "working",
            WatchPhase::Idle => "idle",
            WatchPhase::Waiting => "waiting",
            WatchPhase::Ended => "ended",
        },
        "title": f.title,
        "project": f.project,
        "text": f.text,
        "steps": steps,
        "stepsOmitted": f.steps_omitted,
        "todos": todos,
        "activeElapsedSecs": f.active_elapsed_secs,
        "at": f.at,
    })
}

/// 幂等确保各渠道 watch 卡按钮回调路由在位：在渠道 Router 上注册一条专用路由并认领本渠道
/// 全部卡片 message_id。绑定的 Router 失活 / 订阅集合变化 → 停旧任务整体重建；无订阅则撤路由。
pub(super) async fn ensure_watch_routes(state: &Arc<ServerState>) {
    // 渠道 → 该渠道当前应认领的卡 id 集合（已排序）。
    let mut desired: HashMap<String, Vec<String>> = HashMap::new();
    for s in state.watch.subs.lock().unwrap().iter() {
        desired
            .entry(s.channel.clone())
            .or_default()
            .push(s.message_id.clone());
    }
    for mids in desired.values_mut() {
        mids.sort();
    }
    // 撤掉已无订阅的渠道路由。
    {
        let mut routes = state.watch.routes.lock().unwrap();
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
        ensure_watch_route_for(state, &config, &ch, mids).await;
    }
}

/// 幂等确保单一渠道的 watch 回调路由任务在位。
pub(super) async fn ensure_watch_route_for(
    state: &Arc<ServerState>,
    config: &AppConfig,
    channel_id: &str,
    mids: Vec<String>,
) {
    // 取该渠道的共享 Router（渠道不可用则跳过；订阅仍在，渠道恢复后下一拍补挂）。
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
        _ => return,
    };
    // 现任务仍绑定同一存活 Router 且卡集合未变 → 无事可做。
    {
        let routes = state.watch.routes.lock().unwrap();
        if let Some(h) = routes.get(channel_id) {
            if h.router.is_same_alive(&router) && h.mids == mids {
                return;
            }
        }
    }
    // 重建：注册新路由认领全部卡，替换句柄并停旧任务（其 Routed* Drop 时自清路由表）。
    let stop = Arc::new(tokio::sync::Notify::new());
    let (router_ref, task): (WatchRouterRef, tokio::task::JoinHandle<()>) = match &router {
        WatchChannelRouter::Feishu(r) => {
            let mut routed = r.register();
            for mid in &mids {
                routed.set_active(Some(mid), "");
            }
            let st = state.clone();
            let stop2 = stop.clone();
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::feishu::router::FsInbound::Card { data, ack }) => {
                                handle_watch_card_action(&st, &data, ack);
                            }
                            Some(_) => {} // 未认领聊天消息，不会到达；防御性忽略。
                            None => break, // Router 断开：下一拍 ensure 重建。
                        },
                    }
                }
            });
            (WatchRouterRef::Feishu(Arc::downgrade(r)), task)
        }
        WatchChannelRouter::Telegram(r) => {
            // 仅认领卡片回调（`set_card_route`），**不**认领自由文字——不得抢走提问卡答案。
            let routed = r.register();
            for mid in &mids {
                if let Ok(m) = mid.parse::<i64>() {
                    routed.set_card_route(m);
                }
            }
            let st = state.clone();
            let stop2 = stop.clone();
            let mut routed = routed;
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::telegram::router::TgInbound::Callback(cb)) => {
                                handle_watch_tg_action(&st, &cb).await;
                            }
                            Some(_) => {} // 未认领自由文字，不会到达；防御性忽略。
                            None => break,
                        },
                    }
                }
            });
            (WatchRouterRef::Telegram(Arc::downgrade(r)), task)
        }
        WatchChannelRouter::Slack(r) => {
            // user_id 传空 → 只认领卡片交互（message_ts），不认领聊天消息。
            let mut routed = r.register();
            for mid in &mids {
                routed.set_active(Some(mid), "");
            }
            let st = state.clone();
            let stop2 = stop.clone();
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::slack::router::SlInbound::Interactive(payload)) => {
                                handle_watch_slack_action(&st, &payload).await;
                            }
                            Some(_) => {}
                            None => break,
                        },
                    }
                }
            });
            (WatchRouterRef::Slack(Arc::downgrade(r)), task)
        }
        WatchChannelRouter::DingTalk(r) => {
            // user_id 传空 → 只认领卡片回调（outTrackId），不认领该用户的聊天消息。
            let mut routed = r.register();
            for mid in &mids {
                routed.set_active(Some(mid), "");
            }
            let st = state.clone();
            let stop2 = stop.clone();
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop2.notified() => break,
                        ev = routed.recv() => match ev {
                            Some(crate::dingtalk::router::DdInbound::Card { data, ack }) => {
                                // 先空 ACK 满足 3 秒回包（钉钉无「回调同步回卡」，新帧走 OpenAPI 编辑）。
                                let _ = ack.send(serde_json::json!({}));
                                handle_watch_dd_action(&st, &data).await;
                            }
                            Some(_) => {} // 未认领聊天消息，不会到达；防御性忽略。
                            None => break, // Router 断开：下一拍 ensure 重建。
                        },
                    }
                }
            });
            (WatchRouterRef::DingTalk(Arc::downgrade(r)), task)
        }
    };
    drop(task); // 任务由 stop 信号控制生命周期；句柄本身无需保留。
    if let Some(old) = state.watch.routes.lock().unwrap().insert(
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

/// 处理 watch 卡按钮回调（取消关注 / 立即刷新）：经 oneshot **同步回新卡**——
/// 按钮 Loading 直接变新帧 / 终态，无闪烁（复用提问卡的 callback_update_card 机制）。
pub(super) fn handle_watch_card_action(
    state: &Arc<ServerState>,
    data: &serde_json::Value,
    ack: crate::feishu::router::CardAck,
) {
    use crate::feishu::card::{build_watch_card, callback_update_card, WatchAction};
    let Some((mid, action)) = crate::feishu::card::parse_watch_action(data) else {
        let _ = ack.send(None);
        return;
    };
    let entry = {
        let subs = state.watch.subs.lock().unwrap();
        subs.iter()
            .find(|s| s.channel == "feishu" && s.message_id == mid)
            .cloned()
    };
    let Some(entry) = entry else {
        let _ = ack.send(None); // 已退订的卡（终态按钮本应禁用）：空 ACK。
        return;
    };
    let lang = Lang::current();
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&entry.session_id);
    let rec = find_agent_by_session(&snapshot, &entry.session_id);
    let frame = crate::watch::build_frame(entry.seq, rec, waiting);
    let automatic_final = crate::watch::automatic_final_kind(&frame, rec, now);
    match action {
        WatchAction::Unwatch => {
            let card = build_watch_card(&crate::watch::card_view(
                &frame,
                crate::watch::CardMode::Final(crate::watch::FinalKind::Cancelled),
                now,
                lang,
                Some(&entry.session_id),
            ));
            let _ = ack.send(Some(callback_update_card(card)));
            {
                let mut subs = state.watch.subs.lock().unwrap();
                if let Some(s) = subs.iter_mut().find(|s| s.message_id == mid) {
                    s.rewatchable = true;
                }
            }
            persist_watch_subs(state);
            state.watch.notify.notify_one();
        }
        WatchAction::Refresh => {
            let finalize = automatic_final.is_some();
            let mode = automatic_final
                .map(crate::watch::CardMode::Final)
                .unwrap_or(crate::watch::CardMode::Active);
            let card = build_watch_card(&crate::watch::card_view(&frame, mode, now, lang, None));
            let _ = ack.send(Some(callback_update_card(card)));
            {
                let mut subs = state.watch.subs.lock().unwrap();
                if finalize {
                    subs.retain(|s| s.message_id != mid);
                } else if let Some(s) = subs.iter_mut().find(|s| s.message_id == mid) {
                    s.last_sig = crate::watch::signature(&frame);
                    s.last_edit_ms = now_ms();
                    s.fails = 0;
                    s.working = frame.phase == crate::watch::WatchPhase::Working;
                }
            }
            if finalize {
                persist_watch_subs(state);
            }
            state.watch.notify.notify_one();
        }
        WatchAction::Rewatch(session_id) => {
            // 旧卡立即 ACK 为「已重新关注」禁用态。
            let card = build_watch_card(&crate::watch::card_view(
                &frame,
                crate::watch::CardMode::Final(crate::watch::FinalKind::Rewatched),
                now,
                lang,
                None,
            ));
            let _ = ack.send(Some(callback_update_card(card)));
            // 移除旧的 rewatchable entry。
            state
                .watch
                .subs
                .lock()
                .unwrap()
                .retain(|s| s.message_id != mid);
            persist_watch_subs(state);
            // 异步发新 watch 卡 + 激活渠道（复用 handle_watch_cmd 路径）。
            let state = Arc::clone(state);
            let sid = session_id;
            tokio::spawn(async move {
                let config = state.config_snapshot();
                let lang = Lang::current();
                activate_channel_on_action(&state, "feishu", &config, lang).await;
                let snapshot = state.agents.snapshot();
                let rec = find_agent_by_session(&snapshot, &sid);
                let seq = rec
                    .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
                    .unwrap_or(0);
                if seq == 0 {
                    log("watch: rewatch target session not found, skipping");
                    return;
                }
                handle_watch_cmd(&state, "feishu", Some(seq), &config, lang).await;
            });
        }
    }
}

/// 非飞书渠道的 rewatch 统一处理：编辑旧卡为 Rewatched 终态，移除旧 entry，激活渠道，异步发新卡。
pub(super) async fn handle_rewatch(state: &Arc<ServerState>, channel_id: &str, mid: &str) {
    let entry = {
        let subs = state.watch.subs.lock().unwrap();
        subs.iter()
            .find(|s| s.channel == channel_id && s.message_id == mid && s.rewatchable)
            .cloned()
    };
    let Some(entry) = entry else {
        return;
    };
    let config = state.config_snapshot();
    let Some(client) = watch_client(state, channel_id, &config).await else {
        return;
    };
    let lang = Lang::current();
    activate_channel_on_action(state, channel_id, &config, lang).await;
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let rec = find_agent_by_session(&snapshot, &entry.session_id);
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&entry.session_id);
    let frame = crate::watch::build_frame(entry.seq, rec, waiting);
    if let Err(err) = client
        .edit(
            mid,
            &frame,
            crate::watch::CardMode::Final(crate::watch::FinalKind::Rewatched),
            now,
            lang,
            None,
        )
        .await
    {
        log(&format!("watch: rewatch ack card failed: {}", err));
    }
    {
        let mut subs = state.watch.subs.lock().unwrap();
        subs.retain(|s| s.message_id != mid);
    }
    persist_watch_subs(state);
    let state = Arc::clone(state);
    let sid = entry.session_id;
    let ch = channel_id.to_string();
    tokio::spawn(async move {
        let config = state.config_snapshot();
        let lang = Lang::current();
        let snapshot = state.agents.snapshot();
        let rec = find_agent_by_session(&snapshot, &sid);
        let seq = rec
            .and_then(|r| r.get("seq").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        if seq == 0 {
            log("watch: rewatch target session not found, skipping");
            return;
        }
        handle_watch_cmd(&state, &ch, Some(seq), &config, lang).await;
    });
}

/// 非飞书渠道的 watch 按钮统一处理：计算新帧并**就地编辑**卡片（这些渠道无「回调同步回卡」
/// 机制，编辑即生效；飞书走 `handle_watch_card_action` 的 oneshot 回卡）。
pub(super) async fn apply_watch_action(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    btn: WatchBtn,
) {
    let entry = {
        let subs = state.watch.subs.lock().unwrap();
        subs.iter()
            .find(|s| s.channel == channel_id && s.message_id == mid)
            .cloned()
    };
    let Some(entry) = entry else {
        return; // 已退订的卡（终态卡无按钮，孤儿回调已在 Router 层应答）。
    };
    let config = state.config_snapshot();
    let Some(client) = watch_client(state, channel_id, &config).await else {
        return;
    };
    let lang = Lang::current();
    let now = now_secs();
    let snapshot = state.agents.snapshot();
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&entry.session_id);
    let rec = find_agent_by_session(&snapshot, &entry.session_id);
    let frame = crate::watch::build_frame(entry.seq, rec, waiting);
    let automatic_final = crate::watch::automatic_final_kind(&frame, rec, now);
    match btn {
        WatchBtn::Unwatch => {
            if let Err(err) = client
                .edit(
                    mid,
                    &frame,
                    crate::watch::CardMode::Final(crate::watch::FinalKind::Cancelled),
                    now,
                    lang,
                    Some(&entry.session_id),
                )
                .await
            {
                log(&format!("watch: finalize cancelled card failed: {}", err));
            }
            {
                let mut subs = state.watch.subs.lock().unwrap();
                if let Some(s) = subs.iter_mut().find(|s| s.message_id == mid) {
                    s.rewatchable = true;
                }
            }
            persist_watch_subs(state);
            state.watch.notify.notify_one();
        }
        WatchBtn::Refresh => {
            let finalize = automatic_final.is_some();
            let mode = automatic_final
                .map(crate::watch::CardMode::Final)
                .unwrap_or(crate::watch::CardMode::Active);
            if let Err(err) = client.edit(mid, &frame, mode, now, lang, None).await {
                log(&format!("watch: refresh card failed: {}", err));
                return;
            }
            {
                let mut subs = state.watch.subs.lock().unwrap();
                if finalize {
                    subs.retain(|s| s.message_id != mid);
                } else if let Some(s) = subs.iter_mut().find(|s| s.message_id == mid) {
                    s.last_sig = crate::watch::signature(&frame);
                    s.last_edit_ms = now_ms();
                    s.fails = 0;
                    s.working = frame.phase == crate::watch::WatchPhase::Working;
                }
            }
            if finalize {
                persist_watch_subs(state);
            }
            state.watch.notify.notify_one();
        }
    }
}

/// 处理 Telegram watch 卡按钮回调：先应答（消除客户端转圈），再就地编辑。
pub(super) async fn handle_watch_tg_action(state: &Arc<ServerState>, cb: &serde_json::Value) {
    let data = cb.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let Some(mid) = cb
        .get("message")
        .and_then(|m| m.get("message_id"))
        .and_then(|v| v.as_i64())
    else {
        return;
    };
    // 应答 callback（best-effort）。
    if let Some(id) = cb.get("id").and_then(|i| i.as_str()) {
        let tg = &state.config_snapshot().channels.telegram;
        if let Ok(c) = crate::telegram::TelegramClient::new(
            tg.bot_token.clone(),
            tg.chat_id.clone(),
            tg.api_base_url.clone(),
        ) {
            c.answer_callback_query(id).await;
        }
    }
    if data == crate::telegram::watch::CB_REWATCH {
        handle_rewatch(state, "telegram", &mid.to_string()).await;
        return;
    }
    let btn = match data {
        crate::telegram::watch::CB_UNWATCH => WatchBtn::Unwatch,
        crate::telegram::watch::CB_REFRESH => WatchBtn::Refresh,
        _ => return,
    };
    apply_watch_action(state, "telegram", &mid.to_string(), btn).await;
}

/// 处理 Slack watch 卡按钮回调（ack 已在 ws 层完成，这里只做编辑）。
pub(super) async fn handle_watch_slack_action(
    state: &Arc<ServerState>,
    payload: &serde_json::Value,
) {
    let Some((ts, action_id)) = crate::slack::watch::parse_watch_action(payload) else {
        return;
    };
    if action_id == crate::slack::watch::ACTION_REWATCH {
        handle_rewatch(state, "slack", &ts).await;
        return;
    }
    let btn = match action_id.as_str() {
        crate::slack::watch::ACTION_UNWATCH => WatchBtn::Unwatch,
        _ => WatchBtn::Refresh,
    };
    apply_watch_action(state, "slack", &ts, btn).await;
}

/// 处理钉钉 watch 卡按钮回调（空 ACK 已在路由任务发出，这里只做编辑）。
pub(super) async fn handle_watch_dd_action(state: &Arc<ServerState>, data: &serde_json::Value) {
    let Some((otid, action_id)) = crate::dingtalk::watch::parse_watch_action(data) else {
        return;
    };
    if action_id == crate::dingtalk::watch::ACTION_REWATCH {
        handle_rewatch(state, "dingding", &otid).await;
        return;
    }
    let btn = match action_id.as_str() {
        crate::dingtalk::watch::ACTION_UNWATCH => WatchBtn::Unwatch,
        _ => WatchBtn::Refresh,
    };
    apply_watch_action(state, "dingding", &otid, btn).await;
}

/// watch 列表一行：`[编号] 类型 — 标题（项目）· 状态`。记录已消失按已结束显示。
pub(super) fn watch_line(snapshot: &serde_json::Value, e: &WatchEntry, lang: Lang) -> String {
    let rec = find_agent_by_session(snapshot, &e.session_id);
    let head = rec
        .map(|r| crate::autochannel::kind_title_project(r, lang))
        .unwrap_or_else(|| crate::i18n::tr(lang, "autoChannel.noTitle").to_string());
    let state_key = match rec.and_then(|r| r.get("state")).and_then(|v| v.as_str()) {
        Some("working") => "autoChannel.stateWorking",
        Some("idle") => "autoChannel.stateIdle",
        _ => "autoChannel.stateEnded",
    };
    format!(
        "[{}] {} · {}",
        e.seq,
        head,
        crate::i18n::tr(lang, state_key)
    )
}

/// `/watch` 命令：`Some(编号)` 关注该 agent（发实时状态卡，成功回执就是卡片本身）；
/// `None` 列出当前关注。渠道门控见 `watch::channel_supported`（四渠道全支持）。
pub(super) async fn handle_watch_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    config: &AppConfig,
    lang: Lang,
) {
    if !crate::watch::channel_supported(channel_id) {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "watch.unsupported"),
        )
        .await;
        return;
    }
    let Some(id) = sel else {
        // `/watch` 无参文本回退：首行提示 + agent 列表 + 已关注段。仅工作中 agent 时才显示
        // 关注提示（空闲 agent 关注没有意义）。列表仍含全部 working + idle 便于了解全貌。
        let snapshot = state.agents.snapshot();
        let has_working = snapshot
            .as_array()
            .map(|l| {
                l.iter()
                    .any(|r| r.get("state").and_then(|v| v.as_str()) == Some("working"))
            })
            .unwrap_or(false);
        let mut out = String::new();
        if has_working {
            out.push_str(
                &crate::i18n::tr(lang, "watch.pickHintWorkingOnly")
                    .replace("{p}", crate::autochannel::cmd_prefix(channel_id)),
            );
            out.push_str("\n\n");
        }
        out.push_str(&crate::autochannel::status_text(&snapshot, lang));
        let entries: Vec<WatchEntry> = state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.channel == channel_id && !e.rewatchable)
            .cloned()
            .collect();
        if !entries.is_empty() {
            out.push_str("\n\n");
            out.push_str(crate::i18n::tr(lang, "watch.listTitle"));
            for e in &entries {
                out.push('\n');
                out.push_str(&watch_line(&snapshot, e, lang));
            }
        }
        let _ = reply_channel_text(channel_id, config, &out).await;
        return;
    };
    let snapshot = state.agents.snapshot();
    let Some(rec) = snapshot.as_array().and_then(|l| {
        l.iter()
            .find(|r| r.get("seq").and_then(|v| v.as_u64()) == Some(id))
    }) else {
        let text = crate::i18n::tr(lang, "autoChannel.statusDetailNotFound")
            .replace("{id}", &id.to_string())
            .replace("{p}", crate::autochannel::cmd_prefix(channel_id));
        let _ = reply_channel_text(channel_id, config, &text).await;
        return;
    };
    let session_id = rec
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // 重复 watch 同一 agent＝换新卡：旧卡稍后定格「已由新卡片接替」（把卡拉到会话底部）。
    // 仅限本渠道：同一 agent 在不同渠道的关注互相独立。
    let replaced = {
        let subs = state.watch.subs.lock().unwrap();
        subs.iter()
            .find(|s| s.channel == channel_id && s.session_id == session_id)
            .cloned()
    };
    // 关注上限（每渠道各算；换新卡不算新增）。
    if replaced.is_none()
        && state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.channel == channel_id && !s.rewatchable)
            .count()
            >= crate::watch::MAX_WATCHES
    {
        let text = crate::i18n::tr(lang, "watch.limit")
            .replace("{n}", &crate::watch::MAX_WATCHES.to_string())
            .replace("{p}", crate::autochannel::cmd_prefix(channel_id));
        let _ = reply_channel_text(channel_id, config, &text).await;
        return;
    }
    let Some(client) = watch_client(state, channel_id, config).await else {
        return;
    };
    let waiting = state
        .registry
        .in_flight_agent_session_ids()
        .contains(&session_id);
    let now = now_secs();
    let frame = crate::watch::build_frame(id, Some(rec), waiting);
    // 已结束 / 空闲的 agent：直接发一张定格终态卡（回顾当前状态，不订阅后续更新）。
    // Waiting（有在途 AskHuman 提问）不算空闲。
    let ended = frame.phase == crate::watch::WatchPhase::Ended;
    let idle = frame.phase == crate::watch::WatchPhase::Idle;
    let one_shot = ended || idle;
    let mode = if ended {
        crate::watch::CardMode::Final(crate::watch::FinalKind::Ended)
    } else if idle {
        crate::watch::CardMode::Final(crate::watch::FinalKind::Idle)
    } else {
        crate::watch::CardMode::Active
    };
    let message_id = match client.send(&frame, mode, now, lang).await {
        Ok(mid) => mid,
        Err(e) => {
            let text = crate::i18n::tr(lang, "watch.sendFailed").replace("{e}", &e);
            let _ = reply_channel_text(channel_id, config, &text).await;
            return;
        }
    };
    // 新卡已发成功 → 换新卡收尾（旧卡定格 Replaced + 退订）+ 登记新订阅（`register_watch_at`
    // 与「单选卡点选就地变卡」共用同一套 bookkeeping；`replaced` 仅供上面的上限判定，收尾在
    // helper 内按 session 重算）。
    register_watch_at(
        state,
        channel_id,
        &session_id,
        id,
        &message_id,
        &frame,
        one_shot,
        config,
        lang,
    )
    .await;
}

/// 登记一条新的 watch 订阅到 `message_id`（命令发新卡 / 单选卡点选就地变卡 两条路径共用）：
/// 本渠道已在关注**同一 session**（且是别的消息）→ 旧卡定格 `Replaced` 并退订（换新卡语义）；
/// 然后（非 ended 时）push 新 `WatchEntry`；持久化 + 唤醒引擎。调用方已完成「发卡 / 回卡」拿到
/// `message_id`、并已做上限校验。
#[allow(clippy::too_many_arguments)]
pub(super) async fn register_watch_at(
    state: &Arc<ServerState>,
    channel_id: &str,
    session_id: &str,
    seq: u64,
    message_id: &str,
    frame: &crate::watch::WatchFrame,
    ended: bool,
    config: &AppConfig,
    lang: Lang,
) {
    let now = now_secs();
    // 换新卡：本渠道同 session 的旧订阅（message_id 不同）定格 Replaced 并退订。
    let replaced: Option<WatchEntry> = {
        let subs = state.watch.subs.lock().unwrap();
        subs.iter()
            .find(|s| {
                s.channel == channel_id && s.session_id == session_id && s.message_id != message_id
            })
            .cloned()
    };
    if let Some(old) = replaced {
        if let Some(client) = watch_client(state, channel_id, config).await {
            let snapshot = state.agents.snapshot();
            let waiting = state
                .registry
                .in_flight_agent_session_ids()
                .contains(&old.session_id);
            let old_frame = crate::watch::build_frame(
                old.seq,
                find_agent_by_session(&snapshot, &old.session_id),
                waiting,
            );
            if let Err(err) = client
                .edit(
                    &old.message_id,
                    &old_frame,
                    crate::watch::CardMode::Final(crate::watch::FinalKind::Replaced),
                    now,
                    lang,
                    None,
                )
                .await
            {
                log(&format!("watch: finalize replaced card failed: {}", err));
            }
        }
        state
            .watch
            .subs
            .lock()
            .unwrap()
            .retain(|s| s.message_id != old.message_id);
    }
    if !ended {
        state.watch.subs.lock().unwrap().push(WatchEntry {
            channel: channel_id.to_string(),
            session_id: session_id.to_string(),
            message_id: message_id.to_string(),
            seq,
            created_at: now,
            last_sig: crate::watch::signature(frame),
            last_edit_ms: now_ms(),
            fails: 0,
            working: frame.phase == crate::watch::WatchPhase::Working,
            sent_at_ms: now_ms(),
            // 从创建起算 30s 节流（新卡本就在底部，避免刚发就跟底重发）。
            last_move_ms: now_ms(),
            rewatchable: false,
        });
    }
    persist_watch_subs(state);
    // 引擎即醒：重算 tick 间隔 + 挂卡片回调路由（按钮立即可用）。
    state.watch.notify.notify_one();
}

/// `/unwatch` 命令：取消关注（编号 / 全部 / 缺省自动），旧卡定格「已取消关注」+ 回确认文本。
pub(super) async fn handle_unwatch_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: crate::autochannel::WatchSel,
    config: &AppConfig,
    lang: Lang,
) {
    use crate::autochannel::WatchSel;
    if !crate::watch::channel_supported(channel_id) {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "watch.unsupported"),
        )
        .await;
        return;
    }
    // 只操作本渠道的活跃订阅（rewatchable 已是终态，不参与 unwatch）。
    let entries: Vec<WatchEntry> = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.channel == channel_id && !e.rewatchable)
        .cloned()
        .collect();
    let targets: Vec<WatchEntry> = match sel {
        WatchSel::One(id) => {
            let found: Vec<WatchEntry> = entries.iter().filter(|e| e.seq == id).cloned().collect();
            if found.is_empty() {
                let text = crate::i18n::tr(lang, "watch.notWatching")
                    .replace("{id}", &id.to_string())
                    .replace("{p}", crate::autochannel::cmd_prefix(channel_id));
                let _ = reply_channel_text(channel_id, config, &text).await;
                return;
            }
            found
        }
        WatchSel::All => {
            if entries.is_empty() {
                let _ = reply_channel_text(
                    channel_id,
                    config,
                    crate::i18n::tr(lang, "watch.unwatchNone"),
                )
                .await;
                return;
            }
            entries.clone()
        }
        WatchSel::Auto => match entries.len() {
            0 => {
                let _ = reply_channel_text(
                    channel_id,
                    config,
                    crate::i18n::tr(lang, "watch.unwatchNone"),
                )
                .await;
                return;
            }
            1 => entries.clone(),
            // 多个：回列表让用户指定编号。
            _ => {
                let snapshot = state.agents.snapshot();
                let mut out = crate::i18n::tr(lang, "watch.unwatchWhich")
                    .replace("{p}", crate::autochannel::cmd_prefix(channel_id));
                for e in &entries {
                    out.push('\n');
                    out.push_str(&watch_line(&snapshot, e, lang));
                }
                let _ = reply_channel_text(channel_id, config, &out).await;
                return;
            }
        },
    };
    // 旧卡定格 Cancelled + 移除订阅（复用共享收尾）→ 回确认。渠道不可用则整段跳过（订阅保留，稍后重试）。
    let dropped = finalize_and_drop_watches(
        state,
        channel_id,
        &targets,
        crate::watch::FinalKind::Cancelled,
        config,
        lang,
    )
    .await;
    if dropped == 0 {
        return; // 渠道客户端不可用：与旧行为一致（不退订、不回执）。
    }
    let text = if targets.len() == 1 {
        crate::i18n::tr(lang, "watch.unwatchDone").replace("{id}", &targets[0].seq.to_string())
    } else {
        crate::i18n::tr(lang, "watch.unwatchAllDone").replace("{n}", &targets.len().to_string())
    };
    let _ = reply_channel_text(channel_id, config, &text).await;
}

/// 对某渠道的一批 watch 订阅统一收尾：逐个把卡片定格为 `final_kind`。
/// `AutoStopped` 的 entry 标记 `rewatchable`（保留路由供重新关注）而非移除；其余终态移除。
/// 渠道客户端不可用则**整段跳过**（订阅保留、返回 0）。
pub(super) async fn finalize_and_drop_watches(
    state: &Arc<ServerState>,
    channel_id: &str,
    targets: &[WatchEntry],
    final_kind: crate::watch::FinalKind,
    config: &AppConfig,
    lang: Lang,
) -> usize {
    if targets.is_empty() {
        return 0;
    }
    let Some(client) = watch_client(state, channel_id, config).await else {
        return 0;
    };
    let keep_rewatchable = final_kind.is_rewatchable();
    let snapshot = state.agents.snapshot();
    let waiting = state.registry.in_flight_agent_session_ids();
    let now = now_secs();
    for e in targets {
        let rec = find_agent_by_session(&snapshot, &e.session_id);
        let frame = crate::watch::build_frame(e.seq, rec, waiting.contains(&e.session_id));
        let sid = if keep_rewatchable {
            Some(e.session_id.as_str())
        } else {
            None
        };
        if let Err(err) = client
            .edit(
                &e.message_id,
                &frame,
                crate::watch::CardMode::Final(final_kind.clone()),
                now,
                lang,
                sid,
            )
            .await
        {
            log(&format!(
                "watch: finalize card failed ({}): {}",
                channel_id, err
            ));
        }
    }
    {
        let mut subs = state.watch.subs.lock().unwrap();
        if keep_rewatchable {
            for s in subs.iter_mut() {
                if targets.iter().any(|t| t.message_id == s.message_id) {
                    s.rewatchable = true;
                }
            }
        } else {
            subs.retain(|s| !targets.iter().any(|t| t.message_id == s.message_id));
        }
    }
    persist_watch_subs(state);
    state.watch.notify.notify_one();
    targets.len()
}

/// 本渠道已在关注的 session_id 集合（`/watch` 单选卡「· 关注中」徽标用）。
pub(super) fn watching_sessions(
    state: &Arc<ServerState>,
    channel_id: &str,
) -> std::collections::HashSet<String> {
    state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.channel == channel_id)
        .map(|s| s.session_id.clone())
        .collect()
}

/// 本渠道各 watch 订阅 → 单选卡选项（`/unwatch` 单选卡）。按 session 在快照定位记录组装
/// （圆点/类型·工作目录名/标题）；记录已消失时按 `seq` 兜底降级（见 `agent_option_by_session`）。
pub(super) fn unwatch_options(
    state: &Arc<ServerState>,
    channel_id: &str,
    snapshot: &serde_json::Value,
    lang: Lang,
) -> Vec<crate::select::SelectOption> {
    let now = now_secs();
    state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.channel == channel_id)
        .map(|s| crate::select::agent_option_by_session(snapshot, &s.session_id, s.seq, now, lang))
        .collect()
}

pub(super) fn register_pending_launch_watch(
    state: &Arc<ServerState>,
    record: &crate::integrations::agent_launch::LaunchRecord,
    channel_id: &str,
    config: &AppConfig,
    lang: Lang,
) {
    state
        .pending_launches
        .lock()
        .unwrap()
        .push(PendingLaunchWatch {
            id: record.id.clone(),
            channel: Some(channel_id.to_string()),
            kind: record.kind,
            cwd: record.cwd.clone(),
            task_sha256: record.task_sha256.clone(),
            source_session_id: match &record.launch_mode {
                crate::integrations::agent_launch::LaunchMode::New => None,
                crate::integrations::agent_launch::LaunchMode::Fork { source_session_id } => {
                    Some(source_session_id.clone())
                }
            },
            created_at: now_secs(),
        });
    let state = state.clone();
    let config = config.clone();
    let id = record.id.clone();
    let channel = channel_id.to_string();
    let is_fork = matches!(
        record.launch_mode,
        crate::integrations::agent_launch::LaunchMode::Fork { .. }
    );
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let expired = {
            let mut pending = state.pending_launches.lock().unwrap();
            let found = pending.iter().any(|item| item.id == id);
            pending.retain(|item| item.id != id);
            found
        };
        if expired {
            let text = match (lang, is_fork) {
                (Lang::Zh, true) => "Agent 已启动，但 60 秒内未匹配到分叉会话，无法记录父子关系或自动关注；任务不会被终止。",
                (Lang::En, true) => "The Agent was started, but no forked session was matched within 60 seconds, so lineage and automatic watch could not be recorded. The task was not stopped.",
                (Lang::Zh, false) => "Agent 已启动，但 60 秒内未检测到可关注的会话；任务不会被终止。",
                (Lang::En, false) => "The Agent was started, but no watchable session was detected within 60 seconds. The task was not stopped.",
            };
            let _ = reply_channel_text(&channel, &config, text).await;
        }
    });
}

pub(super) async fn match_pending_launch_watch(
    state: &Arc<ServerState>,
    kind: AgentKind,
    session_id: &str,
    cwd: Option<&str>,
    launch_id: Option<&str>,
    prompt_sha256: Option<&str>,
) {
    let matched = {
        let mut pending = state.pending_launches.lock().unwrap();
        let now = now_secs();
        pending.retain(|item| now.saturating_sub(item.created_at) <= 60);
        let position = pending
            .iter()
            .position(|item| pending_launch_matches(item, kind, cwd, launch_id, prompt_sha256));
        position.map(|index| pending.remove(index))
    };
    let Some(matched) = matched else { return };
    if let Some(parent) = matched.source_session_id.as_deref() {
        if state.agents.set_fork_parent(session_id, parent) {
            state.agents.persist();
            broadcast_agents_state(state);
        }
    }
    let snapshot = state.agents.snapshot();
    let seq = snapshot
        .as_array()
        .and_then(|items| {
            items.iter().find(|item| {
                item.get("sessionId").and_then(|value| value.as_str()) == Some(session_id)
            })
        })
        .and_then(|item| item.get("seq").and_then(|value| value.as_u64()));
    let Some(seq) = seq else { return };
    let Some(channel) = matched.channel.as_deref() else {
        return;
    };
    let config = state.config_snapshot();
    if let Some(parent) = matched.source_session_id.as_deref() {
        let parent_seq = snapshot
            .as_array()
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("sessionId").and_then(|value| value.as_str()) == Some(parent)
                })
            })
            .and_then(|item| item.get("seq").and_then(|value| value.as_u64()));
        let text = match parent_seq {
            Some(source_seq) => match Lang::current() {
                Lang::Zh => format!("已分叉 #{source_seq} → #{seq}"),
                Lang::En => format!("Forked #{source_seq} → #{seq}"),
            },
            None => match Lang::current() {
                Lang::Zh => format!("已分叉 {} → #{seq}", short_session_id(parent)),
                Lang::En => format!("Forked {} → #{seq}", short_session_id(parent)),
            },
        };
        let _ = reply_channel_text(channel, &config, &text).await;
    }
    handle_watch_cmd(state, channel, Some(seq), &config, Lang::current()).await;
}

fn short_session_id(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

#[cfg(test)]
mod gui_focus_tests {
    use super::frame_detail_json;

    /// detail JSON（spec gui-agent-console C8/R5）：tagged 结构、结构化工具类别、状态枚举串。
    #[test]
    fn frame_detail_json_is_tagged_and_structured() {
        let rec = serde_json::json!({
            "seq": 7,
            "kind": "cursor",
            "sessionId": "s-detail-test",
            "state": "working",
            "title": "重构空闲退出",
            "cwd": "/tmp/HumanInLoop",
            "activeElapsedSecs": 360,
        });
        let frame = crate::watch::build_frame(7, Some(&rec), false);
        let v = frame_detail_json("s-detail-test", &frame);
        assert_eq!(v["type"], "watchFrame");
        assert_eq!(v["sessionId"], "s-detail-test");
        assert_eq!(v["seq"], 7);
        assert_eq!(v["phase"], "working");
        assert_eq!(v["kindLabel"], "Cursor");
        assert_eq!(v["title"], "重构空闲退出");
        assert_eq!(v["project"], "HumanInLoop");
        assert_eq!(v["activeElapsedSecs"], 360);
        assert!(v["steps"].is_array());
        assert!(v["todos"].is_array());
        // waiting 覆盖 working；ended 定格。
        let waiting = crate::watch::build_frame(7, Some(&rec), true);
        assert_eq!(frame_detail_json("s", &waiting)["phase"], "waiting");
        let gone = crate::watch::build_frame(7, None, false);
        assert_eq!(frame_detail_json("s", &gone)["phase"], "ended");
    }
}
