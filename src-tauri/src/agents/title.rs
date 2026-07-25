//! 把 `session_id` 解析成「对话标题」，复刻各家恢复对话列表里显示的标题（FINDINGS / spec D10）。
//!
//! - Cursor：`~/.cursor/chats/*/<sid>/meta.json` 的 `title`；缺失回退 transcript 首条用户消息。
//! - Codex：`~/.codex/sessions/**/rollout-*-<sid>.jsonl` 首条**真实**用户消息（跳过注入块）。
//! - Claude：`~/.claude/projects/*/<sid>.jsonl` 最后一条 `summary`，否则首条真实用户消息。
//!
//! 全部 best-effort：文件可能不存在 / 正在写 / 巨大，任何失败都返回 `None`（窗口显示「未命名」）。
//! 读 jsonl 有行数与字节上限，避免拖慢 daemon。

use crate::paths;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use super::AgentKind;

/// 标题最大展示长度（字符）。
const MAX_TITLE_CHARS: usize = 80;
/// 扫描 jsonl 的行数上限。
const MAX_LINES: usize = 4000;

/// 解析指定家族某 session 的标题。取不到返回 `None`。
pub fn resolve_title(kind: AgentKind, session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let raw = match kind {
        AgentKind::Cursor => cursor_title(session_id),
        AgentKind::Codex => codex_title(session_id),
        AgentKind::Claude => claude_title(session_id),
        AgentKind::Grok => grok_title(session_id),
    }?;
    Some(clean_title(&raw))
}

fn clean_title(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_TITLE_CHARS {
        let truncated: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
        format!("{}…", truncated.trim_end())
    } else {
        collapsed
    }
}

// ── Cursor ──

fn cursor_title(session_id: &str) -> Option<String> {
    // 1) ~/.cursor/chats/*/<sid>/meta.json 的 title
    let chats = paths::cursor_dir().join("chats");
    if let Ok(entries) = fs::read_dir(&chats) {
        for e in entries.flatten() {
            let meta = e.path().join(session_id).join("meta.json");
            if let Some(t) = read_json_field(&meta, "title") {
                if !t.trim().is_empty() {
                    return Some(t);
                }
            }
        }
    }
    // 2) 回退：transcript 首条用户消息
    // ~/.cursor/projects/*/agent-transcripts/<sid>/<sid>.jsonl
    let projects = paths::cursor_dir().join("projects");
    if let Ok(entries) = fs::read_dir(&projects) {
        for e in entries.flatten() {
            let f = e
                .path()
                .join("agent-transcripts")
                .join(session_id)
                .join(format!("{session_id}.jsonl"));
            if f.is_file() {
                if let Some(t) = first_user_message(&f) {
                    return Some(t);
                }
            }
        }
    }
    None
}

// ── Codex ──

fn codex_title(session_id: &str) -> Option<String> {
    let sessions = paths::codex_dir().join("sessions");
    let needle = format!("-{session_id}.jsonl");
    let file = find_file_recursive(&sessions, &needle, 4)?;
    // 优先：`event_msg{payload.type=="user_message"}` 的 message——这是用户真正键入的内容，
    // 绕开会话开头作为 role=user 注入的 AGENTS.md 指令块与 `<environment_context>` 等。
    if let Some(t) = codex_user_message(&file) {
        return Some(t);
    }
    // 回退：response_item 首条真实用户消息（已跳过注入块）。
    first_user_message(&file)
}

