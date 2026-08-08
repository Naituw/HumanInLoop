//! `/watch` 实时状态卡的 Slack 渲染（Block Kit + mrkdwn）。
//!
//! 与飞书卡片同一份 `WatchFrame` / 共享文案构件（`watch::header_text` 等），差异仅在标记语言：
//! - 状态圆点用 emoji 🟢/⚪/🔴（mrkdwn 无彩色字体）；类别词 `*粗体*`、参数 `_斜体_`。
//! - TODO 只显示摘要行（用户定案：无折叠组件，不做展开）。
//! - 终态：`chat.update` 置为无按钮 blocks + `context` 终态标签（Block Kit 无禁用按钮）。

use crate::agents::activity::{StepState, ToolStep};
use crate::autochannel;
use crate::i18n::{self, Lang};
use crate::watch::{self, CardMode, WatchFrame};
use serde_json::{json, Value};

/// 按钮 action_id（`block_actions` 回调据此识别）。
pub const ACTION_UNWATCH: &str = "watch_unwatch";
pub const ACTION_REFRESH: &str = "watch_refresh";
pub const ACTION_REWATCH: &str = "watch_rewatch";

/// 组装 watch 卡 blocks + 通知回退文本。`now` 为渲染时刻（Unix 秒）。
/// `session_id`：可重新关注终态传入以渲染 rewatch 按钮。
pub fn build_watch_blocks(
    f: &WatchFrame,
    mode: CardMode,
    now: u64,
    lang: Lang,
    session_id: Option<&str>,
) -> (Value, String) {
    use super::markdown::escape as esc;
    let header = watch::header_text(f, lang);
    let state_line = watch::state_line_text(f, now, lang);

    let mut blocks: Vec<Value> = Vec::new();
    // 头部（context 小字，对应飞书蓝色小字）。
    blocks.push(json!({
        "type": "context",
        "elements": [{ "type": "mrkdwn", "text": format!("🤖 {}", esc(&header)) }]
    }));

    // 状态 + 标题。
    let mut head = format!("*{}*", esc(&state_line));
    if let Some(t) = &f.title {
        head.push_str(&format!("\n「{}」", esc(t)));
    }
    blocks.push(json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": head }
    }));

    // 最近动态：标题 + 文字 + 足迹时间线。
    let mut body = esc(&watch::activity_heading_text(f, now, lang));
    if f.text.is_none() && f.steps.is_empty() {
        body.push('\n');
        body.push_str(&esc(i18n::tr(lang, "autoChannel.statusNoActivity")));
    } else if let Some(t) = &f.text {
        body.push('\n');
        body.push_str(&esc(t));
    }
    if !f.steps.is_empty() {
        body.push('\n');
        if let Some(om) = watch::omitted_line_text(f, lang) {
            body.push_str(&format!("\n_{}_", esc(&om)));
        }
        for step in &f.steps {
            body.push('\n');
            body.push_str(&render_step_mrkdwn(step, lang));
        }
    }
    // TODO 摘要（仅摘要行，用户定案）。
    if let Some(s) = autochannel::todo_summary(&f.todos, lang) {
        body.push_str(&format!("\n\n{}", esc(&s)));
    }
    blocks.push(json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": body }
    }));

    // 底部：更新时刻（context 小字）+ 按钮 / 终态标签。
    let mut footer = watch::updated_line_text(now, lang);
    match &mode {
        CardMode::Active => {
            blocks.push(json!({
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": esc(&footer) }]
            }));
            blocks.push(json!({
                "type": "actions",
                "elements": [
                    {
                        "type": "button",
                        "action_id": ACTION_UNWATCH,
                        "text": { "type": "plain_text", "text": i18n::tr(lang, "watch.btnUnwatch") }
                    },
                    {
                        "type": "button",
                        "action_id": ACTION_REFRESH,
                        "text": { "type": "plain_text", "text": i18n::tr(lang, "watch.btnRefresh") }
                    }
                ]
            }));
        }
        CardMode::Final(ref kind) if session_id.is_some() && kind.is_rewatchable() => {
            blocks.push(json!({
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": esc(&footer) }]
            }));
            blocks.push(json!({
                "type": "actions",
                "elements": [{
                    "type": "button",
                    "action_id": ACTION_REWATCH,
                    "text": { "type": "plain_text", "text": watch::rewatch_label_text(kind, lang) }
                }]
            }));
        }
        CardMode::Final(kind) => {
            footer.push_str(&format!(
                "\n*{}*",
                esc(&watch::final_label_text(kind, lang))
            ));
            blocks.push(json!({
                "type": "context",
                "elements": [{ "type": "mrkdwn", "text": footer }]
            }));
        }
    }

    // 通知回退文本：头部行 + 状态（Slack 弹窗/列表预览用）。
    let fallback = format!("🤖 {} — {}", header, state_line);
    (Value::Array(blocks), fallback)
}

