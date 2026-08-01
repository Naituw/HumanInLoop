//! Slack Channel：Socket Mode 长连接收（交互回调 block_actions + 用户消息 message.im）
//! + Web API 发（文本 / 图片 / 文件 / Block Kit 互动卡片）。
//!
//! 方案 A（默认）：提问以 **Block Kit 消息内表单**下发——`input` 块放复选框(多选) + 多行输入框(补充文字)
//! 加提交按钮；用户点「提交」产生一次 `block_actions` 回调（`state.values` 汇总取值）完成该题；
//! 作答期间在 DM 里发的图片/文件会被累积进答案（纯文字忽略，请用卡片输入框）。回调走 Socket Mode，零公网。
//!
//! 方案 B（兜底）：当卡片投放失败时，回退「纯文本 + 编号选项」——用户回一条消息即完成该题。
//!
//! 编排逻辑复用 `conversation::run_conversation`，本文件提供传输实现 `SlackSession`
//! （`MessagingChannel`）+ 薄外层 `SlackChannel`。
//!
//! 与飞书差异：终态不能「禁用控件保留外观」，故收尾用 `chat.update` 把卡片替换为**静态终态**
//! （回显已选项 + 补充文字 + 状态行，移除控件）；ack 在 `ws` 层收帧即完成，无需 oneshot 回包。

use super::conversation::{run_conversation, InboundReply, MessagingChannel, QuestionCtx};
use super::{Channel, ConversationOrigin, Interruption, Preemption, ResultSink};
use crate::config::SlackChannelConfig;
use crate::i18n::{self, Lang};
use crate::models::{ImageAttachment, MessagePrompt, QuestionAnswer};
use crate::slack::blockkit;
use crate::slack::client::SlackClient;
use crate::slack::markdown;
use crate::slack::router::{RoutedSl, SlInbound, SlRouter};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 抢答轮询粒度：每隔此时长检查一次抢答信号。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 卡片 nonce 自增计数器（进程内唯一，配合毫秒时间戳保证跨进程/重启也不重复）。
static CARD_SEQ: AtomicU64 = AtomicU64::new(0);

/// 生成每张卡片唯一的 nonce：毫秒时间戳 + 进程内自增序号。
fn next_card_nonce() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = CARD_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}{}", ms, seq)
}

/// Message（正文+附件）发完到发第一道题之间的等待时长，保证「先 message 后题目」的视觉顺序。
const MESSAGE_SETTLE_DELAY: Duration = Duration::from_millis(500);

async fn send_inbound_reply(client: &SlackClient, dm: &str, reply: InboundReply, lang: Lang) {
    match reply {
        InboundReply::Text(text) => {
            let _ = client.post_text(dm, &text).await;
        }
        InboundReply::Help(view) => {
            let plain = crate::autochannel::render_help_plain(&view, lang);
            let blocks = blockkit::build_help_blocks(&view, lang);
            if client
                .post_message(dm, Some(&blocks), &plain)
                .await
                .is_err()
            {
                let _ = client.post_text(dm, &plain).await;
            }
        }
    }
}

/// Router 归属：单进程自建一个仅挂本会话的 Router；Daemon 复用共享且常热的 Router。
#[derive(Clone)]
enum SlTransport {
    Own,
    Shared(Arc<SlRouter>),
}

/// 薄外层：接 Coordinator（并行抢答），把会话委托给 `run_conversation` + `SlackSession`。
pub struct SlackChannel {
    config: SlackChannelConfig,
    preempt: Arc<Preemption>,
    transport: SlTransport,
}

impl SlackChannel {
    /// 单进程外层：本会话自建并独占一条连接（每进程一个 Router、仅挂本会话）。
    pub fn new(config: SlackChannelConfig) -> Self {
        Self {
            config,
            preempt: Arc::new(Preemption::new()),
            transport: SlTransport::Own,
        }
    }

