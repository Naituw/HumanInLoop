//! 飞书消息卡片 JSON 2.0：组装提问卡片（表单容器：勾选器 + 输入框 + 提交按钮）+ 解析提交回调。
//!
//! 设计（见 `docs/plans/feishu-channel.md`）：
//! - 提问卡片直接以 JSON 下发（无需后台模板），用 `msg_type=interactive`。
//! - 预定义选项用 `checker`（复选框/勾选器，平铺直接勾），置于 `form` 表单容器内；
//!   一个 `input` 输入框收补充文字；一个 `button`(`form_action_type=submit`) 提交。
//! - 用户点「提交」→ 一次 `card.action.trigger` 回调，`action.form_value` 汇总所有组件取值。
//! - 选项 ↔ 组件名映射 `opt_{i}`，便于回调里还原勾选了哪些选项（规避超长/重复选项文案）。

use crate::autochannel::{self, HelpQuestionState, HelpView};
use crate::i18n::Lang;
use crate::models::OptionItem;
use serde_json::{json, Value};

/// 选项组件名前缀（`opt_0` / `opt_1` ...）。
const OPT_NAME_PREFIX: &str = "opt_";
/// 输入框组件名。
const INPUT_NAME: &str = "user_input";

/// 一次卡片「提交」回调的解析结果。
pub struct CardSubmit {
    pub open_id: String,
    /// 卡片所在消息 ID（`context.open_message_id`），用于匹配当前题卡片。
    pub message_id: String,
    /// 勾选的预定义选项（选项文本，已按下标还原）。
    pub selected_options: Vec<String>,
    /// 补充文字输入（空则 None）。
    pub user_input: Option<String>,
}

/// 组装提问卡片（卡片 JSON 2.0）。
/// `title` 为题首（空则省略 header）；`text` 为问题正文；`options` 为预定义选项（空则无选项区）；
/// `is_markdown` 决定正文用 markdown 还是 plain_text 组件；`input_placeholder` 为输入框占位提示；
/// `submit_label` 为提交按钮文案；`recommended_prefix` 为推荐选项的显示前缀（lark_md，由渠道层
/// 本地化后传入；提交值始终为选项原文）。
/// `single`→真 radio：勾选器移出表单、各挂 toggle 回调（互斥由会话自管，按 `selected` 渲染勾选）；
/// `select_only`→去掉补充输入框（严格选择）。`selected` 为当前已选原文（首次渲染传空）。
#[allow(clippy::too_many_arguments)]
pub fn build_question_card(
    title: &str,
    text: &str,
    options: &[OptionItem],
    is_markdown: bool,
    single: bool,
    select_only: bool,
    selected: &[String],
    input_placeholder: &str,
    submit_label: &str,
    recommended_prefix: &str,
) -> Value {
    build_question_card_with_todo(
        title,
        text,
        options,
        is_markdown,
        single,
        select_only,
        selected,
        input_placeholder,
        submit_label,
        recommended_prefix,
        "",
        "",
    )
}

/// Build a question card with a channel-native colored marker for todo options.
/// The stored option text remains untouched so callbacks still return the original value.
#[allow(clippy::too_many_arguments)]
pub fn build_question_card_with_todo(
    title: &str,
    text: &str,
    options: &[OptionItem],
    is_markdown: bool,
    single: bool,
    select_only: bool,
    selected: &[String],
    input_placeholder: &str,
    submit_label: &str,
    recommended_prefix: &str,
    todo_text_prefix: &str,
    todo_badge_prefix: &str,
) -> Value {
    let mut elements: Vec<Value> = Vec::new();
    if !text.trim().is_empty() {
        elements.push(body_text(text, is_markdown));
    }
    // 单选：勾选器置于表单外（各挂 toggle 回调，实现点击互斥）。
    if single {
        for (i, opt) in options.iter().enumerate() {
            elements.push(checker_element(
                i,
                opt,
                selected.contains(&opt.text),
                false,
                true,
                recommended_prefix,
                todo_text_prefix,
                todo_badge_prefix,
            ));
        }
    }
    elements.push(build_form(
        options,
        selected,
        None,
        false,
        single,
        select_only,
        input_placeholder,
        "",
        submit_label,
        recommended_prefix,
        todo_text_prefix,
        todo_badge_prefix,
    ));
    assemble_card(title, elements, true)
}

/// `/msg` 一次性输入卡。正文始终按纯文本渲染，避免待发送内容被解释为卡片 markdown。
pub fn build_msg_compose_card(
    view: &crate::msg_card::MsgComposeView,
    user_input_draft: Option<&str>,
) -> Value {
    let elements = vec![
        body_text(&view.plain_body(), false),
        build_form(
            &[],
            &[],
            user_input_draft,
            false,
            false,
            false,
            &view.input_placeholder,
            "",
            &view.send_label,
            "",
            "",
            "",
        ),
    ];
    assemble_card(&view.title, elements, true)
}

/// 终态卡片入参（复刻钉钉「已提交」态）。
pub struct Finalized<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub is_markdown: bool,
    pub options: &'a [OptionItem],
    /// 用户已选选项（原文；被抢答收尾时为空 → 勾选器都不勾）。
    pub selected: &'a [String],
    /// 补充文字回显（无则 None → 整段不渲染）。
    pub user_input: Option<&'a str>,
    pub input_placeholder: &'a str,
    /// 补充文字上方的小标题（本地化「我的补充：」）。
    pub note_label: &'a str,
    /// 禁用按钮的文案（「已提交」/「已在 X 回答」）。
    pub button_label: &'a str,
    /// 推荐选项的显示前缀（本地化 lark_md）。
    pub recommended_prefix: &'a str,
    /// 单选：勾选器在表单外（与提问态布局一致，终态禁用）。
    pub single: bool,
    /// 严格选择：无补充输入框。
    pub select_only: bool,
}

/// 提示消息（message prompt）的 markdown 卡片：标题（来源头部）+ markdown 正文。
/// 飞书 IM 没有像钉钉 sampleMarkdown 那样的「markdown 文本消息」类型，故以卡片渲染
/// markdown（粗体/标题/列表/代码/部分表格）。正文为空时仅留标题。
pub fn build_message_card(title: &str, markdown_body: &str) -> Value {
    let mut elements: Vec<Value> = Vec::new();
    if !markdown_body.trim().is_empty() {
        elements.push(body_text(markdown_body, true));
    }
    assemble_card(title, elements, false)
}

/// Build a non-interactive JSON 2.0 help card with grouped Markdown command lists.
pub fn build_help_card(view: &HelpView, lang: Lang) -> Value {
    let mut elements = vec![body_text(&view.intro, false)];
    for section in &view.sections {
        let mut markdown = format!("**{}**", section.title);
        for command in &section.commands {
            markdown.push_str("\n- `");
            markdown.push_str(&command.syntax);
            markdown.push_str("` — ");
            markdown.push_str(&command.description);
            if let Some(phrase) = &command.phrase {
                markdown.push_str("　<font color='grey'>· ");
                markdown.push_str(&autochannel::help_phrase_hint(phrase, lang));
                markdown.push_str("</font>");
            }
        }
        elements.push(body_text(&markdown, true));
    }
    elements.push(json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }));

    let state = match &view.question_state {
        HelpQuestionState::Active { instruction } => instruction,
        HelpQuestionState::None { message } => message,
    };
    let mut footer = format!("**{state}**");
    if let Some(hint) = &view.switch_hint {
        footer.push_str("\n<font color='grey'>");
        footer.push_str(hint);
        footer.push_str("</font>");
    }
    elements.push(body_text(&footer, true));
    assemble_card(&view.title, elements, true)
}

#[cfg(test)]
mod help_tests {
    use super::*;

    #[test]
    fn help_card_uses_bullets_code_and_grey_phrases() {
        let view = autochannel::help_view(true, false, true, "/", Lang::Zh);
        let card = build_help_card(&view, Lang::Zh);
        assert_eq!(card["schema"], "2.0");
        assert_eq!(card["config"]["update_multi"], true);
        let serialized = card.to_string();
        assert!(serialized.contains("**Agent 管理**"));
        assert!(serialized.contains("- `/status [编号]`"));
        assert!(serialized.contains("<font color='grey'>· 直接说「状态」</font>"));
        assert!(serialized.contains("- `/msg-clear <编号>`"));
        assert!(!serialized.contains("msg-clear <编号>` — 撤回该 Agent 待送达的插话　<font"));
    }
}

/// 卡片回调的同步「更新卡片」回包体：`{card:{type:"raw",data:<新卡片>}}`。
/// 点提交时由会话经 Router 同步回此包 → 按钮 Loading 直接变终态（否则空 ACK 会令按钮先弹回 Submit，
/// 再由 OpenAPI `patch_card` 异步置灰，出现可见闪烁）。
pub fn callback_update_card(card: Value) -> Value {
    json!({ "card": { "type": "raw", "data": card } })
}

