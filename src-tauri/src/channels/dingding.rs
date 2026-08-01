//! 钉钉 Channel：Stream 长连接收（卡片回调 + 用户文字/图片/文件）+ OpenAPI 发（Message、互动卡片）。
//!
//! 方案 A（默认）：提问以**互动卡片高级版**下发，用户在卡片内勾选预定义选项（多选）、可补充文字，
//! 点「提交」完成该题；作答期间在聊天里发的图片/文件会被累积进答案（纯文字忽略，请用卡片输入框）。
//! 卡片回调走 Stream（topic `/v1.0/card/instances/callback`），零公网。
//!
//! 方案 B（兜底）：当卡片投放失败时，回退为「纯文本 + 编号选项」——用户回复一条消息即完成该题
//! （回复编号映射选项，或直接输入文字，或发图片/文件）。
//!
//! 编排逻辑复用 `conversation::run_conversation`，本文件提供传输实现 `DingTalkSession`
//! （`MessagingChannel`）+ 薄外层 `DingTalkChannel`。

use super::conversation::{run_conversation, InboundReply, MessagingChannel, QuestionCtx};
use super::{Channel, ConversationOrigin, Interruption, Preemption, ResultSink};
use crate::config::DingTalkChannelConfig;
use crate::dingtalk::card;
use crate::dingtalk::client::DingTalkClient;
use crate::dingtalk::router::{DdInbound, DdRouter, RoutedDd};
use crate::dingtalk::textfile::{self, TextAction};
use crate::i18n::{self, Lang};
use crate::models::{ImageAttachment, MessagePrompt, QuestionAnswer};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 抢答轮询粒度：每隔此时长检查一次抢答信号。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Message（正文+附件）发完到发第一道题之间的等待时长。
/// 钉钉 oToMessages/batchSend 是异步投递（HTTP 200 仅代表受理，真正落到聊天有队列延迟），
/// 而卡片 createAndDeliver 投递更快，会插队先到，导致问题把内联内容顶上去、默认看不见。
/// 这里短暂等待让 batchSend 的正文/内联先落地，保证「先 message 后题目」的视觉顺序。
const MESSAGE_SETTLE_DELAY: Duration = Duration::from_millis(1000);

async fn send_inbound_reply(client: &DingTalkClient, reply: InboundReply, lang: Lang) {
    match reply {
        InboundReply::Text(text) => {
            let _ = client.send_oto_text(&text).await;
        }
        InboundReply::Help(view) => {
            let markdown = crate::dingtalk::help::render(&view, lang);
            if client
                .send_oto_markdown(&view.title, &markdown)
                .await
                .is_err()
            {
                let plain = crate::autochannel::render_help_plain(&view, lang);
                let _ = client.send_oto_text(&plain).await;
            }
        }
    }
}

/// 内置默认卡片模板 ID（设置项 `cardTemplateId` 留空时使用）。
/// D15 定稿模板：`options=[{id,md}]` + `single` + `allow_input` 变量条件渲染（单/多选、严格）。
pub const DEFAULT_CARD_TEMPLATE_ID: &str = "d5dc7ac5-1fca-443a-8230-d33ce63e837f.schema";

/// 取生效的卡片模板 ID：配置非空用配置，否则用内置默认。
pub fn effective_template_id(config: &DingTalkChannelConfig) -> &str {
    let t = config.card_template_id.trim();
    if t.is_empty() {
        DEFAULT_CARD_TEMPLATE_ID
    } else {
        t
    }
}

/// Router 归属：单进程自建一个仅挂本会话的 Router；Daemon 复用共享且常热的 Router。
#[derive(Clone)]
enum DdTransport {
    Own,
    Shared(Arc<DdRouter>),
}

/// 薄外层：接 Coordinator（并行抢答），把会话委托给 `run_conversation` + `DingTalkSession`。
pub struct DingTalkChannel {
    config: DingTalkChannelConfig,
    preempt: Arc<Preemption>,
    transport: DdTransport,
}

impl DingTalkChannel {
    /// 单进程外层：本会话自建并独占一条连接（每进程一个 Router、仅挂本会话）。
    pub fn new(config: DingTalkChannelConfig) -> Self {
        Self {
            config,
            preempt: Arc::new(Preemption::new()),
            transport: DdTransport::Own,
        }
    }

