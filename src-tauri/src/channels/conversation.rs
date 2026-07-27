//! 会话型消息渠道的公共抽象：把「多问题逐条发送 / 单题特例 / 收集答案 / 投递」
//! 这套与传输无关的编排逻辑抽出来，各渠道（Telegram / 钉钉 / 未来飞书等）只需实现
//! `MessagingChannel` 的传输相关原语。

use super::{ConversationOrigin, Preemption, ResultSink};
use crate::i18n::{self, Lang};
use crate::models::{
    AskRequest, ChannelAction, ChannelResult, MessagePrompt, OptionItem, QuestionAnswer,
};
use std::sync::Arc;

/// 单道题的上下文（传给 `ask_question`）。
pub struct QuestionCtx<'a> {
    /// Fully assembled title with per-request source / Agent / project context.
    pub header: &'a str,
    pub text: &'a str,
    pub options: &'a [OptionItem],
    pub is_markdown: bool,
    /// 严格选择：禁用自由文本 / 附件，只能勾选预设项。
    pub select_only: bool,
    /// 单选：每题恰好一个选择（默认多选）。
    pub single: bool,
    /// 0 基序号与总题数（供渠道按需展示进度）。
    pub index: usize,
    pub total: usize,
    /// 当前界面语言（渠道据此本地化发给用户的提示/按钮）。
    pub lang: Lang,
}

/// 选项的【显示文本】：推荐选项加本地化「【👍推荐】 」前缀，普通选项即原文。
/// 仅用于各渠道展示；提交值（`selected_options`）必须用 `opt.text` 原文。
pub fn display_text(opt: &OptionItem, lang: Lang) -> String {
    if opt.recommended {
        format!(
            "{}{}",
            i18n::tr(lang, "channel.recommendedPrefix"),
            opt.text
        )
    } else {
        opt.text.clone()
    }
}

fn question_title(
    origin: &ConversationOrigin,
    lang: Lang,
    index: usize,
    total: usize,
    has_message: bool,
) -> String {
    if total == 1 && !has_message {
        return origin.source_title(lang, "channel.questionFrom");
    }
    let base = if total > 1 {
        i18n::tr(lang, "channel.questionIndexed")
            .replace("{i}", &(index + 1).to_string())
            .replace("{n}", &total.to_string())
    } else {
        i18n::tr(lang, "channel.questionTitle").to_string()
    };
    origin.question_title(base)
}

