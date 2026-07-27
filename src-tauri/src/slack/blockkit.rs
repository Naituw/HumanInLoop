//! Slack Block Kit：组装消息内表单提问卡片（复选框 + 多行输入框 + 提交按钮）+ 解析提交回调。
//!
//! 设计（见 `docs/plans/slack-channel.md`）：
//! - 提问卡片为一组 blocks：标题 section + 正文 section + `input`(checkboxes) + `input`(plain_text_input)
//!   + `actions`(提交按钮)。Slack 消息内 `input` 块支持复选框与多行文本输入。
//! - 用户点「提交」→ 一次 `block_actions` 回调，`payload.state.values` 汇总整条消息所有 input 取值。
//! - 选项 ↔ value 映射 `opt_{i}`，便于回调里还原勾选了哪些选项（规避超长/重复选项文案）。
//! - 复选框单元素最多 10 项，超出则拆分为多个 `input` 块（提交时各块 state.values 合并）。
//! - 终态为**静态** blocks（无交互控件，回显已选项与补充文字 + 状态行），由 `chat.update` 置入。

use super::markdown;
use crate::autochannel::{self, HelpQuestionState, HelpView};
use crate::i18n::Lang;
use crate::models::OptionItem;
use serde_json::{json, Value};

/// 选项 value 前缀（`opt_0` / `opt_1` ...，全局连续下标）。
const OPT_VALUE_PREFIX: &str = "opt_";
/// 文本输入框 action_id。
const INPUT_ACTION: &str = "user_input";
/// 提交按钮 action_id。
const SUBMIT_ACTION: &str = "submit";
const SINGLE_SELECT_PREFIX: &str = "single_select_";
/// 复选框单元素选项上限（Slack 限制）。
const CHECKBOXES_MAX: usize = 10;
/// section 文本安全上限（Slack 约 3000）。
const SECTION_MAX: usize = 2900;

/// 一次卡片「提交」回调的解析结果。
pub struct CardSubmit {
    pub user_id: String,
    /// 卡片所在消息 ts（`container.message_ts`），用于匹配当前题卡片。
    pub message_ts: String,
    /// 卡片所在频道 id。
    pub channel_id: String,
    /// 勾选的预定义选项（选项文本，已按下标还原）。
    pub selected_options: Vec<String>,
    /// 补充文字输入（空则 None）。
    pub user_input: Option<String>,
}

pub struct CardSelect {
    pub user_id: String,
    pub message_ts: String,
    pub channel_id: String,
    pub index: usize,
    pub user_input: Option<String>,
}

