//! 互动卡片高级版（A 方案）：按已发布模板组装 cardData（公有数据）与解析「提交」回调。
//!
//! 模板变量约定（D15/D16 定稿，见 `docs/specs/strict-choice-and-structured-output.md`）：
//! - 公有：`title`(标题) / `markdown`(正文) / `options`(JSON 串 `[{id:下标(int), md:富文本}]`) /
//!   `single`("true"|"false" 单选列表/多选列表) / `allow_input`("true"|"false" 输入框显隐) /
//!   `submit_status`(终态文案：已提交 / 已在X回答)。
//! - 私有：`submitted`("false") / `private_input`("")。
//! - 提交按钮 `actionId="submit_action"`，回传 `params={user_input, selected_options}`，
//!   `selected_options` 装【选项 id(下标)】（多选=id 数组、单选=单值或 `{id}`，解析需兼容三态）。
//!
//! cardData 填充规则：复杂值（对象/数组）需转成 JSON 字符串放入 `cardParamMap`；
//! 布尔/数字同样以字符串下发（钉钉约定，下发真布尔会报「StringValue is mandatory」）。

use crate::models::OptionItem;
use serde_json::{json, Value};

/// 「提交」按钮回传的 actionId。
pub const SUBMIT_ACTION_ID: &str = "submit_action";

/// 选项富文本字号（钉钉互动卡片只认预设 sizeToken；h5=15px，介于 footnote(12) 与 body(14/17) 之间，D18）。
const OPT_FONT_SIZE: &str = "common_h5_text_style__font_size";
/// 推荐徽标绿色 colorToken。
const GREEN_COLOR: &str = "common_green1_color";
/// Todo marker amber colorToken.
const ORANGE_COLOR: &str = "common_orange1_color";
/// Todo attachment badge uses secondary text so it remains distinct without competing with TODO.
const GREY_COLOR: &str = "common_level3_base_color";

/// 一次卡片「提交」回调的解析结果。
pub struct CardSubmit {
    pub user_id: String,
    pub out_track_id: String,
    /// 勾选的预定义选项【下标】（id；已去重）。由会话按下标还原选项原文。
    pub selected_indices: Vec<usize>,
    /// 补充文字输入（空则 None）。
    pub user_input: Option<String>,
}

/// 单个选项的富文本 `md`：h5 字号包裹；推荐项前置绿色含括号徽标。
fn option_md(
    opt: &OptionItem,
    recommended_label: &str,
    todo_text_prefix: &str,
    todo_label: &str,
) -> String {
    let display_text = if opt.todo_id.is_some() {
        opt.text.strip_prefix(todo_text_prefix).unwrap_or(&opt.text)
    } else {
        &opt.text
    };
    let (display_text, attachment_badge) = crate::todos::split_attachment_badge(display_text);
    let mut body = format!("<font sizeToken={}>{}</font>", OPT_FONT_SIZE, display_text);
    if let Some(badge) = attachment_badge {
        body.push_str(&format!(
            " <font sizeToken={} colorTokenV2={}>{}</font>",
            OPT_FONT_SIZE, GREY_COLOR, badge
        ));
    }
    if opt.todo_id.is_some() {
        format!(
            "<font sizeToken={} colorTokenV2={}>{}</font> {}",
            OPT_FONT_SIZE, ORANGE_COLOR, todo_label, body
        )
    } else if opt.recommended {
        format!(
            "<font sizeToken={} colorTokenV2={}>{}</font> {}",
            OPT_FONT_SIZE, GREEN_COLOR, recommended_label, body
        )
    } else {
        body
    }
}

