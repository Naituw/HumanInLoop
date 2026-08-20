//! 解析各家 agent 会话 transcript 的**尾部**，得到「当前在做什么」：最后一段助手文字 + 最近的
//! 工具**足迹时间线**（≤3 步，含每步完成/进行中标志）。供 IM `/status <编号>` 与 `/watch` 实时卡
//! 展示（设计见 `docs/plans/im-status-activity.md` 与 `docs/specs/im-watch.md`）。
//!
//! 规则：只要助手在会话里输出过文字，就**永远**带上「最后一段助手文字」；足迹时间线只含
//! **最后一段文字之后**的工具调用（用户定案：最新事件是文字时不显示任何工具行；文字之前的
//! 调用属于「上一段叙述」，不再罗列），超出 3 步的部分计入 `steps_omitted` 供「省略 N 步」标注。
//! 仅当尾部停在未完的工具调用时，末步标「进行中」——但 **Cursor 例外**：其 transcript 在工具
//! 跑完后才落盘该次调用，落盘即已结束，故 Cursor 的 transcript 步一律「已完成」，「进行中」
//! 只能由实时 hook 的 `currentTool` 并入（`autochannel`）。
//!
//! 全部 best-effort：文件不存在 / 正在写 / 巨大 / 解析失败都尽量降级；尾部读取有字节上限，不拖慢 daemon。

use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::title::transcript_path;
use super::AgentKind;

/// 尾部读取的字节上限（只关心最新事件，对超大 transcript 有界）。
const MAX_TAIL_BYTES: u64 = 256 * 1024;
/// 「最后一段助手文字」最大展示字符数。
const MAX_ACTIVITY_TEXT_CHARS: usize = 500;
/// 工具对象（文件名 / 命令首段 / 参数前段）最大展示字符数。
const MAX_TOOL_OBJECT_CHARS: usize = 60;
/// 足迹时间线保留的最近步数。
pub const MAX_STEPS: usize = 3;
/// TODO 清单条目内容最大展示字符数。
const MAX_TODO_CHARS: usize = 60;

/// 一次「当前活动」解析结果。`text`/`steps`/`todos` 至少一个非空时才会返回（否则 `resolve_activity` 返回 `None`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    /// 最后一段助手自然语言文字（已截断）。
    pub text: Option<String>,
    /// 最后一段文字**之后**的工具足迹（≤`MAX_STEPS` 步，旧→新；文字出现即清空重计）。
    pub steps: Vec<ToolStep>,
    /// 文字之后被挤出时间线的更早调用数（「省略 N 步」标注；TODO 更新不计）。
    pub steps_omitted: usize,
    /// 当前 TODO 清单（TodoWrite / update_plan 解析重放结果；已剔除 cancelled 条目）。
    pub todos: Vec<TodoItem>,
    /// 最后活动时间（transcript 文件 mtime 的 Unix 秒；取不到为 `None`）。
    /// 用文件写入时间作「最近一次事件」的通用代理：各家 transcript 每次事件都会追加写盘，
    /// 无需逐家解析事件时间戳；对 Cursor「工具跑完才落盘」也与展示内容（最后完成事件）一致。
    pub at: Option<u64>,
}

/// TODO 清单一条（agent 自报计划：Cursor/Claude 的 TodoWrite、Codex 的 update_plan）。
#[derive(Debug, Clone, PartialEq)]
pub struct TodoItem {
    /// 条目文字（已截断）。
    pub content: String,
    pub state: TodoState,
}

/// TODO 条目状态。`Cancelled` 仅在合并重放期间存在（merge 更新可取消既有条目），
/// 最终结果（`Activity::todos`）已剔除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoState {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

/// 足迹时间线中的一步：一次工具调用 + 状态。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolStep {
    pub tool: ToolDisplay,
    pub state: StepState,
}

/// 一步的状态。**只有末步可能是 Running**：Cursor 等家族的 transcript 不一定写工具结果事件，
/// 「后面又发生了别的调用/文字」即视为前面的步已完成；带 `is_error` 的工具结果 → Failed。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Failed,
}

/// 一次工具调用的展示信息。类别词与前缀符号的本地化渲染在 `autochannel`（`object` 是内容，不本地化）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDisplay {
    pub label: ToolLabel,
    /// 简短对象：文件名 / 命令首段 / 参数前段（已截断）。
    pub object: Option<String>,
}

/// 工具类别。只归一化常见工具；其余保留原始工具名。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolLabel {
    /// 运行命令（Bash/Shell/…）。
    Run,
    /// 读取文件。
    Read,
    /// 写入 / 编辑文件。
    Write,
    /// 其它工具：携带原始工具名。
    Other(String),
}

/// 解析某家 agent 某 session 的「当前活动」。取不到（文件缺失 / 无文字也无工具）返回 `None`。
/// Cursor 先试 IDE 的 vscdb 实时源（jsonl 在 IDE 长回合内会冻结数小时，spec gui-agent-console
/// 反馈记录 2026-07-25）；未命中（CLI 会话 / 库缺失 / schema 变化）回退 jsonl。
pub fn resolve_activity(kind: AgentKind, session_id: &str) -> Option<Activity> {
    if kind == AgentKind::Cursor {
        if let Some(a) = super::cursor_vscdb::resolve_activity(session_id) {
            return Some(a);
        }
    }
    let path = transcript_path(kind, session_id)?;
    let lines = read_tail(&path, MAX_TAIL_BYTES);
    let mut activity = analyze(kind, &lines)?;
    activity.at = file_mtime_secs(&path);
    Some(activity)
}

/// Read the last assistant text from a caller-validated transcript path without the `/status`
/// 500-character display limit. Reads remain bounded by `MAX_TAIL_BYTES`.
pub fn resolve_last_assistant_text_from_path(
    kind: AgentKind,
    path: &Path,
    max_chars: usize,
) -> Option<String> {
    resolve_last_assistant_text_from_path_raw(kind, path)
        .map(|text| truncate_preserving_layout(&text, max_chars))
}

