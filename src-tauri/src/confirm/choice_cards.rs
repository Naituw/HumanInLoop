//! Pure structured-confirmation card builders and callback parsers.
//!
//! Interactive controls carry only short wire indices. Full labels/descriptions remain in static
//! content so platform control limits cannot truncate the security-relevant permission scope.

use crate::confirm::ActionRole;
use crate::i18n::Lang;
use crate::models::{ConfirmFieldKind, ConfirmRequest};
use serde_json::{json, Value};

const SELECT_PREFIX: &str = "confirm_select_";
const SUBMIT_ACTION: &str = "confirm_submit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardAction {
    Select {
        actor: String,
        message_id: String,
        index: usize,
        comment: Option<String>,
    },
    Submit {
        actor: String,
        message_id: String,
        index: Option<usize>,
        comment: Option<String>,
    },
}

/// D14: option taps only select (draft); a separate submit callback finalizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramAction {
    Select(usize),
    Submit,
}

fn bounded(input: &str, max: usize) -> String {
    let mut chars = input.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}\n\n… [truncated]")
    } else {
        head
    }
}

fn context_markdown(request: &ConfirmRequest) -> String {
    request
        .context
        .iter()
        .map(|field| {
            let value = match field.kind {
                ConfirmFieldKind::Path => format!("`{}`", field.value.replace('`', "\\`")),
                _ => field.value.clone(),
            };
            format!("**{}:** {}", field.label, value)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn reason_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "理由：",
        Lang::En => "Reason:",
    }
}

pub(crate) fn is_task_input_form(request: &ConfirmRequest) -> bool {
    request.presentation.input().is_some_and(|input| {
        input.max_chars > 1000 && request.presentation.default_action_id().is_some()
    })
}

/// Prefix-tier variants (D51): cards have no level selector, so only the recommended
/// variant of each group is visible on card surfaces.
fn variant_visible(choice: &crate::models::ConfirmChoice) -> bool {
    choice
        .variant
        .as_ref()
        .is_none_or(|variant| variant.recommended)
}

/// Task forms hide their implementation-only start/cancel actions. When TODO choices are
/// present, expose the manual action plus every non-destructive TODO action.
///
/// Returned values stay wire indices into the full choices list (non-recommended
/// prefix-tier variants are skipped), so callback resolution is unaffected.
pub(crate) fn task_choice_indices(request: &ConfirmRequest) -> Vec<usize> {
    if !is_task_input_form(request) {
        return request
            .choices
            .iter()
            .enumerate()
            .filter_map(|(index, choice)| variant_visible(choice).then_some(index))
            .collect();
    }
    let indices: Vec<_> = request
        .choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| {
            (choice.role != ActionRole::Destructive && variant_visible(choice)).then_some(index)
        })
        .collect();
    if indices.len() > 1 {
        indices
    } else {
        Vec::new()
    }
}

pub(crate) fn is_direct_task_confirmation(request: &ConfirmRequest) -> bool {
    is_task_input_form(request)
        && task_choice_indices(request).is_empty()
        && request
            .presentation
            .input()
            .is_some_and(|input| input.always_visible && !input.requires_value())
}

pub(crate) fn compact_tool_markdown(request: &ConfirmRequest, max: usize, lang: Lang) -> String {
    let mut body = String::new();
    if !request.detail.summary.trim().is_empty() {
        body.push_str(&format!(
            "**{}** {}\n\n",
            reason_label(lang),
            request.detail.summary
        ));
    }
    body.push_str(&format!("**{}**", tool_name(request)));
    if !request.detail.body_md.trim().is_empty() {
        body.push_str("\n\n");
        body.push_str(&request.detail.body_md);
    }
    bounded(&body, max)
}

pub(crate) fn request_markdown(request: &ConfirmRequest, max: usize) -> String {
    let mut body = context_markdown(request);
    if !body.is_empty() {
        body.push_str("\n\n");
    }
    body.push_str(&format!("**{}**", request.detail.summary));
    if !request.detail.body_md.trim().is_empty() {
        body.push_str("\n\n");
        body.push_str(&request.detail.body_md);
    }
    bounded(&body, max)
}

fn input_for_selected(
    request: &ConfirmRequest,
    selected: Option<usize>,
) -> Option<&crate::models::ConfirmInput> {
    let selected_id = selected
        .and_then(|index| request.choices.get(index))
        .map(|choice| choice.id.as_str());
    request.presentation.input().filter(|input| {
        input.always_visible || selected_id == Some(input.visible_when_action_id.as_str())
    })
}

pub(crate) fn tool_name(request: &ConfirmRequest) -> &str {
    request
        .context
        .iter()
        .find(|field| field.id == "tool")
        .map(|field| field.value.as_str())
        .unwrap_or("Tool")
}