/// 组装提问卡片（blocks 数组）。
/// `title` 题首（空则省略）；`text` 正文（空则省略）；`options` 预定义选项（空则无选项控件）；
/// `is_markdown` 决定正文用 mrkdwn 还是 plain_text；其余为各处文案。
/// `single`→选项控件用原生 `radio_buttons`（单选），否则 `checkboxes`（多选）；
/// `select_only`→去掉补充文本输入块（严格选择，只能勾选）。
/// `recommended_label` 为推荐选项的原生 `description` 文案（如「👍 推荐」），并把选项文本加粗；
/// `value=opt_{i}` 按下标还原原文，显示不影响提交值。
/// `nonce` 为每张卡片唯一串，拼入各 `input` 块 block_id：Slack 客户端按 block_id 缓存输入草稿，
/// 唯一化可避免新卡片回填上一题的输入/勾选（见 `docs/plans/slack-channel.md` 反馈意见）。
#[allow(clippy::too_many_arguments)]
pub fn build_question_card(
    title: &str,
    text: &str,
    options: &[OptionItem],
    is_markdown: bool,
    single: bool,
    select_only: bool,
    options_label: &str,
    input_label: &str,
    input_placeholder: &str,
    submit_label: &str,
    recommended_label: &str,
    nonce: &str,
) -> Value {
    build_question_card_with_state(
        title,
        text,
        options,
        is_markdown,
        single,
        select_only,
        options_label,
        input_label,
        input_placeholder,
        submit_label,
        recommended_label,
        nonce,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_question_card_with_state(
    title: &str,
    text: &str,
    options: &[OptionItem],
    is_markdown: bool,
    single: bool,
    select_only: bool,
    options_label: &str,
    input_label: &str,
    input_placeholder: &str,
    submit_label: &str,
    recommended_label: &str,
    nonce: &str,
    selected_single: Option<usize>,
    user_input_draft: Option<&str>,
) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    if !title.trim().is_empty() {
        blocks.push(title_block(title));
    }
    if !text.trim().is_empty() {
        blocks.push(body_section(text, is_markdown));
    }

    // Single-select uses server-side state across all options. Full labels stay in section text;
    // buttons carry only bounded numeric controls, avoiding Slack's 10-option/75-char limits.
    if single {
        for (index, option) in options.iter().enumerate() {
            let selected = selected_single == Some(index);
            let recommended = if option.recommended {
                format!("\n_{recommended_label}_")
            } else {
                String::new()
            };
            blocks.push(json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": truncate(&format!(
                        "{} {}{}",
                        if selected { "●" } else { "○" },
                        markdown::escape(&option.text),
                        recommended,
                    )),
                },
                "accessory": {
                    "type": "button",
                    "action_id": format!("{SINGLE_SELECT_PREFIX}{index}"),
                    "value": index.to_string(),
                    "text": { "type": "plain_text", "text": if selected { "✓".to_string() } else { (index + 1).to_string() } },
                },
            }));
        }
    } else if !options.is_empty() {
        for (k, chunk) in options.chunks(CHECKBOXES_MAX).enumerate() {
            let base = k * CHECKBOXES_MAX;
            let opts: Vec<Value> = chunk
                .iter()
                .enumerate()
                .map(|(j, opt)| {
                    // 推荐项：文本加粗 + 原生 description「👍 推荐」（mrkdwn，控件内展示）。
                    let text_val = if opt.recommended {
                        format!("*{}*", markdown::escape(&opt.text))
                    } else {
                        markdown::escape(&opt.text)
                    };
                    let mut option = json!({
                        "text": { "type": "mrkdwn", "text": text_val },
                        "value": format!("{}{}", OPT_VALUE_PREFIX, base + j),
                    });
                    if opt.recommended {
                        option["description"] =
                            json!({ "type": "mrkdwn", "text": recommended_label });
                    }
                    option
                })
                .collect();
            blocks.push(json!({
                "type": "input",
                "block_id": format!("opts_{}_{}", k, nonce),
                "optional": true,
                "label": { "type": "plain_text", "text": options_label, "emoji": true },
                "element": {
                    "type": "checkboxes",
                    "action_id": format!("options_{}", k),
                    "options": opts,
                },
            }));
        }
    }

    // 多行文本输入框（不 dispatch_action，仅在提交时随 state.values 一并回传）；严格模式去掉。
    if !select_only {
        let mut input = json!({
            "type": "input",
            "block_id": format!("userinput_{}", nonce),
            "optional": true,
            "label": { "type": "plain_text", "text": input_label, "emoji": true },
            "element": {
                "type": "plain_text_input",
                "action_id": INPUT_ACTION,
                "multiline": true,
                "placeholder": { "type": "plain_text", "text": input_placeholder, "emoji": true },
            },
        });
        if let Some(draft) = user_input_draft.filter(|value| !value.is_empty()) {
            input["element"]["initial_value"] = json!(draft);
        }
        blocks.push(input);
    }

    // 提交按钮。
    blocks.push(json!({
        "type": "actions",
        "block_id": "actions",
        "elements": [ {
            "type": "button",
            "action_id": SUBMIT_ACTION,
            "text": { "type": "plain_text", "text": submit_label, "emoji": true },
            "style": "primary",
            "value": "submit",
        } ],
    }));

    Value::Array(blocks)
}

/// `/msg` 一次性输入卡。复用提问卡的原生多行输入和提交动作，不创建额外交互协议。
pub fn build_msg_compose_card(
    view: &crate::msg_card::MsgComposeView,
    nonce: &str,
    user_input_draft: Option<&str>,
) -> Value {
    build_question_card_with_state(
        &view.title,
        &view.plain_body(),
        &[],
        false,
        false,
        false,
        "",
        &view.input_label,
        &view.input_placeholder,
        &view.send_label,
        "",
        nonce,
        None,
        user_input_draft,
    )
}

/// 组装「消息」blocks：大号 header（`header` 已含调用方图标前缀，如 ✉️）+ 可选正文 section。
/// `is_markdown` 决定正文用 mrkdwn 还是 plain_text。
pub fn build_message_blocks(header: &str, body: &str, is_markdown: bool) -> Value {
    let mut blocks: Vec<Value> = vec![header_block(header)];
    if !body.trim().is_empty() {
        blocks.push(body_section(body, is_markdown));
    }
    Value::Array(blocks)
}