/// 组装卡片【公有】数据 `cardParamMap`（值均为字符串）。
/// `title` 题首；`markdown` 正文；`options` 预定义选项；`single`→单选列表、`select_only`→隐藏输入框；
/// `recommended_label` 推荐徽标文案（本地化，card 内包绿色 font）。
/// 注意：只放公有变量。`submitted`/`private_input` 是模板的【私有】变量，
/// 一旦混进公有 cardData，钉钉会拒绝整份公有数据导致卡片空白，故不在此下发
/// （初始未提交态由模板默认值兜底）。
pub fn build_card_param_map(
    title: &str,
    markdown: &str,
    options: &[OptionItem],
    single: bool,
    select_only: bool,
    recommended_label: &str,
) -> Value {
    build_card_param_map_with_todo(
        title,
        markdown,
        options,
        single,
        select_only,
        recommended_label,
        "",
        "",
    )
}

/// Build card data with an amber marker for todo options while preserving callback values.
#[allow(clippy::too_many_arguments)]
pub fn build_card_param_map_with_todo(
    title: &str,
    markdown: &str,
    options: &[OptionItem],
    single: bool,
    select_only: bool,
    recommended_label: &str,
    todo_text_prefix: &str,
    todo_label: &str,
) -> Value {
    // 选项：`[{id:下标, md:富文本}]`（id 供提交回传，md 供展示）。
    let option_objs: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            json!({
                "id": i,
                "md": option_md(o, recommended_label, todo_text_prefix, todo_label),
            })
        })
        .collect();
    json!({
        "title": title,
        "markdown": markdown,
        // 复杂类型 → JSON 字符串。
        "options": Value::Array(option_objs).to_string(),
        // 布尔以字符串下发（模板按变量类型还原；真布尔会报错）。
        "single": if single { "true" } else { "false" },
        "allow_input": if select_only { "false" } else { "true" },
        // 终态文案（submitted=true 时模板展示）；初始为空。公有变量。
        "submit_status": "",
    })
}

/// 组装卡片【私有】数据 `cardParamMap`（值均为字符串）。
/// 投放时必须下发这些私有变量的默认值：模板渲染表达式会读取它们，缺省为 null 会导致
/// 「内容加载失败」。走私有通道（privateData），不能混进公有 cardData。
pub fn build_card_private_map() -> Value {
    json!({
        "submitted": "false",
        "private_input": "",
    })
}

/// 这条卡片回调是否由「提交」按钮触发（供 Router 决定回包类型，无需完整解析）。
pub fn is_submit(data: &Value) -> bool {
    parse_card_submit(data).is_some()
}

/// 「提交」回调的同步成功回包：置灰点击者私有 `submitted=true`，使钉钉端判定提交成功
/// （否则空回包会被互动卡片判为「请求失败」）。公有终态文案（已提交 / 已在 X 回答）
/// 仍由会话经 OpenAPI `update_card_private` 异步写入。`submitted` 须与 `build_card_private_map` 一致。
pub fn submit_ack_success() -> Value {
    json!({
        "cardUpdateOptions": { "updatePrivateDataByKey": true },
        "userPrivateData": { "cardParamMap": { "submitted": "true" } },
    })
}