/// Read the raw last assistant text from a caller-validated transcript path. The file read is
/// bounded even though the returned event text is not display-truncated.
pub fn resolve_last_assistant_text_from_path_raw(kind: AgentKind, path: &Path) -> Option<String> {
    let lines = read_tail(path, MAX_TAIL_BYTES);
    let mut last = None;
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let mut events = Vec::new();
        push_events(kind, &value, &mut events);
        let mut text_parts = Vec::new();
        for event in events {
            if let Ev::Text(text) = event {
                text_parts.push(text);
            }
        }
        if !text_parts.is_empty() {
            last = Some(text_parts.join("\n"));
        }
    }
    last
}

fn truncate_preserving_layout(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars).collect();
        format!("{}\n\n… [truncated]", head.trim_end())
    }
}

/// 取文件 mtime 的 Unix 秒（best-effort）。
fn file_mtime_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// 读文件尾部至多 `max_bytes` 字节，按行返回；若从中间切入则丢弃首个可能不完整的行。
fn read_tail(path: &Path, max_bytes: u64) -> Vec<String> {
    let Ok(mut f) = fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = f.metadata() else {
        return Vec::new();
    };
    let len = meta.len();
    let start = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // 半行
    }
    lines
}

/// 尾部窗口内的一条「有意义事件」。`pub(super)`：Cursor IDE 的 vscdb 适配器
/// （`cursor_vscdb.rs`）把 bubble 转成同一事件流复用 `aggregate` 聚合。
#[derive(Clone)]
pub(super) enum Ev {
    /// 助手自然语言文字。
    Text(String),
    /// 工具调用。
    Tool(ToolDisplay),
    /// 工具结果（bool = 是否失败，取 `is_error` 显式标志；无标志视为成功）。
    ToolResult(bool),
    /// TODO 清单更新（TodoWrite / update_plan；不入足迹时间线，单独重放）。
    /// `replace`：整表替换（Claude / Codex / Cursor merge=false）；否则按 id 合并（Cursor merge=true）。
    Todos {
        replace: bool,
        items: Vec<(Option<String>, TodoItem)>,
    },
}

/// 从尾部行序列计算「当前活动」。抽出便于单测（不触盘）。
fn analyze(kind: AgentKind, lines: &[String]) -> Option<Activity> {
    let mut evs: Vec<Ev> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        push_events(kind, &v, &mut evs);
    }
    // Cursor jsonl 的调用**跑完后**才落盘且从不写 tool_result（见 aggregate 注释）→ 全部收敛；
    // vscdb 源自带真实状态，不走此收敛。
    aggregate(&evs, kind == AgentKind::Cursor)
}

/// 事件流 → 「当前活动」聚合（jsonl 与 vscdb 两源共用）。`settle_all`：结尾把所有仍在跑的
/// 步收敛为已完成（Cursor jsonl 语义——落盘即已结束；其它源维持末步真实状态）。
pub(super) fn aggregate(evs: &[Ev], settle_all: bool) -> Option<Activity> {
    let mut last_text: Option<String> = None;
    let mut steps: Vec<ToolStep> = Vec::new();
    // 文字之后被挤出时间线的调用数（「省略 N 步」标注）。
    let mut omitted: usize = 0;
    // TODO 清单重放台账：`(id, item)`，id 供 Cursor merge=true 按条合并。
    let mut todo_list: Vec<(Option<String>, TodoItem)> = Vec::new();
    // 把所有仍在跑的步收敛为已完成（后续事件的出现即证明其已结束）。
    fn settle_running(steps: &mut [ToolStep]) {
        for s in steps {
            if s.state == StepState::Running {
                s.state = StepState::Done;
            }
        }
    }
    for ev in evs {
        match ev {
            Ev::Text(t) => {
                last_text = Some(t.clone());
                // 时间线只含最后一段文字**之后**的调用：文字出现即清空重计（用户定案——
                // 最新事件是文字时不显示工具行；此前的调用属于上一段叙述）。
                steps.clear();
                omitted = 0;
            }
            Ev::Tool(td) => {
                // 新调用开始 → 更早的步视为已完成（Cursor 等家族不一定写工具结果事件，
                // 「后面又发生了事」是唯一可靠的完成信号）。故**只有末步可能是进行中**。
                settle_running(&mut steps);
                steps.push(ToolStep {
                    tool: td.clone(),
                    state: StepState::Running,
                });
                if steps.len() > MAX_STEPS {
                    steps.remove(0);
                    omitted += 1;
                }
            }
            Ev::ToolResult(failed) => {
                // 结果闭合最早仍在跑的步；带失败标志（is_error）→ 记失败。
                if let Some(s) = steps.iter_mut().find(|s| s.state == StepState::Running) {
                    s.state = if *failed {
                        StepState::Failed
                    } else {
                        StepState::Done
                    };
                }
            }
            Ev::Todos { replace, items } => {
                // TODO 更新本身也是一次工具调用（不入时间线）：证明此前的步已结束。
                // 其后续 tool_result（Claude 会写）落到无 Running 步上自然 no-op。
                settle_running(&mut steps);
                apply_todo_update(&mut todo_list, *replace, items);
            }
        }
    }

    // Cursor jsonl 的 transcript 只在工具**跑完后**才落盘该次调用（实测 in-flight 探针不可见），
    // 且从不写 tool_result 事件：出现在 transcript 里的步必已结束 → 全部收敛为已完成。
    // 「进行中」只能来自实时 hook（`activity_parts` 并入的 `currentTool` 末步）。
    // Claude / Codex / Grok 在调用**开始**时即写盘、vscdb 源自带真实状态 → 维持进行中。
    if settle_all {
        settle_running(&mut steps);
    }

    let todos: Vec<TodoItem> = todo_list
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| t.state != TodoState::Cancelled)
        .collect();
    let text = last_text.map(|t| truncate(&t, MAX_ACTIVITY_TEXT_CHARS));
    if text.is_none() && steps.is_empty() && todos.is_empty() {
        return None;
    }
    // `at` 由 `resolve_activity` 从文件 mtime 填充；`analyze` 是纯函数不触盘。
    Some(Activity {
        text,
        steps,
        steps_omitted: omitted,
        todos,
        at: None,
    })
}

