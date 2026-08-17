//! 飞书 Channel：长连接(WebSocket)收（卡片回调 card.action.trigger + 用户消息 im.message.receive_v1）
//! + OpenAPI 发（文本 / 图片 / 文件 / 互动卡片）。
//!
//! 方案 A（默认）：提问以**互动卡片**（卡片 JSON 2.0）下发——表单容器内放勾选器(多选)、输入框(补充文字)、
//! 提交按钮；用户点「提交」产生一次 `card.action.trigger` 回调（含 `form_value`）完成该题；
//! 作答期间在聊天里发的图片/文件会被累积进答案（纯文字忽略，请用卡片输入框）。卡片回调走长连接，零公网。
//!
//! 方案 B（兜底）：当卡片投放失败时，回退「纯文本 + 编号选项」——用户回一条消息即完成该题。
//!
//! 编排逻辑复用 `conversation::run_conversation`，本文件提供传输实现 `FeishuSession`
//! （`MessagingChannel`）+ 薄外层 `FeishuChannel`。

use super::conversation::{run_conversation, InboundReply, MessagingChannel, QuestionCtx};
use super::{Channel, ConversationOrigin, Interruption, Preemption, ResultSink};
use crate::config::FeishuChannelConfig;
use crate::feishu::card;
use crate::feishu::client::FeishuClient;
use crate::feishu::router::{FsInbound, FsRouter, RoutedFs};
use crate::i18n::{self, Lang};
use crate::models::{ImageAttachment, MessagePrompt, QuestionAnswer};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 抢答轮询粒度：每隔此时长检查一次抢答信号。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Message（正文+附件）发完到发第一道题之间的等待时长，保证「先 message 后题目」的视觉顺序。
const MESSAGE_SETTLE_DELAY: Duration = Duration::from_millis(500);

async fn send_inbound_reply(client: &FeishuClient, reply: InboundReply, lang: Lang) {
    match reply {
        InboundReply::Text(text) => {
            let _ = client.send_text(&text).await;
        }
        InboundReply::Help(view) => {
            let rich = card::build_help_card(&view, lang);
            if client.send_card(&rich).await.is_err() {
                let plain = crate::autochannel::render_help_plain(&view, lang);
                let _ = client.send_text(&plain).await;
            }
        }
    }
}

/// Router 归属：单进程自建一个仅挂本会话的 Router；Daemon 复用共享且常热的 Router。
#[derive(Clone)]
enum FsTransport {
    Own,
    Shared(Arc<FsRouter>),
}

/// 薄外层：接 Coordinator（并行抢答），把会话委托给 `run_conversation` + `FeishuSession`。
pub struct FeishuChannel {
    config: FeishuChannelConfig,
    preempt: Arc<Preemption>,
    transport: FsTransport,
}

impl FeishuChannel {
    /// 单进程外层：本会话自建并独占一条连接（每进程一个 Router、仅挂本会话）。
    pub fn new(config: FeishuChannelConfig) -> Self {
        Self {
            config,
            preempt: Arc::new(Preemption::new()),
            transport: FsTransport::Own,
        }
    }

    /// Daemon 外层：复用共享且常热的 Router（跨请求复用，根治多连接抢消息）。
    pub fn shared(config: FeishuChannelConfig, router: Arc<FsRouter>) -> Self {
        Self {
            config,
            preempt: Arc::new(Preemption::new()),
            transport: FsTransport::Shared(router),
        }
    }
}

impl Channel for FeishuChannel {
    fn id(&self) -> &str {
        "feishu"
    }

