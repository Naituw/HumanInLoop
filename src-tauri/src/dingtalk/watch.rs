//! `/watch` 实时状态卡的钉钉渲染（互动卡片高级版模板变量）。
//!
//! 模板：`docs/assets/dingtalk-watch-card-template.json`（导入开发者后台后发布；内置默认 ID
//! 见 `DEFAULT_WATCH_CARD_TEMPLATE_ID`）。与飞书/TG/Slack 共享同一份 `WatchFrame` 与文案构件，
//! 差异仅在载体：钉钉是「模板 + 变量」——标题/足迹折叠进单一 `body_md` markdown 变量；TODO 走
//! 折叠面板（钉钉有 CollapsePanel 组件，与飞书同级体验：`todo_summary` 作面板标题、`todo_md`
//! 作内容、`has_todos` 控显隐）；终态用 boolean 变量 `finalized` 条件显隐按钮（复刻提问模板
//! 已验证的条件渲染手法）。
//!
//! 钉钉卡片 markdown 支持 `<font sizeToken/colorTokenV2>` 富文本（提问卡选项已验证），
//! 故状态圆点与飞书同款彩色 ●（进行中绿 / 已完成灰 / 失败红），正文统一 h5 字号
//! （默认 body 字号偏大，用户反馈显乱）。

use crate::agents::activity::{StepState, ToolStep};
use crate::autochannel;
use crate::i18n::{self, Lang};
use crate::watch::{self, CardMode, WatchFrame};
use serde_json::{json, Value};

/// 内置默认 watch 卡片模板 ID（开发者后台「AskHuman Watch」模板，用户导入发布）。
pub const DEFAULT_WATCH_CARD_TEMPLATE_ID: &str = "fb330b73-cf00-4a7f-ac80-fd884867f9c1.schema";

/// 按钮回调 actionId。
pub const ACTION_UNWATCH: &str = "watch_unwatch";
pub const ACTION_REFRESH: &str = "watch_refresh";
pub const ACTION_REWATCH: &str = "watch_rewatch";

/// 正文字号（h5=15px，与提问卡选项一致；钉钉 markdown 默认字号偏大）。
const SIZE_BODY: &str = "common_h5_text_style__font_size";
/// 辅助信息字号（footnote=12px）。
const SIZE_SMALL: &str = "common_footnote_text_style__font_size";
/// 圆点/辅文颜色 token（与飞书 green/grey/red 圆点对应）。
const COLOR_GREEN: &str = "common_green1_color";
const COLOR_GREY: &str = "common_level3_base_color";
const COLOR_RED: &str = "common_red1_color";

/// 包一段 `<font>` 富文本（size / color token 均可选）。
fn font(text: &str, size: Option<&str>, color: Option<&str>) -> String {
    let mut attrs = String::new();
    if let Some(s) = size {
        attrs.push_str(&format!(" sizeToken={}", s));
    }
    if let Some(c) = color {
        attrs.push_str(&format!(" colorTokenV2={}", c));
    }
    format!("<font{}>{}</font>", attrs, text)
}

/// 相邻 `<font>` 标签之间的空格（普通空格与标签间 NBSP 均被钉钉渲染器吞掉，用户两轮实测）。
/// 把 NBSP 放进**前一个标签内部**作为内容的一部分才能保住间距。
const NBSP: char = '\u{00a0}';

/// 组装 watch 卡【公有】`cardParamMap`（模板全部 13 个变量；值均为字符串，boolean 按钉钉约定
/// 以字符串下发）。创建与更新共用：更新走 `updateCardDataByKey`，全量下发幂等。
pub fn build_watch_param_map(f: &WatchFrame, mode: CardMode, now: u64, lang: Lang) -> Value {
    let rewatchable = matches!(&mode, CardMode::Final(kind) if kind.is_rewatchable());
    let final_label = match &mode {
        CardMode::Final(kind) if !rewatchable => watch::final_label_text(kind, lang),
        _ => String::new(),
    };
    let rewatch_label = match &mode {
        CardMode::Final(kind) if rewatchable => watch::rewatch_label_text(kind, lang),
        _ => String::new(),
    };
    let todo_summary = autochannel::todo_summary(&f.todos, lang);
    json!({
        "header": watch::header_text(f, lang),
        "state_line": watch::state_line_text(f, now, lang),
        "body_md": body_md(f, now, lang),
        "updated_line": watch::updated_line_text(now, lang),
        "finalized": if matches!(mode, CardMode::Final(_)) { "true" } else { "false" },
        "final_label": final_label,
        "rewatchable": if rewatchable { "true" } else { "false" },
        "rewatch_label": rewatch_label,
        "btn_unwatch": i18n::tr(lang, "watch.btnUnwatch"),
        "btn_refresh": i18n::tr(lang, "watch.btnRefresh"),
        "has_todos": if todo_summary.is_some() { "true" } else { "false" },
        "todo_summary": todo_summary.unwrap_or_default(),
        "todo_md": todo_md(f),
    })
}