    /// Daemon 外层：复用共享且常热的 Router（跨请求复用，根治多连接抢消息）。
    pub fn shared(config: DingTalkChannelConfig, router: Arc<DdRouter>) -> Self {
        Self {
            config,
            preempt: Arc::new(Preemption::new()),
            transport: DdTransport::Shared(router),
        }
    }
}

impl Channel for DingTalkChannel {
    fn id(&self) -> &str {
        "dingding"
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
            // Own Router 直至会话结束；Shared 的 Router 由 Daemon 持有，不在此处保活。
            let (events, _keep): (RoutedDd, Option<Arc<DdRouter>>) = match transport {
                DdTransport::Own => {
                    match DdRouter::connect(config.client_id.trim(), config.client_secret.trim())
                        .await
                    {
                        Ok(router) => (router.register(), Some(router)),
                        Err(e) => {
                            eprintln!(
                                "{}{}",
                                i18n::warn_prefix(lang),
                                i18n::tr(lang, "channel.ddConfigInvalidSkip").replace("{e}", &e)
                            );
                            return;
                        }
                    }
                }
                DdTransport::Shared(router) => (router.register(), None),
            };
            let mut session = DingTalkSession::new(config, events);
            if let Err(e) = session.open().await {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.ddConfigInvalidSkip").replace("{e}", &e)
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

/// 传输实现：持有 OpenAPI client（发送）与 Router 事件源句柄（接收，长连接由 Router 独占）。
pub struct DingTalkSession {
    config: DingTalkChannelConfig,
    client: Option<DingTalkClient>,
    events: Option<RoutedDd>,
}

impl DingTalkSession {
    pub fn new(config: DingTalkChannelConfig, events: RoutedDd) -> Self {
        Self {
            config,
            client: None,
            events: Some(events),
        }
    }
}

#[async_trait::async_trait]
impl MessagingChannel for DingTalkSession {
    fn id(&self) -> &str {
        "dingding"
    }

    async fn open(&mut self) -> Result<(), String> {
        // 长连接由 Router 独占，这里只需 OpenAPI client（发送卡片/消息/置灰）。
        let client = DingTalkClient::new(&self.config).map_err(|e| e.to_string())?;
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
        let result = if is_markdown {
            let body = if message.text.trim().is_empty() {
                format!("**{}**", header)
            } else {
                format!("**{}**\n\n{}", header, message.text)
            };
            client.send_oto_markdown(header, &body).await
        } else {
            let body = if message.text.trim().is_empty() {
                header.to_string()
            } else {
                format!("{}\n\n{}", header, message.text)
            };
            client.send_oto_text(&body).await
        };
        if let Err(e) = result {
            eprintln!(
                "{}{}",
                i18n::warn_prefix(lang),
                i18n::tr(lang, "channel.ddMessageSendFailed").replace("{e}", &e.to_string())
            );
        }

        // 展示文件：图片→sampleImageMsg；文本类→短内联/长转 docx；其余→原样 sampleFile。
        let cfg = &self.config;
        for file in &message.files {
            // 非图片文本类：先按计划内联或转 docx；失败/不适用再原样发送。
            let handled = if file.is_image {
                false
            } else {
                match textfile::plan(cfg, &file.path, &file.name) {
                    TextAction::Inline { title, text } => {
                        client.send_oto_markdown(&title, &text).await.is_ok()
                    }
                    TextAction::Docx { file_name, bytes } => {
                        send_docx(client, &file_name, &bytes).await.is_ok()
                    }
                    TextAction::PassThrough => false,
                }
            };
            if handled {
                continue;
            }
            // 原样发送（图片，或文本处理不适用/失败的兜底）。
            if let Err(e) = send_attachment(client, &file.path, &file.name, file.is_image).await {
                eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.ddFileSendFailedLog")
                        .replace("{path}", &file.path)
                        .replace("{e}", &e.to_string())
                );
                let _ = client
                    .send_oto_text(
                        &i18n::tr(lang, "channel.fileSendFailed").replace("{name}", &file.name),
                    )
                    .await;
            }
        }

        // 等 batchSend 的正文/内联先落地，再让调用方发首题卡片（见常量说明）。
        tokio::time::sleep(MESSAGE_SETTLE_DELAY).await;
    }

    async fn ask_question(
        &mut self,
        ctx: &QuestionCtx<'_>,
        preempt: &Preemption,
    ) -> Option<QuestionAnswer> {
        // 题首：无则用兜底标题。
        let title = if ctx.header.is_empty() {
            i18n::tr(ctx.lang, "channel.ddTitleFallback")
        } else {
            ctx.header
        };
        let template_id = effective_template_id(&self.config).to_string();
        let out_track_id = uuid::Uuid::new_v4().to_string();
        // 选项以 `[{id,md}]` 下发（md 富文本含推荐绿色徽标）；模板回传选项 id（下标），
        // 提交时经 restore_selected 按下标还原原文。单选/严格由 single/allow_input 变量控制。
        let recommended_label = i18n::tr(ctx.lang, "channel.dingtalkRecommended");
        let param_map = card::build_card_param_map_with_todo(
            title,
            ctx.text,
            ctx.options,
            ctx.single,
            ctx.select_only,
            recommended_label,
            i18n::tr(ctx.lang, "whatsNext.todoPrefix"),
            i18n::tr(ctx.lang, "channel.dingtalkTodo"),
        );
        let private_param_map = card::build_card_private_map();

        let Self {
            client,
            events,
            config,
        } = self;
        let client = client.as_ref()?;
        let events = events.as_mut()?;
        let user_id = config.user_id.trim().to_string();

        // 先登记卡片精确路由 + 认领本 user 的聊天消息（投放前登记，规避「秒答」竞态）。
        events.set_active(Some(&out_track_id), &user_id);

        // 1. 投放互动卡片；失败 → 撤销卡片路由，回退纯文本编号方案。
        if let Err(e) = client
            .create_and_deliver_card(&out_track_id, &template_id, param_map, private_param_map)
            .await
        {
            eprintln!(
                "{}{}",
                i18n::warn_prefix(ctx.lang),
                i18n::tr(ctx.lang, "channel.ddCardDeliverFailed").replace("{e}", &e.to_string())
            );
            events.clear_active(Some(&out_track_id), "");
            return ask_question_text(client, events, &user_id, ctx, preempt).await;
        }

        // 2. 等卡片「提交」；作答期间收到的图片/文件**并发下载**（不阻塞收事件循环，保证提交一到
        //    就能被立刻处理、即时回 ACK，A 方案）。下载结果累积进共享集合，提交时再收尾。
        let images: Arc<Mutex<Vec<ImageAttachment>>> = Arc::new(Mutex::new(Vec::new()));
        let files: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut downloads: Vec<tauri::async_runtime::JoinHandle<()>> = Vec::new();
        while !preempt.is_cancelled() {
            let ev = match tokio::time::timeout(POLL_INTERVAL, events.recv()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => break,  // 连接彻底断开
                Err(_) => continue, // 超时：回到循环顶部重新检查 cancelled
            };
            match ev {
                DdInbound::Card { data, ack } => {
                    match card::parse_card_submit(&data) {
                        Some(s) if s.out_track_id == out_track_id && s.user_id == user_id => {
                            // 立刻回成功裁决：消除「请求失败」、置灰点击者（不在此等任何慢活）。
                            let _ = ack.send_and_wait(card::submit_ack_success()).await;
                            // 收尾并发下载（不在 3 秒关键路径），再经 OpenAPI 写公有终态文案。
                            for h in downloads {
                                let _ = h.await;
                            }
                            let submitted_text = i18n::tr(ctx.lang, "channel.ddSubmitted");
                            let _ = client
                                .update_card_private(
                                    &out_track_id,
                                    json!({ "submit_status": submitted_text }),
                                    json!({ "submitted": "true" }),
                                )
                                .await;
                            events.clear_active(Some(&out_track_id), &user_id);
                            let images = std::mem::take(&mut *images.lock().unwrap());
                            let files = std::mem::take(&mut *files.lock().unwrap());
                            return Some(QuestionAnswer {
                                selected_options: restore_selected(s.selected_indices, ctx.options),
                                user_input: s.user_input,
                                images,
                                files,
                                todo_ids: Vec::new(),
                                todo_selections: Vec::new(),
                            });
                        }
                        // 非本卡片（理论上不会路由到此）：回空包让 Router 别空等，继续。
                        _ => {
                            let _ = ack.send(json!({}));
                        }
                    }
                }
                DdInbound::Bot(data) => {
                    if bot_message_belongs(&data, &user_id) {
                        let lang = ctx.lang;
                        // 即时回执 / 引导（spec R2/R3）：spawn 不阻塞事件循环（保证卡片提交即时处理）。
                        // 斜线命令交 handle_inbound 独占响应，会话层不回引导（避免重复）。
                        if let Some(reply) = super::conversation::answer_inbound_reply(
                            ack_kind(&data),
                            crate::autochannel::AckMode::Card,
                            &bot_text(&data),
                            "dingding",
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
                            accumulate_attachment(&client, &data, &images, &files, lang).await;
                        }));
                    }
                }
            }
        }

        // Interrupted (preempted / cancelled) or disconnected: best-effort finalize the card.
        // Preempted → "Answered via X"; cancelled (with/without source) → "Cancelled [by X]";
        // disconnect with no reason → generic "Cancelled".
        let status = match preempt.reason() {
            Some(Interruption::AnsweredBy(w)) => {
                i18n::tr(ctx.lang, "channel.ddAnsweredVia").replace("{source}", &w)
            }
            Some(Interruption::Cancelled(src)) if !src.is_empty() => {
                i18n::tr(ctx.lang, "channel.ddCancelledBy").replace("{source}", &src)
            }
            _ => i18n::tr(ctx.lang, "channel.ddCancelled").to_string(),
        };
        let _ = client
            .update_card_private(
                &out_track_id,
                json!({ "submit_status": status }),
                json!({ "submitted": "true" }),
            )
            .await;
        events.clear_active(Some(&out_track_id), &user_id);
        None
    }