    fn start(
        &self,
        request: &crate::models::AskRequest,
        origin: &ConversationOrigin,
        sink: ResultSink,
    ) {
        let config = self.config.clone();
        let preempt = self.preempt.clone();
        let request = request.clone();
        let origin = origin.clone();
        let transport = self.transport.clone();
        tauri::async_runtime::spawn(async move {
            let lang = Lang::current();
            // 取得本会话的事件源句柄（Own：现连一个 Router；Shared：复用）。`_keep` 持有
            // Own Router 直至会话结束；Shared 的 Router 由 Daemon 持有。
            let (events, _keep): (RoutedFs, Option<Arc<FsRouter>>) = match transport {
                FsTransport::Own => match FsRouter::connect(&config).await {
                    Ok(router) => (router.register(), Some(router)),
                    Err(e) => {
                        eprintln!(
                            "{}{}",
                            i18n::warn_prefix(lang),
                            i18n::tr(lang, "channel.fsConfigInvalidSkip").replace("{e}", &e)
                        );
                        return;
                    }
                },
                FsTransport::Shared(router) => (router.register(), None),
            };
            let mut session = FeishuSession::new(config, events);
            if let Err(e) = session.open().await {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.fsConfigInvalidSkip").replace("{e}", &e)
                );
                return;
            }
            run_conversation(&mut session, &request, &origin, preempt, sink).await;
        });
    }

    fn interrupt(&self, reason: &Interruption) {
        self.preempt.interrupt(reason.clone());
    }
}

/// 传输实现：持有 OpenAPI client（发送/置灰）与 Router 事件源句柄（接收，长连接由 Router 独占）。
pub struct FeishuSession {
    config: FeishuChannelConfig,
    client: Option<FeishuClient>,
    events: Option<RoutedFs>,
}

impl FeishuSession {
    pub fn new(config: FeishuChannelConfig, events: RoutedFs) -> Self {
        Self {
            config,
            client: None,
            events: Some(events),
        }
    }
}

#[async_trait::async_trait]
impl MessagingChannel for FeishuSession {
    fn id(&self) -> &str {
        "feishu"
    }

    async fn open(&mut self) -> Result<(), String> {
        // 长连接由 Router 独占，这里只需 OpenAPI client（发送卡片/消息/置灰）。
        let client = FeishuClient::new(&self.config).map_err(|e| e.to_string())?;
        if client.open_id().is_empty() {
            return Err(
                i18n::tr(Lang::current(), "err.fsEmptyConfig").replace("{field}", "Open ID")
            );
        }
        self.client = Some(client);
        Ok(())
    }

    async fn send_message_prompt(
        &mut self,
        message: &MessagePrompt,
        is_markdown: bool,
        header: &str,
        lang: Lang,
    ) {
        let Some(client) = self.client.as_ref() else {
            return;
        };
        // 飞书无 markdown 文本消息：markdown 模式下用卡片（markdown 组件）渲染正文；
        // 非 markdown 或正文为空则发纯文本。
        let result = if is_markdown && !message.text.trim().is_empty() {
            let card = card::build_message_card(header, &message.text);
            client.send_card(&card).await.map(|_| ())
        } else {
            let body = if message.text.trim().is_empty() {
                header.to_string()
            } else {
                format!("{}\n\n{}", header, message.text)
            };
            client.send_text(&body).await.map(|_| ())
        };
        if let Err(e) = result {
            eprintln!(
                "{}{}",
                i18n::warn_prefix(lang),
                i18n::tr(lang, "channel.fsMessageSendFailed").replace("{e}", &e.to_string())
            );
        }

        // 展示文件：图片用 image 消息，其余用 file 消息（原生收发，不做文本转 docx）。
        for file in &message.files {
            if let Err(e) = send_attachment(client, &file.path, &file.name, file.is_image).await {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.fsFileSendFailedLog")
                        .replace("{path}", &file.path)
                        .replace("{e}", &e.to_string())
                );
                let _ = client
                    .send_text(
                        &i18n::tr(lang, "channel.fileSendFailed").replace("{name}", &file.name),
                    )
                    .await;
            }
        }