/// 正文 markdown（模板只有一个 Markdown 组件）：标题「…」+ 最近动态 + 足迹时间线。
/// 逐行包 h5 字号 `<font>`（钉钉 markdown 默认字号偏大）；省略标注小灰字。
fn body_md(f: &WatchFrame, now: u64, lang: Lang) -> String {
    let mut out = String::new();
    if let Some(t) = &f.title {
        out.push_str(&font(&format!("「{}」", t), Some(SIZE_BODY), None));
        out.push_str("\n\n");
    }
    let mut activity = watch::activity_heading_text(f, now, lang);
    if f.text.is_none() && f.steps.is_empty() {
        activity.push('\n');
        activity.push_str(i18n::tr(lang, "autoChannel.statusNoActivity"));
    } else if let Some(t) = &f.text {
        activity.push('\n');
        activity.push_str(t);
    }
    out.push_str(&font(&activity, Some(SIZE_BODY), None));
    if !f.steps.is_empty() {
        out.push('\n');
        if let Some(om) = watch::omitted_line_text(f, lang) {
            out.push('\n');
            out.push_str(&font(&om, Some(SIZE_SMALL), Some(COLOR_GREY)));
        }
        for step in &f.steps {
            out.push_str("\n\n");
            out.push_str(&render_step_md(step, lang));
        }
    }
    out
}