    async fn close(&mut self) {
        // 丢弃事件源句柄 → 从 Router 注销路由（Daemon 下及时清理，避免陈旧路由堆积）。
        self.events = None;
    }
}

/// 兜底：纯文本 + 编号选项问一题（卡片投放失败时使用）。用户回一条消息即完成该题。
async fn ask_question_text(
    client: &DingTalkClient,
    events: &mut RoutedDd,
    user_id: &str,
    ctx: &QuestionCtx<'_>,
    preempt: &Preemption,
) -> Option<QuestionAnswer> {
    let title = if ctx.header.is_empty() {
        i18n::tr(ctx.lang, "channel.ddTitleFallback")
    } else {
        ctx.header
    };
    // 编号回复按原文映射（编号清单展示用显示文本，见 build_question_text）。
    let option_texts: Vec<String> = ctx.options.iter().map(|o| o.text.clone()).collect();
    let body = build_question_text(ctx, ctx.is_markdown);
    let send_res = if ctx.is_markdown {
        client.send_oto_markdown(title, &body).await
    } else {
        client.send_oto_text(&body).await
    };
    if let Err(e) = send_res {
        eprintln!(
            "{}{}",
            i18n::warn_prefix(ctx.lang),
            i18n::tr(ctx.lang, "channel.ddQuestionSendFailed").replace("{e}", &e.to_string())
        );
    }

    // 文本兜底无卡片：认领本 user 的聊天消息即可（不登记卡片精确路由）。
    events.set_active(None, user_id);

    while !preempt.is_cancelled() {
        let ev = match tokio::time::timeout(POLL_INTERVAL, events.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => continue,
        };
        match ev {
            DdInbound::Bot(data) => {
                if !bot_message_belongs(&data, user_id) {
                    continue;
                }
                let kind = ack_kind(&data).or(Some(crate::autochannel::AckKind::Text));
                let text = bot_text(&data);
                if let Some(answer) =
                    message_to_answer(client, &data, &option_texts, ctx.select_only, ctx.single)
                        .await
                {
                    // 接受 → 确认（spec R2 文本兜底）。
                    if let Some(reply) = super::conversation::answer_inbound_reply(
                        kind,
                        crate::autochannel::AckMode::Fallback,
                        &text,
                        "dingding",
                        ctx.lang,
                    ) {
                        send_inbound_reply(client, reply, ctx.lang).await;
                    }
                    events.clear_active(None, user_id);
                    return Some(answer);
                } else {
                    // 未接受 → 引导（spec R3）；命令交 handle_inbound，不回引导。
                    if let Some(reply) = super::conversation::answer_inbound_reply(
                        None,
                        crate::autochannel::AckMode::Fallback,
                        &text,
                        "dingding",
                        ctx.lang,
                    ) {
                        send_inbound_reply(client, reply, ctx.lang).await;
                    }
                }
            }
            // 文本兜底路径未登记卡片路由，不会收到卡片提交回调；忽略（回空包以防万一）。
            DdInbound::Card { ack, .. } => {
                let _ = ack.send(json!({}));
            }
        }
    }
    events.clear_active(None, user_id);
    None
}