        tokio::time::sleep(MESSAGE_SETTLE_DELAY).await;
    }

    async fn ask_question(
        &mut self,
        ctx: &QuestionCtx<'_>,
        preempt: &Preemption,
    ) -> Option<QuestionAnswer> {
        let title = if ctx.header.is_empty() {
            i18n::tr(ctx.lang, "channel.fsTitleFallback")
        } else {
            ctx.header
        };

        let Self {
            client,
            events,
            config,
        } = self;
        let client = client.as_ref()?;
        let events = events.as_mut()?;
        let open_id = config.open_id.trim().to_string();

        let placeholder = i18n::tr(ctx.lang, "channel.fsInputPlaceholder");
        let note_label_text = i18n::tr(ctx.lang, "channel.fsNoteLabel");
        let submit_label = i18n::tr(ctx.lang, "channel.fsSubmitButton");
        let recommended_prefix = i18n::tr(ctx.lang, "channel.feishuRecommendedPrefix");
        let todo_text_prefix = i18n::tr(ctx.lang, "whatsNext.todoPrefix");
        let todo_badge_prefix = i18n::tr(ctx.lang, "channel.feishuTodoPrefix");
        // 单选已选状态由会话自管（勾选器在表单外，靠 toggle 回调互斥）。
        let mut selected_single: Vec<String> = Vec::new();
        let question_card = card::build_question_card_with_todo(
            title,
            ctx.text,
            ctx.options,
            ctx.is_markdown,
            ctx.single,
            ctx.select_only,
            &selected_single,
            placeholder,
            submit_label,
            recommended_prefix,
            todo_text_prefix,
            todo_badge_prefix,
        );

        // 1. 投放互动卡片；失败 → 回退纯文本编号方案。
        let message_id = match client.send_card(&question_card).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(ctx.lang),
                    i18n::tr(ctx.lang, "channel.fsCardDeliverFailed")
                        .replace("{e}", &e.to_string())
                );
                return ask_question_text(client, events, &open_id, ctx, preempt).await;
            }
        };

        // 登记卡片精确路由 + 认领本 open_id 的聊天消息。
        events.set_active(Some(&message_id), &open_id);

        // 2. 等卡片「提交」；作答期间收到的图片/文件**并发下载**（不阻塞收事件循环，保证提交一到
        //    就能被立刻处理、即时回包），下载结果累积进共享集合，提交时再收尾。
        let images: Arc<Mutex<Vec<ImageAttachment>>> = Arc::new(Mutex::new(Vec::new()));
        let files: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut downloads: Vec<tauri::async_runtime::JoinHandle<()>> = Vec::new();
        while !preempt.is_cancelled() {
            let ev = match tokio::time::timeout(POLL_INTERVAL, events.recv()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => break,  // 长连接彻底断开
                Err(_) => continue, // 超时：回到循环顶部重新检查 cancelled
            };
            match ev {
                FsInbound::Card { data, ack } => {
                    let parsed = card::parse_card_submit(&data, ctx.options);
                    match &parsed {
                        Some(s) => crate::feishu::ws::debug_log(&format!(
                            "[feishu] card submit: open_id_match={} message_id_match={} (got open_id={}, message_id={})",
                            s.open_id == open_id,
                            s.message_id == message_id,
                            s.open_id,
                            s.message_id
                        )),
                        None => crate::feishu::ws::debug_log(&format!(
                            "[feishu] card action received but not a form submit (no form_value); raw event={}",
                            data
                        )),
                    }
                    match parsed {
                        Some(s) if s.message_id == message_id && s.open_id == open_id => {
                            // 单选：勾选器在表单外，提交的 form_value 无选项，用会话自管的选中态。
                            let selected_final = if ctx.single {
                                selected_single.clone()
                            } else {
                                s.selected_options.clone()
                            };
                            // 终态卡片（禁用表单 + 保留勾选 + 回显补充文字 + 按钮「已提交」）。
                            let finalized = card::build_finalized_card_with_todo(
                                &card::Finalized {
                                    title,
                                    text: ctx.text,
                                    is_markdown: ctx.is_markdown,
                                    options: ctx.options,
                                    selected: &selected_final,
                                    user_input: s.user_input.as_deref(),
                                    input_placeholder: placeholder,
                                    note_label: note_label_text,
                                    button_label: i18n::tr(ctx.lang, "channel.fsSubmitted"),
                                    recommended_prefix,
                                    single: ctx.single,
                                    select_only: ctx.select_only,
                                },
                                todo_text_prefix,
                                todo_badge_prefix,
                            );
                            // 立刻经 Router 同步回包更新卡片 → 按钮 Loading 直接变终态（无闪烁）。
                            // 不再追加 OpenAPI patch_card：那次二次渲染正是残留「快速回弹」的来源。
                            let _ = ack
                                .send_and_wait(Some(card::callback_update_card(finalized)))
                                .await;
                            // 收尾并发下载（不在 3 秒关键路径）。
                            for h in downloads {
                                let _ = h.await;
                            }
                            events.clear_active(Some(&message_id), &open_id);
                            let images = std::mem::take(&mut *images.lock().unwrap());
                            let files = std::mem::take(&mut *files.lock().unwrap());
                            return Some(QuestionAnswer {
                                selected_options: selected_final,
                                user_input: s.user_input,
                                images,
                                files,
                                todo_ids: Vec::new(),
                                todo_selections: Vec::new(),
                            });
                        }
                        // 非提交回调：单选勾选器 toggle → 互斥更新选中态并重渲染卡片。
                        _ => {
                            if ctx.single {
                                if let Some((oid, mid, idx)) = card::parse_toggle(&data) {
                                    if mid == message_id
                                        && oid == open_id
                                        && idx < ctx.options.len()
                                    {
                                        toggle_single(&mut selected_single, &ctx.options[idx].text);
                                        let updated = card::build_question_card_with_todo(
                                            title,
                                            ctx.text,
                                            ctx.options,
                                            ctx.is_markdown,
                                            ctx.single,
                                            ctx.select_only,
                                            &selected_single,
                                            placeholder,
                                            submit_label,
                                            recommended_prefix,
                                            todo_text_prefix,
                                            todo_badge_prefix,
                                        );
                                        let _ = ack.send(Some(card::callback_update_card(updated)));
                                        continue;
                                    }
                                }
                            }
                            // 非本卡片 / 其它：回空 ACK，继续等待。
                            let _ = ack.send(None);
                        }
                    }
                }
                FsInbound::Message(event) => {
                    if event_open_id(&event) == open_id {
                        let lang = ctx.lang;
                        // 即时回执 / 引导（spec R2/R3）：spawn 不阻塞事件循环（保证卡片提交即时处理）。
                        // 斜线命令交 handle_inbound 独占响应，会话层不回引导（避免重复）。
                        if let Some(reply) = super::conversation::answer_inbound_reply(
                            ack_kind(&event),
                            crate::autochannel::AckMode::Card,
                            &message_text(&event),
                            "feishu",
                            lang,
                        ) {
                            let ack_client = client.clone();
                            tauri::async_runtime::spawn(async move {
                                send_inbound_reply(&ack_client, reply, lang).await;
                            });
                        }
                        // 并发下载：spawn 后立刻回到循环收事件，避免大文件下载卡住提交处理。
                        let client = client.clone();
                        let images = images.clone();
                        let files = files.clone();
                        downloads.push(tauri::async_runtime::spawn(async move {
                            accumulate_attachment(&client, &event, &images, &files, lang).await;
                        }));
                    }
                }
            }
        }

        // Interrupted (preempted / cancelled) or disconnected: best-effort PATCH the card to terminal.
        // Preempted → "Answered via X"; cancelled (with/without source) → "Cancelled [by X]";
        // disconnect with no reason → generic "Cancelled".
        let status = match preempt.reason() {
            Some(Interruption::AnsweredBy(w)) => {
                i18n::tr(ctx.lang, "channel.fsAnsweredVia").replace("{source}", &w)
            }
            Some(Interruption::Cancelled(src)) if !src.is_empty() => {
                i18n::tr(ctx.lang, "channel.fsCancelledBy").replace("{source}", &src)
            }
            _ => i18n::tr(ctx.lang, "channel.fsCancelled").to_string(),
        };
        // 被抢答收尾：同样复刻钉钉禁用表单——本端未作答，故不勾选、不回显，按钮文案为「已在 X 回答」。
        let finalized = card::build_finalized_card_with_todo(
            &card::Finalized {
                title,
                text: ctx.text,
                is_markdown: ctx.is_markdown,
                options: ctx.options,
                selected: &[],
                user_input: None,
                input_placeholder: placeholder,
                note_label: note_label_text,
                button_label: &status,
                recommended_prefix,
                single: ctx.single,
                select_only: ctx.select_only,
            },
            todo_text_prefix,
            todo_badge_prefix,
        );
        let finalize_action = match client.patch_card(&message_id, &finalized).await {
            Ok(()) => "card_finalize_succeeded",
            Err(error) => {
                eprintln!("[feishu] failed to finalize interrupted card: {error}");
                "card_finalize_failed"
            }
        };
        crate::daemon::lifecycle::log_runtime_event("channel_feishu", finalize_action, None);
        events.clear_active(Some(&message_id), &open_id);
        None
    }

    async fn close(&mut self) {
        // 丢弃事件源句柄 → 从 Router 注销路由（Daemon 下及时清理，避免陈旧路由堆积）。
        self.events = None;
    }
}