/// 组装终态卡片（复刻钉钉「已提交」态）：沿用同一表单结构，但全部禁用——
/// 勾选器按用户选择 `checked` 且 `disabled`、输入框 `default_value` 回显补充文字且 `disabled`、
/// 提交按钮 `disabled` 并改文案。
pub fn build_finalized_card(p: &Finalized) -> Value {
    build_finalized_card_with_todo(p, "", "")
}

/// Build a finalized card while preserving the todo-only display treatment.
pub fn build_finalized_card_with_todo(
    p: &Finalized,
    todo_text_prefix: &str,
    todo_badge_prefix: &str,
) -> Value {
    let mut elements: Vec<Value> = Vec::new();
    if !p.text.trim().is_empty() {
        elements.push(body_text(p.text, p.is_markdown));
    }
    // Single-select checkers live outside the form, matching the interactive card layout.
    if p.single {
        for (i, opt) in p.options.iter().enumerate() {
            elements.push(checker_element(
                i,
                opt,
                p.selected.contains(&opt.text),
                true,
                true,
                p.recommended_prefix,
                todo_text_prefix,
                todo_badge_prefix,
            ));
        }
    }
    elements.push(build_form(
        p.options,
        p.selected,
        p.user_input,
        true,
        p.single,
        p.select_only,
        p.input_placeholder,
        p.note_label,
        p.button_label,
        p.recommended_prefix,
        todo_text_prefix,
        todo_badge_prefix,
    ));
    assemble_card(p.title, elements, true)
}

/// 单个勾选器组件：文本用 lark_md（支持推荐项的彩色前缀）。
/// `disabled=true`（终态）禁用；`single=true` 且非终态时挂 toggle 回调（勾选器在表单外，点击互斥）。
#[allow(clippy::too_many_arguments)] // arguments mirror the checker payload fields
fn checker_element(
    i: usize,
    opt: &OptionItem,
    checked: bool,
    disabled: bool,
    single: bool,
    recommended_prefix: &str,
    todo_text_prefix: &str,
    todo_badge_prefix: &str,
) -> Value {
    let display = if opt.todo_id.is_some() {
        let text = opt.text.strip_prefix(todo_text_prefix).unwrap_or(&opt.text);
        let (body, attachment_badge) = crate::todos::split_attachment_badge(text);
        match attachment_badge {
            Some(badge) => format!(
                "{}{} <font color='grey'>{}</font>",
                todo_badge_prefix, body, badge
            ),
            None => format!("{}{}", todo_badge_prefix, body),
        }
    } else if opt.recommended {
        format!("{}{}", recommended_prefix, opt.text)
    } else {
        opt.text.clone()
    };
    let behavior = single.then(|| json!({ "action": "toggle", "index": i }));
    styled_checker(
        &format!("{}{}", OPT_NAME_PREFIX, i),
        &display,
        checked,
        disabled,
        behavior,
        None,
    )
}

/// Shared native checker used by ordinary Ask cards and structured confirmations.
pub(crate) fn styled_checker(
    name: &str,
    content: &str,
    checked: bool,
    disabled: bool,
    callback_value: Option<Value>,
    text_color: Option<&str>,
) -> Value {
    let mut text = json!({ "tag": "lark_md", "content": content });
    if let Some(color) = text_color {
        text["text_color"] = Value::String(color.to_string());
    }
    let mut checker = json!({
        "tag": "checker",
        "name": name,
        "checked": checked,
        "text": text,
    });
    if disabled {
        checker["disabled"] = Value::Bool(true);
    } else if let Some(value) = callback_value {
        checker["behaviors"] = json!([ { "type": "callback", "value": value } ]);
    }
    checker
}

/// 组装表单容器：（多选时）勾选器 +（非严格时）输入框 + 提交按钮。
/// `disabled=false`（提问态）：可交互，按钮带 `callback` behaviors。
/// `disabled=true`（终态）：禁用全部交互，勾选器按 `selected` 勾上，输入框用 `user_input` 回显，按钮无 behaviors。
/// `single=true`：勾选器不在表单内（由调用方置于表单外）；`select_only=true`：无补充输入框。
#[allow(clippy::too_many_arguments)]
fn build_form(
    options: &[OptionItem],
    selected: &[String],
    user_input: Option<&str>,
    disabled: bool,
    single: bool,
    select_only: bool,
    input_placeholder: &str,
    note_label: &str,
    button_label: &str,
    recommended_prefix: &str,
    todo_text_prefix: &str,
    todo_badge_prefix: &str,
) -> Value {
    let mut form_elements: Vec<Value> = Vec::new();
    // 多选：勾选器在表单内（提交时随 form_value 回传）。单选的勾选器在表单外。
    if !single {
        for (i, opt) in options.iter().enumerate() {
            form_elements.push(checker_element(
                i,
                opt,
                selected.contains(&opt.text),
                disabled,
                false,
                recommended_prefix,
                todo_text_prefix,
                todo_badge_prefix,
            ));
        }
    }

    // 严格选择无补充输入框。
    if !select_only {
        // 终态用「灰色小标题 + 引用块」回显补充文字：飞书的 `input` 一旦 disabled 就既不能编辑
        // 也**不能选中复制**，而人常常要回头引用自己写过的话。无补充时整段省略。
        if disabled {
            if let Some(v) = user_input.filter(|s| !s.trim().is_empty()) {
                let quoted = v
                    .lines()
                    .map(|line| format!("> {line}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                form_elements.push(json!({
                    "tag": "markdown",
                    "content": format!("<font color='grey'>{note_label}</font>\n{quoted}"),
                }));
            }
        } else {
            let mut input = json!({
                "tag": "input",
                "name": INPUT_NAME,
                "placeholder": { "tag": "plain_text", "content": input_placeholder },
            });
            if let Some(v) = user_input {
                input["default_value"] = Value::String(v.to_string());
            }
            form_elements.push(input);
        }
    }

    let mut button = json!({
        "tag": "button",
        "name": "submit",
        "form_action_type": "submit",
        "text": { "tag": "plain_text", "content": button_label },
        "type": "primary",
    });
    if disabled {
        button["disabled"] = Value::Bool(true);
    } else {
        button["behaviors"] = json!([ { "type": "callback", "value": { "action": "submit" } } ]);
    }
    form_elements.push(button);

    json!({
        "tag": "form",
        "name": "answer_form",
        "elements": form_elements,
    })
}

/// Assemble the card skeleton: schema 2.0 + body.elements. `config.update_multi` is on to allow later updates.
///
/// `styled_header=true` (question/finalized cards): mimic DingTalk's "icon container" style — put a single
/// plain-text component row at the top of the body "question icon (left) + small blue title (right)" plus a
/// divider, separating it from the body (no native banner, since the banner title font is fixed and large and
/// renders as a full color strip). `false` (message card): keep the plain native header title.
fn assemble_card(title: &str, elements: Vec<Value>, styled_header: bool) -> Value {
    if styled_header && !title.trim().is_empty() {
        // Style matches the card exported from Feishu's card builder: small (notation) title + filled
        // question icon (maybe_filled); blue color (the builder has no blue option, user confirmed blue).
        let header_row = json!({
            "tag": "div",
            "text": {
                "tag": "plain_text",
                "content": title,
                "text_size": "notation",
                "text_align": "left",
                "text_color": "blue",
            },
            "icon": { "tag": "standard_icon", "token": "maybe_filled", "color": "blue" },
            "margin": "0px 0px 0px 0px",
        });
        let mut body = vec![
            header_row,
            json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }),
        ];
        body.extend(elements);
        return json!({
            "schema": "2.0",
            "config": { "update_multi": true },
            "body": { "elements": body },
        });
    }
    let mut card = json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "body": { "elements": elements },
    });
    if !title.trim().is_empty() {
        card["header"] = json!({ "title": { "tag": "plain_text", "content": title } });
    }
    card
}

/// Shared small blue icon header used by ordinary Ask and structured-confirm cards.
pub(crate) fn assemble_styled_card(title: &str, elements: Vec<Value>) -> Value {
    assemble_card(title, elements, true)
}

/// 正文组件：markdown → `markdown` 组件；纯文本 → `div` + plain_text。
fn body_text(text: &str, is_markdown: bool) -> Value {
    if is_markdown {
        json!({ "tag": "markdown", "content": text })
    } else {
        json!({ "tag": "div", "text": { "tag": "plain_text", "content": text } })
    }
}

// ===== /watch 实时状态卡（spec docs/specs/im-watch.md）=====

/// watch 卡按钮回调 value 的键 / 值（`{"watch":"unwatch"|"refresh"}`）。
const WATCH_ACTION_KEY: &str = "watch";