/// 累积聊天里收到的图片/文件（卡片作答期间）；纯文字等忽略。
/// 在并发下载任务中调用：下载完成后再锁住共享集合追加（锁期间不 await）。
async fn accumulate_attachment(
    client: &DingTalkClient,
    data: &Value,
    images: &Mutex<Vec<ImageAttachment>>,
    files: &Mutex<Vec<String>>,
    lang: Lang,
) {
    let msgtype = data.get("msgtype").and_then(|v| v.as_str()).unwrap_or("");
    match msgtype {
        "picture" => {
            let Some(code) = data
                .get("content")
                .and_then(|c| c.get("downloadCode"))
                .and_then(|v| v.as_str())
            else {
                return;
            };
            match download_image(client, code).await {
                Ok(img) => images.lock().unwrap().push(img),
                Err(e) => eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.ddImageDownloadFailed").replace("{e}", &e)
                ),
            }
        }
        "file" => {
            let content = data.get("content");
            let Some(code) = content
                .and_then(|c| c.get("downloadCode"))
                .and_then(|v| v.as_str())
            else {
                return;
            };
            let file_name = content
                .and_then(|c| c.get("fileName"))
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let ext = ext_of(file_name);
            match client.download_message_file_to(code, ext).await {
                Ok(path) => files.lock().unwrap().push(path),
                Err(e) => eprintln!(
                    "{}{}",
                    i18n::warn_prefix(lang),
                    i18n::tr(lang, "channel.ddFileDownloadFailed").replace("{e}", &e.to_string())
                ),
            }
        }
        _ => {} // 文字 / 贴纸等：卡片模式下忽略（请用卡片输入框）。
    }
}