fn feishu_tool_elements(request: &ConfirmRequest, lang: Lang) -> Vec<Value> {
    if is_task_input_form(request) {
        let mut content = request.detail.summary.clone();
        if !request.detail.body_md.trim().is_empty() {
            content.push_str("\n\n");
            content.push_str(&request.detail.body_md);
        }
        return vec![json!({ "tag": "markdown", "content": bounded(&content, 12_000) })];
    }
    let mut elements = Vec::new();
    if !request.detail.summary.trim().is_empty() {
        elements.push(json!({
            "tag": "markdown",
            "content": format!("**{}** {}", reason_label(lang), request.detail.summary),
        }));
    }
    elements.push(json!({
        "tag": "markdown",
        "content": format!("**{}**", tool_name(request)),
    }));
    if !request.detail.body_md.trim().is_empty() {
        elements.push(json!({
            "tag": "markdown",
            "content": bounded(&request.detail.body_md, 12_000),
        }));
    }
    elements
}

fn feishu_choice_text(choice: &crate::models::ConfirmChoice, lang: Lang) -> String {
    let label = if choice.id.starts_with("todo:") {
        let text_prefix = crate::i18n::tr(lang, "whatsNext.todoPrefix");
        let text = choice
            .label
            .strip_prefix(text_prefix)
            .unwrap_or(&choice.label);
        format!(
            "{}{}",
            crate::i18n::tr(lang, "channel.feishuTodoPrefix"),
            text
        )
    } else {
        choice.label.clone()
    };
    if choice.description.trim().is_empty() {
        label
    } else {
        format!(
            "**{}**\n<font color='grey'>{}</font>",
            label, choice.description
        )
    }
}

pub fn feishu_card(
    request: &ConfirmRequest,
    selected: Option<usize>,
    comment: &str,
    lang: Lang,
) -> Value {
    let mut elements = feishu_tool_elements(request, lang);
    elements.push(json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }));
    let choice_indices = task_choice_indices(request);
    if !is_task_input_form(request) || !choice_indices.is_empty() {
        for index in choice_indices {
            let choice = &request.choices[index];
            let checked = selected == Some(index);
            let color = if checked {
                Some(if choice.role == ActionRole::Destructive {
                    "red"
                } else {
                    "blue"
                })
            } else {
                None
            };
            elements.push(crate::feishu::card::styled_checker(
                &format!("confirm_choice_{index}"),
                &feishu_choice_text(choice, lang),
                checked,
                false,
                Some(json!({ "confirm": "select", "index": index })),
                color,
            ));
        }
    }
    let mut form_elements = Vec::new();
    if let Some(input) = input_for_selected(request, selected) {
        form_elements.push(json!({
            "tag": "input",
            "name": input.id,
            "label": { "tag": "plain_text", "content": input.label },
            "placeholder": { "tag": "plain_text", "content": input.placeholder },
            "default_value": bounded(comment, input.max_chars),
        }));
    }
    form_elements.push(json!({
        "tag": "button",
        "name": "confirm_submit",
        "form_action_type": "submit",
        "type": "primary",
        "disabled": selected.is_none(),
        "text": { "tag": "plain_text", "content": request.presentation.submit_label() },
        "behaviors": [{ "type": "callback", "value": { "confirm": "submit" } }],
    }));
    elements.push(json!({ "tag": "form", "name": "confirm_form", "elements": form_elements }));
    crate::feishu::card::assemble_styled_card(&request.title, elements)
}

pub fn feishu_final_card(request: &ConfirmRequest, status: &str, lang: Lang) -> Value {
    let mut elements = feishu_tool_elements(request, lang);
    elements.push(json!({ "tag": "hr", "margin": "0px 0px 0px 0px" }));
    elements.push(json!({
        "tag": "div",
        "text": {
            "tag": "plain_text",
            "content": status,
            "text_size": "notation",
            "text_color": "grey",
        },
    }));
    crate::feishu::card::assemble_styled_card(&request.title, elements)
}

fn value_object(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str(text).ok(),
        other => Some(other.clone()),
    }
}

pub fn parse_feishu_action(event: &Value, input_id: Option<&str>) -> Option<CardAction> {
    let actor = event.get("operator")?.get("open_id")?.as_str()?.to_string();
    let message_id = event
        .get("context")?
        .get("open_message_id")?
        .as_str()?
        .to_string();
    let action = event.get("action")?;
    let value = value_object(action.get("value")?)?;
    match value.get("confirm").and_then(Value::as_str)? {
        "select" => Some(CardAction::Select {
            actor,
            message_id,
            index: value.get("index")?.as_u64()? as usize,
            comment: input_id
                .and_then(|id| action.get("form_value").and_then(|form| form.get(id)))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_string),
        }),
        "submit" => {
            let comment = input_id
                .and_then(|id| action.get("form_value").and_then(|form| form.get(id)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string);
            Some(CardAction::Submit {
                actor,
                message_id,
                index: None,
                comment,
            })
        }
        _ => None,
    }
}