/// TODO 折叠面板内容 markdown：进行中绿点加粗、已完成灰点删除线、待办空心圈
/// （与飞书折叠面板同款；cancelled 条目在解析层已剔除）。
fn todo_md(f: &WatchFrame) -> String {
    use crate::agents::activity::TodoState;
    f.todos
        .iter()
        .map(|item| match item.state {
            TodoState::Completed => format!(
                "{}{}",
                font(&format!("●{NBSP}"), Some(SIZE_BODY), Some(COLOR_GREY)),
                font(&format!("~~{}~~", item.content), Some(SIZE_BODY), None)
            ),
            TodoState::InProgress => format!(
                "{}{}",
                font(&format!("●{NBSP}"), Some(SIZE_BODY), Some(COLOR_GREEN)),
                font(&format!("**{}**", item.content), Some(SIZE_BODY), None)
            ),
            TodoState::Pending | TodoState::Cancelled => {
                font(&format!("○ {}", item.content), Some(SIZE_BODY), None)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 一步足迹的 markdown 行：彩色圆点（进行中绿 / 已完成灰 / 失败红，与飞书同款）+
/// `**类别词**: *参数*`（h5 字号）。
fn render_step_md(step: &ToolStep, lang: Lang) -> String {
    let color = match step.state {
        StepState::Running => COLOR_GREEN,
        StepState::Done => COLOR_GREY,
        StepState::Failed => COLOR_RED,
    };
    let (label, object) = autochannel::step_label_object(step, lang);
    let body = match object {
        Some(o) => format!("**{}**: *{}*", label, o),
        None => format!("**{}**", label),
    };
    format!(
        "{}{}",
        font(&format!("●{NBSP}"), Some(SIZE_BODY), Some(color)),
        font(&body, Some(SIZE_BODY), None)
    )
}

/// 把一条卡片回调 `data` 解析为 watch 按钮动作：`(outTrackId, actionId)`。
/// 非 watch 按钮（如提问卡提交）→ None。
pub fn parse_watch_action(data: &Value) -> Option<(String, String)> {
    let otid = data
        .get("outTrackId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    // content 优先，回退 value；二者皆为 JSON 字符串（也兼容已是对象的情况）。
    let inner: Value = match data.get("content").or_else(|| data.get("value"))? {
        Value::String(s) => serde_json::from_str(s).ok()?,
        other => other.clone(),
    };
    let action = inner
        .get("cardPrivateData")
        .and_then(|p| p.get("actionIds"))
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .find(|id| *id == ACTION_UNWATCH || *id == ACTION_REFRESH || *id == ACTION_REWATCH)
        })?;
    Some((otid, action.to_string()))
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
            text: Some("正在跑单测".into()),
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
    fn param_map_has_all_template_variables() {
        let m = build_watch_param_map(&frame(), CardMode::Active, 1_700_000_010, Lang::Zh);
        for key in [
            "header",
            "state_line",
            "body_md",
            "updated_line",
            "finalized",
            "final_label",
            "btn_unwatch",
            "btn_refresh",
            "has_todos",
            "todo_summary",
            "todo_md",
        ] {
            assert!(m.get(key).is_some(), "missing {key}");
        }
        assert!(m["header"].as_str().unwrap().contains("[3]"));
        assert_eq!(m["state_line"], "🟢 工作中");
        // boolean 以字符串下发。
        assert_eq!(m["finalized"], "false");
        assert_eq!(m["final_label"], "");
        assert_eq!(m["btn_unwatch"], "取消关注");
        let body = m["body_md"].as_str().unwrap();
        assert!(body.contains("「重构空闲退出」"));
        assert!(body.contains("正在跑单测"));
        // 正文包 h5 字号；省略标注小灰字。
        assert!(body.contains("sizeToken=common_h5_text_style__font_size"));
        assert!(body.contains(
            "<font sizeToken=common_footnote_text_style__font_size colorTokenV2=common_level3_base_color>… 已省略 2 步</font>"
        ));
        // 进行中步：绿点（点后 NBSP 顶在标签内保住间距）+ 粗类别 + 斜参数。
        assert!(body.contains("colorTokenV2=common_green1_color>●\u{a0}</font>"));
        assert!(body.contains("**运行命令**: *cargo test*"));
        // TODO 走折叠面板变量，不入正文。
        assert!(!body.contains("📋"));
        assert_eq!(m["has_todos"], "true");
        assert_eq!(m["todo_summary"], "📋 TODO 0/1 · 当前：跑单测");
        assert!(m["todo_md"].as_str().unwrap().contains("**跑单测**"));
    }

    #[test]
    fn param_map_without_todos_hides_panel() {
        let mut f = frame();
        f.todos.clear();
        let m = build_watch_param_map(&f, CardMode::Active, 1_700_000_010, Lang::Zh);
        assert_eq!(m["has_todos"], "false");
        assert_eq!(m["todo_summary"], "");
        assert_eq!(m["todo_md"], "");
    }

    #[test]
    fn todo_md_states() {
        use crate::agents::activity::{TodoItem, TodoState};
        let mut f = frame();
        f.todos = vec![
            TodoItem {
                content: "读代码".into(),
                state: TodoState::Completed,
            },
            TodoItem {
                content: "跑单测".into(),
                state: TodoState::InProgress,
            },
            TodoItem {
                content: "提交".into(),
                state: TodoState::Pending,
            },
        ];
        let md = todo_md(&f);
        let lines: Vec<&str> = md.split("\n\n").collect();
        assert_eq!(lines.len(), 3);
        // 已完成：灰点 + 删除线。
        assert!(lines[0].contains("colorTokenV2=common_level3_base_color>●\u{a0}</font>"));
        assert!(lines[0].contains("~~读代码~~"));
        // 进行中：绿点 + 加粗。
        assert!(lines[1].contains("colorTokenV2=common_green1_color>●\u{a0}</font>"));
        assert!(lines[1].contains("**跑单测**"));
        // 待办：空心圈普通。
        assert!(lines[2].contains("○ 提交"));
    }

    #[test]
    fn param_map_final_sets_flag_and_label() {
        let m = build_watch_param_map(
            &frame(),
            CardMode::Final(FinalKind::Ended),
            1_700_000_010,
            Lang::Zh,
        );
        assert_eq!(m["finalized"], "true");
        assert_eq!(m["final_label"], "已结束 · 已自动取消关注");
        assert_eq!(m["rewatchable"], "false");
    }

    #[test]
    fn param_map_rewatchable_sets_rewatch_label() {
        let m = build_watch_param_map(
            &frame(),
            CardMode::Final(FinalKind::Cancelled),
            1_700_000_010,
            Lang::Zh,
        );
        assert_eq!(m["finalized"], "true");
        assert_eq!(m["rewatchable"], "true");
        assert_eq!(m["rewatch_label"], "已取消关注 · 点击重新关注");
        assert_eq!(m["final_label"], "");
    }

    #[test]
    fn step_dots_by_state() {
        let step = |state: StepState| ToolStep {
            tool: ToolDisplay {
                label: ToolLabel::Run,
                object: None,
            },
            state,
        };
        // 彩色圆点（钉钉 markdown 支持 font colorTokenV2，与飞书同款配色）。
        assert!(render_step_md(&step(StepState::Running), Lang::Zh)
            .contains("colorTokenV2=common_green1_color>●\u{a0}</font>"));
        assert!(render_step_md(&step(StepState::Done), Lang::Zh)
            .contains("colorTokenV2=common_level3_base_color>●\u{a0}</font>"));
        assert!(render_step_md(&step(StepState::Failed), Lang::Zh)
            .contains("colorTokenV2=common_red1_color>●\u{a0}</font>"));
    }

    #[test]
    fn parse_watch_action_roundtrip() {
        let data = serde_json::json!({
            "outTrackId": "watch-poc-1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"watch_refresh\"],\"params\":{}}}",
        });
        assert_eq!(
            parse_watch_action(&data),
            Some(("watch-poc-1".into(), "watch_refresh".into()))
        );
        // 提问卡提交等非 watch 回调 → None。
        let submit = serde_json::json!({
            "outTrackId": "ask-1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"submit_action\"],\"params\":{}}}",
        });
        assert_eq!(parse_watch_action(&submit), None);
    }
}