/// 兜底：纯文本 + 编号选项问一题（卡片投放失败时使用）。用户回一条消息即完成该题。
async fn ask_question_text(
    client: &FeishuClient,
    events: &mut RoutedFs,
    open_id: &str,
    ctx: &QuestionCtx<'_>,
    preempt: &Preemption,
) -> Option<QuestionAnswer> {
    // 编号回复按原文映射（编号清单展示用显示文本，见 build_question_text）。
    let option_texts: Vec<String> = ctx.options.iter().map(|o| o.text.clone()).collect();
    let body = build_question_text(ctx);
    if let Err(e) = client.send_text(&body).await {
        eprintln!(
            "{}{}",
            i18n::warn_prefix(ctx.lang),
            i18n::tr(ctx.lang, "channel.fsQuestionSendFailed").replace("{e}", &e.to_string())
        );
    }

    // 文本兜底无卡片：认领本 open_id 的聊天消息即可（不登记卡片精确路由）。
    events.set_active(None, open_id);

    while !preempt.is_cancelled() {
        let ev = match tokio::time::timeout(POLL_INTERVAL, events.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => continue,
        };
        match ev {
            FsInbound::Message(event) => {
                if event_open_id(&event) != open_id {
                    continue;
                }
                let kind = ack_kind(&event).or(Some(crate::autochannel::AckKind::Text));
                let text = message_text(&event);
                if let Some(answer) =
                    message_to_answer(client, &event, &option_texts, ctx.select_only, ctx.single)
                        .await
                {
                    // 接受 → 确认（spec R2 文本兜底）。
                    if let Some(reply) = super::conversation::answer_inbound_reply(
                        kind,
                        crate::autochannel::AckMode::Fallback,
                        &text,
                        "feishu",
                        ctx.lang,
                    ) {
                        send_inbound_reply(client, reply, ctx.lang).await;
                    }
                    events.clear_active(None, open_id);
                    return Some(answer);
                } else {
                    // 未接受 → 引导（spec R3）；命令交 handle_inbound，不回引导。
                    if let Some(reply) = super::conversation::answer_inbound_reply(
                        None,
                        crate::autochannel::AckMode::Fallback,
                        &text,
                        "feishu",
                        ctx.lang,
                    ) {
                        send_inbound_reply(client, reply, ctx.lang).await;
                    }
                }
            }
            // 文本兜底路径未登记卡片路由，不会收到卡片回调；忽略（回空 ACK 以防万一）。
            FsInbound::Card { ack, .. } => {
                let _ = ack.send(None);
            }
        }
    }
    events.clear_active(None, open_id);
    None
}

