//! 通用「单选卡」的钉钉渲染（互动卡片高级版模板变量）。
//!
//! 模板：用户在开发者后台由 `docs/assets/dingtalk-select-card-template.json` 导入并发布；内置默认 ID
//! 见 `DEFAULT_SELECT_CARD_TEMPLATE_ID`。与飞书/TG/Slack 共享同一份 `SelectView`（传输无关）；差异仅
//! 在载体：钉钉是「模板 + 变量」——
//! - 全局：`title`(卡头标题) / `btn_text`(按钮文案) / `btn_color`(按钮色 blue|red) / `finalized`("true"|"false"
//!   条件显隐循环 vs 定格标签) / `final_label`(定格文案)。
//! - 循环 `loop_object_list`（复杂值→JSON 字符串下发，与提问卡 `options` 同规）：每项
//!   `{option_md:两行富文本, sid:该 agent 的 session_id}`。
//!
//! 钉钉不支持「回调同步回卡」，也不支持把一张卡就地变成另一种卡（模板固定）：故点选后的卡片变化统一
//! 走 OpenAPI `updateCard`（daemon 侧），本模块只负责「变量组装 + 回调解析」。
//!
//! 富文本沿用 `dingtalk/watch.rs` 同款 `<font sizeToken/colorTokenV2>`（彩色圆点 ● 与状态卡一致：
//! 工作中绿 / 空闲灰）。

use crate::i18n::Lang;
use crate::select::{SelectDot, SelectOption, SelectView};
use serde_json::{json, Value};

/// 内置默认单选卡模板 ID（开发者后台「AskHuman Select」模板，用户导入发布）。
pub const DEFAULT_SELECT_CARD_TEMPLATE_ID: &str = "43e7b261-997d-45de-ac5e-92e49d59cad8.schema";

/// 每行触发按钮回传的 actionId（整卡统一）。
pub const ACTION_SELECT: &str = "select";

/// 选项字号（footnote=12px）：主行与次行同用，最紧凑（用户定稿）。
const SIZE_SMALL: &str = "common_footnote_text_style__font_size";
/// 圆点颜色 token（与飞书 green/grey 圆点对应）。
const COLOR_GREEN: &str = "common_green1_color";
const COLOR_GREY: &str = "common_level3_base_color";

/// 相邻 `<font>` 间的空格（普通空格被钉钉渲染器吞掉，把 NBSP 顶在标签内保住间距；见 watch.rs）。
const NBSP: char = '\u{00a0}';

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

/// 每行触发按钮的颜色（钉钉 SingleButton color 枚举）：关注/查看=蓝、取消=红。
fn button_color(action: crate::select::SelectAction) -> &'static str {
    match action {
        crate::select::SelectAction::Watch
        | crate::select::SelectAction::TaskWorkspace
        | crate::select::SelectAction::TaskAgent
        | crate::select::SelectAction::TaskPermission
        | crate::select::SelectAction::TaskInputSource
        | crate::select::SelectAction::Fork
        | crate::select::SelectAction::Status
        | crate::select::SelectAction::Msg
        | crate::select::SelectAction::MsgTarget
        | crate::select::SelectAction::Diff
        | crate::select::SelectAction::Stage
        | crate::select::SelectAction::Transcript
        | crate::select::SelectAction::Todo
        | crate::select::SelectAction::TodoRm
        | crate::select::SelectAction::TodoAuto
        | crate::select::SelectAction::TodoAutoEntry => "blue",
        crate::select::SelectAction::Unwatch
        | crate::select::SelectAction::TodoRmEntry
        | crate::select::SelectAction::Yolo => "red",
    }
}