/// 当前自动激活开关（零钥匙串读配置）。供会话回执文案按开关拼装动态引导。
pub fn auto_activation() -> bool {
    crate::config::AppConfig::load_without_secrets()
        .channels
        .auto_activation
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundReply {
    Text(String),
    Help(crate::autochannel::HelpView),
}

/// 作答期间对一条聊天消息的即时回复文案（spec R2/R3）：
/// - `Some(kind)`：该内容被接受进答案 → 按种类（文字/图片/文件）与模式（卡片/文本兜底）回**确认**；
/// - `None` 且该消息**不是**命令：未被当作答案（卡片模式的纯文字、未接受的兜底消息等）→ 回**动态引导**；
/// - `None` 且该消息**是**命令（`/` 一律、`!` 已知命令，见 `autochannel::classify`）：返回 `None`
///   （**不回**）——命令统一交由观察者 `handle_inbound` 独占响应（已注册回命令输出、未注册回 help），
///   避免作答期收到 `/status` 等命令时出现「引导 + 命令输出」两条重复回复。
///
/// 引导带 `has_active_question=true`（会话进行中），并按当前自动激活开关裁剪命令/提示。
/// `channel_id`：所在渠道（"telegram"/"dingding"/"feishu"/"slack"），决定引导是否列 watch
/// 命令（`watch::channel_supported`）及命令展示前缀（Slack `!`，其余 `/`）。
pub fn answer_inbound_reply(
    kind: Option<crate::autochannel::AckKind>,
    mode: crate::autochannel::AckMode,
    text: &str,
    channel_id: &str,
    lang: Lang,
) -> Option<InboundReply> {
    match kind {
        Some(k) => Some(InboundReply::Text(crate::autochannel::answer_ack_text(
            k, mode, lang,
        ))),
        None => {
            if crate::autochannel::classify(text) != crate::autochannel::Parsed::Text {
                None
            } else if mode == crate::autochannel::AckMode::Card && auto_activation() {
                // Card mode + auto-activation on: plain text is also handled by
                // handle_inbound (channel switch or silent return). Suppress
                // session-side guidance to avoid duplicate messages.
                None
            } else {
                Some(InboundReply::Help(crate::autochannel::help_view(
                    auto_activation(),
                    true,
                    crate::watch::channel_supported(channel_id),
                    crate::autochannel::cmd_prefix(channel_id),
                    lang,
                )))
            }
        }
    }
}

/// 会话型消息渠道的传输原语（与编排逻辑解耦）。
#[async_trait::async_trait]
pub trait MessagingChannel: Send {
    fn id(&self) -> &str;
    /// 建连 / 校验：成功才进入问答；失败返回中文错误（由调用方警告并跳过）。
    async fn open(&mut self) -> Result<(), String>;
    /// Send the shared Message with its assembled origin title, body, and display attachments.
    async fn send_message_prompt(
        &mut self,
        message: &MessagePrompt,
        is_markdown: bool,
        header: &str,
        lang: Lang,
    );
    /// 发送一道题并等到「用户完成作答」；被抢答（`preempt`）时收尾并返回 `None`。
    async fn ask_question(
        &mut self,
        ctx: &QuestionCtx<'_>,
        preempt: &Preemption,
    ) -> Option<QuestionAnswer>;
    /// 收尾 / 断连（完成或被抢答后调用）。
    async fn close(&mut self);
}

/// 公共驱动：单/多题统一编排，全部完成后投递结果；被抢答则中止不投递。
///
/// Rules shared by all IM transports:
/// - A single question without a Message uses `Question from {source} · {agent} · {project}`.
/// - Otherwise the shared Message is sent first, then every question carries the same origin.
pub async fn run_conversation(
    channel: &mut dyn MessagingChannel,
    request: &AskRequest,
    origin: &ConversationOrigin,
    preempt: Arc<Preemption>,
    sink: ResultSink,
) {
    let n = request.questions.len();
    let has_message = !request.message.text.trim().is_empty() || !request.message.files.is_empty();
    let lang = Lang::current();
    let mut answers: Vec<QuestionAnswer> = Vec::with_capacity(n);

    if n == 1 && !has_message {
        let q = &request.questions[0];
        let header = question_title(origin, lang, 0, n, has_message);
        let ctx = QuestionCtx {
            header: &header,
            text: &q.message,
            options: &q.predefined_options,
            is_markdown: request.is_markdown,
            select_only: request.select_only,
            single: request.single,
            index: 0,
            total: 1,
            lang,
        };
        match channel.ask_question(&ctx, &preempt).await {
            Some(answer) => answers.push(answer),
            None => {
                channel.close().await;
                sink.notify_finalized();
                return;
            }
        }
    } else {
        let message_header = origin.source_title(lang, "channel.messageFrom");
        channel
            .send_message_prompt(&request.message, request.is_markdown, &message_header, lang)
            .await;
        for (index, question) in request.questions.iter().enumerate() {
            let header = question_title(origin, lang, index, n, has_message);
            let ctx = QuestionCtx {
                header: &header,
                text: &question.message,
                options: &question.predefined_options,
                is_markdown: request.is_markdown,
                select_only: request.select_only,
                single: request.single,
                index,
                total: n,
                lang,
            };
            match channel.ask_question(&ctx, &preempt).await {
                Some(answer) => answers.push(answer),
                None => {
                    channel.close().await;
                    sink.notify_finalized();
                    return;
                }
            }
        }
    }

    let source_channel_id = channel.id().to_string();
    channel.close().await;
    sink.submit(ChannelResult {
        action: ChannelAction::Send,
        answers,
        source_channel_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_titles_include_agent_and_project_without_duplicates() {
        let origin = ConversationOrigin::new("Codex", Some("codex"), "/tmp/HumanInLoop");
        assert_eq!(
            origin.source_title(Lang::En, "channel.messageFrom"),
            "Message from Codex · HumanInLoop"
        );
        assert_eq!(
            origin.question_title("Question 1/2".into()),
            "Question 1/2 · Codex · HumanInLoop"
        );
        assert_eq!(
            question_title(&origin, Lang::En, 0, 1, false),
            "Question from Codex · HumanInLoop"
        );
        assert_eq!(
            question_title(&origin, Lang::En, 0, 1, true),
            "Question · Codex · HumanInLoop"
        );
        assert_eq!(
            question_title(&origin, Lang::En, 1, 2, true),
            "Question 2/2 · Codex · HumanInLoop"
        );
    }

    #[test]
    fn custom_source_keeps_distinct_agent_and_missing_fields_degrade_cleanly() {
        let custom = ConversationOrigin::new("MyAgent", Some("claude"), "/tmp/api");
        assert_eq!(
            custom.source_title(Lang::En, "channel.questionFrom"),
            "Question from MyAgent · Claude Code · api"
        );

        let fallback = ConversationOrigin::new("", None, "/tmp/api");
        assert_eq!(
            fallback.source_title(Lang::Zh, "channel.messageFrom"),
            "Message · api"
        );
        assert_eq!(fallback.question_title("提问".into()), "提问 · api");
        assert_eq!(
            question_title(&fallback, Lang::En, 0, 1, false),
            "Question · api"
        );
    }

    #[test]
    fn resolved_mcp_agent_replaces_generic_loop_source() {
        let origin = ConversationOrigin::new("the Loop", Some("cursor"), "/repo/web");
        assert_eq!(
            origin.source_title(Lang::En, "channel.messageFrom"),
            "Message from Cursor · web"
        );
    }

    #[test]
    fn inbound_reply_distinguishes_ack_help_and_command_suppression() {
        let ack = answer_inbound_reply(
            Some(crate::autochannel::AckKind::Text),
            crate::autochannel::AckMode::Fallback,
            "answer",
            "slack",
            Lang::En,
        );
        assert!(matches!(ack, Some(InboundReply::Text(_))));

        let help = answer_inbound_reply(
            None,
            crate::autochannel::AckMode::Fallback,
            "ordinary text",
            "slack",
            Lang::En,
        );
        let Some(InboundReply::Help(view)) = help else {
            panic!("unaccepted text should produce structured help");
        };
        assert!(view.sections[0].commands[0].syntax.starts_with("!status"));

        assert_eq!(
            answer_inbound_reply(
                None,
                crate::autochannel::AckMode::Fallback,
                "!help",
                "slack",
                Lang::En,
            ),
            None
        );
    }
}