/// 累积聊天里收到的图片/文件（卡片作答期间）；纯文字等忽略。
/// 在并发下载任务中调用：下载完成后再锁住共享集合追加（锁期间不 await）。
async fn accumulate_attachment(
    client: &FeishuClient,
    event: &Value,
    images: &Mutex<Vec<ImageAttachment>>,
    files: &Mutex<Vec<String>>,
    lang: Lang,
) {
    let Some((msg_type, message_id, content)) = parse_message(event) else {
        return;
    };
    match msg_type.as_str() {
        "image" => {
            let Some(key) = content.get("image_key").and_then(|v| v.as_str()) else {
                return;
            };
            match download_image(client, &message_id, key).await {
                Ok(img) => images.lock().unwrap().push(img),
                Err(e) => eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.fsImageDownloadFailed").replace("{e}", &e)
                ),
            }
        }
        "file" => {
            let Some(key) = content.get("file_key").and_then(|v| v.as_str()) else {
                return;
            };
            let file_name = content
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let ext = ext_of(file_name);
            match client
                .download_resource_to(&message_id, key, "file", ext)
                .await
            {
                Ok(path) => files.lock().unwrap().push(path),
                Err(e) => eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.fsFileDownloadFailed").replace("{e}", &e.to_string())
                ),
            }
        }
        _ => {} // 文字 / 富文本 等：卡片模式下忽略（请用卡片输入框）。
    }
}