/// watch 卡的一次按钮动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchAction {
    /// 取消关注（卡片定格 + 退订）。
    Unwatch,
    /// 立即刷新（按当前状态重算一帧）。
    Refresh,
    /// 重新关注（从 AutoStopped 终态卡点击；携带 session_id）。
    Rewatch(String),
}

/// watch 卡片视图（纯字符串，本地化由 `watch::card_view` 完成）。
pub struct WatchCardView {
    /// 样式化头部：`实时关注 [3] Cursor — HumanInLoop`。
    pub header: String,
    /// 状态行（emoji 编码）：`🟢 工作中` / `🙋 正在等待你的回答` / …
    pub state_line: String,
    /// 会话标题行 `「…」`（无标题则 None）。
    pub title_line: Option<String>,
    /// 「最近动态（14:32:05）：」（绝对时刻）。
    pub activity_heading: String,
    /// 最后一段助手文字。
    pub text: Option<String>,
    /// 已渲染足迹时间线（≤3 行，旧→新；`<font color='green'>●</font> **运行命令**: *cargo test*`）。
    pub step_lines: Vec<String>,
    /// TODO 清单摘要（折叠面板标题：`📋 清单 4/7 · 当前：xxx`；无清单则 None，不出面板）。
    pub todo_summary: Option<String>,
    /// 已渲染 TODO 清单全行（折叠面板内容）。
    pub todo_lines: Vec<String>,
    /// text/steps 均无时的占位（`（暂无可解析的活动）`）。
    pub no_activity: Option<String>,
    /// 底部灰色小字：`最后更新 14:32:07`。
    pub updated_line: String,
    pub buttons: WatchButtons,
}

/// watch 卡按钮区：活动态（取消关注 + 立即刷新，可点）/ 终态（单个禁用按钮）/
/// 可重新关注终态（可点击按钮，携带 session_id 回调数据）。
pub enum WatchButtons {
    Active { unwatch: String, refresh: String },
    Final { label: String },
    Rewatch { label: String, session_id: String },
}

/// 组装 watch 实时状态卡（卡片 JSON 2.0，`update_multi` 开启供后续 PATCH）。
/// 布局：样式化头部行（eye icon + 蓝色小字）+ hr + 状态/标题 markdown + 活动 markdown
/// （助手文字与足迹时间线之间空一行——用户定案：不用分隔线，太割裂）+ TODO 折叠面板
/// （标题即摘要行，默认收起不占高度；无清单不出）+ hr + 灰色更新时刻 +
/// 按钮行（column_set 两列 / 终态单禁用按钮）。
pub fn build_watch_card(v: &WatchCardView) -> Value {
    let mut elements: Vec<Value> = Vec::new();

    // 状态 + 标题。
    let mut head_md = format!("**{}**", v.state_line);
    if let Some(t) = &v.title_line {
        head_md.push('\n');
        head_md.push_str(t);
    }
    elements.push(json!({ "tag": "markdown", "content": head_md }));

    // 最近动态：标签 + 助手文字 + 空行 + 足迹时间线（彩色圆点步行）。
    let mut act_md = v.activity_heading.clone();
    if let Some(na) = &v.no_activity {
        act_md.push('\n');
        act_md.push_str(na);
    } else if let Some(t) = &v.text {
        act_md.push('\n');
        act_md.push_str(t);
    }
    if !v.step_lines.is_empty() {
        act_md.push_str("\n\n");
        act_md.push_str(&v.step_lines.join("\n"));
    }
    elements.push(json!({ "tag": "markdown", "content": act_md }));

    // TODO 清单：折叠面板（标题即摘要行「📋 TODO 4/7 · 当前：xxx」，展开见全清单）。
    // 默认收起不占高度（用户定案 A+B：摘要常显 + 想看再展开）。PATCH 会重置收起态，可接受。
    // 上外边距 12px 与足迹时间线拉开距离（用户定案：贴太近）。
    if let (Some(summary), false) = (&v.todo_summary, v.todo_lines.is_empty()) {
        elements.push(json!({
            "tag": "collapsible_panel",
            "expanded": false,
            "margin": "12px 0px 0px 0px",
            "header": {
                "title": { "tag": "markdown", "content": summary },
                "width": "auto_when_fold",
                "vertical_align": "center",
                "icon": { "tag": "standard_icon", "token": "down-small-ccm_outlined", "color": "grey", "size": "16px 16px" },
                "icon_position": "follow_text",
                "icon_expanded_angle": -180,
            },
            "vertical_spacing": "8px",
            "elements": [ { "tag": "markdown", "content": v.todo_lines.join("\n") } ],
        }));
    }

    // 底部：分隔线 + 灰色更新时刻 + 按钮。
    elements.push(json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }));
    elements.push(json!({
        "tag": "div",
        "text": {
            "tag": "plain_text",
            "content": v.updated_line,
            "text_size": "notation",
            "text_align": "left",
            "text_color": "grey",
        },
        "margin": "0px 0px 0px 0px",
    }));
    elements.push(watch_buttons_element(&v.buttons));

    // 样式化头部行：眼睛 icon + 蓝色小字（与提问卡「icon + 小标题」同风格、不同 icon）。
    let header_row = json!({
        "tag": "div",
        "text": {
            "tag": "plain_text",
            "content": v.header,
            "text_size": "notation",
            "text_align": "left",
            "text_color": "blue",
        },
        "icon": { "tag": "standard_icon", "token": "eye_outlined", "color": "blue" },
        "margin": "0px 0px 0px 0px",
    });
    let mut body = vec![
        header_row,
        json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }),
    ];
    body.extend(elements);
    json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "body": { "elements": body },
    })
}

/// 按钮区元素：活动态 = column_set 两列（取消关注 danger + 立即刷新 default，均挂 callback）；
/// 终态 = 单个禁用按钮（文案标示 已结束/已取消/已接替）；
/// 可重新关注 = 单个可点击按钮（default 样式，携带 session_id 的 rewatch 回调）。
fn watch_buttons_element(buttons: &WatchButtons) -> Value {
    match buttons {
        WatchButtons::Active { unwatch, refresh } => {
            let btn = |label: &str, kind: &str, action: &str| -> Value {
                json!({
                    "tag": "button",
                    "text": { "tag": "plain_text", "content": label },
                    "type": kind,
                    "behaviors": [ { "type": "callback", "value": { WATCH_ACTION_KEY: action } } ],
                })
            };
            json!({
                "tag": "column_set",
                "horizontal_spacing": "8px",
                "columns": [
                    { "tag": "column", "width": "auto",
                      "elements": [ btn(unwatch, "danger", "unwatch") ] },
                    { "tag": "column", "width": "auto",
                      "elements": [ btn(refresh, "default", "refresh") ] },
                ],
            })
        }
        WatchButtons::Final { label } => json!({
            "tag": "button",
            "text": { "tag": "plain_text", "content": label },
            "type": "default",
            "disabled": true,
        }),
        WatchButtons::Rewatch { label, session_id } => json!({
            "tag": "button",
            "text": { "tag": "plain_text", "content": label },
            "type": "default",
            "behaviors": [ { "type": "callback", "value": { WATCH_ACTION_KEY: "rewatch", "sid": session_id } } ],
        }),
    }
}

