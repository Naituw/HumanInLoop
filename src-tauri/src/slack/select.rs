//! 通用「单选卡」的 Slack 渲染（Block Kit + mrkdwn）。
//!
//! 每个选项 = 一个 `section`（两行 mrkdwn：圆点+`*[编号]*`+类型·目录+徽标 / 标题）+ 右侧 button
//! accessory。Slack 要求 `action_id` 在整条消息内唯一，故用 `select_<idx>`（idx=选项下标）；daemon 侧
//! 按下标映射回 session_id（避 seq 漂移）。
//!
//! 点选后（daemon 侧）：`/watch` 就地把本消息 `chat.update` 成实时 watch 卡（与飞书就地变身一致）；
//! `/status` 回文本详情、卡不动；`/unwatch` 旧卡定格 + 就地刷新本卡（移除该项 / 取 0 定格）。

use crate::i18n::Lang;
use crate::select::{SelectAction, SelectDot, SelectView};
use serde_json::{json, Value};

/// 触发按钮 `action_id` 前缀（`select_<idx>`；前缀用于与 watch 等按钮区分且保证整卡唯一）。
pub const ACTION_SELECT_PREFIX: &str = "select_";

/// 状态圆点 emoji（mrkdwn 无彩色字体：工作中🟢 / 空闲⚪，与 watch 卡同风格）。
fn dot_emoji(dot: Option<SelectDot>) -> &'static str {
    match dot {
        Some(SelectDot::Working) => "🟢",
        Some(SelectDot::Idle) => "⚪",
        None => "▫️",
    }
}

/// 按钮样式（watch=primary、status=默认、unwatch=danger，对齐飞书）。
fn button_style(action: SelectAction) -> Option<&'static str> {
    match action {
        SelectAction::Watch
        | SelectAction::TaskWorkspace
        | SelectAction::TaskAgent
        | SelectAction::TaskPermission
        | SelectAction::TaskInputSource
        | SelectAction::Fork
        | SelectAction::Msg
        | SelectAction::MsgTarget
        | SelectAction::Stage
        | SelectAction::TodoRm
        | SelectAction::TodoAuto => Some("primary"),
        SelectAction::Status
        | SelectAction::Diff
        | SelectAction::Transcript
        | SelectAction::Todo
        | SelectAction::TodoAutoEntry => None,
        SelectAction::Unwatch | SelectAction::TodoRmEntry | SelectAction::Yolo => Some("danger"),
    }
}

/// 组装单选卡 blocks + 通知回退文本。
pub fn build_select_blocks(view: &SelectView, lang: Lang) -> (Value, String) {
    use super::markdown::escape as esc;
    let mut blocks: Vec<Value> = Vec::new();
    let mut title = format!("*{}*", esc(&view.title));
    if let Some(note) = &view.truncated_note {
        title.push_str(&format!("  _{}_", esc(note)));
    }
    blocks.push(json!({ "type": "section", "text": { "type": "mrkdwn", "text": title } }));

    let style = button_style(view.action);
    for (idx, opt) in view.options.iter().enumerate() {
        let label = crate::select::option_button_label(opt, view.action, lang);
        let mut text = String::from(dot_emoji(opt.dot));
        if let Some(seq) = opt.seq {
            text.push_str(&format!(" *[{}]*", seq));
        }
        text.push_str(&format!(" {}", esc(&opt.primary)));
        if let Some(badge) = &opt.badge {
            text.push_str(&format!(" {}", esc(badge)));
        }
        if let Some(elapsed) = &opt.elapsed {
            text.push_str(&format!(" {}", esc(elapsed)));
        }
        if let Some(sub) = &opt.secondary {
            text.push_str(&format!("\n{}", esc(sub)));
        }
        let mut button = json!({
            "type": "button",
            "action_id": format!("{}{}", ACTION_SELECT_PREFIX, idx),
            "value": idx.to_string(),
            "text": { "type": "plain_text", "text": label },
        });
        if let Some(s) = style {
            button["style"] = json!(s);
        }
        blocks.push(json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": text },
            "accessory": button,
        }));
    }
    (Value::Array(blocks), view.title.clone())
}