fn slack_escape(text: &str) -> String {
    crate::slack::markdown::escape(text)
}

fn slack_control_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn slack_blocks(
    request: &ConfirmRequest,
    selected: Option<usize>,
    comment: &str,
    lang: Lang,
) -> Value {
    let mut blocks = vec![crate::slack::blockkit::title_block(&request.title)];
    if is_task_input_form(request) {
        let mut content = slack_escape(&request.detail.summary);
        if !request.detail.body_md.trim().is_empty() {
            content.push_str("\n\n");
            content.push_str(&crate::slack::markdown::to_mrkdwn(&bounded(
                &request.detail.body_md,
                2800,
            )));
        }
        blocks.push(crate::slack::blockkit::mrkdwn_section(&content));
    } else {
        if !request.detail.summary.trim().is_empty() {
            blocks.push(crate::slack::blockkit::mrkdwn_section(&format!(
                "*{}* {}",
                slack_escape(reason_label(lang)),
                slack_escape(&request.detail.summary)
            )));
        }
        blocks.push(crate::slack::blockkit::mrkdwn_section(&format!(
            "*{}*",
            slack_escape(tool_name(request))
        )));
        if !request.detail.body_md.trim().is_empty() {
            blocks.push(crate::slack::blockkit::mrkdwn_section(
                &crate::slack::markdown::to_mrkdwn(&bounded(&request.detail.body_md, 2800)),
            ));
        }
    }

    let choice_indices = task_choice_indices(request);

    // Security-relevant scope details stay static and untruncated by radio option limits.
    if !is_task_input_form(request) {
        for index in &choice_indices {
            let choice = &request.choices[*index];
            let inline_detail = choice.description.replace('\n', " · ");
            if inline_detail.chars().count() > 75 || choice.label.chars().count() > 75 {
                let mut detail = format!("*{}*", slack_escape(&choice.label));
                if !choice.description.trim().is_empty() {
                    detail.push_str(&format!("\n{}", slack_escape(&choice.description)));
                }
                blocks.push(crate::slack::blockkit::mrkdwn_section(&detail));
            }
        }
    }

    let options: Vec<Value> = choice_indices
        .iter()
        .map(|index| {
            let choice = &request.choices[*index];
            let mut option = json!({
                "text": { "type": "plain_text", "text": slack_control_text(&choice.label, 75) },
                "value": index.to_string(),
            });
            if !choice.description.trim().is_empty() {
                option["description"] = json!({
                    "type": "plain_text",
                    "text": slack_control_text(&choice.description.replace('\n', " · "), 75),
                });
            }
            option
        })
        .collect();
    let mut radio = json!({
        "type": "radio_buttons",
        "action_id": "confirm_choice",
        "options": options,
    });
    if let Some(position) = selected.and_then(|selected| {
        choice_indices
            .iter()
            .position(|wire_index| *wire_index == selected)
    }) {
        radio["initial_option"] = radio["options"][position].clone();
    }
    if !is_task_input_form(request) || !choice_indices.is_empty() {
        blocks.push(json!({
            "type": "input",
            "block_id": "confirm_choice_block",
            "optional": true,
            "label": {
                "type": "plain_text",
                "text": if lang == Lang::Zh { "决定" } else { "Decision" },
            },
            "element": radio,
        }));
    }

    if let Some(input) = request.presentation.input() {
        blocks.push(json!({
            "type": "input",
            "block_id": "confirm_reason",
            "optional": true,
            "label": {
                "type": "plain_text",
                "text": if is_task_input_form(request) { input.label.as_str() } else if lang == Lang::Zh { "拒绝原因（可选，仅拒绝时发送）" } else { "Denial reason (optional; sent only when denying)" },
            },
            "element": {
                "type": "plain_text_input",
                "action_id": input.id,
                "multiline": true,
                "initial_value": bounded(comment, input.max_chars),
                "placeholder": { "type": "plain_text", "text": bounded(&input.placeholder, 145) },
            },
        }));
    }
    blocks.push(json!({
        "type": "actions",
        "elements": [{
            "type": "button",
            "action_id": SUBMIT_ACTION,
            "value": "submit",
            "style": "primary",
            "text": { "type": "plain_text", "text": bounded(request.presentation.submit_label(), 70) },
        }],
    }));
    Value::Array(blocks)
}