/// 把一条 `card.action.trigger` 解析为 watch 卡按钮动作；非 watch 卡回调返回 None。
/// 返回 `(open_message_id, 动作)`。
pub fn parse_watch_action(event: &Value) -> Option<(String, WatchAction)> {
    let action = event.get("action")?;
    let value = action.get("value")?;
    let obj: Value = match value {
        Value::String(s) => serde_json::from_str(s).ok()?,
        v => v.clone(),
    };
    let act = match obj.get(WATCH_ACTION_KEY).and_then(|a| a.as_str()) {
        Some("unwatch") => WatchAction::Unwatch,
        Some("refresh") => WatchAction::Refresh,
        Some("rewatch") => {
            let sid = obj
                .get("sid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            WatchAction::Rewatch(sid)
        }
        _ => return None,
    };
    let message_id = event
        .get("context")
        .and_then(|c| c.get("open_message_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((message_id, act))
}

// ===== 通用「单选卡」（spec docs/specs/im-select-card.md）=====

/// 单选卡按钮回调 value 的键（`{"select": <idx>}`）。与 watch 的 `{"watch":…}` 键不同，
/// 且卡片按 open_message_id 精确路由（一条消息非 watch 即 select），天然可辨。
const SELECT_ACTION_KEY: &str = "select";

/// 飞书按钮样式（`type`）按单选卡动作种类映射（用户定稿：watch=蓝主色、status=默认、unwatch=红）。
fn select_button_type(action: crate::select::SelectAction) -> &'static str {
    match action {
        crate::select::SelectAction::Watch
        | crate::select::SelectAction::TaskWorkspace
        | crate::select::SelectAction::TaskAgent
        | crate::select::SelectAction::TaskPermission
        | crate::select::SelectAction::TaskInputSource
        | crate::select::SelectAction::Fork
        | crate::select::SelectAction::Msg
        | crate::select::SelectAction::MsgTarget
        | crate::select::SelectAction::Stage
        | crate::select::SelectAction::TodoRm
        | crate::select::SelectAction::TodoAuto => "primary",
        crate::select::SelectAction::Status
        | crate::select::SelectAction::Diff
        | crate::select::SelectAction::Transcript
        | crate::select::SelectAction::Todo
        | crate::select::SelectAction::TodoAutoEntry => "default",
        crate::select::SelectAction::Unwatch
        | crate::select::SelectAction::TodoRmEntry
        | crate::select::SelectAction::Yolo => "danger",
    }
}

/// 单选卡一行左侧富文本（markdown、小字号、可换行）：第一行 `圆点 [编号] 主文本 · 徽标`，
/// 第二行灰色次行（标题）。圆点用 markdown 彩色 `●`（与 watch 卡步行同风格）。
fn select_option_markdown(opt: &crate::select::SelectOption) -> String {
    let mut line1 = String::new();
    match opt.dot {
        Some(crate::select::SelectDot::Working) => line1.push_str("<font color='green'>●</font> "),
        Some(crate::select::SelectDot::Idle) => line1.push_str("<font color='grey'>●</font> "),
        None => {}
    }
    if let Some(seq) = opt.seq {
        line1.push_str(&format!("**[{}]** ", seq));
    }
    let (primary, attachment_badge) = crate::todos::split_attachment_badge(&opt.primary);
    line1.push_str(primary);
    if let Some(badge) = &opt.badge {
        line1.push(' ');
        line1.push_str(badge);
    }
    if let Some(elapsed) = &opt.elapsed {
        line1.push(' ');
        line1.push_str(elapsed);
    }
    if let Some(attachment_badge) = attachment_badge {
        line1.push_str(" <font color='grey'>");
        line1.push_str(attachment_badge);
        line1.push_str("</font>");
    }
    match &opt.secondary {
        Some(sec) => format!("{}\n<font color='grey'>{}</font>", line1, sec),
        None => line1,
    }
}

/// 组装通用单选卡（卡片 JSON 2.0，`update_multi` 供后续 PATCH 就地变卡）：样式化头部（标题）+ hr +
/// 逐选项一行（左侧小字号两行富文本 + 右侧紧凑触发按钮，callback value `{select: idx}`），行间以细
/// 分隔线分隔 +（截断时）灰色小字说明。用户定稿「方案A」（`docs/specs/im-select-card.md`）。
pub fn build_select_card(v: &crate::select::SelectView) -> Value {
    let btn_type = select_button_type(v.action);
    let lang = crate::i18n::Lang::current();
    let mut elements: Vec<Value> = Vec::new();
    for (i, opt) in v.options.iter().enumerate() {
        let btn_label = crate::select::option_button_label(opt, v.action, lang);
        if i > 0 {
            elements.push(json!({ "tag": "hr", "margin": "2px 0px 2px 0px" }));
        }
        elements.push(json!({
            "tag": "column_set",
            "horizontal_spacing": "8px",
            "margin": "0px 0px 0px 0px",
            "columns": [
                {
                    "tag": "column",
                    "width": "weighted",
                    "weight": 1,
                    "vertical_align": "center",
                    "elements": [
                        { "tag": "markdown", "content": select_option_markdown(opt), "text_size": "notation" }
                    ],
                },
                {
                    "tag": "column",
                    "width": "auto",
                    "vertical_align": "center",
                    "elements": [
                        {
                            "tag": "button",
                            "size": "tiny",
                            "type": btn_type,
                            "text": { "tag": "plain_text", "content": btn_label },
                            "behaviors": [ { "type": "callback", "value": { SELECT_ACTION_KEY: i } } ],
                        }
                    ],
                },
            ],
        }));
    }
    if let Some(note) = &v.truncated_note {
        elements.push(json!({
            "tag": "div",
            "text": { "tag": "plain_text", "content": note, "text_size": "notation", "text_align": "left", "text_color": "grey" },
            "margin": "4px 0px 0px 0px",
        }));
    }
    // 样式化头部行（icon + 蓝色小字，与 watch 卡同风格）。
    let header_row = json!({
        "tag": "div",
        "text": { "tag": "plain_text", "content": v.title, "text_size": "notation", "text_align": "left", "text_color": "blue" },
        "icon": { "tag": "standard_icon", "token": "maybe_filled", "color": "blue" },
        "margin": "0px 0px 0px 0px",
    });
    let mut body = vec![
        header_row,
        json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }),
    ];
    body.extend(elements);
    json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "body": { "elements": body },
    })
}

/// `/todo-auto` 管理卡：复用逐条切换列表，并在同一卡片底部追加“新增自动待办”表单。
/// 空项目仍保留表单，以便直接创建第一条自动待办。
pub fn build_todo_auto_card(
    v: &crate::select::SelectView,
    empty_text: &str,
    input_placeholder: &str,
    submit_label: &str,
) -> Value {
    let mut card = build_select_card(v);
    let Some(elements) = card
        .pointer_mut("/body/elements")
        .and_then(Value::as_array_mut)
    else {
        return card;
    };
    if v.options.is_empty() {
        elements.push(body_text(empty_text, true));
    }
    elements.push(json!({ "tag": "hr", "margin": "4px 0px 4px 0px" }));
    elements.push(build_form(
        &[],
        &[],
        None,
        false,
        false,
        false,
        input_placeholder,
        "",
        submit_label,
        "",
        "",
        "",
    ));
    card
}

/// 待办管理卡（spec todo-whats-next D8）：样式化头部（标题）+ markdown 列表正文 +
/// 表单（输入框 + 「新增待办」提交按钮）。提交回调与提问卡同构（`form_value.user_input`），
/// 由 daemon select 路由按台账 kind 分派（不与提问会话冲突）。
pub fn build_todo_manage_card(
    title: &str,
    body_md: &str,
    input_placeholder: &str,
    submit_label: &str,
) -> Value {
    let elements = vec![
        body_text(body_md, true),
        build_form(
            &[],
            &[],
            None,
            false,
            false,
            false,
            input_placeholder,
            "",
            submit_label,
            "",
            "",
            "",
        ),
    ];
    assemble_card(title, elements, true)
}

/// 定格单选卡为一段纯文本（无按钮）——`/unwatch` 取到 0 个后用。
pub fn build_select_final_card(title: &str, text: &str) -> Value {
    json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "body": { "elements": [
            { "tag": "div", "text": { "tag": "plain_text", "content": title, "text_size": "notation", "text_align": "left", "text_color": "grey" } },
            { "tag": "markdown", "content": text },
        ] },
    })
}

/// Confirm 卡回调键：`{"confirm":"ok"|"cancel"}`（wire slot，不代表业务语义）。
const CONFIRM_ACTION_KEY: &str = "confirm";
/// Wire slot callback values.
const WIRE_SLOT_PRIMARY: &str = "ok";
const WIRE_SLOT_SECONDARY: &str = "cancel";

fn feishu_button_type(role: crate::confirm::ActionRole) -> &'static str {
    match role {
        crate::confirm::ActionRole::Primary => "primary",
        crate::confirm::ActionRole::Destructive => "danger",
        crate::confirm::ActionRole::Default => "default",
    }
}

/// 轻量确认卡：标题 + markdown 正文 + 确认/取消两按钮。
pub fn build_confirm_card(view: &crate::confirm::ConfirmView) -> Value {
    let primary_type = feishu_button_type(view.confirm.role);
    let secondary_type = feishu_button_type(view.cancel.role);
    json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "body": { "elements": [
            {
                "tag": "div",
                "text": { "tag": "plain_text", "content": view.title, "text_size": "notation", "text_align": "left", "text_color": "blue" },
                "icon": { "tag": "standard_icon", "token": "file-link-docx_outlined", "color": "blue" },
            },
            { "tag": "hr" },
            { "tag": "markdown", "content": view.body },
            {
                "tag": "column_set",
                "flex_mode": "none",
                "background_style": "default",
                "columns": [
                    {
                        "tag": "column",
                        "width": "weighted",
                        "weight": 1,
                        "elements": [{
                            "tag": "button",
                            "text": { "tag": "plain_text", "content": view.confirm_label() },
                            "type": primary_type,
                            "width": "fill",
                            "behaviors": [{ "type": "callback", "value": { CONFIRM_ACTION_KEY: WIRE_SLOT_PRIMARY } }],
                        }],
                    },
                    {
                        "tag": "column",
                        "width": "weighted",
                        "weight": 1,
                        "elements": [{
                            "tag": "button",
                            "text": { "tag": "plain_text", "content": view.cancel_label() },
                            "type": secondary_type,
                            "width": "fill",
                            "behaviors": [{ "type": "callback", "value": { CONFIRM_ACTION_KEY: WIRE_SLOT_SECONDARY } }],
                        }],
                    },
                ],
            },
        ] },
    })
}