/// 飞书聊天消息 → 回执内容种类：图片/文件是可累积进答案的附件；其余（文字/富文本等）非附件→ None。
fn ack_kind(event: &Value) -> Option<crate::autochannel::AckKind> {
    use crate::autochannel::AckKind;
    let msg_type = parse_message(event).map(|(t, _, _)| t).unwrap_or_default();
    match msg_type.as_str() {
        "image" => Some(AckKind::Image),
        "file" => Some(AckKind::File),
        _ => None,
    }
}

/// 组装提问正文（纯文本兜底）：头部（加粗）+ 正文 + 编号选项 + 作答提示。
fn build_question_text(ctx: &QuestionCtx<'_>) -> String {
    let mut s = String::new();
    if !ctx.header.is_empty() {
        s.push_str(ctx.header);
        s.push_str("\n\n");
    }
    if !ctx.text.is_empty() {
        s.push_str(ctx.text);
        s.push_str("\n\n");
    }
    if ctx.options.is_empty() {
        s.push_str(i18n::tr(ctx.lang, "channel.ddHintFree"));
    } else {
        for (i, opt) in ctx.options.iter().enumerate() {
            s.push_str(&format!(
                "{}. {}\n",
                i + 1,
                super::conversation::display_text(opt, ctx.lang)
            ));
        }
        s.push('\n');
        s.push_str(i18n::tr(ctx.lang, "channel.ddHintOptions"));
    }
    s.trim_end().to_string()
}

/// 把一条用户消息转成回答；非可作答类型返回 None（继续等待）。
/// 严格模式（`select_only`）忽略自由文字与附件，只接受编号选择；单选只取首个编号。
async fn message_to_answer(
    client: &FeishuClient,
    event: &Value,
    options: &[String],
    select_only: bool,
    single: bool,
) -> Option<QuestionAnswer> {
    let (msg_type, message_id, content) = parse_message(event)?;
    match msg_type.as_str() {
        "text" => {
            let text = content
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim();
            if text.is_empty() {
                return None;
            }
            let (mut selected, user_input) = parse_reply(text, options);
            if single {
                selected.truncate(1);
            }
            // 严格模式忽略自由文字（只认编号选择）。
            let user_input = if select_only { None } else { user_input };
            if selected.is_empty() && user_input.is_none() {
                return None;
            }
            Some(QuestionAnswer {
                selected_options: selected,
                user_input,
                images: Vec::new(),
                files: Vec::new(),
                todo_ids: Vec::new(),
                todo_selections: Vec::new(),
            })
        }
        // 严格模式禁附件：图片/文件回复忽略（继续等待编号选择）。
        "image" if select_only => None,
        "file" if select_only => None,
        "image" => {
            let key = content.get("image_key").and_then(|v| v.as_str())?;
            match download_image(client, &message_id, key).await {
                Ok(img) => Some(QuestionAnswer {
                    selected_options: Vec::new(),
                    user_input: None,
                    images: vec![img],
                    files: Vec::new(),
                    todo_ids: Vec::new(),
                    todo_selections: Vec::new(),
                }),
                Err(e) => {
                    let lang = Lang::current();
                    eprintln!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "channel.fsImageDownloadFailed").replace("{e}", &e)
                    );
                    None
                }
            }
        }
        "file" => {
            let key = content.get("file_key").and_then(|v| v.as_str())?;
            let file_name = content
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let ext = ext_of(file_name);
            match client
                .download_resource_to(&message_id, key, "file", ext)
                .await
            {
                Ok(path) => Some(QuestionAnswer {
                    selected_options: Vec::new(),
                    user_input: None,
                    images: Vec::new(),
                    files: vec![path],
                    todo_ids: Vec::new(),
                    todo_selections: Vec::new(),
                }),
                Err(e) => {
                    let lang = Lang::current();
                    eprintln!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "channel.fsFileDownloadFailed")
                            .replace("{e}", &e.to_string())
                    );
                    None
                }
            }
        }
        _ => None,
    }
}