/// Codex：扫描取首条 `event_msg{payload.type=="user_message"}` 的 `message`。
/// Codex 只为用户真实输入发出该事件（注入的上下文走 response_item），故无需再过滤注入块。
fn codex_user_message(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i >= MAX_LINES {
            break;
        }
        let Ok(line) = line else { break };
        if !line.contains("user_message") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("event_msg") {
            continue;
        }
        let Some(payload) = v.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("user_message") {
            continue;
        }
        if let Some(msg) = payload.get("message").and_then(|m| m.as_str()) {
            let t = msg.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

// ── Claude ──

fn claude_title(session_id: &str) -> Option<String> {
    let projects = paths::claude_dir().join("projects");
    let target = format!("{session_id}.jsonl");
    let file = find_file_recursive(&projects, &target, 3)?;
    // 优先：最后一条 summary。
    if let Some(s) = last_summary(&file) {
        return Some(s);
    }
    first_user_message(&file)
}

// ── Grok ──

/// Grok 会话布局：`~/.grok/sessions/<url编码的 cwd>/<session_id>/`，内含 `summary.json`
/// （`session_summary` / `generated_title` 即恢复列表标题）与 `chat_history.jsonl`。
/// 优先取 summary 字段；回退扫 `chat_history.jsonl` 的首条真实用户输入。
///
/// 注意 Grok（cursor harness）会把用户真实输入包在 `<user_query>…</user_query>` 里，而其它以 `<`
/// 开头的块（`<user_info>`/`<git_status>` 等）才是注入上下文；故回退需先**解包** `<user_query>`，
/// 不能直接套用 [`first_user_message`]（它会把所有以 `<` 开头的文本当注入块跳过）。
fn grok_title(session_id: &str) -> Option<String> {
    let sessions = paths::grok_sessions_dir();
    // 遍历各 cwd 目录，定位含本 session_id 子目录者（量小、best-effort）。
    let entries = fs::read_dir(&sessions).ok()?;
    for e in entries.flatten() {
        let dir = e.path().join(session_id);
        if !dir.is_dir() {
            continue;
        }
        // 优先：summary.json 的 session_summary，其次 generated_title。
        let summary = dir.join("summary.json");
        for field in ["session_summary", "generated_title"] {
            if let Some(s) = read_json_field(&summary, field) {
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
        // 回退：chat_history.jsonl 首条真实用户输入（解包 <user_query>、跳过注入块）。
        let chat = dir.join("chat_history.jsonl");
        if chat.is_file() {
            if let Some(t) = grok_first_query(&chat) {
                return Some(t);
            }
        }
    }
    None
}

/// 扫 Grok `chat_history.jsonl` 取首条真实用户输入：优先解包 `<user_query>…</user_query>` 的内层文本；
/// 若某条用户文本本身不以 `<` 开头（Build harness 可能不加包裹）则直接取用。均跳过纯注入块。
fn grok_first_query(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i >= MAX_LINES {
            break;
        }
        let Ok(line) = line else { break };
        if !line.contains("\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !is_user_line(&v) {
            continue;
        }
        let Some(text) = extract_text(&v) else {
            continue;
        };
        let t = text.trim();
        if let Some(inner) = unwrap_tag(t, "user_query") {
            if !inner.trim().is_empty() {
                return Some(inner.trim().to_string());
            }
        } else if !is_injected_block(t) {
            return Some(t.to_string());
        }
    }
    None
}

/// 提取 `<tag>…</tag>` 之间的内层文本（找不到成对标签返回 `None`）。
fn unwrap_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].to_string())
}

// ── transcript 文件定位（供 activity.rs 复用）──

/// 按 `session_id` 定位某家 agent 的 transcript（jsonl）文件路径。取不到返回 `None`。
/// 与 `resolve_title` 的取标题不同，这里定位的是**对话流水**文件（供解析尾部「当前活动」）。
pub(super) fn transcript_path(kind: AgentKind, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }
    match kind {
        // ~/.cursor/projects/*/agent-transcripts/<sid>/<sid>.jsonl
        AgentKind::Cursor => {
            let projects = paths::cursor_dir().join("projects");
            let entries = fs::read_dir(&projects).ok()?;
            for e in entries.flatten() {
                let f = e
                    .path()
                    .join("agent-transcripts")
                    .join(session_id)
                    .join(format!("{session_id}.jsonl"));
                if f.is_file() {
                    return Some(f);
                }
            }
            None
        }
        // ~/.codex/sessions/**/rollout-*-<sid>.jsonl
        AgentKind::Codex => {
            let sessions = paths::codex_dir().join("sessions");
            find_file_recursive(&sessions, &format!("-{session_id}.jsonl"), 4)
        }
        // ~/.claude/projects/*/<sid>.jsonl
        AgentKind::Claude => {
            let projects = paths::claude_dir().join("projects");
            find_file_recursive(&projects, &format!("{session_id}.jsonl"), 3)
        }
        // ~/.grok/sessions/<url编码 cwd>/<sid>/chat_history.jsonl
        AgentKind::Grok => {
            let sessions = paths::grok_sessions_dir();
            let entries = fs::read_dir(&sessions).ok()?;
            for e in entries.flatten() {
                let f = e.path().join(session_id).join("chat_history.jsonl");
                if f.is_file() {
                    return Some(f);
                }
            }
            None
        }
    }
}