pub fn slack_final_blocks(request: &ConfirmRequest, status: &str, lang: Lang) -> Value {
    let mut blocks = vec![crate::slack::blockkit::title_block(&request.title)];
    if is_task_input_form(request) {
        blocks.push(crate::slack::blockkit::mrkdwn_section(&slack_escape(
            &request.detail.summary,
        )));
    } else {
        if !request.detail.summary.trim().is_empty() {
            blocks.push(crate::slack::blockkit::mrkdwn_section(&format!(
                "*{}* {}",
                slack_escape(reason_label(lang)),
                slack_escape(&request.detail.summary)
            )));
        }
        blocks.push(crate::slack::blockkit::mrkdwn_section(&format!(
            "*{}*",
            slack_escape(tool_name(request))
        )));
        if !request.detail.body_md.trim().is_empty() {
            blocks.push(crate::slack::blockkit::mrkdwn_section(
                &crate::slack::markdown::to_mrkdwn(&bounded(&request.detail.body_md, 2800)),
            ));
        }
    }
    blocks.push(json!({
        "type": "context",
        "elements": [{ "type": "mrkdwn", "text": slack_escape(status) }],
    }));
    Value::Array(blocks)
}

pub fn parse_slack_action(payload: &Value, input_id: Option<&str>) -> Option<CardAction> {
    let actor = payload.get("user")?.get("id")?.as_str()?.to_string();
    let message_id = payload
        .get("container")
        .and_then(|container| container.get("message_ts"))
        .or_else(|| payload.get("message").and_then(|message| message.get("ts")))?
        .as_str()?
        .to_string();
    let action_id = payload
        .get("actions")?
        .as_array()?
        .first()?
        .get("action_id")?
        .as_str()?;
    if let Some(index) = action_id
        .strip_prefix(SELECT_PREFIX)
        .and_then(|value| value.parse().ok())
    {
        return Some(CardAction::Select {
            actor,
            message_id,
            index,
            comment: input_id
                .and_then(|id| {
                    payload
                        .get("state")?
                        .get("values")?
                        .as_object()?
                        .values()
                        .find_map(|actions| actions.get(id)?.get("value")?.as_str())
                })
                .map(str::trim)
                .map(str::to_string),
        });
    }
    if action_id != SUBMIT_ACTION {
        return None;
    }
    let index = payload
        .get("state")
        .and_then(|state| state.get("values"))
        .and_then(Value::as_object)
        .and_then(|blocks| {
            blocks.values().find_map(|actions| {
                actions
                    .get("confirm_choice")?
                    .get("selected_option")?
                    .get("value")?
                    .as_str()?
                    .parse::<usize>()
                    .ok()
            })
        });
    let comment = input_id
        .and_then(|id| {
            payload
                .get("state")?
                .get("values")?
                .as_object()
                .and_then(|blocks| {
                    blocks
                        .values()
                        .find_map(|actions| actions.get(id)?.get("value")?.as_str())
                })
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Some(CardAction::Submit {
        actor,
        message_id,
        index,
        comment,
    })
}

pub fn telegram_html(
    request: &ConfirmRequest,
    selected: Option<usize>,
    comment: &str,
    status: Option<&str>,
    lang: Lang,
) -> String {
    use crate::telegram::markdown::{escape_html, to_html};
    let mut out = format!("<b>❓ {}</b>", escape_html(&request.title));
    if is_task_input_form(request) {
        out.push_str(&format!(
            "\n\n{}",
            to_html(&bounded(&request.detail.summary, 2200))
        ));
        if !request.detail.body_md.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(&to_html(&bounded(&request.detail.body_md, 1200)));
        }
    } else {
        if !request.detail.summary.trim().is_empty() {
            out.push_str(&format!(
                "\n\n<b>{}</b> {}",
                escape_html(reason_label(lang)),
                escape_html(&request.detail.summary)
            ));
        }
        out.push_str(&format!("\n\n<b>{}</b>", escape_html(tool_name(request))));
        if !request.detail.body_md.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(&to_html(&bounded(&request.detail.body_md, 2200)));
        }
    }
    let full_labels = telegram_uses_full_labels(request);
    let choice_indices = task_choice_indices(request);
    let show_choice_list = !full_labels
        || choice_indices
            .iter()
            .any(|index| !request.choices[*index].description.trim().is_empty());
    if show_choice_list && (!is_task_input_form(request) || !choice_indices.is_empty()) {
        // Numbered markers follow the visible position (keyboard buttons match).
        for (position, index) in choice_indices.into_iter().enumerate() {
            let choice = &request.choices[index];
            let marker = if full_labels {
                "•".to_string()
            } else {
                crate::channels::telegram::option_label(position)
            };
            out.push_str(&format!(
                "\n\n{} <b>{}</b>",
                marker,
                escape_html(&choice.label)
            ));
            if !choice.description.trim().is_empty() {
                out.push_str(&format!("\n<i>{}</i>", escape_html(&choice.description)));
            }
        }
    }
    if let Some(input) = input_for_selected(request, selected) {
        if !comment.trim().is_empty() {
            out.push_str(&format!(
                "\n\n<b>{}:</b> {}",
                escape_html(&input.label),
                escape_html(&bounded(comment, input.max_chars))
            ));
        }
    }
    if status.is_none() && comment.trim().is_empty() && request.presentation.input().is_some() {
        let required = request
            .presentation
            .input()
            .is_some_and(|input| input.requires_value());
        let hint = match (is_task_input_form(request), required, lang) {
            (true, true, Lang::Zh) => "请直接回复本消息输入任务。",
            (true, true, Lang::En) => "Reply directly to this message with the task.",
            (true, false, Lang::Zh) => "可直接启动；如需补充，请回复本消息后再点击“启动任务”。",
            (true, false, Lang::En) => {
                "Start directly, or reply with extra instructions before starting."
            }
            (false, _, Lang::Zh) => "如需说明拒绝原因，请先回复本消息，再选择“拒绝”并提交。",
            (false, _, Lang::En) => {
                "To include a denial reason, reply to this message before selecting Deny and submitting."
            }
        };
        out.push_str(&format!("\n\n<i>{}</i>", escape_html(hint)));
    }
    if let Some(status) = status {
        out.push_str(&format!("\n\n<i>{}</i>", escape_html(status)));
    }
    bounded(&out, 3900)
}