/// 单选互斥（原文）：点中已选项则清空；否则清空后只保留该项。
fn toggle_single(selected: &mut Vec<String>, option: &str) {
    if selected.iter().any(|s| s == option) {
        selected.clear();
    } else {
        selected.clear();
        selected.push(option.to_string());
    }
}

/// 解析文字回复：纯编号（含逗号/空格分隔）→ 映射选项；否则整条作为自由文本。
fn parse_reply(text: &str, options: &[String]) -> (Vec<String>, Option<String>) {
    let trimmed = text.trim();
    let selection_only = !options.is_empty()
        && trimmed.chars().all(|c| {
            c.is_ascii_digit()
                || c.is_whitespace()
                || matches!(c, ',' | '，' | '、' | '/' | ';' | '；' | '.' | '。')
        });
    if selection_only {
        let mut selected: Vec<String> = Vec::new();
        for tok in trimmed.split(|c: char| !c.is_ascii_digit()) {
            if tok.is_empty() {
                continue;
            }
            if let Ok(n) = tok.parse::<usize>() {
                if n >= 1 && n <= options.len() {
                    let opt = options[n - 1].clone();
                    if !selected.contains(&opt) {
                        selected.push(opt);
                    }
                }
            }
        }
        if !selected.is_empty() {
            return (selected, None);
        }
    }
    (Vec::new(), Some(trimmed.to_string()))
}

/// 取消息发送者 open_id。
fn event_open_id(event: &Value) -> &str {
    event
        .get("sender")
        .and_then(|s| s.get("sender_id"))
        .and_then(|i| i.get("open_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// 取聊天消息的纯文本内容（仅 `text` 类型有；其它类型返回空串）。用于判断是否斜线命令。
fn message_text(event: &Value) -> String {
    match parse_message(event) {
        Some((t, _, content)) if t == "text" => content
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// 解析 `event.message`：返回 (message_type, message_id, 解析后的 content)。
fn parse_message(event: &Value) -> Option<(String, String, Value)> {
    let message = event.get("message")?;
    let msg_type = message
        .get("message_type")
        .and_then(|v| v.as_str())?
        .to_string();
    let message_id = message
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content_str = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let content: Value = serde_json::from_str(content_str).unwrap_or(Value::Null);
    Some((msg_type, message_id, content))
}

/// 下载图片消息并转为 `ImageAttachment`（raw base64 + media_type）。
async fn download_image(
    client: &FeishuClient,
    message_id: &str,
    image_key: &str,
) -> Result<ImageAttachment, String> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let path = client
        .download_resource_to(message_id, image_key, "image", "png")
        .await
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(ImageAttachment {
        data: B64.encode(bytes),
        media_type: "image/png".to_string(),
        filename: None,
    })
}

/// 上传并发送一个附件（图片/文件）。
async fn send_attachment(
    client: &FeishuClient,
    path: &str,
    name: &str,
    is_image: bool,
) -> Result<(), crate::feishu::FeishuError> {
    if is_image {
        let image_key = client.upload_image(path).await?;
        client.send_image(&image_key).await?;
    } else {
        let file_key = client.upload_file(path, name).await?;
        client.send_file(&file_key).await?;
    }
    Ok(())
}

/// 取文件扩展名（无则空串）。
fn ext_of(name: &str) -> &str {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}