    /// Daemon 外层：复用共享且常热的 Router（跨请求复用，根治多连接抢消息）。
    pub fn shared(config: SlackChannelConfig, router: Arc<SlRouter>) -> Self {
        Self {
            config,
            preempt: Arc::new(Preemption::new()),
            transport: SlTransport::Shared(router),
        }
    }
}

impl Channel for SlackChannel {
    fn id(&self) -> &str {
        "slack"
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
            let (events, _keep): (RoutedSl, Option<Arc<SlRouter>>) = match transport {
                SlTransport::Own => match SlRouter::connect(&config).await {
                    Ok(router) => (router.register(), Some(router)),
                    Err(e) => {
                        eprintln!(
                            "{}{}",
                            i18n::warn_prefix(lang),
                            i18n::tr(lang, "channel.slConfigInvalidSkip").replace("{e}", &e)
                        );
                        return;
                    }
                },
                SlTransport::Shared(router) => (router.register(), None),
            };
            let mut session = SlackSession::new(config, events);
            if let Err(e) = session.open().await {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.slConfigInvalidSkip").replace("{e}", &e)
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

/// 传输实现：持有 Web API client（发送/更新）、解析出的 DM 频道 id、Router 事件源句柄（接收）。
pub struct SlackSession {
    config: SlackChannelConfig,
    client: Option<SlackClient>,
    /// 与配置 userId 的 DM 频道 id（open() 时经 conversations.open 解析并缓存）。
    dm_channel: Option<String>,
    events: Option<RoutedSl>,
}

impl SlackSession {
    pub fn new(config: SlackChannelConfig, events: RoutedSl) -> Self {
        Self {
            config,
            client: None,
            dm_channel: None,
            events: Some(events),
        }
    }
}

#[async_trait::async_trait]
impl MessagingChannel for SlackSession {
    fn id(&self) -> &str {
        "slack"
    }

    async fn open(&mut self) -> Result<(), String> {
        // 长连接由 Router 独占，这里只需 Web API client（发送/更新）+ 解析 DM 频道。
        let client = SlackClient::new(&self.config).map_err(|e| e.localized(Lang::current()))?;
        if client.user_id().is_empty() {
            return Err(
                i18n::tr(Lang::current(), "err.slEmptyConfig").replace("{field}", "User ID")
            );
        }
        let dm = client.open_dm().await.map_err(|e| e.to_string())?;
        self.dm_channel = Some(dm);
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
        let (Some(client), Some(dm)) = (self.client.as_ref(), self.dm_channel.as_deref()) else {
            return;
        };
        // Slack 专属：消息头用大号 header + ✉️ 信封前缀（与问题标题同款；messageFrom 为多渠道
        // 共用文案，故图标仅在此处加，不影响其它渠道）。正文随后作为 section（mrkdwn / 纯文本）。
        let header = format!("✉️ {}", header);
        let blocks = blockkit::build_message_blocks(&header, &message.text, is_markdown);
        if let Err(e) = client.post_message(dm, Some(&blocks), &header).await {
            eprintln!(
                "{}{}",
                i18n::warn_prefix(lang),
                i18n::tr(lang, "channel.slMessageSendFailed").replace("{e}", &e.to_string())
            );
        }

        // 展示文件：图片/文件统一走新版上传流程（Slack 自动内联渲染图片）。
        for file in &message.files {
            if let Err(e) = client.upload_file(dm, &file.path, &file.name).await {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.slFileSendFailedLog")
                        .replace("{path}", &file.path)
                        .replace("{e}", &e.to_string())
                );
                let _ = client
                    .post_text(
                        dm,
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
            i18n::tr(ctx.lang, "channel.slTitleFallback")
        } else {
            ctx.header
        };

        let Self {
            client,
            events,
            config,
            dm_channel,
        } = self;
        let client = client.as_ref()?;
        let dm = dm_channel.as_deref()?;
        let events = events.as_mut()?;
        let user_id = config.user_id.trim().to_string();

        let options_label = i18n::tr(ctx.lang, "channel.slOptionsLabel");
        let input_label = i18n::tr(ctx.lang, "channel.slInputLabel");
        let placeholder = i18n::tr(ctx.lang, "channel.slInputPlaceholder");
        let submit_label = i18n::tr(ctx.lang, "channel.slSubmitButton");
        // 每张卡片唯一 nonce 拼入 input 块 block_id：规避 Slack 按 block_id 缓存输入草稿
        // 导致新卡片回填上一题的输入/勾选。
        let nonce = next_card_nonce();
        let card = blockkit::build_question_card(
            title,
            ctx.text,
            ctx.options,
            ctx.is_markdown,
            ctx.single,
            ctx.select_only,
            options_label,
            input_label,
            placeholder,
            submit_label,
            i18n::tr(ctx.lang, "channel.slackRecommended"),
            &nonce,
        );
        // 通知/回退文本（折叠态、推送预览）。
        let notify = fallback_notify(title, ctx);

        // 1. 投放互动卡片；失败 → 回退纯文本编号方案。
        let message_ts = match client.post_message(dm, Some(&card), &notify).await {
            Ok(ts) => ts,
            Err(e) => {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(ctx.lang),
                    i18n::tr(ctx.lang, "channel.slCardDeliverFailed")
                        .replace("{e}", &e.to_string())
                );
                return ask_question_text(client, events, dm, &user_id, ctx, preempt).await;
            }
        };

        // 登记卡片精确路由（按 message_ts）+ 认领本 user_id 的聊天消息。
        events.set_active(Some(&message_ts), &user_id);

        // 2. 等卡片「提交」；作答期间收到的图片/文件**并发下载**（不阻塞收事件循环）。
        let images: Arc<Mutex<Vec<ImageAttachment>>> = Arc::new(Mutex::new(Vec::new()));
        let files: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut downloads: Vec<tauri::async_runtime::JoinHandle<()>> = Vec::new();
        let mut selected_single = None;
        while !preempt.is_cancelled() {
            let ev = match tokio::time::timeout(POLL_INTERVAL, events.recv()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => break,  // 长连接彻底断开
                Err(_) => continue, // 超时：回到循环顶部重新检查 cancelled
            };
            match ev {
                SlInbound::Interactive(payload) => {
                    if let Some(selection) = blockkit::parse_single_select(&payload) {
                        if ctx.single
                            && selection.index < ctx.options.len()
                            && selection.message_ts == message_ts
                            && (user_id.is_empty() || selection.user_id == user_id)
                            && (selection.channel_id.is_empty() || selection.channel_id == dm)
                        {
                            selected_single = Some(selection.index);
                            let updated = blockkit::build_question_card_with_state(
                                title,
                                ctx.text,
                                ctx.options,
                                ctx.is_markdown,
                                ctx.single,
                                ctx.select_only,
                                options_label,
                                input_label,
                                placeholder,
                                submit_label,
                                i18n::tr(ctx.lang, "channel.slackRecommended"),
                                &nonce,
                                selected_single,
                                selection.user_input.as_deref(),
                            );
                            let _ = client
                                .update_message(dm, &message_ts, Some(&updated), &notify)
                                .await;
                        }
                        continue;
                    }
                    let Some(s) =
                        blockkit::parse_submit_with_single(&payload, ctx.options, selected_single)
                    else {
                        continue;
                    };
                    if s.message_ts != message_ts || (!user_id.is_empty() && s.user_id != user_id) {
                        continue; // 非本题卡片 / 非目标用户
                    }
                    // 静态终态卡片（移除控件 + 回显选择与补充文字 + 状态行「已提交」）。
                    let finalized = blockkit::build_finalized_card(&blockkit::Finalized {
                        title,
                        text: ctx.text,
                        is_markdown: ctx.is_markdown,
                        selected: &s.selected_options,
                        user_input: s.user_input.as_deref(),
                        status: i18n::tr(ctx.lang, "channel.slSubmitted"),
                    });
                    let _ = client
                        .update_message(dm, &message_ts, Some(&finalized), &notify)
                        .await;
                    // 收尾并发下载。
                    for h in downloads {
                        let _ = h.await;
                    }
                    events.clear_active(Some(&message_ts), &user_id);
                    let images = std::mem::take(&mut *images.lock().unwrap());
                    let files = std::mem::take(&mut *files.lock().unwrap());
                    return Some(QuestionAnswer {
                        selected_options: s.selected_options,
                        user_input: s.user_input,
                        images,
                        files,
                        todo_ids: Vec::new(),
                        todo_selections: Vec::new(),
                    });
                }
                SlInbound::Message(event) => {
                    if event_user(&event) == user_id {
                        let lang = ctx.lang;
                        // 即时回执 / 引导（spec R2/R3）：spawn 不阻塞事件循环（保证卡片提交即时处理）。
                        // 斜线命令交 handle_inbound 独占响应，会话层不回引导（避免重复）。
                        if let Some(reply) = super::conversation::answer_inbound_reply(
                            ack_kind(&event),
                            crate::autochannel::AckMode::Card,
                            event.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                            "slack",
                            lang,
                        ) {
                            let ack_client = client.clone();
                            let ack_dm = dm.to_string();
                            tauri::async_runtime::spawn(async move {
                                send_inbound_reply(&ack_client, &ack_dm, reply, lang).await;
                            });
                        }
                        let client = client.clone();
                        let images = images.clone();
                        let files = files.clone();
                        downloads.push(tauri::async_runtime::spawn(async move {
                            accumulate_attachments(&client, &event, &images, &files, lang).await;
                        }));
                    }
                }
            }
        }

        // 被抢答 / 取消 / 断连：把卡片更新为静态终态（本端未作答 → 不回显选择，仅状态行）。
        let status = match preempt.reason() {
            Some(Interruption::AnsweredBy(w)) => {
                i18n::tr(ctx.lang, "channel.slAnsweredVia").replace("{source}", &w)
            }
            Some(Interruption::Cancelled(src)) if !src.is_empty() => {
                i18n::tr(ctx.lang, "channel.slCancelledBy").replace("{source}", &src)
            }
            _ => i18n::tr(ctx.lang, "channel.slCancelled").to_string(),
        };
        let finalized = blockkit::build_finalized_card(&blockkit::Finalized {
            title,
            text: ctx.text,
            is_markdown: ctx.is_markdown,
            selected: &[],
            user_input: None,
            status: &status,
        });
        let _ = client
            .update_message(dm, &message_ts, Some(&finalized), &notify)
            .await;
        events.clear_active(Some(&message_ts), &user_id);
        None
    }

    async fn close(&mut self) {
        // 丢弃事件源句柄 → 从 Router 注销路由（Daemon 下及时清理，避免陈旧路由堆积）。
        self.events = None;
    }
}

/// 兜底：纯文本 + 编号选项问一题（卡片投放失败时使用）。用户回一条消息即完成该题。
async fn ask_question_text(
    client: &SlackClient,
    events: &mut RoutedSl,
    dm: &str,
    user_id: &str,
    ctx: &QuestionCtx<'_>,
    preempt: &Preemption,
) -> Option<QuestionAnswer> {
    // 编号回复按原文映射（编号清单展示用显示文本，见 build_question_text）。
    let option_texts: Vec<String> = ctx.options.iter().map(|o| o.text.clone()).collect();
    let body = build_question_text(ctx);
    if let Err(e) = client.post_text(dm, &body).await {
        eprintln!(
            "{}{}",
            i18n::warn_prefix(ctx.lang),
            i18n::tr(ctx.lang, "channel.slQuestionSendFailed").replace("{e}", &e.to_string())
        );
    }

    // 文本兜底无卡片：认领本 user_id 的聊天消息即可（不登记卡片精确路由）。
    events.set_active(None, user_id);

    while !preempt.is_cancelled() {
        let ev = match tokio::time::timeout(POLL_INTERVAL, events.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => continue,
        };
        match ev {
            SlInbound::Message(event) => {
                if event_user(&event) != user_id {
                    continue;
                }
                let kind = ack_kind(&event).or(Some(crate::autochannel::AckKind::Text));
                let text = event
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(answer) = message_to_answer(
                    client,
                    &event,
                    &option_texts,
                    ctx.lang,
                    ctx.select_only,
                    ctx.single,
                )
                .await
                {
                    // 接受 → 确认（spec R2 文本兜底）。
                    if let Some(reply) = super::conversation::answer_inbound_reply(
                        kind,
                        crate::autochannel::AckMode::Fallback,
                        &text,
                        "slack",
                        ctx.lang,
                    ) {
                        send_inbound_reply(client, dm, reply, ctx.lang).await;
                    }
                    events.clear_active(None, user_id);
                    return Some(answer);
                } else {
                    // 未接受 → 引导（spec R3）；命令交 handle_inbound，不回引导。
                    if let Some(reply) = super::conversation::answer_inbound_reply(
                        None,
                        crate::autochannel::AckMode::Fallback,
                        &text,
                        "slack",
                        ctx.lang,
                    ) {
                        send_inbound_reply(client, dm, reply, ctx.lang).await;
                    }
                }
            }
            // 文本兜底路径未登记卡片路由，不应收到交互回调；忽略。
            SlInbound::Interactive(_) => {}
        }
    }
    events.clear_active(None, user_id);
    None
}

/// 累积聊天里收到的图片/文件（卡片作答期间）；纯文字等忽略。
/// 在并发下载任务中调用：下载完成后再锁住共享集合追加（锁期间不 await）。
async fn accumulate_attachments(
    client: &SlackClient,
    event: &Value,
    images: &Mutex<Vec<ImageAttachment>>,
    files: &Mutex<Vec<String>>,
    lang: Lang,
) {
    let Some(arr) = event.get("files").and_then(|f| f.as_array()) else {
        return; // 纯文字/无文件：卡片模式下忽略
    };
    for f in arr {
        match download_one(client, f).await {
            Ok(Attachment::Image(img)) => images.lock().unwrap().push(img),
            Ok(Attachment::File(path)) => files.lock().unwrap().push(path),
            Err(e) => eprintln!(
                "{}{}",
                i18n::warn_prefix(lang),
                i18n::tr(lang, "channel.slFileDownloadFailed").replace("{e}", &e)
            ),
        }
    }
}

/// Slack 聊天消息 → 回执内容种类：带文件（图片优先判定为图片，否则文件）是可累积进答案的附件；
/// 无文件（纯文字）非附件→ None。
fn ack_kind(event: &Value) -> Option<crate::autochannel::AckKind> {
    use crate::autochannel::AckKind;
    let arr = event.get("files").and_then(|f| f.as_array())?;
    if arr.is_empty() {
        return None;
    }
    let any_image = arr.iter().any(|f| {
        f.get("mimetype")
            .and_then(|v| v.as_str())
            .map(|m| m.starts_with("image/"))
            .unwrap_or(false)
    });
    Some(if any_image {
        AckKind::Image
    } else {
        AckKind::File
    })
}

/// 组装提问正文（纯文本兜底）：头部（加粗）+ 正文 + 编号选项 + 作答提示。
fn build_question_text(ctx: &QuestionCtx<'_>) -> String {
    let mut s = String::new();
    if !ctx.header.is_empty() {
        s.push_str(&format!("*{}*", markdown::escape(ctx.header)));
        s.push_str("\n\n");
    }
    if !ctx.text.is_empty() {
        let body = if ctx.is_markdown {
            markdown::to_mrkdwn(ctx.text)
        } else {
            markdown::escape(ctx.text)
        };
        s.push_str(&body);
        s.push_str("\n\n");
    }
    if ctx.options.is_empty() {
        s.push_str(i18n::tr(ctx.lang, "channel.ddHintFree"));
    } else {
        for (i, opt) in ctx.options.iter().enumerate() {
            let display = super::conversation::display_text(opt, ctx.lang);
            s.push_str(&format!("{}. {}\n", i + 1, markdown::escape(&display)));
        }
        s.push('\n');
        s.push_str(i18n::tr(ctx.lang, "channel.ddHintOptions"));
    }
    s.trim_end().to_string()
}

/// 折叠态 / 推送预览用的回退文本（不含卡片时 Slack 展示它）。
fn fallback_notify(title: &str, ctx: &QuestionCtx<'_>) -> String {
    let base = if !title.trim().is_empty() {
        title
    } else if !ctx.text.trim().is_empty() {
        ctx.text
    } else {
        "AskHuman"
    };
    base.chars().take(150).collect()
}

/// 把一条用户消息转成回答（文本兜底）；无可作答内容返回 None（继续等待）。
/// 严格模式（`select_only`）忽略自由文字与附件，只接受编号选择；单选只取首个编号。
async fn message_to_answer(
    client: &SlackClient,
    event: &Value,
    options: &[String],
    lang: Lang,
    select_only: bool,
    single: bool,
) -> Option<QuestionAnswer> {
    let mut images: Vec<ImageAttachment> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    // 严格模式禁附件：不下载，回复里的图片/文件忽略。
    if !select_only {
        if let Some(arr) = event.get("files").and_then(|f| f.as_array()) {
            for f in arr {
                match download_one(client, f).await {
                    Ok(Attachment::Image(img)) => images.push(img),
                    Ok(Attachment::File(path)) => files.push(path),
                    Err(e) => eprintln!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "channel.slFileDownloadFailed").replace("{e}", &e)
                    ),
                }
            }
        }
    }
    let text = event
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    let (mut selected, user_input) = if text.is_empty() {
        (Vec::new(), None)
    } else {
        parse_reply(text, options)
    };
    // 单选：仅保留首个编号。
    if single {
        selected.truncate(1);
    }
    // 严格模式忽略自由文字（只认编号选择）。
    let user_input = if select_only { None } else { user_input };
    if selected.is_empty() && user_input.is_none() && images.is_empty() && files.is_empty() {
        return None;
    }
    Some(QuestionAnswer {
        selected_options: selected,
        user_input,
        images,
        files,
        todo_ids: Vec::new(),
        todo_selections: Vec::new(),
    })
}

enum Attachment {
    Image(ImageAttachment),
    File(String),
}

/// 下载一个 Slack 文件对象：图片 → base64 内联；其余 → 落盘返回路径。
async fn download_one(client: &SlackClient, f: &Value) -> Result<Attachment, String> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let url = f
        .get("url_private_download")
        .and_then(|v| v.as_str())
        .or_else(|| f.get("url_private").and_then(|v| v.as_str()))
        .ok_or_else(|| "missing file url".to_string())?;
    let mimetype = f.get("mimetype").and_then(|v| v.as_str()).unwrap_or("");
    let filetype = f.get("filetype").and_then(|v| v.as_str()).unwrap_or("");
    let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("file");
    let ext = if !filetype.is_empty() {
        filetype
    } else {
        ext_of(name)
    };
    let path = client
        .download_file_to(url, ext)
        .await
        .map_err(|e| e.to_string())?;
    if mimetype.starts_with("image/") {
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        Ok(Attachment::Image(ImageAttachment {
            data: B64.encode(bytes),
            media_type: mimetype.to_string(),
            filename: Some(name.to_string()),
        }))
    } else {
        Ok(Attachment::File(path))
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

/// 取消息发送者 user id。
fn event_user(event: &Value) -> &str {
    event.get("user").and_then(|v| v.as_str()).unwrap_or("")
}

/// 取文件扩展名（无则空串）。
fn ext_of(name: &str) -> &str {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}