/// 单个选项的两行富文本 `option_md`：
/// 第一行 `彩色圆点● + **[编号]** + 主文本(类型·目录) + 徽标`；第二行灰色小字标题（`\n\n` 断行）。
fn option_md(opt: &SelectOption) -> String {
    let mut line1 = String::new();
    if let Some(dot) = opt.dot {
        let color = match dot {
            SelectDot::Working => COLOR_GREEN,
            SelectDot::Idle => COLOR_GREY,
        };
        line1.push_str(&font(&format!("●{NBSP}"), None, Some(color)));
    }
    let mut head = String::new();
    if let Some(seq) = opt.seq {
        head.push_str(&format!("**[{}]** ", seq));
    }
    let (primary, attachment_badge) = crate::todos::split_attachment_badge(&opt.primary);
    head.push_str(primary);
    if let Some(badge) = &opt.badge {
        head.push(' ');
        head.push_str(badge);
    }
    if let Some(elapsed) = &opt.elapsed {
        head.push(' ');
        head.push_str(elapsed);
    }
    // 主行与次行同为 footnote 字号（用户定：agent 标题行缩到与下方描述一致，最紧凑）。
    line1.push_str(&font(&head, Some(SIZE_SMALL), None));
    if let Some(attachment_badge) = attachment_badge {
        line1.push(NBSP);
        line1.push_str(&font(attachment_badge, Some(SIZE_SMALL), Some(COLOR_GREY)));
    }
    match &opt.secondary {
        Some(sub) if !sub.is_empty() => {
            format!(
                "{}\n\n{}",
                line1,
                font(sub, Some(SIZE_SMALL), Some(COLOR_GREY))
            )
        }
        _ => line1,
    }
}

/// 组装单选卡【公有】`cardParamMap`（活动态；值均为字符串，复杂值转 JSON 字符串）。
/// `finalized="false"` → 显示循环列表；`final_label` 留空。
pub fn build_select_param_map(view: &SelectView, lang: Lang) -> Value {
    let title = match &view.truncated_note {
        Some(note) => format!("{} {}", view.title, note),
        None => view.title.clone(),
    };
    let items: Vec<Value> = view
        .options
        .iter()
        .map(|o| json!({ "option_md": option_md(o), "sid": o.id }))
        .collect();
    json!({
        "title": title,
        "btn_text": view.action.button_label(lang),
        "btn_color": button_color(view.action),
        // 布尔以字符串下发（钉钉约定，真布尔会报「StringValue is mandatory」）。
        "finalized": "false",
        "final_label": "",
        // 复杂类型 → JSON 字符串（与提问卡 options 同规）。
        "loop_object_list": Value::Array(items).to_string(),
    })
}

/// 定格【公有】`cardParamMap`（按 key 更新即可）：隐藏循环、显示 `final_label`。
pub fn build_select_final_param_map(final_label: &str) -> Value {
    json!({
        "finalized": "true",
        "final_label": final_label,
    })
}

