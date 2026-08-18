//! GUI, tray, and Agent subscriptions plus Interject composer and hold connections.

use super::*;

/// 是否有状态窗口在订阅。
pub(super) fn has_agent_subs(state: &Arc<ServerState>) -> bool {
    state
        .agent_subs
        .lock()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// 构造给状态窗口的 agent 全量快照：注册表 snapshot + 注入插话「待送达」徽标
/// （`pendingInterject: true`，spec agent-interject D7）与在途提问的 `waitingRequestId`
/// （spec gui-agent-console C7/R2）；IM /status 等其它 snapshot 消费方不注入。
pub(super) fn agents_snapshot_for_gui(state: &Arc<ServerState>) -> serde_json::Value {
    let mut snap = state.agents.snapshot();
    let pending = state.interject.pending_sessions();
    let waiting = state.registry.in_flight_agent_requests();
    if !pending.is_empty() || !waiting.is_empty() {
        if let Some(arr) = snap.as_array_mut() {
            for rec in arr.iter_mut() {
                let sid = rec
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if sid.is_empty() {
                    continue;
                }
                if pending.iter().any(|p| p == &sid) {
                    if let Some(obj) = rec.as_object_mut() {
                        obj.insert("pendingInterject".to_string(), serde_json::json!(true));
                    }
                }
                if let Some((_, rid, preview)) = waiting.iter().find(|(s, _, _)| s == &sid) {
                    if let Some(obj) = rec.as_object_mut() {
                        obj.insert("waitingRequestId".to_string(), serde_json::json!(rid));
                        if !preview.is_empty() {
                            obj.insert("waitingPreview".to_string(), serde_json::json!(preview));
                        }
                    }
                }
            }
        }
    }
    snap
}

/// 向所有状态窗口推送一次 agent 全量快照（顺带剔除已断开的发送端）。
/// agent 忙闲变化也影响菜单栏状态，故顺带刷新 TrayState。
pub(super) fn broadcast_agents_state(state: &Arc<ServerState>) {
    let msg = ServerMsg::AgentsState {
        agents: agents_snapshot_for_gui(state),
    };
    if let Ok(mut subs) = state.agent_subs.lock() {
        subs.retain(|tx| tx.send(msg.clone()).is_ok());
    }
    broadcast_tray_state(state);
}

/// 状态窗口订阅连接：注册发送端、立即推一次快照，随后专用写任务持续推送；读端接收
/// `AgentsFocus`（控制台焦点会话，spec gui-agent-console C8）并探测断开。
/// 该连接保持期间计入 `active`（连同「工作中」agent 一起阻止 daemon 闲退，spec D18）。
pub(super) async fn handle_agents_sub(
    mut reader: Reader,
    w: OwnedWriteHalf,
    state: &Arc<ServerState>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerMsg>();
    let mut w = w;
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ipc::write_msg(&mut w, &msg).await.is_err() {
                break;
            }
        }
    });
    // 注册订阅端并立即推一次当前快照。
    if let Ok(mut subs) = state.agent_subs.lock() {
        subs.push(tx.clone());
    }
    let _ = tx.send(ServerMsg::AgentsState {
        agents: agents_snapshot_for_gui(state),
    });

    // 读循环：焦点会话更新 + 断开探测（其它消息忽略）。
    loop {
        match ipc::read_msg::<_, ClientMsg>(&mut reader).await {
            Ok(Some(ClientMsg::AgentsFocus { session_id })) => {
                set_gui_focus(state, &tx, session_id);
                // 引擎即醒：焦点帧首拍立即推（last_sig 为空必然与新帧不同）。
                state.watch.notify.notify_one();
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break, // 关窗 / 宿主崩溃：连接断开。
        }
    }

    // 收尾：从订阅表与焦点表移除本端（按指针标识），结束写任务。
    if let Ok(mut subs) = state.agent_subs.lock() {
        subs.retain(|s| !s.same_channel(&tx));
    }
    if let Ok(mut focus) = state.gui_focus.lock() {
        focus.retain(|f| !f.tx.same_channel(&tx));
    }
    drop(tx);
    let _ = writer.await;
}

/// 更新某订阅者的焦点会话（None＝取消）。同一订阅者重设焦点就地替换并重置签名
/// （切换会话后首帧必推）。
fn set_gui_focus(
    state: &Arc<ServerState>,
    tx: &tokio::sync::mpsc::UnboundedSender<ServerMsg>,
    session_id: Option<String>,
) {
    let Ok(mut focus) = state.gui_focus.lock() else {
        return;
    };
    focus.retain(|f| !f.tx.same_channel(tx));
    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        focus.push(GuiFocusEntry {
            tx: tx.clone(),
            session_id: sid,
            last_sig: String::new(),
            working: true, // 首拍前保守按工作中取 2s 节奏，首帧后校正。
        });
    }
}