/// TODO 重放一步（jsonl 聚合与 vscdb 适配器共用）：`replace` 整表替换；否则按 id 就地
/// 更新、未知 id 追加（保持原有顺序，Cursor merge=true 语义）。
pub(super) fn apply_todo_update(
    ledger: &mut Vec<(Option<String>, TodoItem)>,
    replace: bool,
    items: &[(Option<String>, TodoItem)],
) {
    if replace {
        *ledger = items.to_vec();
        return;
    }
    for (id, item) in items {
        let hit = id.as_deref().and_then(|id| {
            ledger
                .iter_mut()
                .find(|(eid, _)| eid.as_deref() == Some(id))
        });
        match hit {
            Some((_, existing)) => *existing = item.clone(),
            None => ledger.push((id.clone(), item.clone())),
        }
    }
}

fn push_events(kind: AgentKind, v: &Value, out: &mut Vec<Ev>) {
    match kind {
        AgentKind::Cursor | AgentKind::Claude => push_events_msg(v, out),
        AgentKind::Codex => push_events_codex(v, out),
        AgentKind::Grok => push_events_grok(v, out),
        AgentKind::Pi => push_events_pi(v, out),
    }
}

/// Pi v3 JSONL wraps every conversational message in `{ type: "message", message: ... }`.
fn push_events_pi(v: &Value, out: &mut Vec<Ev>) {
    if v.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    let Some(message) = v.get("message") else {
        return;
    };
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    if role == "toolResult" {
        out.push(Ev::ToolResult(
            message
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ));
        return;
    }
    let Some(content) = message.get("content") else {
        return;
    };
    if role == "assistant" {
        let Some(parts) = content.as_array() else {
            return;
        };
        for part in parts {
            match part.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        let text = text.trim();
                        if !text.is_empty() {
                            out.push(Ev::Text(text.to_string()));
                        }
                    }
                }
                "toolCall" => {
                    let name = part.get("name").and_then(Value::as_str).unwrap_or("");
                    let arguments = part.get("arguments");
                    if is_todo_tool(name) {
                        if let Some(event) = parse_todos(arguments) {
                            out.push(event);
                        }
                    } else {
                        out.push(Ev::Tool(classify_tool(name, arguments)));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Cursor / Claude：一次 assistant 消息含 `content:[{type:text},{type:tool_use}]`；工具结果在 user 消息的
/// `content:[{type:tool_result}]`。文字仅取 assistant 行，工具结果不限角色。
fn push_events_msg(v: &Value, out: &mut Vec<Ev>) {
    let is_assistant = v.get("role").and_then(|r| r.as_str()) == Some("assistant")
        || v.get("type").and_then(|t| t.as_str()) == Some("assistant");
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| v.get("content"));
    let Some(arr) = content.and_then(|c| c.as_array()) else {
        return;
    };
    for item in arr {
        match item.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" if is_assistant => {
                if let Some(t) = item.get("text").and_then(|x| x.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() {
                        out.push(Ev::Text(t.to_string()));
                    }
                }
            }
            "tool_use" => {
                let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
                if is_todo_tool(name) {
                    // TODO 清单更新不入足迹时间线，单独重放（Cursor/Claude 的 TodoWrite）。
                    if let Some(ev) = parse_todos(item.get("input")) {
                        out.push(ev);
                    }
                } else {
                    out.push(Ev::Tool(classify_tool(name, item.get("input"))));
                }
            }
            "tool_result" => out.push(Ev::ToolResult(
                item.get("is_error")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
            )),
            _ => {}
        }
    }
}

/// Codex rollout：`response_item.payload` 的 `message`(assistant output_text) / `function_call` /
/// `function_call_output`，以及 Code Mode 的 `custom_tool_call` / `custom_tool_call_output`；
/// `event_msg.payload` 的 `agent_message` 与 `patch_apply_end`；paginated 会话另有
/// `item_completed` 的 `AgentMessage` / `FileChange`。reasoning / token_count 忽略。
fn push_events_codex(v: &Value, out: &mut Vec<Ev>) {
    let ttype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let Some(payload) = v.get("payload") else {
        return;
    };
    let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match (ttype, ptype) {
        ("response_item", "message") => {
            if payload.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                if let Some(t) = value_text(payload.get("content")) {
                    let t = t.trim();
                    if !t.is_empty() {
                        out.push(Ev::Text(t.to_string()));
                    }
                }
            }
        }
        ("response_item", "function_call") => {
            let name = payload.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if is_todo_tool(name) {
                // Codex 的 update_plan：整表计划更新，不入足迹时间线。
                if let Some(ev) = parse_todos(payload.get("arguments")) {
                    out.push(ev);
                }
            } else {
                let args = parse_args(payload.get("arguments"));
                out.push(Ev::Tool(classify_tool(name, args.as_ref())));
            }
        }
        // Codex 的 output 是纯字符串（无结构化成败标志）→ 一律视为成功。
        ("response_item", "function_call_output") => out.push(Ev::ToolResult(false)),
        // Code Mode 把实际工具包在 custom `exec` 的 JavaScript 中。对 exec_command 提取其 JSON
        // 参数：常见只读命令归一为 Read，其余保底显示为 Run；其它内层工具保留工具名。
        // apply_patch 由下方结构化 patch_apply_end 展示，避免同一次编辑重复出现。
        ("response_item", "custom_tool_call") => {
            if let Some(tool) = codex_custom_tool(payload) {
                out.push(Ev::Tool(tool));
            }
        }
        ("response_item", "custom_tool_call_output") => {
            out.push(Ev::ToolResult(custom_tool_output_failed(payload)))
        }
        // Code Mode 的 apply_patch 被包在 custom `exec` 调用内，不再以 function_call 暴露；
        // 完成时会落一条结构化 patch_apply_end，直接用 changes 文件表生成稳定、无正文泄露的足迹。
        ("event_msg", "patch_apply_end") => {
            out.push(Ev::Tool(ToolDisplay {
                label: ToolLabel::Write,
                object: patch_changes_object(payload.get("changes")),
            }));
            let failed = !payload
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            out.push(Ev::ToolResult(failed));
        }
        ("event_msg", "agent_message") => {
            if let Some(t) = payload.get("message").and_then(|m| m.as_str()) {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(Ev::Text(t.to_string()));
                }
            }
        }
        // Codex 0.148 paginated history: legacy completion events are replaced by
        // `item_completed` + TurnItem. FileChange is the Write footprint; AgentMessage
        // is last-assistant text when no `response_item` assistant copy is present.
        ("event_msg", "item_completed") => {
            push_codex_item_completed(payload.get("item"), out);
        }
        _ => {}
    }
}