fn telegram_uses_full_labels(request: &ConfirmRequest) -> bool {
    // Same visibility set as telegram_keyboard (variant filter only).
    let visible: Vec<&crate::models::ConfirmChoice> = request
        .choices
        .iter()
        .filter(|choice| variant_visible(choice))
        .collect();
    visible.len() <= 3
        && visible
            .iter()
            .all(|choice| choice.label.chars().count() <= 12)
        && visible
            .iter()
            .map(|choice| choice.label.chars().count())
            .sum::<usize>()
            <= 24
}

pub fn telegram_keyboard(request: &ConfirmRequest, selected: Option<usize>) -> Value {
    // Task forms keep every actionable button (the html list is hidden instead), so
    // only non-recommended prefix-tier variants are skipped here. Buttons show visible
    // positions; callbacks carry the wire index (pc:do:<wire>), so hidden variants
    // never shift submission resolution.
    let indices: Vec<usize> = request
        .choices
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| variant_visible(choice).then_some(index))
        .collect();
    let full_labels = telegram_uses_full_labels(request);
    let width = if full_labels {
        indices.len().max(1)
    } else {
        crate::channels::telegram::KEYBOARD_ROW_WIDTH
    };
    let mut rows = indices
        .chunks(width)
        .enumerate()
        .map(|(chunk_position, indices)| {
            Value::Array(
            indices
                .iter()
                .enumerate()
                .map(|(offset, index)| {
                    let label = if full_labels {
                        request.choices[*index].label.clone()
                    } else {
                        crate::channels::telegram::option_label(chunk_position * width + offset)
                    };
                    json!({
                        "text": if selected == Some(*index) { format!("✅ {label}") } else { label },
                        "callback_data": format!("pc:do:{index}"),
                    })
                })
                .collect(),
        )
        })
        .collect::<Vec<_>>();
    // D14: taps only select; the submit row appears once a draft selection exists
    // (Telegram has no disabled buttons, so the row is hidden instead).
    if selected.is_some() && !is_direct_task_confirmation(request) {
        rows.push(Value::Array(vec![json!({
            "text": request.presentation.submit_label(),
            "callback_data": "pc:submit",
        })]));
    }
    json!({ "inline_keyboard": rows })
}