// ── 通用 jsonl 解析 ──

/// 读 JSON 文件取顶层字符串字段。
fn read_json_field(path: &Path, field: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get(field).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// 扫描 jsonl 取「首条真实用户消息」（跳过 `<...>` 注入块与空文本）。
fn first_user_message(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i >= MAX_LINES {
            break;
        }
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !is_user_line(&v) {
            continue;
        }
        if let Some(text) = extract_text(&v) {
            // Cursor 把用户输入包在 `<timestamp>…</timestamp>` + `<user_query>` 里：先用
            // transcript 同款清洗剥壳（其它家族原样通过），再判注入块，否则真实输入会被
            // 「`<` 开头＝注入块」误滤而永远取不到标题（用户验收反馈）。
            let (cleaned, _) = super::transcript_full::clean_user(&text);
            let t = cleaned.trim();
            if t.is_empty() || is_injected_block(t) {
                continue; // 跳过注入块（见 is_injected_block）
            }
            return Some(t.to_string());
        }
    }
    None
}

/// 是否为会话开头注入的上下文块（非用户真实输入），用于回退路径过滤。
/// - 以 `<` 开头：`<environment_context>` / `<user_instructions>` / `<turn_aborted>` 等。
/// - Codex 把项目 AGENTS.md 作为 role=user 注入，文本以 `# AGENTS.md instructions` 开头。
fn is_injected_block(t: &str) -> bool {
    t.is_empty() || t.starts_with('<') || t.starts_with("# AGENTS.md instructions")
}

/// Claude：扫描取最后一条 summary。
fn last_summary(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last: Option<String> = None;
    for (i, line) in reader.lines().enumerate() {
        if i >= MAX_LINES {
            break;
        }
        let Ok(line) = line else { break };
        if !line.contains("summary") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("summary") {
            if let Some(s) = v.get("summary").and_then(|s| s.as_str()) {
                if !s.trim().is_empty() {
                    last = Some(s.to_string());
                }
            }
        }
    }
    last
}

/// 判断一行 jsonl 是否「用户消息」。兼容三家不同结构。
fn is_user_line(v: &Value) -> bool {
    // claude: {type:"user", isMeta?:bool, message:{role:"user",...}}
    if v.get("type").and_then(|t| t.as_str()) == Some("user") {
        if v.get("isMeta").and_then(|b| b.as_bool()) == Some(true) {
            return false;
        }
        return true;
    }
    // cursor: {role:"user", ...} 或 {type:"user"}
    if v.get("role").and_then(|r| r.as_str()) == Some("user") {
        return true;
    }
    // codex rollout: {payload:{role:"user"|type:"message"...}} 或 {type:"response_item"...}
    if let Some(p) = v.get("payload") {
        if p.get("role").and_then(|r| r.as_str()) == Some("user") {
            return true;
        }
    }
    false
}