/// Build grouped Block Kit help. The top-level fallback text is supplied by the caller.
pub fn build_help_blocks(view: &HelpView, lang: Lang) -> Value {
    let mut blocks = vec![
        header_block(&view.title),
        mrkdwn_section(&markdown::escape(&view.intro)),
    ];
    for section in &view.sections {
        let mut text = format!("*{}*", markdown::escape(&section.title));
        for command in &section.commands {
            text.push_str("\n• `");
            text.push_str(&markdown::escape(&command.syntax));
            text.push_str("` — ");
            text.push_str(&markdown::escape(&command.description));
            if let Some(phrase) = &command.phrase {
                text.push_str("  _· ");
                text.push_str(&markdown::escape(&autochannel::help_phrase_hint(
                    phrase, lang,
                )));
                text.push('_');
            }
        }
        blocks.push(mrkdwn_section(&text));
    }
    blocks.push(json!({ "type": "divider" }));

    let state = match &view.question_state {
        HelpQuestionState::Active { instruction } => instruction,
        HelpQuestionState::None { message } => message,
    };
    let mut footer = format!("*{}*", markdown::escape(state));
    if let Some(hint) = &view.switch_hint {
        footer.push('\n');
        footer.push_str(&markdown::escape(hint));
    }
    blocks.push(json!({
        "type": "context",
        "elements": [ { "type": "mrkdwn", "text": footer } ],
    }));
    Value::Array(blocks)
}

#[cfg(test)]
mod help_tests {
    use super::*;

    #[test]
    fn help_blocks_use_sections_bullets_code_and_emphasis() {
        let view = autochannel::help_view(true, false, true, "!", Lang::Zh);
        let blocks = build_help_blocks(&view, Lang::Zh);
        let array = blocks.as_array().unwrap();
        assert_eq!(array[0]["type"], "header");
        assert!(array.iter().any(|block| block["type"] == "divider"));
        let serialized = blocks.to_string();
        assert!(serialized.contains("*Agent 管理*"));
        assert!(serialized.contains("• `!status [编号]`"));
        assert!(serialized.contains("_· 直接说「状态」_"));
        assert!(serialized.contains("• `!msg-clear &lt;编号&gt;`"));
    }
}

/// 终态卡片入参。
pub struct Finalized<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub is_markdown: bool,
    /// 用户已选选项（被抢答收尾时为空）。
    pub selected: &'a [String],
    /// 补充文字回显（无则 None）。
    pub user_input: Option<&'a str>,
    /// 状态行文案（「已提交」/「已在 X 回答」/「已取消」等）。
    pub status: &'a str,
}

/// 组装静态终态卡片（无交互控件）：标题 + 正文 + 已选项（✓）+ 补充文字（💬）+ 状态行（context）。
pub fn build_finalized_card(p: &Finalized) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    if !p.title.trim().is_empty() {
        blocks.push(title_block(p.title));
    }
    if !p.text.trim().is_empty() {
        blocks.push(body_section(p.text, p.is_markdown));
    }
    if !p.selected.is_empty() {
        let lines: Vec<String> = p
            .selected
            .iter()
            .map(|o| format!("✓ {}", markdown::escape(o)))
            .collect();
        blocks.push(mrkdwn_section(&lines.join("\n")));
    }
    if let Some(note) = p.user_input.filter(|s| !s.trim().is_empty()) {
        blocks.push(mrkdwn_section(&format!("💬 {}", markdown::escape(note))));
    }
    if !p.status.trim().is_empty() {
        blocks.push(json!({
            "type": "context",
            "elements": [ { "type": "mrkdwn", "text": markdown::escape(p.status) } ],
        }));
    }
    Value::Array(blocks)
}

/// 把一次 `block_actions` 回调解析为「提交」结果；非提交按钮 / 缺字段返回 None。
/// `options` 用于把 `opt_{i}` 还原为选项文本。
pub fn parse_submit(payload: &Value, options: &[OptionItem]) -> Option<CardSubmit> {
    parse_submit_with_single(payload, options, None)
}