pub fn parse_telegram_callback(data: &str) -> Option<TelegramAction> {
    if data == "pc:submit" {
        return Some(TelegramAction::Submit);
    }
    data.strip_prefix("pc:do:")
        .and_then(|value| value.parse().ok())
        .map(TelegramAction::Select)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        ConfirmChoice, ConfirmDetail, ConfirmField, ConfirmFieldKind, ConfirmInput,
        ConfirmPresentation, ConfirmSpec,
    };

    fn request() -> ConfirmRequest {
        ConfirmSpec {
            title: "Permission".into(),
            context: vec![ConfirmField {
                id: "tool".into(),
                label: "Tool".into(),
                value: "Bash".into(),
                kind: ConfirmFieldKind::Text,
            }],
            detail: ConfirmDetail {
                summary: "Run".into(),
                body_md: "```sh\ngit status\n```".into(),
            },
            choices: vec![
                ConfirmChoice {
                    id: "approve_once".into(),
                    label: "A".repeat(100),
                    description: "full scope".into(),
                    role: ActionRole::Primary,
                    variant: None,
                },
                ConfirmChoice {
                    id: "deny".into(),
                    label: "Deny".into(),
                    description: String::new(),
                    role: ActionRole::Destructive,
                    variant: None,
                },
            ],
            presentation: ConfirmPresentation::SingleSelectSubmit {
                input: Some(ConfirmInput {
                    id: "reason".into(),
                    visible_when_action_id: "deny".into(),
                    always_visible: false,
                    required: false,
                    prefix_chars_by_action_id: Default::default(),
                    label: "Reason".into(),
                    placeholder: "Tell the Agent what it should do".into(),
                    max_chars: 1000,
                }),
                submit_label: "Submit".into(),
                default_action_id: None,
            },
            dismiss_action_id: "deny".into(),
        }
        .into_request("r1".into(), 1, 2)
        .unwrap()
    }

    fn task_request() -> ConfirmRequest {
        let mut request = request();
        request.title = "Enter task".into();
        request.presentation = ConfirmPresentation::SingleSelectSubmit {
            input: Some(ConfirmInput {
                id: "task".into(),
                visible_when_action_id: "approve_once".into(),
                always_visible: false,
                required: true,
                prefix_chars_by_action_id: Default::default(),
                label: "Task".into(),
                placeholder: "Describe the task".into(),
                max_chars: 3000,
            }),
            submit_label: "Start task".into(),
            default_action_id: Some("approve_once".into()),
        };
        request
    }

    #[test]
    fn task_input_forms_hide_semantic_choice_controls() {
        let request = task_request();
        let feishu = feishu_card(&request, Some(0), "", Lang::En).to_string();
        assert!(!feishu.contains("confirm_choice_0"));
        assert!(feishu.contains("Start task"));

        let slack = slack_blocks(&request, Some(0), "", Lang::En).to_string();
        assert!(!slack.contains("radio_buttons"));
        assert!(slack.contains("plain_text_input"));

        let telegram = telegram_html(&request, Some(0), "", None, Lang::En);
        assert!(!telegram.contains("full scope"));
        assert!(telegram.contains("Reply directly to this message with the task"));
    }

    #[test]
    fn task_input_forms_show_manual_and_todo_choices_with_shared_input() {
        let mut request = task_request();
        request.choices.insert(
            1,
            ConfirmChoice {
                id: "todo:1".into(),
                label: "Run todo: ⚡ Project TODO".into(),
                description: String::new(),
                role: ActionRole::Default,
                variant: None,
            },
        );
        let input = match &mut request.presentation {
            ConfirmPresentation::SingleSelectSubmit { input, .. } => input.as_mut().unwrap(),
        };
        input.always_visible = true;

        let feishu = feishu_card(&request, Some(1), "extra", Lang::En).to_string();
        assert!(feishu.contains("confirm_choice_0"));
        assert!(feishu.contains("confirm_choice_1"));
        assert!(!feishu.contains("confirm_choice_2"));
        assert!(feishu.contains("【TODO】"));
        assert!(!feishu.contains("Run todo:"));
        assert!(feishu.contains("extra"));

        let slack = slack_blocks(&request, Some(1), "extra", Lang::En).to_string();
        assert!(slack.contains("radio_buttons"));
        assert!(slack.contains("Run todo:"));
        assert!(slack.contains("⚡ Project TODO"));
        assert!(!slack.contains("Deny"));
        assert!(slack.contains("extra"));
    }

    /// Permission request with two prefix-tier groups × 2 levels (D51):
    /// [approve, prefix:0(level0), prefix(level1, recommended), always:0, always, deny].
    fn variant_request() -> ConfirmRequest {
        let variant = |group: &str, level: usize, recommended: bool| {
            Some(crate::models::ChoiceVariant {
                group: group.into(),
                level,
                level_label: if level == 0 { "cargo" } else { "cargo build" }.into(),
                segment_label: if level == 0 { "cargo" } else { "build" }.into(),
                recommended,
            })
        };
        let mut request = request();
        let deny = request.choices.pop().unwrap();
        request.choices[0].label = "Approve once".into();
        request.choices[0].description = String::new();
        for (id, label, meta) in [
            (
                "remember_shell_prefix:1",
                "Session cargo",
                variant("shell-prefix-session", 0, false),
            ),
            (
                "remember_shell_prefix",
                "Session cargo build",
                variant("shell-prefix-session", 1, true),
            ),
            (
                "remember_shell_always:1",
                "Always cargo",
                variant("shell-prefix-always", 0, false),
            ),
            (
                "remember_shell_always",
                "Always cargo build",
                variant("shell-prefix-always", 1, true),
            ),
        ] {
            request.choices.push(ConfirmChoice {
                id: id.into(),
                label: label.into(),
                description: String::new(),
                role: ActionRole::Default,
                variant: meta,
            });
        }
        request.choices.push(deny);
        request
    }

    #[test]
    fn cards_show_only_recommended_prefix_variants_with_wire_callbacks() {
        let request = variant_request();
        // Visible: approve(0), recommended session(2), recommended always(4), deny(5).
        assert_eq!(task_choice_indices(&request), vec![0, 2, 4, 5]);

        let keyboard = telegram_keyboard(&request, None).to_string();
        assert!(keyboard.contains("pc:do:0"));
        assert!(keyboard.contains("pc:do:2"));
        assert!(keyboard.contains("pc:do:4"));
        assert!(keyboard.contains("pc:do:5"));
        assert!(!keyboard.contains("pc:do:1"));
        assert!(!keyboard.contains("pc:do:3"));
        // Numbered button labels follow visible positions (no gaps).
        assert!(keyboard.contains("4\u{fe0f}\u{20e3}"));
        assert!(!keyboard.contains("6\u{fe0f}\u{20e3}"));
        let html = telegram_html(&request, None, "", None, Lang::En);
        assert!(html.contains("Session cargo build"));
        assert!(!html.contains(">Session cargo<"));

        let slack = slack_blocks(&request, None, "", Lang::En).to_string();
        assert!(slack.contains("Session cargo build"));
        assert!(!slack.contains("\"Session cargo\""));

        let feishu = feishu_card(&request, None, "", Lang::En).to_string();
        assert!(feishu.contains("confirm_choice_2"));
        assert!(!feishu.contains("confirm_choice_1"));
    }

    #[test]
    fn telegram_optional_todo_confirmation_submits_choice_directly() {
        let mut request = task_request();
        let input = match &mut request.presentation {
            ConfirmPresentation::SingleSelectSubmit { input, .. } => input.as_mut().unwrap(),
        };
        input.always_visible = true;
        input.required = false;
        let keyboard = telegram_keyboard(&request, Some(0)).to_string();
        assert!(!keyboard.contains("pc:submit"));
        assert!(keyboard.contains("pc:do:0"));
        let html = telegram_html(&request, Some(0), "", None, Lang::En);
        assert!(html.contains("reply with extra instructions"));
    }

    #[test]
    fn feishu_uses_ask_checker_and_tool_first_hierarchy() {
        let card = feishu_card(&request(), Some(1), "", Lang::En);
        let elements = card["body"]["elements"].as_array().unwrap();
        assert_eq!(elements[0]["tag"], "div");
        assert_eq!(elements[0]["text"]["content"], "Permission");
        assert_eq!(elements[2]["tag"], "markdown");
        assert_eq!(elements[2]["content"], "**Reason:** Run");
        assert_eq!(elements[3]["content"], "**Bash**");
        assert!(elements[4]["content"]
            .as_str()
            .unwrap()
            .contains("git status"));
        let checkers: Vec<&Value> = elements
            .iter()
            .filter(|element| element["tag"] == "checker")
            .collect();
        assert_eq!(checkers.len(), 2);
        assert_eq!(checkers[0]["checked"], false);
        assert_eq!(checkers[0]["behaviors"][0]["value"]["confirm"], "select");
        assert_eq!(checkers[1]["checked"], true);
        assert_eq!(checkers[1]["text"]["text_color"], "red");
        assert!(elements
            .iter()
            .all(|element| element["tag"] != "column_set"));
        assert!(!card.to_string().contains("**Tool:**"));
    }

    #[test]
    fn feishu_final_keeps_compact_tool_hierarchy_without_context() {
        let card = feishu_final_card(&request(), "Submitted", Lang::En);
        let text = card.to_string();
        assert!(text.contains("**Bash**"));
        assert!(text.contains("git status"));
        assert!(text.contains("Submitted"));
        assert!(!text.contains("**Tool:**"));
        assert!(!text.contains("confirm_choice_"));
    }

    #[test]
    fn slack_uses_native_radio_full_labels_and_static_scope_details() {
        let blocks = slack_blocks(&request(), None, "", Lang::En);
        let text = blocks.to_string();
        assert!(text.contains(&"A".repeat(100)));
        let blocks = blocks.as_array().unwrap();
        assert_eq!(blocks[0]["text"]["text"], "❓ Permission");
        let radio = blocks
            .iter()
            .find(|block| block["element"]["type"] == "radio_buttons")
            .unwrap();
        assert_eq!(radio["element"]["options"].as_array().unwrap().len(), 2);
        assert!(
            radio["element"]["options"][0]["text"]["text"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= 75
        );
        assert_eq!(
            radio["element"]["options"][0]["description"]["text"],
            "full scope"
        );
        let reason = blocks
            .iter()
            .find(|block| block["element"]["type"] == "plain_text_input")
            .unwrap();
        assert_eq!(
            reason["label"]["text"],
            "Denial reason (optional; sent only when denying)"
        );
        assert!(blocks
            .iter()
            .all(|block| block["accessory"]["type"] != "button"));
    }

    #[test]
    fn slack_multi_rule_choice_uses_compact_label_and_inline_rule_details() {
        let mut request = request();
        request.choices[0].label = "Always allow (Project): 2 rules".into();
        request.choices[0].description = "Bash: git status\nRead: entire tool".into();
        let blocks = slack_blocks(&request, None, "", Lang::En);
        let blocks = blocks.as_array().unwrap();
        let radio = blocks
            .iter()
            .find(|block| block["element"]["type"] == "radio_buttons")
            .unwrap();
        assert_eq!(
            radio["element"]["options"][0]["text"]["text"],
            "Always allow (Project): 2 rules"
        );
        assert_eq!(
            radio["element"]["options"][0]["description"]["text"],
            "Bash: git status · Read: entire tool"
        );
        let text = Value::Array(blocks.clone()).to_string();
        assert!(text.contains("Bash: git status"));
        assert!(text.contains("Read: entire tool"));
    }

    #[test]
    fn slack_submit_reads_native_radio_and_reason() {
        let payload = json!({
            "user": { "id": "U1" },
            "container": { "message_ts": "1.2" },
            "actions": [{ "action_id": "confirm_submit" }],
            "state": { "values": {
                "confirm_choice_block": {
                    "confirm_choice": {
                        "selected_option": { "value": "1" }
                    }
                },
                "confirm_reason": {
                    "reason": { "value": " use read-only mode " }
                }
            } }
        });
        assert_eq!(
            parse_slack_action(&payload, Some("reason")),
            Some(CardAction::Submit {
                actor: "U1".into(),
                message_id: "1.2".into(),
                index: Some(1),
                comment: Some("use read-only mode".into()),
            })
        );
    }

    #[test]
    fn telegram_callbacks_carry_only_wire_indices() {
        assert_eq!(
            parse_telegram_callback("pc:do:7"),
            Some(TelegramAction::Select(7))
        );
        assert_eq!(
            parse_telegram_callback("pc:submit"),
            Some(TelegramAction::Submit)
        );
        assert_eq!(parse_telegram_callback("pc:s:7"), None);
        assert_eq!(parse_telegram_callback("approve_once"), None);
    }

    #[test]
    fn telegram_uses_tool_first_hierarchy_and_ask_keycaps() {
        let request = request();
        let html = telegram_html(&request, Some(1), "", None, Lang::En);
        assert!(html.starts_with("<b>❓ Permission</b>"));
        let summary = html.find("<b>Reason:</b> Run").unwrap();
        let tool = html.find("<b>Bash</b>").unwrap();
        let body = html.find("git status").unwrap();
        assert!(summary < tool && tool < body);
        assert!(!html.contains("<b>Tool:</b>"));
        let keyboard = telegram_keyboard(&request, Some(1));
        let rows = keyboard["inline_keyboard"].as_array().unwrap();
        // D14: a submit row appears once a draft selection exists.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
        assert_eq!(rows[0][0]["text"], "1️⃣");
        assert_eq!(rows[0][1]["text"], "✅ 2️⃣");
        assert_eq!(rows[0][1]["callback_data"], "pc:do:1");
        assert_eq!(rows[1][0]["text"], "Submit");
        assert_eq!(rows[1][0]["callback_data"], "pc:submit");
        assert!(html.contains("reply to this message before selecting Deny and submitting"));
    }

    #[test]
    fn telegram_keyboard_hides_submit_row_until_a_choice_is_selected() {
        let request = request();
        let keyboard = telegram_keyboard(&request, None);
        let rows = keyboard["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]
            .as_array()
            .unwrap()
            .iter()
            .all(|button| button["callback_data"] != "pc:submit"));
    }

    #[test]
    fn telegram_short_choices_use_direct_full_label_buttons() {
        let mut request = request();
        request.choices[0].label = "Approve".into();
        request.choices[0].description.clear();
        let html = telegram_html(&request, None, "", None, Lang::En);
        assert!(!html.contains("1️⃣ <b>Approve</b>"));
        let keyboard = telegram_keyboard(&request, None);
        let rows = keyboard["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0]["text"], "Approve");
        assert_eq!(rows[0][1]["text"], "Deny");
        assert_eq!(rows[0][0]["callback_data"], "pc:do:0");
    }

    #[test]
    fn feishu_parser_never_accepts_action_ids() {
        let event = json!({
            "operator": { "open_id": "u1" },
            "context": { "open_message_id": "m1" },
            "action": { "value": { "confirm": "select", "index": 1 } },
        });
        assert_eq!(
            parse_feishu_action(&event, None),
            Some(CardAction::Select {
                actor: "u1".into(),
                message_id: "m1".into(),
                index: 1,
                comment: None,
            })
        );
    }
}