/// 从一行 jsonl 提取用户文本（兼容 string / {content} / [{text}] 等多种结构）。
fn extract_text(v: &Value) -> Option<String> {
    // 优先 message.content；其次 payload.content；其次顶层 content / text。
    let candidates = [
        v.get("message").and_then(|m| m.get("content")),
        v.get("payload").and_then(|p| p.get("content")),
        v.get("content"),
        v.get("text"),
        v.get("message").and_then(|m| m.get("text")),
    ];
    for c in candidates.into_iter().flatten() {
        if let Some(t) = content_to_text(c) {
            if !t.trim().is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// content 可能是字符串，或数组 `[{type:"text"|"input_text", text:"..."}]`。
fn content_to_text(c: &Value) -> Option<String> {
    match c {
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
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
}

/// 在目录下递归（限深度）查找文件名以 `suffix` 结尾的第一个文件。
pub(super) fn find_file_recursive(root: &Path, suffix: &str, max_depth: usize) -> Option<PathBuf> {
    fn walk(dir: &Path, suffix: &str, depth: usize, max_depth: usize) -> Option<PathBuf> {
        let entries = fs::read_dir(dir).ok()?;
        let mut subdirs = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                subdirs.push(p);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(suffix))
                .unwrap_or(false)
            {
                return Some(p);
            }
        }
        if depth < max_depth {
            for d in subdirs {
                if let Some(found) = walk(&d, suffix, depth + 1, max_depth) {
                    return Some(found);
                }
            }
        }
        None
    }
    walk(root, suffix, 0, max_depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_truncates_and_collapses() {
        assert_eq!(clean_title("  a\n b   c "), "a b c");
        let long = "x".repeat(200);
        let out = clean_title(&long);
        assert!(out.chars().count() <= MAX_TITLE_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn extract_text_handles_array_and_string() {
        let v: Value = serde_json::json!({"message":{"content":"hello"}});
        assert_eq!(extract_text(&v).as_deref(), Some("hello"));
        let v: Value =
            serde_json::json!({"payload":{"content":[{"type":"input_text","text":"hi there"}]}});
        assert_eq!(extract_text(&v).as_deref(), Some("hi there"));
    }

    #[test]
    fn first_user_message_skips_injected_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.jsonl");
        let lines = [
            r#"{"type":"user","message":{"content":"<environment_context>ctx</environment_context>"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"meta only"}}"#,
            r#"{"type":"user","message":{"content":"实际的第一句话"}}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        assert_eq!(first_user_message(&f).as_deref(), Some("实际的第一句话"));
    }

    /// Cursor 把用户输入包在 `<timestamp>…</timestamp>` + `<user_query>` 里（用户验收反馈：
    /// 全部消息被当注入块滤掉 → 永远「未命名」）：回退路径须剥壳后取到真实问题。
    #[test]
    fn first_user_message_unwraps_cursor_timestamp_and_user_query() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.jsonl");
        let lines = [
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Friday, Jul 24, 2026, 11:07 PM (UTC+8)</timestamp>\n<user_query>\n我想优化一下 agent 状态窗口\n</user_query>"}]}}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        assert_eq!(
            first_user_message(&f).as_deref(),
            Some("我想优化一下 agent 状态窗口")
        );
    }

    #[test]
    fn codex_title_skips_agents_md_injection() {
        // 复刻 Codex rollout 开头结构：先注入 AGENTS.md(role=user) + environment_context，
        // 再是用户真实问题（既有 response_item 也有 event_msg/user_message）。
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("rollout-2026-06-13T21-00-16-sid.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"sid"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>...</permissions instructions>"}]}}"#,
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>\n...\n</INSTRUCTIONS>"},{"type":"input_text","text":"<environment_context>\n  <cwd>/x</cwd>\n</environment_context>"}]}}"##,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"帮我修一个 bug"}]}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"帮我修一个 bug","images":[]}}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        // 主路径：event_msg/user_message。
        assert_eq!(codex_user_message(&f).as_deref(), Some("帮我修一个 bug"));
        // 回退路径：response_item 也要跳过 AGENTS.md 注入块、取到真实问题。
        assert_eq!(first_user_message(&f).as_deref(), Some("帮我修一个 bug"));
    }

    #[test]
    fn grok_first_query_unwraps_user_query_and_skips_injection() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("chat_history.jsonl");
        let lines = [
            r#"{"type":"system","content":"You are ..."}"#,
            r#"{"type":"user","content":[{"type":"text","text":"<user_info>\nOS...\n</user_info>\n<git_status>...</git_status>"}]}"#,
            r#"{"type":"user","content":[{"type":"text","text":"<user_query>\n帮我加一个功能\n</user_query>"}]}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        assert_eq!(grok_first_query(&f).as_deref(), Some("帮我加一个功能"));
    }

    #[test]
    fn grok_first_query_falls_back_to_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("chat_history.jsonl");
        // Build harness 可能不加 <user_query> 包裹：直接取首条非注入用户文本。
        let lines = [
            r#"{"type":"user","content":[{"type":"text","text":"<user_info>ctx</user_info>"}]}"#,
            r#"{"type":"user","content":"直接的问题"}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        assert_eq!(grok_first_query(&f).as_deref(), Some("直接的问题"));
    }

    #[test]
    fn unwrap_tag_extracts_inner() {
        assert_eq!(
            unwrap_tag("a<user_query>\nhi\n</user_query>b", "user_query").as_deref(),
            Some("\nhi\n")
        );
        assert_eq!(unwrap_tag("no tags here", "user_query"), None);
    }

    #[test]
    fn last_summary_picks_last() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("s.jsonl");
        let lines = [
            r#"{"type":"summary","summary":"first"}"#,
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"summary","summary":"second"}"#,
        ];
        std::fs::write(&f, lines.join("\n")).unwrap();
        assert_eq!(last_summary(&f).as_deref(), Some("second"));
    }
}