pub fn parse_submit_with_single(
    payload: &Value,
    options: &[OptionItem],
    selected_single: Option<usize>,
) -> Option<CardSubmit> {
    // 必须是「提交」按钮触发。
    let is_submit = payload
        .get("actions")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .any(|a| a.get("action_id").and_then(|v| v.as_str()) == Some(SUBMIT_ACTION))
        })
        .unwrap_or(false);
    if !is_submit {
        return None;
    }

    let user_id = payload
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let message_ts = payload
        .get("container")
        .and_then(|c| c.get("message_ts"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("message")
                .and_then(|m| m.get("ts"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    let channel_id = payload
        .get("channel")
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 遍历 state.values 汇总各 input 取值（复选框 selected_options + 文本输入 value）。
    let mut chosen = vec![false; options.len()];
    let mut user_input: Option<String> = None;
    if let Some(values) = payload
        .get("state")
        .and_then(|s| s.get("values"))
        .and_then(|v| v.as_object())
    {
        for actions in values.values() {
            let Some(actions) = actions.as_object() else {
                continue;
            };
            for (action_id, el) in actions {
                // 多选 checkboxes → selected_options（数组）。
                if let Some(sel) = el.get("selected_options").and_then(|s| s.as_array()) {
                    for o in sel {
                        mark_chosen(o, &mut chosen);
                    }
                }
                // 单选 radio_buttons → selected_option（单对象，可能为 null）。
                if let Some(o) = el.get("selected_option") {
                    if o.is_object() {
                        mark_chosen(o, &mut chosen);
                    }
                }
                let is_text = action_id == INPUT_ACTION
                    || el.get("type").and_then(|t| t.as_str()) == Some("plain_text_input");
                if is_text {
                    if let Some(txt) = el.get("value").and_then(|v| v.as_str()) {
                        let t = txt.trim();
                        if !t.is_empty() {
                            user_input = Some(t.to_string());
                        }
                    }
                }
            }
        }
    }
    if let Some(index) = selected_single.filter(|index| *index < chosen.len()) {
        chosen.fill(false);
        chosen[index] = true;
    }
    let selected_options = options
        .iter()
        .enumerate()
        .filter(|(i, _)| chosen[*i])
        .map(|(_, o)| o.text.clone())
        .collect();

    Some(CardSubmit {
        user_id,
        message_ts,
        channel_id,
        selected_options,
        user_input,
    })
}

pub fn parse_single_select(payload: &Value) -> Option<CardSelect> {
    let action = payload.get("actions")?.as_array()?.first()?;
    let index = action
        .get("action_id")?
        .as_str()?
        .strip_prefix(SINGLE_SELECT_PREFIX)?
        .parse::<usize>()
        .ok()?;
    let user_id = payload
        .get("user")
        .and_then(|user| user.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let message_ts = payload
        .get("container")
        .and_then(|container| container.get("message_ts"))
        .and_then(Value::as_str)
        .or_else(|| payload.get("message")?.get("ts")?.as_str())
        .unwrap_or("")
        .to_string();
    let channel_id = payload
        .get("channel")
        .and_then(|channel| channel.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let user_input = state_user_input(payload);
    Some(CardSelect {
        user_id,
        message_ts,
        channel_id,
        index,
        user_input,
    })
}

fn state_user_input(payload: &Value) -> Option<String> {
    payload
        .get("state")?
        .get("values")?
        .as_object()?
        .values()
        .filter_map(Value::as_object)
        .flat_map(|actions| actions.iter())
        .find_map(|(action_id, value)| {
            (action_id == INPUT_ACTION
                || value.get("type").and_then(Value::as_str) == Some("plain_text_input"))
            .then(|| value.get("value").and_then(Value::as_str))
            .flatten()
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 把一个 `{ value: "opt_{i}" }` 选项标记为已选（下标越界则忽略）。
fn mark_chosen(o: &Value, chosen: &mut [bool]) {
    if let Some(val) = o.get("value").and_then(|v| v.as_str()) {
        if let Some(idx) = val.strip_prefix(OPT_VALUE_PREFIX) {
            if let Ok(i) = idx.parse::<usize>() {
                if i < chosen.len() {
                    chosen[i] = true;
                }
            }
        }
    }
}

/// 大号 `header` 块（更醒目）。`text` 应已含调用方需要的图标前缀（如 ❓ / ✉️）。
/// `header` 限 plain_text、≤150 字符；超长回退普通加粗 mrkdwn section（避免 Slack 报错）。
fn header_block(text: &str) -> Value {
    if text.chars().count() <= 150 {
        json!({
            "type": "header",
            "text": { "type": "plain_text", "text": text, "emoji": true },
        })
    } else {
        mrkdwn_section(&format!("*{}*", markdown::escape(text)))
    }
}

/// 题首：大号标题 + ❓ 图标突出「这是一个问题」。
pub(crate) fn title_block(title: &str) -> Value {
    header_block(&format!("❓ {}", title))
}

/// 正文 section：markdown → mrkdwn；纯文本 → plain_text。
fn body_section(text: &str, is_markdown: bool) -> Value {
    if is_markdown {
        mrkdwn_section(&markdown::to_mrkdwn(text))
    } else {
        json!({
            "type": "section",
            "text": { "type": "plain_text", "text": truncate(text), "emoji": true },
        })
    }
}

pub(crate) fn mrkdwn_section(text: &str) -> Value {
    json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": truncate(text) },
    })
}

/// 截断到 section 安全上限（按字符）。
fn truncate(s: &str) -> String {
    if s.chars().count() <= SECTION_MAX {
        return s.to_string();
    }
    s.chars().take(SECTION_MAX).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(v: &Value) -> &Vec<Value> {
        v.as_array().unwrap()
    }

    fn plain(items: &[&str]) -> Vec<OptionItem> {
        items.iter().map(|s| OptionItem::new(*s, false)).collect()
    }

    #[test]
    fn build_card_has_form_and_options() {
        let card = build_question_card(
            "Question 1/2",
            "要继续吗？",
            &plain(&["继续", "停止"]),
            true,
            false,
            false,
            "Options",
            "Note",
            "Add a note",
            "Submit",
            "👍 推荐",
            "n1",
        );
        let bs = blocks(&card);
        // 标题 + 正文 + 1 个复选框 input + 文本 input + actions。
        assert!(bs.iter().any(|b| b["type"] == "section"));
        let checkboxes = bs
            .iter()
            .find(|b| b["element"]["type"] == "checkboxes")
            .unwrap();
        let opts = checkboxes["element"]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "opt_0");
        assert_eq!(opts[1]["value"], "opt_1");
        assert!(bs
            .iter()
            .any(|b| b["element"]["type"] == "plain_text_input"));
        let actions = bs.iter().find(|b| b["type"] == "actions").unwrap();
        assert_eq!(actions["elements"][0]["action_id"], "submit");
    }

    #[test]
    fn single_uses_one_server_side_selection_across_many_long_options() {
        let options: Vec<OptionItem> = (0..11)
            .map(|index| OptionItem::new(format!("{index}: {}", "x".repeat(80)), false))
            .collect();
        let card = build_question_card(
            "t", "x", &options, false, true, false, "L", "N", "p", "S", "R", "n",
        );
        let bs = blocks(&card);
        let selectors: Vec<&Value> = bs
            .iter()
            .filter(|block| {
                block["accessory"]["action_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with(SINGLE_SELECT_PREFIX))
            })
            .collect();
        assert_eq!(selectors.len(), 11);
        assert!(
            selectors[0]["text"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                > 75
        );
        assert!(
            selectors[0]["accessory"]["text"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                < 3
        );
        assert!(!bs.iter().any(|b| b["element"]["type"] == "radio_buttons"));
        assert!(!bs.iter().any(|b| b["element"]["type"] == "checkboxes"));
    }

    #[test]
    fn parses_server_side_single_selection_and_draft() {
        let payload = json!({
            "user": { "id": "U1" },
            "channel": { "id": "D1" },
            "container": { "message_ts": "1.2" },
            "actions": [{ "action_id": "single_select_10", "value": "10" }],
            "state": { "values": {
                "draft": { "user_input": { "type": "plain_text_input", "value": " note " } }
            } }
        });
        let selection = parse_single_select(&payload).unwrap();
        assert_eq!(selection.index, 10);
        assert_eq!(selection.user_input.as_deref(), Some("note"));
    }

    #[test]
    fn select_only_omits_text_input() {
        let card = build_question_card(
            "t",
            "x",
            &plain(&["a", "b"]),
            false,
            false,
            true,
            "L",
            "N",
            "p",
            "S",
            "R",
            "n",
        );
        let bs = blocks(&card);
        assert!(!bs
            .iter()
            .any(|b| b["element"]["type"] == "plain_text_input"));
        assert!(bs.iter().any(|b| b["element"]["type"] == "checkboxes"));
    }

    #[test]
    fn recommended_option_bolds_text_and_adds_description() {
        let opts = vec![
            OptionItem::new("继续", true),
            OptionItem::new("停止", false),
        ];
        let card = build_question_card(
            "t",
            "x",
            &opts,
            false,
            false,
            false,
            "L",
            "N",
            "p",
            "S",
            "👍 推荐",
            "n",
        );
        let checkboxes = blocks(&card)
            .iter()
            .find(|b| b["element"]["type"] == "checkboxes")
            .unwrap();
        let items = checkboxes["element"]["options"].as_array().unwrap();
        assert_eq!(items[0]["text"]["type"], "mrkdwn");
        assert_eq!(items[0]["text"]["text"], "*继续*");
        assert_eq!(items[0]["description"]["text"], "👍 推荐");
        assert_eq!(items[0]["value"], "opt_0");
        // 普通项不加粗、无 description。
        assert_eq!(items[1]["text"]["text"], "停止");
        assert!(items[1].get("description").is_none());
    }

    #[test]
    fn title_rendered_as_header_with_question_icon() {
        let card = build_question_card(
            "继续吗",
            "正文",
            &[],
            false,
            false,
            false,
            "L",
            "N",
            "p",
            "S",
            "R",
            "n",
        );
        let header = blocks(&card)
            .iter()
            .find(|b| b["type"] == "header")
            .unwrap();
        assert_eq!(header["text"]["type"], "plain_text");
        assert!(header["text"]["text"].as_str().unwrap().starts_with("❓ "));
        // 超长标题回退为普通加粗 section（不致 Slack 报错）。
        let long: String = "题".repeat(200);
        let card2 = build_question_card(
            &long,
            "",
            &[],
            false,
            false,
            false,
            "L",
            "N",
            "p",
            "S",
            "R",
            "n",
        );
        assert!(!blocks(&card2).iter().any(|b| b["type"] == "header"));
    }

    #[test]
    fn input_block_ids_carry_nonce() {
        // 唯一 nonce 应拼入各 input 块 block_id（清除 Slack 跨卡片输入缓存）。
        let a = build_question_card(
            "t",
            "x",
            &plain(&["o"]),
            false,
            false,
            false,
            "L",
            "N",
            "p",
            "S",
            "R",
            "AAA",
        );
        let b = build_question_card(
            "t",
            "x",
            &plain(&["o"]),
            false,
            false,
            false,
            "L",
            "N",
            "p",
            "S",
            "R",
            "BBB",
        );
        let id_of = |card: &Value, typ: &str| -> String {
            blocks(card)
                .iter()
                .find(|bl| bl["element"]["type"] == typ)
                .and_then(|bl| bl["block_id"].as_str())
                .unwrap_or("")
                .to_string()
        };
        assert!(id_of(&a, "plain_text_input").ends_with("AAA"));
        assert!(id_of(&a, "checkboxes").ends_with("AAA"));
        // 不同 nonce → 不同 block_id（避免回填）。
        assert_ne!(id_of(&a, "plain_text_input"), id_of(&b, "plain_text_input"));
        assert_ne!(id_of(&a, "checkboxes"), id_of(&b, "checkboxes"));
    }

    #[test]
    fn options_split_into_chunks_of_ten() {
        let options: Vec<OptionItem> = (0..23)
            .map(|i| OptionItem::new(format!("o{}", i), false))
            .collect();
        let card = build_question_card(
            "t", "x", &options, false, false, false, "Options", "Note", "ph", "Submit", "R", "n",
        );
        let bs = blocks(&card);
        let checkbox_blocks: Vec<&Value> = bs
            .iter()
            .filter(|b| b["element"]["type"] == "checkboxes")
            .collect();
        assert_eq!(checkbox_blocks.len(), 3); // 10 + 10 + 3
                                              // 第三块第一项的全局下标应为 20。
        assert_eq!(
            checkbox_blocks[2]["element"]["options"][0]["value"],
            "opt_20"
        );
    }

    #[test]
    fn build_card_without_options_omits_checkboxes() {
        let card = build_question_card(
            "",
            "随便说点什么",
            &[],
            false,
            false,
            false,
            "Options",
            "Note",
            "ph",
            "Submit",
            "R",
            "n",
        );
        let bs = blocks(&card);
        assert!(!bs.iter().any(|b| b["element"]["type"] == "checkboxes"));
        assert!(bs
            .iter()
            .any(|b| b["element"]["type"] == "plain_text_input"));
    }

    #[test]
    fn parse_submit_maps_radio_selected_option() {
        let payload = json!({
            "type": "block_actions",
            "user": { "id": "U1" },
            "container": { "message_ts": "1.2" },
            "actions": [ { "action_id": "submit" } ],
            "state": { "values": {
                "opts_0": { "options_0": {
                    "type": "radio_buttons",
                    "selected_option": { "value": "opt_1" }
                } }
            } }
        });
        let opts = plain(&["A", "B", "C"]);
        let s = parse_submit(&payload, &opts).unwrap();
        assert_eq!(s.selected_options, vec!["B".to_string()]);
    }

    #[test]
    fn parse_submit_maps_checked_options_and_input() {
        let payload = json!({
            "type": "block_actions",
            "user": { "id": "U1" },
            "channel": { "id": "D1" },
            "container": { "message_ts": "111.222" },
            "actions": [ { "action_id": "submit" } ],
            "state": { "values": {
                "opts_0": { "options_0": {
                    "type": "checkboxes",
                    "selected_options": [ { "value": "opt_0" }, { "value": "opt_2" } ]
                } },
                "userinput": { "user_input": { "type": "plain_text_input", "value": "  hi  " } }
            } }
        });
        let opts = plain(&["A", "B", "C"]);
        let s = parse_submit(&payload, &opts).unwrap();
        assert_eq!(s.user_id, "U1");
        assert_eq!(s.channel_id, "D1");
        assert_eq!(s.message_ts, "111.222");
        assert_eq!(s.selected_options, vec!["A".to_string(), "C".to_string()]);
        assert_eq!(s.user_input.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_submit_empty_input_is_none() {
        let payload = json!({
            "type": "block_actions",
            "user": { "id": "U1" },
            "container": { "message_ts": "1.2" },
            "actions": [ { "action_id": "submit" } ],
            "state": { "values": {
                "userinput": { "user_input": { "type": "plain_text_input", "value": "" } }
            } }
        });
        let s = parse_submit(&payload, &[]).unwrap();
        assert!(s.user_input.is_none());
        assert!(s.selected_options.is_empty());
    }

    #[test]
    fn parse_non_submit_returns_none() {
        let payload = json!({
            "type": "block_actions",
            "user": { "id": "U1" },
            "actions": [ { "action_id": "options_0" } ],
            "state": { "values": {} }
        });
        assert!(parse_submit(&payload, &[]).is_none());
    }

    #[test]
    fn finalized_card_is_static_with_selection() {
        let card = build_finalized_card(&Finalized {
            title: "Question 1/2",
            text: "要继续吗？",
            is_markdown: true,
            selected: &["停止".to_string()],
            user_input: Some("再想想"),
            status: "已提交",
        });
        let bs = blocks(&card);
        // 无任何交互控件。
        assert!(!bs
            .iter()
            .any(|b| b["type"] == "actions" || b["type"] == "input"));
        // 含勾选回显 + 补充文字 + 状态 context。
        assert!(bs
            .iter()
            .any(|b| b["text"]["text"].as_str().unwrap_or("").contains("✓ 停止")));
        assert!(bs.iter().any(|b| b["text"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("💬 再想想")));
        assert!(bs.iter().any(|b| b["type"] == "context"));
    }

    #[test]
    fn msg_compose_card_uses_multiline_input_nonce_and_draft() {
        let view = crate::msg_card::MsgComposeView {
            seq: 3,
            title: "Message [3] Codex".into(),
            target_label: "Target".into(),
            target: "Codex — project".into(),
            pending_label: "No message is pending.".into(),
            pending_preview: None,
            preview_omitted: None,
            input_label: "Message".into(),
            input_placeholder: "Enter a message".into(),
            send_label: "Send".into(),
            error: None,
        };
        let card = build_msg_compose_card(&view, "nonce-1", Some("draft"));
        let input = blocks(&card)
            .iter()
            .find(|block| block["type"] == "input")
            .unwrap();
        assert_eq!(input["block_id"], "userinput_nonce-1");
        assert_eq!(input["element"]["multiline"], true);
        assert_eq!(input["element"]["initial_value"], "draft");
        assert_eq!(
            blocks(&card)
                .iter()
                .filter(|block| block["type"] == "actions")
                .count(),
            1
        );
    }
}
