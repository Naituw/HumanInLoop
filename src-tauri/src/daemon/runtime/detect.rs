//! Channel detection flows started from Settings for DingTalk, Feishu, and Slack.

use super::*;

/// 处理「自动识别 userId/open_id」（Q6）：观察现有同 app_key 的长连接，否则临时开连完成识别。
/// 结果经 `Detected`（成功）/ `Error`（失败，已本地化）回设置进程。
pub(super) async fn handle_detect(
    req: &DetectRequest,
    state: &Arc<ServerState>,
    reader: &mut Reader,
    w: &mut OwnedWriteHalf,
) {
    let lang = Lang::resolve(&req.lang);
    let work = async {
        match req.kind.as_str() {
            "dingtalk" => detect_dingtalk(state, req, lang).await,
            "feishu" => detect_feishu(state, req, lang).await,
            "slack" => detect_slack(state, req, lang).await,
            other => Err(format!("unknown detect kind: {}", other)),
        }
    };
    // 识别可能阻塞至多 120s。其间同时监听控制连接：设置进程点「取消」会丢弃 wait 命令的
    // future 并关闭这条连接，`wait_conn_closed` 即返回 → 丢弃 `work`（连带 drop 掉临时长连接，
    // 不残留），不再回包。正常完成则回 `Detected`/`Error`。
    tokio::select! {
        result = work => {
            let msg = match result {
                Ok(id) => {
                    // spec R5：识别成功 → 经该 IM 给识别到的用户回一条「识别成功」回执（best-effort）。
                    send_detect_ack(req, &id, lang).await;
                    ServerMsg::Detected { id }
                }
                Err(message) => ServerMsg::Error { message },
            };
            let _ = ipc::write_msg(w, &msg).await;
        }
        _ = wait_conn_closed(reader) => {
            log("detect cancelled by client (connection closed)");
        }
    }
}