/// 取钉钉 bot 文本消息内容（仅 `text` 类型有；其它类型返回空串）。用于判断是否斜线命令。
fn bot_text(data: &Value) -> String {
    data.get("text")
        .and_then(|t| t.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

/// 钉钉聊天消息 msgtype → 回执内容种类：图片/文件是可累积进答案的附件；其余（文字等）非附件→ None。
fn ack_kind(data: &Value) -> Option<crate::autochannel::AckKind> {
    use crate::autochannel::AckKind;
    match data.get("msgtype").and_then(|v| v.as_str()).unwrap_or("") {
        "picture" => Some(AckKind::Image),
        "file" => Some(AckKind::File),
        _ => None,
    }
}

/// 组装提问正文：头部（加粗）+ 正文 + 编号选项 + 作答提示。
fn build_question_text(ctx: &QuestionCtx<'_>, is_markdown: bool) -> String {
    let mut s = String::new();
    if !ctx.header.is_empty() {
        if is_markdown {
            s.push_str(&format!("**{}**", ctx.header));
        } else {
            s.push_str(ctx.header);
        }
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

/// 钉钉卡片模板回传的是选项【下标】(id)；按本题选项还原为原文（越界则忽略）。
fn restore_selected(indices: Vec<usize>, options: &[crate::models::OptionItem]) -> Vec<String> {
    indices
        .into_iter()
        .filter_map(|i| options.get(i).map(|o| o.text.clone()))
        .collect()
}

/// 把一条 bot 消息转成回答；非可作答类型返回 None（继续等待）。
/// 严格模式（`select_only`）忽略自由文字与附件，只接受编号选择；单选只取首个编号。
async fn message_to_answer(
    client: &DingTalkClient,
    data: &Value,
    options: &[String],
    select_only: bool,
    single: bool,
) -> Option<QuestionAnswer> {
    let msgtype = data.get("msgtype").and_then(|v| v.as_str()).unwrap_or("");
    match msgtype {
        "text" => {
            let content = data
                .get("text")
                .and_then(|t| t.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .trim();
            if content.is_empty() {
                return None;
            }
            let (mut selected, user_input) = parse_reply(content, options);
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
        "picture" | "file" if select_only => None,
        "picture" => {
            let code = data
                .get("content")
                .and_then(|c| c.get("downloadCode"))
                .and_then(|v| v.as_str())?;
            match download_image(client, code).await {
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
                        i18n::tr(lang, "channel.ddImageDownloadFailed").replace("{e}", &e)
                    );
                    None
                }
            }
        }
        "file" => {
            let content = data.get("content");
            let code = content
                .and_then(|c| c.get("downloadCode"))
                .and_then(|v| v.as_str())?;
            let file_name = content
                .and_then(|c| c.get("fileName"))
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let ext = ext_of(file_name);
            match client.download_message_file_to(code, ext).await {
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
                        i18n::tr(lang, "channel.ddFileDownloadFailed")
                            .replace("{e}", &e.to_string())
                    );
                    None
                }
            }
        }
        _ => None,
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

/// 判定 bot 消息是否来自目标用户。
fn bot_message_belongs(data: &Value, user_id: &str) -> bool {
    data.get("senderStaffId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        == user_id
}

/// 下载图片消息并转为 `ImageAttachment`（raw base64 + media_type）。
async fn download_image(client: &DingTalkClient, code: &str) -> Result<ImageAttachment, String> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let path = client
        .download_message_file_to(code, "png")
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
    client: &DingTalkClient,
    path: &str,
    name: &str,
    is_image: bool,
) -> Result<(), crate::dingtalk::DingTalkError> {
    if is_image {
        // 钉钉图片仅支持 jpg/png/gif/bmp(≤1MB)：webp 等转码、超限图缩放后再上传。
        use crate::dingtalk::image_convert::{self, Prepared};
        match image_convert::prepare(path, name) {
            Prepared::Original => {
                let media_id = client.upload_media(path, "image").await?;
                client.send_oto_image(&media_id).await
            }
            Prepared::Converted { bytes, file_name } => {
                send_image_bytes(client, &file_name, &bytes).await
            }
        }
    } else {
        let media_id = client.upload_media(path, "file").await?;
        let ext = ext_of(name);
        client.send_oto_file(&media_id, name, ext).await
    }
}

/// 把转码 / 缩放后的图片字节写临时文件后上传并发送（临时文件名保留正确扩展名供钉钉识别）。
async fn send_image_bytes(
    client: &DingTalkClient,
    file_name: &str,
    bytes: &[u8],
) -> Result<(), crate::dingtalk::DingTalkError> {
    use crate::dingtalk::DingTalkError;
    let tmp = std::env::temp_dir().join(format!("ha-{}-{}", uuid::Uuid::new_v4(), file_name));
    std::fs::write(&tmp, bytes)
        .map_err(|e| DingTalkError::Network(format!("write temp image failed: {}", e)))?;
    let result = async {
        let media_id = client
            .upload_media(tmp.to_str().unwrap_or_default(), "image")
            .await?;
        client.send_oto_image(&media_id).await
    }
    .await;
    let _ = std::fs::remove_file(&tmp);
    result
}

/// 把 docx 字节写临时文件后上传并以 sampleFile(fileType=docx) 发送。
async fn send_docx(
    client: &DingTalkClient,
    file_name: &str,
    bytes: &[u8],
) -> Result<(), crate::dingtalk::DingTalkError> {
    use crate::dingtalk::DingTalkError;
    let tmp = std::env::temp_dir().join(format!("ha-{}.docx", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)
        .map_err(|e| DingTalkError::Network(format!("write temp docx failed: {}", e)))?;
    let result = async {
        let media_id = client
            .upload_media(tmp.to_str().unwrap_or_default(), "file")
            .await?;
        client.send_oto_file(&media_id, file_name, "docx").await
    }
    .await;
    let _ = std::fs::remove_file(&tmp);
    result
}

/// 取文件扩展名（无则空串）。
fn ext_of(name: &str) -> &str {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::OptionItem;

    #[test]
    fn restore_selected_maps_indices_to_text() {
        let options = vec![
            OptionItem::new("继续", true),
            OptionItem::new("停止", false),
        ];
        assert_eq!(
            restore_selected(vec![0, 1], &options),
            vec!["继续".to_string(), "停止".to_string()]
        );
    }

    #[test]
    fn restore_selected_skips_out_of_range() {
        let options = vec![OptionItem::new("A", true)];
        assert_eq!(
            restore_selected(vec![0, 5], &options),
            vec!["A".to_string()]
        );
    }
}