/// 一步足迹的 mrkdwn 行：圆点符号 + `*类别词*: _参数_`。
/// 无彩色字体的渠道用空心/实心圆点区分未完成/完成（用户定案：不用 emoji 圆点）；失败 ✕。
fn render_step_mrkdwn(step: &ToolStep, lang: Lang) -> String {
    use super::markdown::escape as esc;
    let dot = match step.state {
        StepState::Running => "○",
        StepState::Done => "●",
        StepState::Failed => "✕",
    };
    let (label, object) = autochannel::step_label_object(step, lang);
    match object {
        Some(o) => format!("{} *{}*: _{}_", dot, esc(&label), esc(&o)),
        None => format!("{} *{}*", dot, esc(&label)),
    }
}

/// 从 `block_actions` 回调解析 watch 按钮点击：返回 `(message_ts, action_id)`。
/// 非 watch 按钮（如提问卡的 submit）返回 None。
pub fn parse_watch_action(payload: &Value) -> Option<(String, String)> {
    let action_id = payload
        .get("actions")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.get("action_id"))
        .and_then(|v| v.as_str())?;
    if action_id != ACTION_UNWATCH && action_id != ACTION_REFRESH && action_id != ACTION_REWATCH {
        return None;
    }
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
    Some((ts.to_string(), action_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::activity::{TodoItem, TodoState, ToolDisplay, ToolLabel};
    use crate::watch::{FinalKind, WatchPhase};

    fn frame() -> WatchFrame {
        WatchFrame {
            seq: 3,
            kind_label: "Cursor".into(),
            title: Some("重构空闲退出".into()),
            project: Some("HumanInLoop".into()),
            phase: WatchPhase::Working,
            text: Some("正在跑 <单测>".into()),
            steps: vec![ToolStep {
                tool: ToolDisplay {
                    label: ToolLabel::Run,
                    object: Some("cargo test".into()),
                },
                state: StepState::Running,
            }],
            steps_omitted: 2,
            todos: vec![TodoItem {
                content: "跑单测".into(),
                state: TodoState::InProgress,
            }],
            active_elapsed_secs: None,
            at: Some(1_700_000_000),
            forked_from: None,
        }
    }

    #[test]
    fn blocks_layout_active() {
        let (blocks, fallback) =
            build_watch_blocks(&frame(), CardMode::Active, 1_700_000_010, Lang::Zh, None);
        let arr = blocks.as_array().unwrap();
        // context 头部 + section 状态 + section 动态 + context 更新行 + actions。
        assert_eq!(arr.len(), 5);
        assert!(arr[0]["elements"][0]["text"]
            .as_str()
            .unwrap()
            .contains("实时关注 [3] Cursor — HumanInLoop"));
        assert!(arr[1]["text"]["text"]
            .as_str()
            .unwrap()
            .contains("*🟢 工作中*"));
        let body = arr[2]["text"]["text"].as_str().unwrap();
        // 用户内容做 mrkdwn 转义。
        assert!(body.contains("正在跑 &lt;单测&gt;"));
        assert!(body.contains("_… 已省略 2 步_"));
        // 空心圈=进行中 / 实心点=已完成。
        assert!(body.contains("○ *运行命令*: _cargo test_"));
        assert!(body.contains("📋 TODO 0/1 · 当前：跑单测"));
        assert_eq!(arr[4]["elements"][0]["action_id"], ACTION_UNWATCH);
        assert_eq!(arr[4]["elements"][1]["action_id"], ACTION_REFRESH);
        assert!(fallback.contains("实时关注 [3]"));
    }

    #[test]
    fn blocks_final_no_buttons() {
        let (blocks, _) = build_watch_blocks(
            &frame(),
            CardMode::Final(FinalKind::Moved),
            1_700_000_010,
            Lang::Zh,
            None,
        );
        let arr = blocks.as_array().unwrap();
        assert_eq!(arr.len(), 4); // 无 actions 块。
        assert!(arr[3]["elements"][0]["text"]
            .as_str()
            .unwrap()
            .contains("已移至最新卡片"));
    }

    #[test]
    fn parses_watch_actions_only() {
        let payload = json!({
            "container": { "message_ts": "123.456" },
            "actions": [{ "action_id": ACTION_REFRESH }]
        });
        assert_eq!(
            parse_watch_action(&payload),
            Some(("123.456".into(), ACTION_REFRESH.into()))
        );
        let other = json!({
            "container": { "message_ts": "123.456" },
            "actions": [{ "action_id": "submit" }]
        });
        assert!(parse_watch_action(&other).is_none());
    }
}