/// 是否有菜单栏宿主在订阅 TrayState。
pub(super) fn has_tray_subs(state: &Arc<ServerState>) -> bool {
    state
        .tray_subs
        .lock()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// 构造一帧整合的 `TrayState`（含 IM 连接、agent 忙闲、更新态）。需 await（读 tokio Mutex）。
pub(super) async fn build_tray_state(state: &Arc<ServerState>) -> ServerMsg {
    let u = { state.update.lock().unwrap().clone() };
    // Agent 子菜单摘要（spec agent-interject D7）：注册表活动会话 + 插话「待送达」标记。
    let mut agents = state.agents.tray_agent_infos();
    let pending_ij = state.interject.pending_sessions();
    for a in agents.iter_mut() {
        if pending_ij.iter().any(|p| p == &a.session_id) {
            a.pending_interject = true;
        }
    }
    ServerMsg::TrayState {
        running: true,
        version: version(),
        uptime_secs: now_secs().saturating_sub(state.started_at),
        active_requests: state.registry.active_count(),
        im_connections: active_im_connections(state).await,
        draining: state.draining.load(Ordering::SeqCst),
        agents_working: state.agents.working_count(),
        agents_idle: state.agents.idle_count(),
        update_available: u.available,
        update_latest: u.latest_version,
        pending: u.pending,
        pending_requests: state.registry.pending_infos(),
        agents,
        channel_issues: channel_issue_infos(),
    }
}

/// 渠道健康表快照 → IPC 摘要（R7）。
pub(super) fn channel_issue_infos() -> Vec<ipc::ChannelIssueInfo> {
    crate::channels::health::snapshot()
        .into_iter()
        .map(|i| ipc::ChannelIssueInfo {
            channel: i.channel,
            message: i.message,
            at_ms: i.at_ms,
        })
        .collect()
}

/// 向所有菜单栏宿主推送一帧 `TrayState`（顺带剔除已断开的发送端）。
/// 因 `build_tray_state` 需 await 而本函数被大量同步调用点引用，故 spawn 一个任务异步构造并发送；
/// 无订阅者时早退（廉价）。
pub(super) fn broadcast_tray_state(state: &Arc<ServerState>) {
    if !has_tray_subs(state) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let msg = build_tray_state(&state).await;
        if let Ok(mut subs) = state.tray_subs.lock() {
            subs.retain(|tx| tx.send(msg.clone()).is_ok());
        }
    });
}

/// 菜单栏宿主订阅连接（spec D10/D13）：注册发送端、立即推一帧，随后持续推送；读端探测断开。
///
/// **关键：非保活。** `handle_conn` 在连接建立时对 `active` 自增了 1；这里立即 `fetch_sub(1)`
/// 抵消，连接存续期间净占用为 0，再于退出前 `fetch_add(1)` 让 `handle_conn` 末尾的 `fetch_sub(1)`
/// 归零。配合空闲判定不引用 `tray_subs`，从而图标订阅**不会**把 daemon 续命（spec D5 核心）。
pub(super) async fn handle_tray_sub(
    mut reader: Reader,
    w: OwnedWriteHalf,
    state: &Arc<ServerState>,
) {
    state.active.fetch_sub(1, Ordering::SeqCst);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ServerMsg>();
    let mut w = w;
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ipc::write_msg(&mut w, &msg).await.is_err() {
                break;
            }
        }
    });
    // 注册订阅端并立即推一帧当前状态。
    if let Ok(mut subs) = state.tray_subs.lock() {
        subs.push(tx.clone());
    }
    let _ = tx.send(build_tray_state(state).await);

    // 读端仅用于探测断开；宿主正常不发消息。
    wait_cli_eof(&mut reader).await;

    // 收尾：从订阅表移除本端，结束写任务，恢复 active 计数。
    if let Ok(mut subs) = state.tray_subs.lock() {
        subs.retain(|s| !s.same_channel(&tx));
    }
    drop(tx);
    let _ = writer.await;
    state.active.fetch_add(1, Ordering::SeqCst);
}