fn push_codex_item_completed(item: Option<&Value>, out: &mut Vec<Ev>) {
    let Some(item) = item else {
        return;
    };
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "FileChange" => {
            out.push(Ev::Tool(ToolDisplay {
                label: ToolLabel::Write,
                object: patch_changes_object(item.get("changes")),
            }));
            out.push(Ev::ToolResult(codex_file_change_failed(item)));
        }
        "AgentMessage" => {
            if let Some(text) = value_text(item.get("content")) {
                let text = text.trim();
                if !text.is_empty() {
                    out.push(Ev::Text(text.to_string()));
                }
            }
        }
        _ => {}
    }
}

pub(super) fn codex_file_change_failed(item: &Value) -> bool {
    match item.get("status") {
        Some(Value::Bool(success)) => !success,
        Some(Value::String(status)) => {
            let status = status.to_ascii_lowercase();
            status != "completed" && status != "success" && status != "ok"
        }
        Some(Value::Object(status)) => !status
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        _ => false,
    }
}

/// Parse a Codex Code Mode custom tool wrapper into a visible activity step.
///
/// The wrapper currently uses canonical JavaScript such as
/// `await tools.exec_command({"cmd":"..."})`. The argument itself is JSON, so this parser only
/// depends on that stable fragment and falls back to the outer tool when a future wrapper changes.
fn codex_custom_tool(payload: &Value) -> Option<ToolDisplay> {
    let outer_name = payload.get("name").and_then(Value::as_str).unwrap_or("");
    let input = payload.get("input").and_then(Value::as_str).unwrap_or("");
    if outer_name != "exec" {
        let args = (!input.is_empty()).then(|| Value::String(input.to_string()));
        return Some(classify_tool(outer_name, args.as_ref()));
    }

    let Some((inner_name, args)) = codex_inner_tool(input) else {
        return Some(ToolDisplay {
            label: ToolLabel::Run,
            object: (!input.trim().is_empty()).then(|| truncate(input, MAX_TOOL_OBJECT_CHARS)),
        });
    };
    if inner_name == "apply_patch" {
        return None;
    }
    if inner_name == "exec_command" {
        let cmd = args
            .as_ref()
            .and_then(|value| value.get("cmd"))
            .and_then(Value::as_str);
        return Some(ToolDisplay {
            label: if cmd.is_some_and(is_common_read_command) {
                ToolLabel::Read
            } else {
                ToolLabel::Run
            },
            object: arg_command(args.as_ref()),
        });
    }
    Some(classify_tool(&inner_name, args.as_ref()))
}

/// Extract the first `tools.<name>(<JSON value>)` invocation from a Code Mode wrapper.
fn codex_inner_tool(input: &str) -> Option<(String, Option<Value>)> {
    let marker = "tools.";
    let start = input.find(marker)? + marker.len();
    let rest = &input[start..];
    let open = rest.find('(')?;
    let name = rest[..open].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    let json = rest[open + 1..].trim_start();
    let args = serde_json::Deserializer::from_str(json)
        .into_iter::<Value>()
        .next()
        .and_then(Result::ok);
    Some((name.to_string(), args))
}

/// Code Mode does not expose a structured read event. Recognize the conservative set of commands
/// Codex normally uses to inspect files; every other command remains visible through the Run fallback.
fn is_common_read_command(cmd: &str) -> bool {
    let first = cmd.split_whitespace().next().unwrap_or("");
    matches!(
        first,
        "rg" | "grep" | "cat" | "head" | "tail" | "less" | "jq"
    ) || (first == "sed"
        && !cmd
            .split_whitespace()
            .any(|part| part == "-i" || part.starts_with("-i.")))
}

fn custom_tool_output_failed(payload: &Value) -> bool {
    value_text(payload.get("output"))
        .is_some_and(|output| output.trim_start().starts_with("Script failed"))
}

/// `patch_apply_end.changes` 的路径表 → `首文件名 +N`。只展示 basename，不读取 diff/stdout。
pub(super) fn patch_changes_object(changes: Option<&Value>) -> Option<String> {
    let mut names: Vec<String> = changes?
        .as_object()?
        .keys()
        .filter_map(|path| {
            Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let first = names.first()?;
    let value = if names.len() == 1 {
        first.clone()
    } else {
        format!("{} +{}", first, names.len() - 1)
    };
    Some(truncate(&value, MAX_TOOL_OBJECT_CHARS))
}

/// Grok：`{type:assistant, content, tool_calls:[{function:{name,arguments}}]}`；`{type:tool_result}`。
/// reasoning / user / system 忽略。
fn push_events_grok(v: &Value, out: &mut Vec<Ev>) {
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "assistant" => {
            if let Some(t) = value_text(v.get("content")) {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(Ev::Text(t.to_string()));
                }
            }
            if let Some(arr) = v.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in arr {
                    let func = tc.get("function");
                    let name = func
                        .and_then(|f| f.get("name"))
                        .or_else(|| tc.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let args_val = func
                        .and_then(|f| f.get("arguments"))
                        .or_else(|| tc.get("arguments"));
                    let args = parse_args(args_val);
                    out.push(Ev::Tool(classify_tool(name, args.as_ref())));
                }
            }
        }
        "tool_result" => out.push(Ev::ToolResult(
            v.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false),
        )),
        _ => {}
    }
}

// ── TODO 清单解析 ──

/// TODO 类工具名判定（这些调用不入足迹时间线，单独作清单重放；实时 hook 侧同样过滤）。
pub(crate) fn is_todo_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "todowrite" | "todo_write" | "update_plan"
    )
}