/// 定格 blocks（无按钮）：标题 + 定格文案（context 小字）。
pub fn build_select_final_blocks(title: &str, final_label: &str) -> (Value, String) {
    use super::markdown::escape as esc;
    let blocks = json!([
        { "type": "section", "text": { "type": "mrkdwn", "text": format!("*{}*", esc(title)) } },
        { "type": "context", "elements": [{ "type": "mrkdwn", "text": esc(final_label) }] },
    ]);
    (blocks, title.to_string())
}

/// 从 `block_actions` 回调解析单选卡点击：返回 `(message_ts, 选项下标)`。非本卡按钮返回 None。
pub fn parse_select_action(payload: &Value) -> Option<(String, usize)> {
    let action = payload
        .get("actions")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())?;
    let action_id = action.get("action_id").and_then(|v| v.as_str())?;
    let idx = action_id.strip_prefix(ACTION_SELECT_PREFIX)?.parse().ok()?;
    let ts = payload
        .get("container")
        .and_then(|c| c.get("message_ts"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("message")
                .and_then(|m| m.get("ts"))
                .and_then(|v| v.as_str())
        })?;
    Some((ts.to_string(), idx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::{build_view, SelectAction, SelectDot, SelectOption};

    fn opt(
        id: &str,
        dot: Option<SelectDot>,
        seq: u64,
        primary: &str,
        badge: Option<&str>,
        sub: &str,
    ) -> SelectOption {
        SelectOption {
            id: id.to_string(),
            dot,
            seq: Some(seq),
            primary: primary.to_string(),
            badge: badge.map(|b| b.to_string()),
            elapsed: None,
            secondary: Some(sub.to_string()),
        }
    }

    fn view(action: SelectAction) -> SelectView {
        build_view(
            "选择要取消关注的 Agent：".into(),
            vec![
                opt(
                    "s-work",
                    Some(SelectDot::Working),
                    2,
                    "Cursor · api",
                    Some("· 关注中"),
                    "重构",
                ),
                opt(
                    "s-idle",
                    Some(SelectDot::Idle),
                    5,
                    "Claude Code · web",
                    None,
                    "写文档",
                ),
            ],
            action,
            Lang::Zh,
        )
    }

    #[test]
    fn blocks_layout_and_unique_action_ids() {
        let (blocks, fallback) = build_select_blocks(&view(SelectAction::Unwatch), Lang::Zh);
        let arr = blocks.as_array().unwrap();
        // 标题 section + 2 个选项 section。
        assert_eq!(arr.len(), 3);
        assert!(arr[0]["text"]["text"]
            .as_str()
            .unwrap()
            .contains("*选择要取消关注的 Agent：*"));
        assert!(arr[1]["text"]["text"]
            .as_str()
            .unwrap()
            .contains("🟢 *[2]* Cursor · api · 关注中"));
        // 每个按钮 action_id 唯一（select_0 / select_1），unwatch=danger。
        assert_eq!(arr[1]["accessory"]["action_id"], "select_0");
        assert_eq!(arr[1]["accessory"]["style"], "danger");
        assert_eq!(arr[1]["accessory"]["text"]["text"], "取消");
        assert_eq!(arr[2]["accessory"]["action_id"], "select_1");
        assert_eq!(fallback, "选择要取消关注的 Agent：");
    }

    #[test]
    fn watch_button_is_primary() {
        let (blocks, _) = build_select_blocks(&view(SelectAction::Watch), Lang::Zh);
        assert_eq!(
            blocks.as_array().unwrap()[1]["accessory"]["style"],
            "primary"
        );
        assert_eq!(
            blocks.as_array().unwrap()[1]["accessory"]["text"]["text"],
            "关注"
        );
    }

    #[test]
    fn parse_roundtrip() {
        let payload = json!({
            "container": { "message_ts": "123.456" },
            "actions": [{ "action_id": "select_1", "value": "1" }]
        });
        assert_eq!(parse_select_action(&payload), Some(("123.456".into(), 1)));
        // 非 select 按钮（如 watch）→ None。
        let watch = json!({
            "container": { "message_ts": "1.2" },
            "actions": [{ "action_id": "watch_unwatch" }]
        });
        assert_eq!(parse_select_action(&watch), None);
    }
}