/// 把一条卡片回调 `data` 解析为「提交」结果；非提交 / 非本类回调返回 None。
pub fn parse_card_submit(data: &Value) -> Option<CardSubmit> {
    let user_id = data
        .get("userId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let out_track_id = data
        .get("outTrackId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // content 优先，回退 value；二者皆为 JSON 字符串（也兼容已是对象的情况）。
    let inner: Value = data
        .get("content")
        .or_else(|| data.get("value"))
        .and_then(parse_maybe_json)?;
    let private = inner.get("cardPrivateData")?;

    // 必须是「提交」按钮触发。
    let is_submit = private
        .get("actionIds")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().any(|v| v.as_str() == Some(SUBMIT_ACTION_ID)))
        .unwrap_or(false);
    if !is_submit {
        return None;
    }

    let params = private.get("params");
    let selected_indices = params
        .and_then(|p| p.get("selected_options"))
        .map(extract_ids)
        .unwrap_or_default();
    let user_input = params
        .and_then(|p| p.get("user_input"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Some(CardSubmit {
        user_id,
        out_track_id,
        selected_indices,
        user_input,
    })
}

/// 把回调里可能是「JSON 字符串」或「对象」的字段统一解析成 `Value`。
fn parse_maybe_json(v: &Value) -> Option<Value> {
    match v {
        Value::String(s) => serde_json::from_str(s).ok(),
        other => Some(other.clone()),
    }
}

/// 把 `selected_options` 抽取成选项【下标】列表，兼容三态（去重）：
/// - 多选：id 数组 `[0, 2]`（元素为数字 / 数字串 / `{id}`/`{value}` 对象）；
/// - 单选：单值 `0` / 数字串 `"0"` / 单对象 `{id:0}`。
fn extract_ids(v: &Value) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let push = |id: usize, out: &mut Vec<usize>| {
        if !out.contains(&id) {
            out.push(id);
        }
    };
    match v {
        Value::Array(arr) => {
            for el in arr {
                if let Some(id) = value_to_id(el) {
                    push(id, &mut out);
                }
            }
        }
        other => {
            if let Some(id) = value_to_id(other) {
                push(id, &mut out);
            }
        }
    }
    out
}

/// 把一个值解析为选项下标：数字 / 数字串 / `{id}`/`{value}` 对象。
fn value_to_id(v: &Value) -> Option<usize> {
    match v {
        Value::Number(n) => n.as_u64().map(|x| x as usize),
        Value::String(s) => s.trim().parse::<usize>().ok(),
        Value::Object(_) => v.get("id").or_else(|| v.get("value")).and_then(value_to_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(items: &[&str]) -> Vec<OptionItem> {
        items.iter().map(|s| OptionItem::new(*s, false)).collect()
    }

    #[test]
    fn build_param_map_stringifies_complex() {
        let m = build_card_param_map(
            "Question 1/2",
            "要继续吗？",
            &opts(&["继续", "停止"]),
            false,
            false,
            "【👍推荐】",
        );
        assert_eq!(m.get("title").unwrap(), "Question 1/2");
        assert_eq!(m.get("markdown").unwrap(), "要继续吗？");
        assert_eq!(m.get("submit_status").unwrap(), "");
        // 布尔以字符串下发。
        assert_eq!(m.get("single").unwrap(), "false");
        assert_eq!(m.get("allow_input").unwrap(), "true");
        // 私有变量不应出现在公有 cardParamMap 中。
        assert!(m.get("submitted").is_none());
        assert!(m.get("private_input").is_none());
        // options 为 JSON 字符串：`[{id, md}]`，md 带 h5 字号。
        let parsed: Value =
            serde_json::from_str(m.get("options").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(parsed[0]["id"], 0);
        assert_eq!(parsed[1]["id"], 1);
        assert_eq!(
            parsed[0]["md"].as_str().unwrap(),
            "<font sizeToken=common_h5_text_style__font_size>继续</font>"
        );
    }

    #[test]
    fn build_param_map_single_and_select_only_flags() {
        let m = build_card_param_map("T", "Q", &opts(&["a"]), true, true, "R");
        assert_eq!(m.get("single").unwrap(), "true");
        assert_eq!(m.get("allow_input").unwrap(), "false");
    }

    #[test]
    fn recommended_option_md_has_green_prefix() {
        let options = vec![OptionItem::new("继续", true)];
        let m = build_card_param_map("T", "Q", &options, false, false, "【👍推荐】");
        let parsed: Value =
            serde_json::from_str(m.get("options").unwrap().as_str().unwrap()).unwrap();
        let md = parsed[0]["md"].as_str().unwrap();
        assert!(md.contains("colorTokenV2=common_green1_color"));
        assert!(md.contains("【👍推荐】"));
        assert!(md.contains("继续"));
    }

    #[test]
    fn todo_option_md_has_amber_marker_without_legacy_prefix() {
        let options = vec![OptionItem::with_todo("执行待办：修复登录", "todo-1")];
        let m = build_card_param_map_with_todo(
            "T",
            "Q",
            &options,
            false,
            false,
            "【👍推荐】",
            "执行待办：",
            "【TODO】",
        );
        let parsed: Value =
            serde_json::from_str(m.get("options").unwrap().as_str().unwrap()).unwrap();
        let md = parsed[0]["md"].as_str().unwrap();
        assert!(md.contains("colorTokenV2=common_orange1_color"));
        assert!(md.contains("【TODO】"));
        assert!(md.contains("修复登录"));
        assert!(!md.contains("执行待办："));
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
        let map = build_card_param_map_with_todo(
            "T",
            "Q",
            &[option],
            false,
            false,
            "【👍推荐】",
            "执行待办：",
            "【TODO】",
        );
        let parsed: Value =
            serde_json::from_str(map.get("options").unwrap().as_str().unwrap()).unwrap();
        let md = parsed[0]["md"].as_str().unwrap();
        assert!(md.contains("colorTokenV2=common_level3_base_color>【1 个附件】</font>"));
        assert!(!md.contains("📎"));
    }

    #[test]
    fn parse_submit_id_array() {
        let data = json!({
            "userId": "u1",
            "outTrackId": "t1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"submit_action\"],\"params\":{\"user_input\":\" hi \",\"selected_options\":[0,2,0]}}}",
        });
        let s = parse_card_submit(&data).unwrap();
        assert_eq!(s.user_id, "u1");
        assert_eq!(s.out_track_id, "t1");
        assert_eq!(s.selected_indices, vec![0, 2]);
        assert_eq!(s.user_input.as_deref(), Some("hi"));
    }

    #[test]
    fn parse_submit_single_value_and_object_and_string() {
        // 单选单值。
        let single_val = json!({
            "userId": "u1", "outTrackId": "t1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"submit_action\"],\"params\":{\"selected_options\":1}}}",
        });
        assert_eq!(
            parse_card_submit(&single_val).unwrap().selected_indices,
            vec![1]
        );
        // 单选对象 {id}。
        let single_obj = json!({
            "userId": "u1", "outTrackId": "t1",
            "value": {"cardPrivateData":{"actionIds":["submit_action"],"params":{"selected_options":{"id":2}}}},
        });
        assert_eq!(
            parse_card_submit(&single_obj).unwrap().selected_indices,
            vec![2]
        );
        // 数字串数组。
        let str_arr = json!({
            "userId": "u1", "outTrackId": "t1",
            "value": {"cardPrivateData":{"actionIds":["submit_action"],"params":{"selected_options":["0","1"]}}},
        });
        assert_eq!(
            parse_card_submit(&str_arr).unwrap().selected_indices,
            vec![0, 1]
        );
    }

    #[test]
    fn parse_non_submit_returns_none() {
        let data = json!({
            "userId": "u1",
            "outTrackId": "t1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"opt_0\"],\"params\":{}}}",
        });
        assert!(parse_card_submit(&data).is_none());
    }

    #[test]
    fn is_submit_distinguishes_submit_from_toggle() {
        let submit = json!({
            "userId": "u1",
            "outTrackId": "t1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"submit_action\"],\"params\":{}}}",
        });
        let toggle = json!({
            "userId": "u1",
            "outTrackId": "t1",
            "content": "{\"cardPrivateData\":{\"actionIds\":[\"opt_0\"],\"params\":{}}}",
        });
        assert!(is_submit(&submit));
        assert!(!is_submit(&toggle));
    }

    #[test]
    fn submit_ack_success_greys_private_submitted() {
        let v = submit_ack_success();
        // 必须置灰私有 submitted=true 且开启 updatePrivateDataByKey，否则钉钉端会判「请求失败」。
        assert_eq!(
            v["cardUpdateOptions"]["updatePrivateDataByKey"],
            json!(true)
        );
        assert_eq!(
            v["userPrivateData"]["cardParamMap"]["submitted"],
            json!("true")
        );
    }
}