/// 插话提交的统一处理：覆盖队列（有等待 hook 时立即交付）→ 落盘 → 刷新徽标。
pub(super) fn interject_submit(
    state: &Arc<ServerState>,
    session_id: &str,
    text: &str,
    attachments: Vec<crate::models::FileAttachment>,
) {
    state.interject.submit(session_id, text, attachments);
    state.interject.persist();
    broadcast_agents_state(state);
}

/// 插话追加的统一处理：保留既有队列，追加一条消息；若有等待 hook 则立即交付。
pub(super) fn interject_append(
    state: &Arc<ServerState>,
    session_id: &str,
    text: &str,
    attachments: Vec<crate::models::FileAttachment>,
) {
    state.interject.append(session_id, text, attachments, None);
    state.interject.persist();
    broadcast_agents_state(state);
}

/// 插话 composer 窗口连接（spec agent-interject D7）：登记「composer 打开」（此后到来的
/// PreToolUse poll 挂起等待），同连接上处理提交/查询；**连接断开＝关闭**（放行所有等待 hook）。
///
/// **非保活**（同 `handle_tray_sub` 抵消法）：composer 可能开着放几个小时，不能借此续命 daemon。
pub(super) async fn handle_interject_composer(
    mut reader: Reader,
    mut w: OwnedWriteHalf,
    state: &Arc<ServerState>,
    session_id: String,
) {
    state.active.fetch_sub(1, Ordering::SeqCst);
    state.interject.composer_opened(&session_id);

    loop {
        match ipc::read_msg::<_, ClientMsg>(&mut reader).await {
            Ok(Some(ClientMsg::InterjectSubmit {
                session_id: sid,
                text,
                attachments,
            })) => {
                interject_submit(state, &sid, &text, attachments);
            }
            Ok(Some(ClientMsg::InterjectClear { session_id: sid })) => {
                if state.interject.clear(&sid) {
                    state.interject.persist();
                    broadcast_agents_state(state);
                }
            }
            Ok(Some(ClientMsg::InterjectQuery { session_id: sid })) => {
                let _ = ipc::write_msg(
                    &mut w,
                    &ServerMsg::InterjectState {
                        text: state.interject.full_text(&sid),
                        entries: state.interject.pending_count(&sid),
                        attachments: state.interject.attachments(&sid),
                    },
                )
                .await;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break, // 关窗 / 取消 / 宿主崩溃：连接断开即关闭。
        }
    }

    state.interject.composer_closed(&session_id);
    state.active.fetch_add(1, Ordering::SeqCst);
}

/// 插话 Hold：hook 连接已收到首帧 `hold`，在此等待 composer 提交/取消后回二帧
/// `message`/`release`（spec agent-interject D3/D4）。hook 断开（自身超时/被杀）则放弃；
/// 若消息已交付到本连接但写回失败，重新入队（不丢消息）。
///
/// **非保活**（同 `handle_tray_sub` 抵消法）：等待可长达数小时，不能借此续命 daemon。
pub(super) async fn handle_interject_hold(
    mut reader: Reader,
    mut w: OwnedWriteHalf,
    state: &Arc<ServerState>,
    session_id: String,
    rx: tokio::sync::oneshot::Receiver<crate::agents::interject::WaitOutcome>,
) {
    use crate::agents::interject::WaitOutcome;
    use crate::ipc::InterjectAction;

    state.active.fetch_sub(1, Ordering::SeqCst);
    tokio::select! {
        outcome = rx => {
            let (action, delivery) = match outcome {
                Ok(WaitOutcome::Message(delivery)) => (InterjectAction::Message, Some(delivery)),
                // Release / 发送端消失（会话清理）→ 放行。
                _ => (InterjectAction::Release, None),
            };
            let text = delivery.as_ref().map(|value| value.text.clone()).unwrap_or_default();
            let attachments = delivery
                .as_ref()
                .map(|value| value.attachments.clone())
                .unwrap_or_default();
            let delivered = ipc::write_msg(
                &mut w,
                &ServerMsg::InterjectDecision { action, text, attachments },
            )
            .await
            .is_ok();
            if !delivered {
                if let Some(delivery) = delivery {
                // 极端竞态：交付瞬间 hook 恰好断开 → 消息回队，等下一次工具调用送达。
                    state.interject.requeue_front(&session_id, delivery, Vec::new());
                    state.interject.persist();
                    broadcast_agents_state(state);
                }
            }
        }
        _ = wait_cli_eof(&mut reader) => {
            // hook 侧放弃（超时 fail-open / 进程被杀）：丢弃接收端即可——
            // 交付时发送端 send 失败会自动跳过本等待者（消息不丢）。
        }
    }
    state.active.fetch_add(1, Ordering::SeqCst);
}
