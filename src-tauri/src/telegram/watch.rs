//! `/watch` 实时状态卡的 Telegram 渲染（HTML 消息 + inline keyboard）。
//!
//! 与飞书卡片同一份 `WatchFrame` / 共享文案构件（`watch::header_text` 等），差异仅在标记语言：
//! - 状态圆点用 emoji 🟢/⚪/🔴（无彩色字体）；类别词 `<b>` 加粗、参数 `<i>` 斜体。
//! - TODO 只显示摘要行（用户定案：无折叠组件，不做展开）。
//! - 终态：编辑为无按钮消息 + 末行终态标签（Telegram 不支持禁用按钮）。

use crate::agents::activity::{StepState, ToolStep};
use crate::autochannel;
use crate::i18n::{self, Lang};
use crate::watch::{self, CardMode, WatchFrame};
use serde_json::{json, Value};

/// 按钮回调 data 前缀（`watch:unwatch` / `watch:refresh` / `watch:rewatch`）。
pub const CB_UNWATCH: &str = "watch:unwatch";
pub const CB_REFRESH: &str = "watch:refresh";
pub const CB_REWATCH: &str = "watch:rewatch";

/// 渲染整卡 HTML（`parse_mode=HTML`）。`now` 为渲染时刻（Unix 秒）。
pub fn render_watch_html(f: &WatchFrame, mode: CardMode, now: u64, lang: Lang) -> String {
    use super::markdown::escape_html as esc;
    let mut out = String::new();
    // 头部行（斜体弱化，对应飞书蓝色小字）。
    out.push_str(&format!(
        "🤖 <i>{}</i>\n",
        esc(&watch::header_text(f, lang))
    ));
    // 状态行（加粗）+ 标题。
    out.push_str(&format!(
        "<b>{}</b>\n",
        esc(&watch::state_line_text(f, now, lang))
    ));
    if let Some(t) = &f.title {
        out.push_str(&format!("「{}」\n", esc(t)));
    }
    // 最近动态。
    out.push('\n');
    out.push_str(&esc(&watch::activity_heading_text(f, now, lang)));
    if f.text.is_none() && f.steps.is_empty() {
        out.push('\n');
        out.push_str(&esc(i18n::tr(lang, "autoChannel.statusNoActivity")));
    } else if let Some(t) = &f.text {
        out.push('\n');
        out.push_str(&esc(t));
    }
    if !f.steps.is_empty() {
        out.push('\n');
        if let Some(om) = watch::omitted_line_text(f, lang) {
            out.push_str(&format!("\n<i>{}</i>", esc(&om)));
        }
        for step in &f.steps {
            out.push('\n');
            out.push_str(&render_step_html(step, lang));
        }
    }
    // TODO 摘要（仅摘要行，用户定案）。
    if let Some(s) = autochannel::todo_summary(&f.todos, lang) {
        out.push_str(&format!("\n\n{}", esc(&s)));
    }
    // 底部：更新时刻（斜体弱化）+ 终态标签。
    out.push_str(&format!(
        "\n\n<i>{}</i>",
        esc(&watch::updated_line_text(now, lang))
    ));
    if let CardMode::Final(kind) = &mode {
        out.push_str(&format!(
            "\n<b>{}</b>",
            esc(&watch::final_label_text(kind, lang))
        ));
    }
    out
}

/// 活动态 inline keyboard：两按钮（取消关注 / 立即刷新）。终态编辑不带 markup 即移除按钮。
pub fn inline_keyboard(lang: Lang) -> Value {
    json!({
        "inline_keyboard": [[
            { "text": i18n::tr(lang, "watch.btnUnwatch"), "callback_data": CB_UNWATCH },
            { "text": i18n::tr(lang, "watch.btnRefresh"), "callback_data": CB_REFRESH },
        ]]
    })
}

/// 可重新关注终态的 inline keyboard：单按钮，文案含原因 + 操作提示。
pub fn rewatch_keyboard(kind: &watch::FinalKind, lang: Lang) -> Value {
    json!({
        "inline_keyboard": [[
            { "text": watch::rewatch_label_text(kind, lang), "callback_data": CB_REWATCH },
        ]]
    })
}

/// 一步足迹的 HTML 行：圆点符号 + `<b>类别词</b>: <i>参数</i>`。
/// 无彩色字体的渠道用空心/实心圆点区分未完成/完成（用户定案：不用 emoji 圆点）；失败 ✕。
fn render_step_html(step: &ToolStep, lang: Lang) -> String {
    use super::markdown::escape_html as esc;
    let dot = match step.state {
        StepState::Running => "○",
        StepState::Done => "●",
        StepState::Failed => "✕",
    };
    let (label, object) = autochannel::step_label_object(step, lang);
    match object {
        Some(o) => format!("{} <b>{}</b>: <i>{}</i>", dot, esc(&label), esc(&o)),
        None => format!("{} <b>{}</b>", dot, esc(&label)),
    }
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
    fn html_layout_escapes_and_marks_up() {
        let html = render_watch_html(&frame(), CardMode::Active, 1_700_000_010, Lang::Zh);
        assert!(html.contains("🤖 <i>实时关注 [3] Cursor — HumanInLoop</i>"));
        assert!(html.contains("<b>🟢 工作中</b>"));
        assert!(html.contains("「重构空闲退出」"));
        // 用户内容做 HTML 转义。
        assert!(html.contains("正在跑 &lt;单测&gt;"));
        // 省略标注 + 步行（空心圈=进行中 / 实心点=已完成 + 粗体类别 + 斜体参数）。
        assert!(html.contains("<i>… 已省略 2 步</i>"));
        assert!(html.contains("○ <b>运行命令</b>: <i>cargo test</i>"));
        // TODO 仅摘要行。
        assert!(html.contains("📋 TODO 0/1 · 当前：跑单测"));
        assert!(html.contains("<i>最后更新"));
        // 活动态无终态标签。
        assert!(!html.contains("已移至"));
    }

    #[test]
    fn html_final_appends_label() {
        let html = render_watch_html(
            &frame(),
            CardMode::Final(FinalKind::Moved),
            1_700_000_010,
            Lang::Zh,
        );
        assert!(html.contains("<b>已移至最新卡片 ⬇</b>"));
    }

    #[test]
    fn keyboard_has_two_callbacks() {
        let kb = inline_keyboard(Lang::Zh);
        let row = kb["inline_keyboard"][0].as_array().unwrap();
        assert_eq!(row.len(), 2);
        assert_eq!(row[0]["callback_data"], CB_UNWATCH);
        assert_eq!(row[1]["callback_data"], CB_REFRESH);
    }
}