/// 解析 TodoWrite / update_plan 参数为 TODO 更新事件。
/// Cursor：`{merge, todos:[{id,content,status}]}`（merge=true 按 id 增量合并）；
/// Claude：`{todos:[{content,status}]}`（恒整表）；Codex：`{plan:[{step,status}]}`（恒整表）。
/// 空列表 / 无法解析 → None（忽略，不清空既有清单）。`pub(super)`：vscdb 适配器复用。
pub(super) fn parse_todos(args: Option<&Value>) -> Option<Ev> {
    let o = parse_args(args)?;
    let arr = o.get("todos").or_else(|| o.get("plan"))?.as_array()?;
    let replace = !o.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut items = Vec::new();
    for it in arr {
        let Some(content) = it
            .get("content")
            .or_else(|| it.get("step"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let state = match it.get("status").and_then(|v| v.as_str()).unwrap_or("") {
            "in_progress" => TodoState::InProgress,
            "completed" => TodoState::Completed,
            "cancelled" => TodoState::Cancelled,
            _ => TodoState::Pending,
        };
        let id = it.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
        items.push((
            id,
            TodoItem {
                content: truncate(content, MAX_TODO_CHARS),
                state,
            },
        ));
    }
    if items.is_empty() {
        return None;
    }
    Some(Ev::Todos { replace, items })
}

// ── 工具归一化 ──

/// 归一化一次工具调用为展示信息。`args` 可为已解析对象或原始 JSON 字符串（内部再 `parse_args`
/// 兼容），供 transcript 尾部解析与 hook 实时上报（`report.rs`）共用。
pub(crate) fn classify_tool(name: &str, args: Option<&Value>) -> ToolDisplay {
    let parsed = parse_args(args);
    let args = parsed.as_ref();
    let lower = name.to_ascii_lowercase();
    let label = if is_run(&lower) {
        ToolLabel::Run
    } else if is_read(&lower) {
        ToolLabel::Read
    } else if is_write(&lower) {
        ToolLabel::Write
    } else {
        ToolLabel::Other(name.to_string())
    };
    let object = match &label {
        ToolLabel::Run => arg_command(args),
        ToolLabel::Read | ToolLabel::Write => arg_filename(args),
        ToolLabel::Other(_) => arg_generic(args),
    };
    ToolDisplay { label, object }
}

fn is_run(n: &str) -> bool {
    matches!(
        n,
        "bash"
            | "shell"
            | "run_terminal_cmd"
            // Cursor IDE（vscdb 源）的工具名。
            | "run_terminal_command_v2"
            | "local_shell"
            | "local_shell_call"
            | "exec"
            | "run"
    )
}

fn is_read(n: &str) -> bool {
    matches!(n, "read" | "read_file" | "read_file_v2" | "view" | "cat")
}

fn is_write(n: &str) -> bool {
    matches!(
        n,
        "write"
            | "edit"
            // Cursor IDE（vscdb 源）的工具名。
            | "edit_file_v2"
            | "multiedit"
            | "str_replace"
            | "str_replace_editor"
            | "str_replace_based_edit_tool"
            | "search_replace"
            | "apply_patch"
            | "create_file"
            | "write_file"
    )
}

/// 从参数取命令展示：优先「人话描述 (命令首段)」——Cursor / Claude 的 Shell 调用自带一句
/// `description`（如 `Rebuild after reverting`），比原始命令更直观；无描述则退回命令本身。
fn arg_command(args: Option<&Value>) -> Option<String> {
    let o = args?;
    let desc = o
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let cmd = ["command", "cmd"]
        .iter()
        .find_map(|k| o.get(*k).and_then(value_scalar_or_join))
        .filter(|s| !s.trim().is_empty());
    match (desc, cmd) {
        (Some(d), Some(c)) => Some(format!(
            "{} ({})",
            truncate(d, MAX_TOOL_OBJECT_CHARS),
            truncate(&c, 32)
        )),
        (Some(d), None) => Some(truncate(d, MAX_TOOL_OBJECT_CHARS)),
        (None, Some(c)) => Some(truncate(&c, MAX_TOOL_OBJECT_CHARS)),
        (None, None) => o.as_str().map(|s| truncate(s, MAX_TOOL_OBJECT_CHARS)),
    }
}

/// 从参数取文件名（路径末段）。
fn arg_filename(args: Option<&Value>) -> Option<String> {
    let o = args?;
    for k in [
        "path",
        "file_path",
        "target_file",
        "filename",
        "file",
        "notebook_path",
        // Cursor IDE（vscdb 源）edit_file_v2 的参数键。
        "relativeWorkspacePath",
    ] {
        if let Some(s) = o.get(k).and_then(|v| v.as_str()) {
            let seg = s.trim_end_matches('/').rsplit('/').next().unwrap_or(s);
            let seg = if seg.is_empty() { s } else { seg };
            return Some(truncate(seg, MAX_TOOL_OBJECT_CHARS));
        }
    }
    None
}

/// 其它工具：取参数前一小段（先看整串，再看常见键）。
fn arg_generic(args: Option<&Value>) -> Option<String> {
    let o = args?;
    if let Some(s) = o.as_str() {
        if !s.trim().is_empty() {
            return Some(truncate(s, MAX_TOOL_OBJECT_CHARS));
        }
    }
    for k in [
        "query",
        "pattern",
        "q",
        "glob_pattern",
        "search_term",
        "description",
        "prompt",
        "url",
        "path",
        "file_path",
        "command",
    ] {
        if let Some(s) = o.get(k).and_then(value_scalar_or_join) {
            if !s.trim().is_empty() {
                return Some(truncate(&s, MAX_TOOL_OBJECT_CHARS));
            }
        }
    }
    None
}

// ── 通用小工具 ──

/// 参数值 `arguments` 常为 JSON 字符串（Codex/Grok）或对象（Cursor/Claude 的 input）。
/// 字符串则尝试解析成 JSON；解析失败保留原始字符串（供 `arg_generic` 兜底）。
fn parse_args(v: Option<&Value>) -> Option<Value> {
    match v {
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            Some(serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.to_string())))
        }
        Some(other) => Some(other.clone()),
        None => None,
    }
}