/// 定格确认卡：正文 + **单个禁用按钮**（「已取消」/「已暂存」/「暂存失败」）。
pub fn build_confirm_final_card(title: &str, body_md: &str, button_label: &str) -> Value {
    json!({
        "schema": "2.0",
        "config": { "update_multi": true },
        "body": { "elements": [
            { "tag": "div", "text": { "tag": "plain_text", "content": title, "text_size": "notation", "text_align": "left", "text_color": "grey" } },
            { "tag": "markdown", "content": body_md },
            {
                "tag": "button",
                "text": { "tag": "plain_text", "content": button_label },
                "type": "default",
                "width": "default",
                "disabled": true,
            },
        ] },
    })
}

/// 解析确认卡点击 → `(open_message_id, slot)`；非 confirm 回调 None。
pub fn parse_confirm_action(event: &Value) -> Option<(String, crate::confirm::ConfirmSlot)> {
    let action = event.get("action")?;
    let value = action.get("value")?;
    let obj: Value = match value {
        Value::String(s) => serde_json::from_str(s).ok()?,
        v => v.clone(),
    };
    let verb = obj.get(CONFIRM_ACTION_KEY)?.as_str()?;
    let slot = match verb {
        "ok" | "confirm" => crate::confirm::ConfirmSlot::Primary,
        "cancel" => crate::confirm::ConfirmSlot::Secondary,
        _ => return None,
    };
    let message_id = event
        .get("context")
        .and_then(|c| c.get("open_message_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((message_id, slot))
}

/// 把一条 `card.action.trigger` 解析为单选卡点击：`(open_message_id, 选项下标)`；非单选卡回调 None。
/// value 可能是对象或 JSON 字符串；下标可能以数字或数字字符串回传（两者都接受）。
pub fn parse_select_action(event: &Value) -> Option<(String, usize)> {
    let action = event.get("action")?;
    let value = action.get("value")?;
    let obj: Value = match value {
        Value::String(s) => serde_json::from_str(s).ok()?,
        v => v.clone(),
    };
    let idx = obj.get(SELECT_ACTION_KEY).and_then(|a| {
        a.as_u64()
            .or_else(|| a.as_str().and_then(|s| s.parse::<u64>().ok()))
    })? as usize;
    let message_id = event
        .get("context")
        .and_then(|c| c.get("open_message_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((message_id, idx))
}

/// 把一条 `card.action.trigger` 的 `event` 解析为「提交」结果；非提交回调返回 None。
/// `options` 用于把 `opt_{i}` 还原为选项文本。
///
/// 提交判定（两者满足其一）：
/// - `action.form_value` 存在（多选：勾选器在表单内，提交会汇总 form_value；含补充输入框时同样）；
/// - 或 `action.value.action == "submit"`（单选严格态：表单内只有提交按钮、无任何输入组件，
///   飞书此时**不下发 form_value**，只能靠按钮回调 value 判定，否则会被误当作非提交而把卡片弹回）。
pub fn parse_card_submit(event: &Value, options: &[OptionItem]) -> Option<CardSubmit> {
    let open_id = event
        .get("operator")
        .and_then(|o| o.get("open_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let message_id = event
        .get("context")
        .and_then(|c| c.get("open_message_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let action = event.get("action")?;
    let form_value = action.get("form_value");
    let is_submit = form_value.is_some() || action_kind(action).as_deref() == Some("submit");
    if !is_submit {
        return None;
    }
    // 无 form_value（单选严格态）时按空表单处理：选项由会话自管的 selected_single 兜底。
    let empty = Value::Object(serde_json::Map::new());
    let form_value = form_value.unwrap_or(&empty);

    let mut selected: Vec<String> = Vec::new();
    for (i, opt) in options.iter().enumerate() {
        let key = format!("{}{}", OPT_NAME_PREFIX, i);
        if is_checked(form_value.get(&key)) {
            selected.push(opt.text.clone());
        }
    }
    let user_input = form_value
        .get(INPUT_NAME)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some(CardSubmit {
        open_id,
        message_id,
        selected_options: selected,
        user_input,
    })
}

/// 读取 `action.value.action`（value 可能是对象，也可能是 JSON 字符串）。
fn action_kind(action: &Value) -> Option<String> {
    let value = action.get("value")?;
    let obj: Value = match value {
        Value::String(s) => serde_json::from_str(s).ok()?,
        v => v.clone(),
    };
    obj.get("action")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())
}

/// 把一条 `card.action.trigger` 解析为「单选勾选器 toggle」回调（表单外勾选器，无 form_value）。
/// 返回 (open_id, message_id, 选项下标)；非 toggle（如提交 / 其它）返回 None。
pub fn parse_toggle(event: &Value) -> Option<(String, String, usize)> {
    let action = event.get("action")?;
    // 提交带 form_value；toggle 勾选器在表单外，不产生 form_value。
    if action.get("form_value").is_some() {
        return None;
    }
    // behaviors 回调 value：可能是对象，也可能是 JSON 字符串。
    let value = action.get("value")?;
    let value_obj: Value = match value {
        Value::String(s) => serde_json::from_str(s).ok()?,
        v => v.clone(),
    };
    if value_obj.get("action").and_then(|a| a.as_str()) != Some("toggle") {
        return None;
    }
    let index = value_obj.get("index").and_then(|i| i.as_u64())? as usize;
    let open_id = event
        .get("operator")
        .and_then(|o| o.get("open_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let message_id = event
        .get("context")
        .and_then(|c| c.get("open_message_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((open_id, message_id, index))
}

/// 勾选状态判定：兼容布尔 `true` 或字符串 `"true"`。
fn is_checked(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(items: &[&str]) -> Vec<OptionItem> {
        items.iter().map(|s| OptionItem::new(*s, false)).collect()
    }

    fn form_of(card: &Value) -> Value {
        card["body"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["tag"] == "form")
            .unwrap()
            .clone()
    }

    #[test]
    fn todo_manage_card_has_list_body_and_add_form() {
        let card = build_todo_manage_card(
            "「proj」的待办",
            "1. 修复登录\n2. 写文档\n\n<font color='grey'>删除：发送 /todo-rm 后选择本项目。</font>",
            "输入新待办，提交即新增",
            "新增待办",
        );
        assert_eq!(card["schema"], "2.0");
        let elements = card["body"]["elements"].as_array().unwrap();
        // 样式化头部 + hr + markdown 列表正文。
        assert_eq!(elements[0]["text"]["content"], "「proj」的待办");
        assert_eq!(elements[2]["tag"], "markdown");
        assert!(elements[2]["content"]
            .as_str()
            .unwrap()
            .contains("修复登录"));
        // 表单只有输入框 + 提交按钮（无勾选器）。
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        assert_eq!(fe.len(), 2);
        assert_eq!(fe[0]["tag"], "input");
        assert_eq!(fe[0]["name"], INPUT_NAME);
        assert_eq!(fe[1]["tag"], "button");
        assert_eq!(fe[1]["text"]["content"], "新增待办");
        // 提交回调与提问卡同构（parse_card_submit 可解析）。
        assert_eq!(fe[1]["behaviors"][0]["value"]["action"], "submit");
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
            &[],
            "补充说明（可选）",
            "提交",
            "【👍推荐】 ",
        );
        assert_eq!(card["schema"], "2.0");
        // Question card: title is now a body-top "question icon + blue title" row + divider (DingTalk-style), no native header.
        assert!(card.get("header").is_none());
        let elements = card["body"]["elements"].as_array().unwrap();
        assert_eq!(elements[0]["text"]["content"], "Question 1/2");
        assert_eq!(elements[0]["icon"]["token"], "maybe_filled");
        assert_eq!(elements[1]["tag"], "hr");
        // 正文 + 表单容器。多选：勾选器在表单内。
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        // 两个 checker + 一个 input + 一个 submit button。
        assert_eq!(fe.iter().filter(|e| e["tag"] == "checker").count(), 2);
        assert!(fe
            .iter()
            .any(|e| e["tag"] == "input" && e["name"] == "user_input"));
        assert!(fe
            .iter()
            .any(|e| e["tag"] == "button" && e["form_action_type"] == "submit"));
        assert_eq!(fe[0]["name"], "opt_0");
        assert_eq!(fe[1]["name"], "opt_1");
    }

    #[test]
    fn single_moves_checkers_out_of_form_with_toggle_callback() {
        let card = build_question_card(
            "T",
            "Q",
            &plain(&["a", "b"]),
            false,
            true,
            false,
            &[],
            "ph",
            "提交",
            "R",
        );
        let elements = card["body"]["elements"].as_array().unwrap();
        // 勾选器在表单外（顶层），各挂 toggle 回调。
        let top_checkers: Vec<&Value> = elements.iter().filter(|e| e["tag"] == "checker").collect();
        assert_eq!(top_checkers.len(), 2);
        assert_eq!(top_checkers[0]["behaviors"][0]["value"]["action"], "toggle");
        assert_eq!(top_checkers[0]["behaviors"][0]["value"]["index"], 0);
        // 表单内无勾选器（只有 input + submit）。
        let form = form_of(&card);
        assert_eq!(
            form["elements"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["tag"] == "checker")
                .count(),
            0
        );
    }

    #[test]
    fn select_only_omits_input() {
        let card = build_question_card(
            "T",
            "Q",
            &plain(&["a", "b"]),
            false,
            false,
            true,
            &[],
            "ph",
            "提交",
            "R",
        );
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        assert!(!fe.iter().any(|e| e["tag"] == "input"));
        assert!(fe.iter().any(|e| e["tag"] == "button"));
    }

    #[test]
    fn recommended_option_uses_lark_md_prefix() {
        let opts = vec![
            OptionItem::new("继续", true),
            OptionItem::new("停止", false),
        ];
        let card = build_question_card(
            "T",
            "Q",
            &opts,
            true,
            false,
            false,
            &[],
            "ph",
            "提交",
            "<font color='green'>【👍推荐】</font> ",
        );
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        let checkers: Vec<&Value> = fe.iter().filter(|e| e["tag"] == "checker").collect();
        // 勾选器文本用 lark_md，推荐项带绿色前缀。
        assert_eq!(checkers[0]["text"]["tag"], "lark_md");
        assert_eq!(
            checkers[0]["text"]["content"],
            "<font color='green'>【👍推荐】</font> 继续"
        );
        assert_eq!(checkers[1]["text"]["content"], "停止");
        // 提交按下标还原，回传原文。
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "form_value": { "opt_0": true } }
        });
        let s = parse_card_submit(&event, &opts).unwrap();
        assert_eq!(s.selected_options, vec!["继续".to_string()]);
    }

    #[test]
    fn todo_option_uses_amber_marker_but_submits_original_text() {
        let opts = vec![OptionItem::with_todo("执行待办：修复登录", "todo-1")];
        let card = build_question_card_with_todo(
            "T",
            "Q",
            &opts,
            true,
            false,
            false,
            &[],
            "ph",
            "提交",
            "<font color='green'>【👍推荐】</font> ",
            "执行待办：",
            "<font color='orange'>【TODO】</font> ",
        );
        let form = form_of(&card);
        let checker = form["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["tag"] == "checker")
            .unwrap();
        assert_eq!(
            checker["text"]["content"],
            "<font color='orange'>【TODO】</font> 修复登录"
        );
        assert!(!checker["text"]["content"]
            .as_str()
            .unwrap()
            .contains("执行待办："));

        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "form_value": { "opt_0": true } }
        });
        let submitted = parse_card_submit(&event, &opts).unwrap();
        assert_eq!(submitted.selected_options, vec!["执行待办：修复登录"]);
    }

    #[test]
    fn todo_attachment_badge_uses_grey_rich_text() {
        let mut option = OptionItem::with_todo("执行待办：修复登录 【1 个附件】", "todo-1");
        option.todo_attachments = vec![crate::todo_attachments::TodoAttachmentSnapshot {
            id: "attachment-1".into(),
            name: "brief.md".into(),
            path: "/tmp/brief.md".into(),
            source_path: "/tmp/brief.md".into(),
            storage: crate::todo_attachments::TodoAttachmentStorage::Reference,
        }];
        let card = build_question_card_with_todo(
            "T",
            "Q",
            &[option],
            true,
            false,
            false,
            &[],
            "ph",
            "提交",
            "<font color='green'>【👍推荐】</font> ",
            "执行待办：",
            "<font color='orange'>【TODO】</font> ",
        );
        let form = form_of(&card);
        let content = form["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|element| element["tag"] == "checker")
            .unwrap()["text"]["content"]
            .as_str()
            .unwrap();
        assert_eq!(
            content,
            "<font color='orange'>【TODO】</font> 修复登录 <font color='grey'>【1 个附件】</font>"
        );
    }

    #[test]
    fn parse_toggle_reads_index() {
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "value": { "action": "toggle", "index": 2 } }
        });
        let (oid, mid, idx) = parse_toggle(&event).unwrap();
        assert_eq!(oid, "ou_1");
        assert_eq!(mid, "om_1");
        assert_eq!(idx, 2);
        // 提交（带 form_value）不是 toggle。
        let submit = json!({ "action": { "form_value": { "opt_0": true } } });
        assert!(parse_toggle(&submit).is_none());
    }

    #[test]
    fn build_card_without_options_omits_checkers() {
        let card = build_question_card(
            "",
            "随便说点什么",
            &[],
            false,
            false,
            false,
            &[],
            "请输入",
            "提交",
            "【👍推荐】 ",
        );
        assert!(card.get("header").is_none());
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        assert_eq!(fe.iter().filter(|e| e["tag"] == "checker").count(), 0);
        // 非 markdown 正文用 div + plain_text。
        let div = card["body"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["tag"] == "div");
        assert!(div.is_some());
    }

    #[test]
    fn finalized_card_disables_form_and_reflects_selection() {
        // 「停止」为推荐项：显示带前缀，但勾选比对仍按原文命中。
        let opts = vec![
            OptionItem::new("继续", false),
            OptionItem::new("停止", true),
        ];
        let sel = vec!["停止".to_string()];
        let card = build_finalized_card(&Finalized {
            title: "Question 1/2",
            text: "要继续吗？",
            is_markdown: true,
            options: &opts,
            selected: &sel,
            user_input: Some("再想想\n第二行"),
            input_placeholder: "补充说明（可选）",
            note_label: "我的补充：",
            button_label: "已提交",
            recommended_prefix: "【👍推荐】 ",
            single: false,
            select_only: false,
        });
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        // 勾选器：均禁用；仅「停止」勾上。
        let checkers: Vec<&Value> = fe.iter().filter(|e| e["tag"] == "checker").collect();
        assert_eq!(checkers.len(), 2);
        assert_eq!(checkers[0]["checked"], false);
        assert_eq!(checkers[0]["disabled"], true);
        assert_eq!(checkers[1]["checked"], true);
        assert_eq!(checkers[1]["disabled"], true);
        assert_eq!(checkers[1]["text"]["content"], "【👍推荐】 停止");
        // 补充文字：不再是禁用输入框（那种在飞书里选不中），而是小标题 + 引用块，可选中复制。
        assert!(fe.iter().all(|e| e["tag"] != "input"));
        let note = fe.iter().find(|e| e["tag"] == "markdown").unwrap();
        assert_eq!(
            note["content"],
            "<font color='grey'>我的补充：</font>\n> 再想想\n> 第二行"
        );
        // 按钮：禁用 + 改文案 + 无 behaviors。
        let button = fe.iter().find(|e| e["tag"] == "button").unwrap();
        assert_eq!(button["disabled"], true);
        assert_eq!(button["text"]["content"], "已提交");
        assert!(button.get("behaviors").is_none());
    }

    #[test]
    fn finalized_card_without_note_drops_the_whole_note_block() {
        let opts = vec![OptionItem::new("继续", false)];
        let card = build_finalized_card(&Finalized {
            title: "Question 1/1",
            text: "要继续吗？",
            is_markdown: true,
            options: &opts,
            selected: &[],
            user_input: None,
            input_placeholder: "补充说明（可选）",
            note_label: "我的补充：",
            button_label: "已在 弹窗 回答",
            recommended_prefix: "",
            single: false,
            select_only: false,
        });
        let form = form_of(&card);
        let fe = form["elements"].as_array().unwrap();
        assert!(fe.iter().all(|e| e["tag"] != "input"));
        assert!(fe.iter().all(|e| e["tag"] != "markdown"));
    }

    #[test]
    fn parse_submit_maps_checked_options_and_input() {
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": {
                "tag": "button",
                "name": "submit",
                "form_value": {
                    "opt_0": true,
                    "opt_1": false,
                    "opt_2": "true",
                    "user_input": "  hi  "
                }
            }
        });
        let opts = plain(&["A", "B", "C"]);
        let s = parse_card_submit(&event, &opts).unwrap();
        assert_eq!(s.open_id, "ou_1");
        assert_eq!(s.message_id, "om_1");
        assert_eq!(s.selected_options, vec!["A".to_string(), "C".to_string()]);
        assert_eq!(s.user_input.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_submit_single_strict_without_form_value() {
        // 单选严格态：表单内只有提交按钮，飞书不下发 form_value；靠 value.action=="submit" 判定。
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "tag": "button", "name": "submit", "value": { "action": "submit" } }
        });
        let s = parse_card_submit(&event, &plain(&["a", "b"])).unwrap();
        assert_eq!(s.open_id, "ou_1");
        assert_eq!(s.message_id, "om_1");
        assert!(s.selected_options.is_empty()); // 选项由会话 selected_single 兜底
        assert!(s.user_input.is_none());
    }

    #[test]
    fn parse_submit_value_as_json_string() {
        // 部分回调把 value 下发为 JSON 字符串。
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "value": "{\"action\":\"submit\"}" }
        });
        assert!(parse_card_submit(&event, &[]).is_some());
    }

    #[test]
    fn todo_auto_card_keeps_toggle_rows_and_add_form() {
        let view = crate::select::build_view(
            "自动待办".into(),
            vec![crate::select::SelectOption {
                id: "t1".into(),
                dot: None,
                seq: Some(1),
                primary: "跑回归".into(),
                badge: Some("⚡".into()),
                elapsed: None,
                secondary: None,
            }],
            crate::select::SelectAction::TodoAutoEntry,
            crate::i18n::Lang::Zh,
        );
        let card = build_todo_auto_card(&view, "暂无待办", "输入自动待办", "新增自动待办");
        let elements = card["body"]["elements"].as_array().unwrap();
        assert!(elements.iter().any(|element| {
            element
                .pointer("/columns/1/elements/0/behaviors/0/value/select")
                .and_then(Value::as_u64)
                == Some(0)
        }));
        let form = form_of(&card);
        assert_eq!(form["elements"][0]["name"], INPUT_NAME);
        assert_eq!(form["elements"][1]["text"]["content"], "新增自动待办");
    }

    #[test]
    fn empty_todo_auto_card_still_has_add_form() {
        let view = crate::select::build_view(
            "自动待办".into(),
            Vec::new(),
            crate::select::SelectAction::TodoAutoEntry,
            crate::i18n::Lang::Zh,
        );
        let card = build_todo_auto_card(&view, "暂无待办", "输入自动待办", "新增自动待办");
        assert!(card["body"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|element| element.to_string().contains("暂无待办")));
        assert_eq!(form_of(&card)["elements"][0]["name"], INPUT_NAME);
    }

    #[test]
    fn parse_submit_empty_input_is_none() {
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "form_value": { "user_input": "" } }
        });
        let s = parse_card_submit(&event, &[]).unwrap();
        assert!(s.user_input.is_none());
        assert!(s.selected_options.is_empty());
    }

    #[test]
    fn parse_non_form_returns_none() {
        let event = json!({
            "operator": { "open_id": "ou_1" },
            "context": { "open_message_id": "om_1" },
            "action": { "tag": "button", "value": { "action": "noop" } }
        });
        assert!(parse_card_submit(&event, &[]).is_none());
    }

    // ===== /watch 实时状态卡 =====

    fn watch_view(buttons: WatchButtons) -> WatchCardView {
        WatchCardView {
            header: "实时关注 [3] Cursor — HumanInLoop".into(),
            state_line: "🟢 工作中 · 已 6 分钟".into(),
            title_line: Some("「重构空闲退出」".into()),
            activity_heading: "最近动态（14:32:05）：".into(),
            text: Some("正在跑单测".into()),
            step_lines: vec![
                "<font color='grey'>●</font> **读取**: *registry.rs*".into(),
                "<font color='green'>●</font> **运行命令**: *cargo test*".into(),
            ],
            todo_summary: None,
            todo_lines: Vec::new(),
            no_activity: None,
            updated_line: "最后更新 14:32:07".into(),
            buttons,
        }
    }

    #[test]
    fn watch_card_layout_and_active_buttons() {
        let card = build_watch_card(&watch_view(WatchButtons::Active {
            unwatch: "取消关注".into(),
            refresh: "立即刷新".into(),
        }));
        assert_eq!(card["schema"], "2.0");
        assert_eq!(card["config"]["update_multi"], true);
        let elements = card["body"]["elements"].as_array().unwrap();
        // 样式化头部行（eye icon + 蓝色小字）+ 分隔线。
        assert_eq!(elements[0]["icon"]["token"], "eye_outlined");
        assert_eq!(
            elements[0]["text"]["content"],
            "实时关注 [3] Cursor — HumanInLoop"
        );
        assert_eq!(elements[1]["tag"], "hr");
        // 状态行加粗（含回合统计）+ 标题；活动区带绝对时刻标签、文字 + 空行 + 彩色圆点足迹时间线。
        let head_md = elements[2]["content"].as_str().unwrap();
        assert!(head_md.contains("**🟢 工作中 · 已 6 分钟**"));
        assert!(head_md.contains("「重构空闲退出」"));
        let act_md = elements[3]["content"].as_str().unwrap();
        assert!(act_md.contains("最近动态（14:32:05）："));
        assert!(act_md.contains(
            "正在跑单测\n\n<font color='grey'>●</font> **读取**: *registry.rs*\n<font color='green'>●</font> **运行命令**: *cargo test*"
        ));
        // 底部：hr + 灰色更新时刻 + column_set 两按钮。
        assert_eq!(elements[4]["tag"], "hr");
        assert_eq!(elements[5]["text"]["content"], "最后更新 14:32:07");
        assert_eq!(elements[5]["text"]["text_color"], "grey");
        let cols = elements[6]["columns"].as_array().unwrap();
        assert_eq!(cols.len(), 2);
        let b0 = &cols[0]["elements"][0];
        assert_eq!(b0["type"], "danger");
        assert_eq!(b0["behaviors"][0]["value"]["watch"], "unwatch");
        let b1 = &cols[1]["elements"][0];
        assert_eq!(b1["behaviors"][0]["value"]["watch"], "refresh");
    }

    #[test]
    fn watch_card_final_disables_button() {
        let card = build_watch_card(&watch_view(WatchButtons::Final {
            label: "已结束 · 已自动取消关注".into(),
        }));
        let elements = card["body"]["elements"].as_array().unwrap();
        let btn = elements.last().unwrap();
        assert_eq!(btn["tag"], "button");
        assert_eq!(btn["disabled"], true);
        assert_eq!(btn["text"]["content"], "已结束 · 已自动取消关注");
        assert!(btn.get("behaviors").is_none());
    }

    #[test]
    fn watch_card_no_activity_placeholder() {
        let mut view = watch_view(WatchButtons::Final { label: "x".into() });
        view.text = None;
        view.step_lines = Vec::new();
        view.no_activity = Some("（暂无可解析的活动）".into());
        let card = build_watch_card(&view);
        let elements = card["body"]["elements"].as_array().unwrap();
        let act_md = elements[3]["content"].as_str().unwrap();
        assert!(act_md.contains("（暂无可解析的活动）"));
        assert!(!act_md.contains("cargo"));
        assert!(!act_md.contains("●"));
    }

    #[test]
    fn watch_card_todo_collapsible_panel() {
        // 有清单 → 活动区之后插入默认收起的折叠面板：标题即摘要行，内容为全清单 markdown。
        let mut view = watch_view(WatchButtons::Active {
            unwatch: "取消关注".into(),
            refresh: "立即刷新".into(),
        });
        view.todo_summary = Some("📋 TODO 1/3 · 当前：跑单测".into());
        view.todo_lines = vec![
            "<font color='grey'>●</font> ~~改 registry~~".into(),
            "<font color='green'>●</font> **跑单测**".into(),
            "○ 更新文档".into(),
        ];
        let card = build_watch_card(&view);
        let elements = card["body"]["elements"].as_array().unwrap();
        let panel = &elements[4];
        assert_eq!(panel["tag"], "collapsible_panel");
        assert_eq!(panel["expanded"], false);
        assert_eq!(
            panel["header"]["title"]["content"],
            "📋 TODO 1/3 · 当前：跑单测"
        );
        let inner = panel["elements"][0]["content"].as_str().unwrap();
        assert!(inner.contains("~~改 registry~~"));
        assert!(inner.contains("**跑单测**"));
        assert!(inner.contains("○ 更新文档"));
        // 面板之后仍是 hr + 更新时刻 + 按钮。
        assert_eq!(elements[5]["tag"], "hr");
        assert_eq!(elements[6]["text"]["content"], "最后更新 14:32:07");
        // 无清单 → 不出面板。
        let card2 = build_watch_card(&watch_view(WatchButtons::Final { label: "x".into() }));
        let els2 = card2["body"]["elements"].as_array().unwrap();
        assert!(els2.iter().all(|e| e["tag"] != "collapsible_panel"));
    }

    #[test]
    fn parse_watch_action_maps_value() {
        let ev = |action: &str| {
            json!({
                "operator": { "open_id": "ou_1" },
                "context": { "open_message_id": "om_w" },
                "action": { "tag": "button", "value": { "watch": action } }
            })
        };
        assert_eq!(
            parse_watch_action(&ev("unwatch")),
            Some(("om_w".to_string(), WatchAction::Unwatch))
        );
        assert_eq!(
            parse_watch_action(&ev("refresh")),
            Some(("om_w".to_string(), WatchAction::Refresh))
        );
        // value 为 JSON 字符串（飞书部分回调形态）。
        let ev_str = json!({
            "context": { "open_message_id": "om_w" },
            "action": { "value": "{\"watch\":\"refresh\"}" }
        });
        assert_eq!(
            parse_watch_action(&ev_str),
            Some(("om_w".to_string(), WatchAction::Refresh))
        );
        // 非 watch 卡回调 → None。
        let other = json!({
            "context": { "open_message_id": "om_q" },
            "action": { "value": { "action": "submit" } }
        });
        assert!(parse_watch_action(&other).is_none());
    }

    // ===== 通用单选卡 =====

    #[test]
    fn select_card_renders_row_per_option_with_index_callback() {
        let view = crate::select::SelectView {
            title: "选择要实时关注的 Agent".into(),
            options: vec![
                crate::select::SelectOption {
                    id: "s-a".into(),
                    dot: Some(crate::select::SelectDot::Working),
                    seq: Some(2),
                    primary: "Cursor · my-frontend".into(),
                    badge: Some("· 关注中".into()),
                    elapsed: Some("· 累计工作 6 分钟".into()),
                    secondary: Some("甲".into()),
                },
                crate::select::SelectOption {
                    id: "s-b".into(),
                    dot: Some(crate::select::SelectDot::Idle),
                    seq: Some(1),
                    primary: "Claude Code · api-server".into(),
                    badge: None,
                    elapsed: None,
                    secondary: Some("乙".into()),
                },
            ],
            truncated_note: None,
            action: crate::select::SelectAction::Watch,
        };
        let card = build_select_card(&view);
        let els = card["body"]["elements"].as_array().unwrap();
        // 每个选项一行 column_set；行间以 hr 分隔。
        let rows: Vec<&Value> = els.iter().filter(|e| e["tag"] == "column_set").collect();
        assert_eq!(rows.len(), 2);
        let expected_label =
            crate::select::SelectAction::Watch.button_label(crate::i18n::Lang::current());
        let btn0 = &rows[0]["columns"][1]["elements"][0];
        assert_eq!(btn0["tag"], "button");
        assert_eq!(btn0["type"], "primary");
        assert_eq!(btn0["text"]["content"], expected_label.as_str());
        assert_eq!(btn0["behaviors"][0]["value"]["select"], 0);
        assert_eq!(
            rows[1]["columns"][1]["elements"][0]["behaviors"][0]["value"]["select"],
            1
        );
        // 左列富文本：圆点 + 加粗编号 + 主文本 + 徽标 + 灰色次行。
        let md0 = rows[0]["columns"][0]["elements"][0]["content"]
            .as_str()
            .unwrap();
        assert!(md0.contains("<font color='green'>●</font>"));
        // 主行：编号 + 主文本 + 关注徽标 + 运行时长（徽标之后）。
        assert!(md0.contains("**[2]** Cursor · my-frontend · 关注中 · 累计工作 6 分钟"));
        assert!(md0.contains("<font color='grey'>甲</font>"));
        let md1 = rows[1]["columns"][0]["elements"][0]["content"]
            .as_str()
            .unwrap();
        assert!(md1.contains("<font color='grey'>●</font>"));
        assert!(!md1.contains("关注中"));
    }

    #[test]
    fn select_card_renders_todo_attachment_badge_in_grey() {
        let view = crate::select::SelectView {
            title: "选择任务来源".into(),
            options: vec![crate::select::SelectOption {
                id: "todo:1".into(),
                dot: None,
                seq: Some(1),
                primary: "执行待办：修复登录 【2 个附件】".into(),
                badge: None,
                elapsed: None,
                secondary: None,
            }],
            truncated_note: None,
            action: crate::select::SelectAction::TaskInputSource,
        };
        let card = build_select_card(&view);
        let row = card["body"]["elements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|element| element["tag"] == "column_set")
            .unwrap();
        let markdown = row["columns"][0]["elements"][0]["content"]
            .as_str()
            .unwrap();
        assert!(markdown.contains("执行待办：修复登录 <font color='grey'>【2 个附件】</font>"));
    }

    #[test]
    fn parse_select_action_reads_index() {
        // 数字下标。
        let ev = json!({
            "context": { "open_message_id": "om_s" },
            "action": { "value": { "select": 3 } }
        });
        assert_eq!(parse_select_action(&ev), Some(("om_s".to_string(), 3)));
        // value 为 JSON 字符串 + 字符串下标。
        let ev_str = json!({
            "context": { "open_message_id": "om_s" },
            "action": { "value": "{\"select\":\"5\"}" }
        });
        assert_eq!(parse_select_action(&ev_str), Some(("om_s".to_string(), 5)));
        // 非单选卡回调（watch / submit）→ None。
        let watch = json!({
            "context": { "open_message_id": "om_w" },
            "action": { "value": { "watch": "refresh" } }
        });
        assert!(parse_select_action(&watch).is_none());
    }

    #[test]
    fn confirm_card_uses_roles_and_wire_slots() {
        let view = crate::confirm::ConfirmView {
            title: "Approve?".into(),
            body: "Run command".into(),
            confirm: crate::confirm::ConfirmAction {
                id: "approve_once".into(),
                label: "Approve once".into(),
                role: crate::confirm::ActionRole::Primary,
            },
            cancel: crate::confirm::ConfirmAction {
                id: "deny".into(),
                label: "Deny".into(),
                role: crate::confirm::ActionRole::Destructive,
            },
        };
        let card = build_confirm_card(&view);
        let actions = &card["body"]["elements"][3]["columns"];
        assert_eq!(actions[0]["elements"][0]["type"], "primary");
        assert_eq!(
            actions[0]["elements"][0]["behaviors"][0]["value"]["confirm"],
            "ok"
        );
        assert_eq!(actions[1]["elements"][0]["type"], "danger");
        assert_eq!(
            actions[1]["elements"][0]["behaviors"][0]["value"]["confirm"],
            "cancel"
        );
    }

    #[test]
    fn parse_confirm_action_maps_only_wire_slots() {
        let primary = json!({
            "context": { "open_message_id": "om_confirm" },
            "action": { "value": { "confirm": "ok" } }
        });
        assert_eq!(
            parse_confirm_action(&primary),
            Some((
                "om_confirm".to_string(),
                crate::confirm::ConfirmSlot::Primary
            ))
        );

        let secondary = json!({
            "context": { "open_message_id": "om_confirm" },
            "action": { "value": "{\"confirm\":\"cancel\"}" }
        });
        assert_eq!(
            parse_confirm_action(&secondary),
            Some((
                "om_confirm".to_string(),
                crate::confirm::ConfirmSlot::Secondary
            ))
        );

        let injected = json!({
            "context": { "open_message_id": "om_confirm" },
            "action": { "value": { "confirm": "approve_once" } }
        });
        assert!(parse_confirm_action(&injected).is_none());
    }

    #[test]
    fn msg_compose_card_has_one_active_input_and_preserves_draft() {
        let view = crate::msg_card::MsgComposeView {
            seq: 3,
            title: "给 [3] Codex 发送消息".into(),
            target_label: "目标".into(),
            target: "Codex — project".into(),
            pending_label: "待送达 1 条：".into(),
            pending_preview: Some(crate::msg_card::PendingPreview {
                text: "<pending>".into(),
                omitted_chars: 0,
            }),
            preview_omitted: None,
            input_label: "消息".into(),
            input_placeholder: "请输入".into(),
            send_label: "发送".into(),
            error: Some("太长".into()),
        };
        let card = build_msg_compose_card(&view, Some("draft"));
        let elements = card["body"]["elements"].as_array().unwrap();
        let form = elements.iter().find(|item| item["tag"] == "form").unwrap();
        let controls = form["elements"].as_array().unwrap();
        assert_eq!(
            controls
                .iter()
                .filter(|item| item["tag"] == "input")
                .count(),
            1
        );
        assert_eq!(
            controls
                .iter()
                .filter(|item| item["tag"] == "button")
                .count(),
            1
        );
        assert_eq!(controls[0]["default_value"], "draft");
        assert!(elements.iter().any(|item| item["text"]["content"]
            .as_str()
            .unwrap_or("")
            .contains("<pending>")));
    }
}