/// spec R5：识别成功后，用识别时的凭据 + 识别到的用户 id 构造一次性 client，回一条「已自动填入<字段>」
/// 回执（不回显 ID 值）。best-effort——失败仅日志，不影响把 id 回设置进程。
pub(super) async fn send_detect_ack(req: &DetectRequest, id: &str, lang: Lang) {
    use crate::autochannel::detect_ack_text;
    let result: Result<(), String> = match req.kind.as_str() {
        "dingtalk" => {
            let field = crate::i18n::tr(lang, "autoChannel.detectFieldUserId");
            let cfg = crate::config::DingTalkChannelConfig {
                enabled: true,
                client_id: req.app_key.trim().to_string(),
                client_secret: req.app_secret.trim().to_string(),
                user_id: id.to_string(),
                ..Default::default()
            };
            match crate::dingtalk::client::DingTalkClient::new(&cfg) {
                Ok(client) => client
                    .send_oto_text(&detect_ack_text(field, lang))
                    .await
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        "feishu" => {
            let field = crate::i18n::tr(lang, "autoChannel.detectFieldOpenId");
            let cfg = crate::config::FeishuChannelConfig {
                enabled: true,
                app_id: req.app_key.trim().to_string(),
                app_secret: req.app_secret.trim().to_string(),
                open_id: id.to_string(),
                base_url: req.base_url.trim().to_string(),
            };
            match crate::feishu::client::FeishuClient::new(&cfg) {
                Ok(client) => client
                    .send_text(&detect_ack_text(field, lang))
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        "slack" => {
            let field = crate::i18n::tr(lang, "autoChannel.detectFieldUserId");
            let cfg = crate::config::SlackChannelConfig {
                enabled: true,
                bot_token: req.app_secret.trim().to_string(),
                app_token: req.app_key.trim().to_string(),
                user_id: id.to_string(),
            };
            match crate::slack::client::SlackClient::new(&cfg) {
                Ok(client) => match client.open_dm().await {
                    Ok(dm) => client
                        .post_text(&dm, &detect_ack_text(field, lang))
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Ok(()),
    };
    if let Err(e) = result {
        log(&format!("detect ack send failed ({}): {}", req.kind, e));
    }
}

/// 等到该控制连接关闭/出错（或对端发来任何消息）即返回——用于在 detect 等待期间感知客户端取消。
pub(super) async fn wait_conn_closed(reader: &mut Reader) {
    let _ = ipc::read_msg::<_, ClientMsg>(reader).await;
}

/// 钉钉识别：优先观察现有同 client_id 的活动连接（零冲突），否则临时开连。
pub(super) async fn detect_dingtalk(
    state: &Arc<ServerState>,
    req: &DetectRequest,
    lang: Lang,
) -> Result<String, String> {
    let code = req.code.trim().to_string();
    if code.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.detectCodeInvalid").to_string());
    }
    // 复用：已有同 client_id 的活动 Router → 观察现有连接（忽略表单 secret）。
    let existing = {
        let guard = state.dd_router.lock().await;
        match guard.as_ref() {
            Some(r) if r.is_alive() && r.client_id() == req.app_key.trim() => Some(r.observe_bot()),
            _ => None,
        }
    };
    if let Some(mut rx) = existing {
        return wait_dd_code(&mut rx, &code, lang).await;
    }
    // 否则 daemon 自行临时开连；完成后 drop（Drop 中止 reader、关闭连接，零泄漏）。
    let router = DdRouter::connect(req.app_key.trim(), req.app_secret.trim()).await?;
    let mut rx = router.observe_bot();
    let out = wait_dd_code(&mut rx, &code, lang).await;
    drop(rx);
    drop(router);
    out
}

/// 等钉钉单聊文本内容等于识别码的消息，返回 senderStaffId；120s 超时。
pub(super) async fn wait_dd_code(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    code: &str,
    lang: Lang,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(data)) => {
                let content = data
                    .get("text")
                    .and_then(|t| t.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim();
                if content == code {
                    if let Some(sender) = data.get("senderStaffId").and_then(|v| v.as_str()) {
                        return Ok(sender.to_string());
                    }
                }
            }
            Ok(None) => return Err(crate::i18n::tr(lang, "cmd.streamDisconnected").to_string()),
            Err(_) => return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string()),
        }
    }
}

/// 飞书识别：优先观察现有同 app_id 的活动连接（零冲突），否则临时开连。
pub(super) async fn detect_feishu(
    state: &Arc<ServerState>,
    req: &DetectRequest,
    lang: Lang,
) -> Result<String, String> {
    let code = req.code.trim().to_string();
    if code.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.detectCodeInvalid").to_string());
    }
    let existing = {
        let guard = state.fs_router.lock().await;
        match guard.as_ref() {
            Some(r) if r.is_alive() && r.app_id() == req.app_key.trim() => {
                Some(r.observe_message())
            }
            _ => None,
        }
    };
    if let Some(mut rx) = existing {
        return wait_fs_code(&mut rx, &code, lang).await;
    }
    let cfg = crate::config::FeishuChannelConfig {
        enabled: true,
        app_id: req.app_key.trim().to_string(),
        app_secret: req.app_secret.trim().to_string(),
        open_id: String::new(),
        base_url: req.base_url.trim().to_string(),
    };
    let router = FsRouter::connect(&cfg).await?;
    let mut rx = router.observe_message();
    let out = wait_fs_code(&mut rx, &code, lang).await;
    drop(rx);
    drop(router);
    out
}

/// 等飞书单聊文本内容等于识别码的消息，返回发送者 open_id；120s 超时。
pub(super) async fn wait_fs_code(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    code: &str,
    lang: Lang,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(event)) => {
                if let Some((open_id, text)) = fs_text_and_sender(&event) {
                    if text.trim() == code {
                        return Ok(open_id);
                    }
                }
            }
            Ok(None) => return Err(crate::i18n::tr(lang, "cmd.streamDisconnected").to_string()),
            Err(_) => return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string()),
        }
    }
}

/// Slack 识别：优先观察现有同 app_token 的活动连接（零冲突），否则临时开连。
/// app_key = App Token（Socket 复用键），app_secret = Bot Token（建连校验齐全）。
pub(super) async fn detect_slack(
    state: &Arc<ServerState>,
    req: &DetectRequest,
    lang: Lang,
) -> Result<String, String> {
    let code = req.code.trim().to_string();
    if code.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.detectCodeInvalid").to_string());
    }
    let existing = {
        let guard = state.sl_router.lock().await;
        match guard.as_ref() {
            Some(r) if r.is_alive() && r.app_token() == req.app_key.trim() => {
                Some(r.observe_message())
            }
            _ => None,
        }
    };
    if let Some(mut rx) = existing {
        return wait_sl_code(&mut rx, &code, lang).await;
    }
    let cfg = crate::config::SlackChannelConfig {
        enabled: true,
        bot_token: req.app_secret.trim().to_string(),
        app_token: req.app_key.trim().to_string(),
        user_id: String::new(),
    };
    let router = SlRouter::connect(&cfg).await?;
    let mut rx = router.observe_message();
    let out = wait_sl_code(&mut rx, &code, lang).await;
    drop(rx);
    drop(router);
    out
}

/// 等 Slack 单聊文本内容等于识别码的消息，返回发送者 user id；120s 超时。
pub(super) async fn wait_sl_code(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    code: &str,
    lang: Lang,
) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string());
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(event)) => {
                let user = event.get("user").and_then(|v| v.as_str()).unwrap_or("");
                let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if !user.is_empty() && text.trim() == code {
                    return Ok(user.to_string());
                }
            }
            Ok(None) => return Err(crate::i18n::tr(lang, "cmd.streamDisconnected").to_string()),
            Err(_) => return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string()),
        }
    }
}