/// 把一条卡片回调 `data` 解析为单选卡点选：`(outTrackId, sid)`。
/// 非 `select` 按钮 → None；`sid` 可能为空（模板未把选中项绑到 param，调用方据此判无效）。
pub fn parse_select_action(data: &Value) -> Option<(String, String)> {
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
    let private = inner.get("cardPrivateData")?;
    let is_select = private
        .get("actionIds")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some(ACTION_SELECT)))
        .unwrap_or(false);
    if !is_select {
        return None;
    }
    let sid = private
        .get("params")
        .and_then(|p| p.get("sid"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((otid, sid))
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

    #[test]
    fn param_map_has_all_template_variables() {
        let view = build_view(
            "选择要实时关注的 Agent：".into(),
            vec![
                opt(
                    "s-work",
                    Some(SelectDot::Working),
                    2,
                    "Claude Code · api-server",
                    Some("· 关注中"),
                    "重构数据层",
                ),
                opt(
                    "s-idle",
                    Some(SelectDot::Idle),
                    5,
                    "Cursor · web",
                    None,
                    "写测试",
                ),
            ],
            SelectAction::Watch,
            Lang::Zh,
        );
        let m = build_select_param_map(&view, Lang::Zh);
        for key in [
            "title",
            "btn_text",
            "btn_color",
            "finalized",
            "final_label",
            "loop_object_list",
        ] {
            assert!(m.get(key).is_some(), "missing {key}");
        }
        assert_eq!(m["btn_text"], "关注");
        assert_eq!(m["btn_color"], "blue");
        assert_eq!(m["finalized"], "false");
        // loop_object_list 为 JSON 字符串：`[{option_md, sid}]`。
        let parsed: Value = serde_json::from_str(m["loop_object_list"].as_str().unwrap()).unwrap();
        assert_eq!(parsed[0]["sid"], "s-work");
        let md0 = parsed[0]["option_md"].as_str().unwrap();
        // 工作中绿点（点后 NBSP 顶在标签内）+ 加粗编号 + 主文本 + 徽标；第二行灰色标题。
        assert!(md0.contains("colorTokenV2=common_green1_color>●\u{a0}</font>"));
        assert!(md0.contains("**[2]** Claude Code · api-server · 关注中"));
        assert!(md0.contains("\n\n"));
        assert!(md0.contains("colorTokenV2=common_level3_base_color>重构数据层</font>"));
        // 空闲灰点。
        let md1 = parsed[1]["option_md"].as_str().unwrap();
        assert!(md1.contains("colorTokenV2=common_level3_base_color>●\u{a0}</font>"));
    }

    #[test]
    fn todo_attachment_badge_uses_grey_rich_text() {
        let view = build_view(
            "选择任务来源".into(),
            vec![opt(
                "todo:1",
                None,
                1,
                "执行待办：修复登录 【2 个附件】",
                None,
                "",
            )],
            SelectAction::TaskInputSource,
            Lang::Zh,
        );
        let map = build_select_param_map(&view, Lang::Zh);
        let parsed: Value =
            serde_json::from_str(map["loop_object_list"].as_str().unwrap()).unwrap();
        let markdown = parsed[0]["option_md"].as_str().unwrap();
        assert!(markdown.contains("colorTokenV2=common_level3_base_color>【2 个附件】</font>"));
        assert!(!markdown.contains("📎"));
    }

    #[test]
    fn button_color_and_text_by_action() {
        let mk = |action: SelectAction| {
            let v = build_view(
                "T".into(),
                vec![opt("s", None, 1, "p", None, "t")],
                action,
                Lang::Zh,
            );
            build_select_param_map(&v, Lang::Zh)
        };
        let w = mk(SelectAction::Watch);
        assert_eq!(w["btn_text"], "关注");
        assert_eq!(w["btn_color"], "blue");
        let s = mk(SelectAction::Status);
        assert_eq!(s["btn_text"], "查看");
        assert_eq!(s["btn_color"], "blue");
        let u = mk(SelectAction::Unwatch);
        assert_eq!(u["btn_text"], "取消");
        assert_eq!(u["btn_color"], "red");
    }

    #[test]
    fn final_param_map_sets_flag_and_label() {
        let m = build_select_final_param_map("已选择 [3]");
        assert_eq!(m["finalized"], "true");
        assert_eq!(m["final_label"], "已选择 [3]");
    }

    #[test]
    fn parse_select_roundtrip() {
        let data = json!({
            "outTrackId": "select-abc",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"select\"],\"params\":{\"sid\":\"s-42\"}}}",
        });
        assert_eq!(
            parse_select_action(&data),
            Some(("select-abc".into(), "s-42".into()))
        );
        // 非 select 按钮（如 watch 卡按钮）→ None。
        let watch = json!({
            "outTrackId": "watch-1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"watch_unwatch\"],\"params\":{}}}",
        });
        assert_eq!(parse_select_action(&watch), None);
        // select 但 param 未带 sid（模板未绑定）→ sid 为空，调用方判无效。
        let empty = json!({
            "outTrackId": "select-x",
            "value": {"cardPrivateData":{"actionIds":["select"],"params":{}}},
        });
        assert_eq!(
            parse_select_action(&empty),
            Some(("select-x".into(), String::new()))
        );
    }
}