/// 标量或字符串数组 → 单个字符串（数组以空格连接）。
fn value_scalar_or_join(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(arr) => {
            let parts: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
}

/// 提取文字：字符串，或数组 `[{text:"..."}]` / `["..."]`。
fn value_text(c: Option<&Value>) -> Option<String> {
    match c? {
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    parts.push(s.to_string());
                } else if let Some(s) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(s.to_string());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

/// 折叠空白并按字符数截断（超出补 `…`）。
fn truncate(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max {
        let t: String = collapsed.chars().take(max).collect();
        format!("{}…", t.trim_end())
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cursor_text_plus_tool_run() {
        let ls = lines(&[
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"先跑测试"},{"type":"tool_use","name":"Shell","input":{"command":"cargo test"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Cursor, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("先跑测试"));
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Run);
        assert_eq!(step.tool.object.as_deref(), Some("cargo test"));
        // Cursor 落盘即已跑完：transcript 步一律已完成（进行中只来自实时 hook 并入）。
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn claude_tail_tool_without_result_is_running() {
        // Claude 在调用开始时即写盘：末步无结果 = 真在跑（如 AskHuman 等待答复期间）。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"先跑测试"},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.steps.last().unwrap().state, StepState::Running);
    }

    #[test]
    fn tail_text_after_tool_clears_steps() {
        // 工具之后又产出文字（最终答复）→ 时间线清空（用户定案：最新事件是文字时不显示工具行）。
        let ls = lines(&[
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/b/registry.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"改好了"}]}}"#,
        ]);
        let a = analyze(AgentKind::Cursor, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("改好了"));
        assert!(a.steps.is_empty());
        assert_eq!(a.steps_omitted, 0);
    }

    #[test]
    fn steps_omitted_counts_beyond_window() {
        // 文字之后连发 5 次调用：留最近 3 步，省略 2 步。
        let mut raw = vec![
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"开始干活"}]}}"#
                .to_string(),
        ];
        for i in 0..5 {
            raw.push(format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"/a/f{i}.rs"}}}}]}}}}"#
            ));
        }
        let a = analyze(AgentKind::Claude, &raw).unwrap();
        assert_eq!(a.steps.len(), MAX_STEPS);
        assert_eq!(a.steps_omitted, 2);
        assert_eq!(a.steps[0].tool.object.as_deref(), Some("f2.rs"));
        // 再来一段文字 → 全部清空。
        let mut raw2 = raw.clone();
        raw2.push(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"搞定"}]}}"#
                .to_string(),
        );
        let a2 = analyze(AgentKind::Claude, &raw2).unwrap();
        assert!(a2.steps.is_empty());
        assert_eq!(a2.steps_omitted, 0);
    }

    #[test]
    fn tool_result_tail_marks_step_done() {
        // 末尾停在 tool_result（工具刚跑完、助手未回话）→ 步骤保留且已完成。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"读一下"},{"type":"tool_use","name":"Read","input":{"file_path":"src/agents/title.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("读一下"));
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Read);
        assert_eq!(step.tool.object.as_deref(), Some("title.rs"));
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn timeline_keeps_last_steps_in_order() {
        // 连跑 4 步只留最近 3 步（旧→新），Claude 末步无结果 → 进行中。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/one.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/two.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/a/three.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"cargo test"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.steps.len(), MAX_STEPS);
        assert_eq!(a.steps[0].tool.object.as_deref(), Some("two.rs"));
        assert_eq!(a.steps[1].tool.object.as_deref(), Some("three.rs"));
        assert_eq!(a.steps[2].tool.object.as_deref(), Some("cargo test"));
        assert_eq!(a.steps[0].state, StepState::Done);
        assert_eq!(a.steps[1].state, StepState::Done);
        assert_eq!(a.steps[2].state, StepState::Running);
    }

    #[test]
    fn only_last_step_can_be_running_without_results() {
        // 整段窗口没有 tool_result 事件时：新调用开始即证明前一步已结束 → 只有末步进行中。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/one.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/a/two.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"AskHuman -q hi"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.steps.len(), 3);
        assert_eq!(a.steps[0].state, StepState::Done);
        assert_eq!(a.steps[1].state, StepState::Done);
        assert_eq!(a.steps[2].state, StepState::Running);
    }

    #[test]
    fn cursor_transcript_steps_all_done() {
        // Cursor 在工具**跑完后**才落盘该次调用（实测 in-flight 探针不可见），且从不写
        // tool_result：落盘即已结束 → 末步也已完成（修正「答完问题 AskHuman 仍绿点」）。
        let ls = lines(&[
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/one.rs"}}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"AskHuman -q hi"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Cursor, &ls).unwrap();
        assert_eq!(a.steps.len(), 2);
        assert!(a.steps.iter().all(|s| s.state == StepState::Done));
    }

    #[test]
    fn tool_result_error_marks_step_failed() {
        // is_error 显式失败标志 → 该步记失败。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"boom"}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.steps.last().unwrap().state, StepState::Failed);
    }

    #[test]
    fn shell_description_prefixed_over_command() {
        // Cursor / Claude Shell 自带人话 description → 「描述 (命令)」。
        let ls = lines(&[
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"cargo build --release","description":"Rebuild after reverting"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Cursor, &ls).unwrap();
        let step = a.steps.last().unwrap();
        assert_eq!(
            step.tool.object.as_deref(),
            Some("Rebuild after reverting (cargo build --release)")
        );
    }

    #[test]
    fn codex_function_call_shell() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"运行一下"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"ls -la\"]}"}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("运行一下"));
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Run);
        assert_eq!(step.tool.object.as_deref(), Some("bash -lc ls -la"));
        assert_eq!(step.state, StepState::Running);
    }

    #[test]
    fn codex_custom_exec_read_command_is_visible() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"sed -n '1,80p' src/lib.rs && rg -n TODO src\",\"workdir\":\"/repo\"});\ntext(r.output);"}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"Script completed\nWall time 0.1 seconds\nOutput:\n"}]}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Read);
        assert_eq!(
            step.tool.object.as_deref(),
            Some("sed -n '1,80p' src/lib.rs && rg -n TODO src")
        );
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn codex_custom_exec_non_read_command_falls_back_to_run() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"cargo test\",\"workdir\":\"/repo\"});\ntext(r.output);"}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"Script failed\nWall time 0.1 seconds\nOutput:\nboom"}]}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Run);
        assert_eq!(step.tool.object.as_deref(), Some("cargo test"));
        assert_eq!(step.state, StepState::Failed);
    }

    #[test]
    fn codex_custom_inner_tool_uses_named_fallback() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const r = await tools.write_stdin({\"session_id\":42,\"chars\":\"\"});\ntext(r.output);"}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Other("write_stdin".to_string()));
        assert_eq!(step.state, StepState::Running);
    }

    #[test]
    fn codex_patch_apply_end_is_completed_write_step() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"修改文件"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"patch_apply_end","success":true,"changes":{"/repo/src/b.rs":{"type":"update"},"/repo/docs/a.md":{"type":"update"},"/repo/src/c.rs":{"type":"add"}}}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert_eq!(a.steps.len(), 1);
        let step = &a.steps[0];
        assert_eq!(step.tool.label, ToolLabel::Write);
        assert_eq!(step.tool.object.as_deref(), Some("a.md +2"));
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn codex_custom_apply_patch_is_not_duplicated() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const patch = \"*** Begin Patch\\n*** End Patch\";\ntext(await tools.apply_patch(patch));"}}"#,
            r#"{"type":"event_msg","payload":{"type":"patch_apply_end","success":true,"changes":{"/repo/src/lib.rs":{"type":"update"}}}}"#,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":[{"type":"input_text","text":"Script completed\nWall time 0.0 seconds\nOutput:\n"}]}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert_eq!(a.steps.len(), 1);
        let step = &a.steps[0];
        assert_eq!(step.tool.label, ToolLabel::Write);
        assert_eq!(step.tool.object.as_deref(), Some("lib.rs"));
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn codex_failed_patch_apply_end_is_failed_write_step() {
        let ls = lines(&[
            r#"{"type":"event_msg","payload":{"type":"patch_apply_end","success":false,"changes":{}}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Write);
        assert_eq!(step.tool.object, None);
        assert_eq!(step.state, StepState::Failed);
    }

    #[test]
    fn codex_agent_message_only() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"function_call_output","output":"done"}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"任务完成"}}"#,
            r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("任务完成"));
        assert!(a.steps.is_empty());
    }

    #[test]
    fn grok_text_plus_tool_read() {
        let ls = lines(&[
            r#"{"type":"assistant","content":"看看这个文件","tool_calls":[{"function":{"name":"read_file","arguments":"{\"path\":\"/x/y/registry.rs\"}"}}]}"#,
        ]);
        let a = analyze(AgentKind::Grok, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("看看这个文件"));
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Read);
        assert_eq!(step.tool.object.as_deref(), Some("registry.rs"));
    }

    #[test]
    fn pi_v3_text_and_tool_result_are_replayed() {
        let ls = lines(&[
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"检查文件"},{"type":"toolCall","id":"tool-1","name":"read","arguments":{"path":"/x/src/main.rs"}}]}}"#,
            r#"{"type":"message","message":{"role":"toolResult","toolCallId":"tool-1","content":[{"type":"text","text":"ok"}],"isError":false}}"#,
        ]);
        let activity = analyze(AgentKind::Pi, &ls).unwrap();
        assert_eq!(activity.text.as_deref(), Some("检查文件"));
        let step = activity.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Read);
        assert_eq!(step.tool.object.as_deref(), Some("main.rs"));
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn other_tool_keeps_raw_name_and_arg() {
        let ls = lines(&[
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Grep","input":{"pattern":"AgentRecord"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Cursor, &ls).unwrap();
        let step = a.steps.last().unwrap();
        assert_eq!(step.tool.label, ToolLabel::Other("Grep".to_string()));
        assert_eq!(step.tool.object.as_deref(), Some("AgentRecord"));
    }

    #[test]
    fn cursor_todowrite_merge_replay() {
        // 全量写入（merge=false）后按 id 增量合并（merge=true）：状态就地更新、未知 id 追加、
        // cancelled 条目从最终结果剔除；TodoWrite 本身不入足迹时间线。
        let ls = lines(&[
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"merge":false,"todos":[{"id":"a","content":"改 registry","status":"in_progress"},{"id":"b","content":"跑单测","status":"pending"},{"id":"c","content":"废弃项","status":"pending"}]}}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"merge":true,"todos":[{"id":"a","content":"改 registry","status":"completed"},{"id":"b","content":"跑单测","status":"in_progress"},{"id":"c","content":"废弃项","status":"cancelled"},{"id":"d","content":"更新文档","status":"pending"}]}}]}}"#,
        ]);
        let a = analyze(AgentKind::Cursor, &ls).unwrap();
        assert!(a.steps.is_empty(), "TodoWrite 不入足迹时间线");
        assert_eq!(a.todos.len(), 3, "cancelled 条目剔除");
        assert_eq!(a.todos[0].content, "改 registry");
        assert_eq!(a.todos[0].state, TodoState::Completed);
        assert_eq!(a.todos[1].state, TodoState::InProgress);
        assert_eq!(a.todos[2].content, "更新文档");
        assert_eq!(a.todos[2].state, TodoState::Pending);
    }

    #[test]
    fn claude_todowrite_full_replace() {
        // Claude 无 merge 字段 → 恒整表替换，取最后一次。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"步骤一","status":"in_progress"},{"content":"步骤二","status":"pending"}]}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"步骤一","status":"completed"},{"content":"步骤二","status":"in_progress"}]}}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.todos.len(), 2);
        assert_eq!(a.todos[0].state, TodoState::Completed);
        assert_eq!(a.todos[1].state, TodoState::InProgress);
    }

    #[test]
    fn codex_update_plan_parsed() {
        let ls = lines(&[
            r#"{"type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[{\"step\":\"初始化仓库\",\"status\":\"completed\"},{\"step\":\"实现脚本\",\"status\":\"in_progress\"}]}"}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert!(a.steps.is_empty());
        assert_eq!(a.todos.len(), 2);
        assert_eq!(a.todos[0].content, "初始化仓库");
        assert_eq!(a.todos[1].state, TodoState::InProgress);
    }

    #[test]
    fn todowrite_result_does_not_close_running_step() {
        // Claude：Bash 在跑 → TodoWrite 更新（另一并行不可能，顺序上 Bash 必已结束）——
        // 这里验证 TodoWrite 自己的 tool_result 不会误闭合后续真正在跑的步。
        let ls = lines(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"x","status":"pending"}]}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        ]);
        let a = analyze(AgentKind::Claude, &ls).unwrap();
        assert_eq!(a.steps.len(), 1);
        assert_eq!(a.steps[0].state, StepState::Running, "末步 Bash 仍在跑");
    }

    #[test]
    fn nothing_meaningful_returns_none() {
        let ls = lines(&[
            r#"{"type":"reasoning","summary":"..."}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
        ]);
        assert!(analyze(AgentKind::Codex, &ls).is_none());
    }

    #[test]
    fn text_truncated_to_limit() {
        let long = "字".repeat(600);
        let line = format!(
            r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let a = analyze(AgentKind::Cursor, &lines(&[&line])).unwrap();
        let t = a.text.unwrap();
        assert!(t.chars().count() <= MAX_ACTIVITY_TEXT_CHARS + 1);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn read_tail_drops_partial_first_line_and_keeps_tail() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.jsonl");
        let mut content = String::new();
        for i in 0..100 {
            content.push_str(&format!("line-{i}-{}\n", "x".repeat(50)));
        }
        std::fs::write(&f, &content).unwrap();
        let tail = read_tail(&f, 512);
        // 只取到尾部若干行，且不含最早的行。
        assert!(!tail.is_empty());
        assert!(tail.iter().all(|l| !l.starts_with("line-0-")));
        assert!(tail.iter().any(|l| l.starts_with("line-99-")));
    }

    #[test]
    fn last_assistant_text_from_cursor_transcript_is_bounded_and_best_effort() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.jsonl");
        let old = serde_json::json!({
            "role": "assistant",
            "message": {"content": [{"type":"text", "text":"old"}]}
        });
        let latest = serde_json::json!({
            "role": "assistant",
            "message": {"content": [{"type":"text", "text":"最后一段\n第二行"}]}
        });
        std::fs::write(&path, format!("{old}\nmalformed\n{latest}\n")).unwrap();
        assert_eq!(
            resolve_last_assistant_text_from_path(AgentKind::Cursor, &path, 2_000).as_deref(),
            Some("最后一段\n第二行")
        );

        let long = "你".repeat(2_001);
        let record = serde_json::json!({
            "role": "assistant",
            "message": {"content": [{"type":"text", "text":long}]}
        });
        std::fs::write(&path, format!("{record}\n")).unwrap();
        let text = resolve_last_assistant_text_from_path(AgentKind::Cursor, &path, 2_000).unwrap();
        assert_eq!(text.matches('你').count(), 2_000);
        assert!(text.ends_with("… [truncated]"));
    }

    #[test]
    fn paginated_item_completed_file_change_is_completed_write_step() {
        // Codex 0.148 paginated exec writes FileChange instead of patch_apply_end.
        let ls = lines(&[
            r#"{"timestamp":"t","ordinal":15,"type":"event_msg","payload":{"type":"item_completed","thread_id":"t","turn_id":"u","item":{"type":"FileChange","id":"exec-1","changes":{"/tmp/probe.txt":{"type":"add","content":"PING\n"}},"status":"completed","stdout":"Success.","stderr":""}}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert_eq!(a.steps.len(), 1);
        let step = &a.steps[0];
        assert_eq!(step.tool.label, ToolLabel::Write);
        assert_eq!(step.tool.object.as_deref(), Some("probe.txt"));
        assert_eq!(step.state, StepState::Done);
    }

    #[test]
    fn paginated_item_completed_agent_message_is_last_text() {
        let ls = lines(&[
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"m1","content":[{"type":"Text","text":"ASKHUMAN_PAGINATED_PROBE_20260820"}],"phase":"final_answer"}}}"#,
        ]);
        let a = analyze(AgentKind::Codex, &ls).unwrap();
        assert_eq!(a.text.as_deref(), Some("ASKHUMAN_PAGINATED_PROBE_20260820"));
        assert!(a.steps.is_empty());
    }

    #[test]
    fn paginated_codex_148_real_session_activity_when_present() {
        let sid = "01a01ec8-3867-7730-8acf-8911ef19b587";
        if transcript_path(AgentKind::Codex, sid).is_none() {
            eprintln!("skip: paginated probe session not on disk");
            return;
        }
        let activity = resolve_activity(AgentKind::Codex, sid);
        eprintln!("real paginated activity={activity:?}");
        let Some(activity) = activity else {
            panic!("expected activity from assistant response_item");
        };
        assert!(
            activity
                .text
                .as_deref()
                .is_some_and(|text| text.contains("ASKHUMAN_PAGINATED_PROBE_20260820")),
            "expected last assistant text, got {:?}",
            activity.text
        );
    }

    #[test]
    fn paginated_codex_148_write_session_activity_when_present() {
        let sid = "01a01ecc-4f91-7740-8354-494eea787be6";
        let Some(path) = super::transcript_path(AgentKind::Codex, sid) else {
            eprintln!("skip: paginated write-session probe not on disk");
            return;
        };
        let activity = resolve_activity(AgentKind::Codex, sid);
        eprintln!("write-session activity={activity:?} path={path:?}");
        assert!(
            activity.is_some(),
            "expected activity from the write session"
        );
        // Later exec/whats_next steps can push FileChange out of the 3-step window;
        // the TurnItem itself must still parse as a Write.
        let file_change: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| line.contains("\"FileChange\""))
            .map(str::to_string)
            .collect();
        let write = analyze(AgentKind::Codex, &file_change).expect("FileChange");
        assert_eq!(write.steps.len(), 1);
        assert_eq!(write.steps[0].tool.label, ToolLabel::Write);
        assert_eq!(write.steps[0].tool.object.as_deref(), Some("probe.txt"));
        assert_eq!(write.steps[0].state, StepState::Done);
    }
}
