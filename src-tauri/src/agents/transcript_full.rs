//! Full-session transcript parse for IM `/transcript` (best-effort, four agent families).
//!
//! Separate from `activity.rs` (tail-only “what now”). Spec: im-diff-stage-transcript D17–D21.

use super::title::transcript_path;
use super::AgentKind;
use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Max bytes read from a transcript file (prefer tail when larger).
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;
/// Max normalized events retained (drop oldest).
pub const MAX_EVENTS: usize = 2000;
const MAX_TEXT_CHARS: usize = 8_000;
/// Tool results: short summary only — full dumps bloat IM export.
const MAX_TOOL_RESULT_CHARS: usize = 400;
const MAX_ARG_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptDoc {
    pub events: Vec<TranscriptEvent>,
    pub truncated_head: bool,
    pub partial: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptEvent {
    UserText {
        text: String,
        /// Unix seconds when known (Claude/Codex ISO timestamps).
        at: Option<u64>,
        /// Preformatted local label when source only has a human string
        /// (Cursor embeds `<timestamp>…</timestamp>` in user text).
        at_label: Option<String>,
    },
    AssistantText {
        text: String,
        at: Option<u64>,
        at_label: Option<String>,
    },
    Thinking {
        text: String,
        at: Option<u64>,
        at_label: Option<String>,
    },
    ToolCall {
        name: String,
        args_summary: String,
        result_summary: Option<String>,
        is_error: bool,
        ask_human: Option<AskHumanBlock>,
        at: Option<u64>,
        at_label: Option<String>,
    },
    Meta(String),
}

/// Structured AskHuman interaction (spec gui-agent-console C14): shared message + per-question
/// answers. A plain single-question ask keeps one entry with empty `text`.
#[derive(Debug, Clone, PartialEq)]
pub struct AskHumanBlock {
    /// Shared message shown above the questions (may be long Markdown).
    pub message: String,
    pub questions: Vec<AskQA>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AskQA {
    pub text: String,
    /// None while in-flight / cancelled.
    pub answer: Option<String>,
}

/// Best-effort event time (unix seconds) from common transcript fields.
/// Real formats seen: Claude/Codex `"2026-06-13T10:09:57.062Z"` (RFC3339 string);
/// numeric epoch is rare.
fn event_time(v: &Value) -> Option<u64> {
    for key in [
        "timestamp",
        "ts",
        "created_at",
        "createdAt",
        "time",
        "event_time",
    ] {
        if let Some(n) = v.get(key).and_then(|x| x.as_u64()) {
            return Some(if n > 10_000_000_000 { n / 1000 } else { n });
        }
        if let Some(f) = v.get(key).and_then(|x| x.as_f64()) {
            let n = f as u64;
            return Some(if n > 10_000_000_000 { n / 1000 } else { n });
        }
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if let Some(secs) = parse_iso8601_secs(s) {
                return Some(secs);
            }
            if let Ok(n) = s.parse::<u64>() {
                return Some(if n > 10_000_000_000 { n / 1000 } else { n });
            }
        }
    }
    // Claude snapshot.timestamp
    if let Some(s) = v
        .get("snapshot")
        .and_then(|p| p.get("timestamp"))
        .and_then(|x| x.as_str())
    {
        if let Some(secs) = parse_iso8601_secs(s) {
            return Some(secs);
        }
    }
    if let Some(p) = v.get("payload") {
        if let Some(t) = event_time(p) {
            return Some(t);
        }
    }
    if let Some(m) = v.get("message") {
        if let Some(t) = event_time(m) {
            return Some(t);
        }
    }
    None
}

/// Parse `2026-06-13T10:09:57.062Z` / `2026-06-13T10:09:57+08:00` → unix seconds (UTC).
pub(super) fn parse_iso8601_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    // date
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    if s.as_bytes().get(10).copied()? != b'T' {
        return None;
    }
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    // optional fractional seconds then Z or ±HH:MM
    let rest = s.get(19..).unwrap_or("");
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let mut offset_secs: i64 = 0;
    if rest.starts_with('Z') || rest.is_empty() {
        offset_secs = 0;
    } else if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let body = &rest[1..];
        let oh: i64 = body.get(0..2)?.parse().ok()?;
        let om: i64 = if body.len() >= 5 {
            body.get(3..5)?.parse().ok()?
        } else {
            0
        };
        let off = oh * 3600 + om * 60;
        offset_secs = if sign == '+' { off } else { -off };
    }
    let days = days_from_civil(y, mo, d)?;
    let utc = days * 86400 + h * 3600 + mi * 60 + sec - offset_secs;
    if utc < 0 {
        None
    } else {
        Some(utc as u64)
    }
}

/// Howard Hinnant civil-from-days inverse (proleptic Gregorian) → days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

pub fn load_events(kind: AgentKind, session_id: &str) -> Result<TranscriptDoc, String> {
    // Cursor IDE 形态：全局 state.vscdb 实时源优先（jsonl 长回合内冻结）；
    // 未命中（CLI 会话 / 库缺失）回退 jsonl。
    if kind == AgentKind::Cursor {
        if let Ok(doc) = super::cursor_vscdb::load_events(session_id) {
            return Ok(doc);
        }
    }
    let path =
        transcript_path(kind, session_id).ok_or_else(|| "transcript not found".to_string())?;
    let mut doc = load_path(kind, &path)?;
    // Grok chat_history has no per-line times; backfill from sibling updates.jsonl when present.
    if kind == AgentKind::Grok {
        grok_backfill_times(path.parent(), &mut doc.events);
    }
    Ok(doc)
}

/// Transcript file mtime（控制台分页缓存的失效键，spec gui-agent-console C14）。
/// Cursor IDE 形态取 vscdb 的 `lastUpdatedAt`（全局库文件 mtime 恒变，不能当键）。
pub fn transcript_mtime(kind: AgentKind, session_id: &str) -> Option<std::time::SystemTime> {
    if kind == AgentKind::Cursor {
        if let Some(t) = super::cursor_vscdb::last_updated(session_id) {
            return Some(t);
        }
    }
    transcript_path(kind, session_id)
        .and_then(|p| fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// 控制台事件 JSON（spec gui-agent-console C14）：按 `type` 打标
/// （user/assistant/thinking/tool/ask/meta）；工具行 label/object 拆分与 IM 渲染器同源。
pub fn event_json(ev: &TranscriptEvent) -> Value {
    match ev {
        TranscriptEvent::UserText { text, at, at_label } => serde_json::json!({
            "type": "user", "text": text, "at": at, "atLabel": at_label,
        }),
        TranscriptEvent::AssistantText { text, at, at_label } => serde_json::json!({
            "type": "assistant", "text": text, "at": at, "atLabel": at_label,
        }),
        TranscriptEvent::Thinking { text, at, at_label } => serde_json::json!({
            "type": "thinking", "text": text, "at": at, "atLabel": at_label,
        }),
        TranscriptEvent::ToolCall {
            args_summary,
            result_summary,
            is_error,
            ask_human,
            at,
            at_label,
            ..
        } => {
            if let Some(ah) = ask_human {
                serde_json::json!({
                    "type": "ask",
                    "message": ah.message,
                    "questions": ah
                        .questions
                        .iter()
                        .map(|q| serde_json::json!({ "text": q.text, "answer": q.answer }))
                        .collect::<Vec<_>>(),
                    "at": at, "atLabel": at_label,
                })
            } else {
                let (label, object) = split_tool_line(args_summary);
                serde_json::json!({
                    "type": "tool", "label": label, "object": object,
                    "isError": is_error, "resultSummary": result_summary,
                    "at": at, "atLabel": at_label,
                })
            }
        }
        TranscriptEvent::Meta(t) => serde_json::json!({ "type": "meta", "text": t }),
    }
}

pub fn load_path(kind: AgentKind, path: &Path) -> Result<TranscriptDoc, String> {
    let (lines, truncated_head) = read_lines_bounded(path, MAX_READ_BYTES)?;
    let mut events = Vec::new();
    let mut partial = false;
    // Open tool calls waiting for result (order pairing fallback).
    let mut open_tools: Vec<usize> = Vec::new();

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            partial = true;
            continue;
        };
        let before = events.len();
        push_full(kind, &v, &mut events, &mut open_tools);
        if events.len() == before {
            // unrecognized line — soft partial if it looked like json with type
            if v.get("type").is_some() || v.get("role").is_some() {
                // ignore known noise silently
            }
        }
    }

    if events.len() > MAX_EVENTS {
        let skip = events.len() - MAX_EVENTS;
        events.drain(0..skip);
    }
    // Grok: chat_history has no timestamps — try sibling updates.jsonl.
    if kind == AgentKind::Grok {
        grok_backfill_times(path.parent(), &mut events);
    }
    Ok(TranscriptDoc {
        events,
        truncated_head,
        partial,
    })
}

/// Grok: assign times from `updates.jsonl` (`params._meta.agentTimestampMs` / `timestamp`)
/// onto User/Assistant/Tool events in order of appearance.
fn grok_backfill_times(session_dir: Option<&Path>, events: &mut [TranscriptEvent]) {
    let Some(dir) = session_dir else {
        return;
    };
    let path = dir.join("updates.jsonl");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let mut user_ts: Vec<u64> = Vec::new();
    let mut asst_ts: Vec<u64> = Vec::new();
    let mut tool_ts: Vec<u64> = Vec::new();
    // agent_message_chunk fires many times per turn — take first ms of each contiguous run.
    let mut last_kind = String::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let su = v
            .pointer("/params/update/sessionUpdate")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let ms = v
            .pointer("/params/_meta/agentTimestampMs")
            .and_then(|x| x.as_u64())
            .or_else(|| {
                v.get("timestamp").and_then(|x| {
                    x.as_u64()
                        .map(|n| if n < 10_000_000_000 { n * 1000 } else { n })
                })
            });
        let Some(ms) = ms else {
            continue;
        };
        let secs = ms / 1000;
        match su.as_str() {
            "user_message_chunk" if last_kind != su => user_ts.push(secs),
            "agent_message_chunk" if last_kind != su => asst_ts.push(secs),
            "tool_call" if last_kind != su => tool_ts.push(secs),
            _ => {}
        }
        if matches!(
            su.as_str(),
            "user_message_chunk" | "agent_message_chunk" | "tool_call"
        ) {
            last_kind = su;
        } else if !su.is_empty() {
            last_kind.clear();
        }
    }
    let mut ui = 0usize;
    let mut ai = 0usize;
    let mut ti = 0usize;
    for ev in events.iter_mut() {
        match ev {
            TranscriptEvent::UserText { at, .. } if at.is_none() && ui < user_ts.len() => {
                *at = Some(user_ts[ui]);
                ui += 1;
            }
            TranscriptEvent::AssistantText { at, .. } if at.is_none() && ai < asst_ts.len() => {
                *at = Some(asst_ts[ai]);
                ai += 1;
            }
            TranscriptEvent::ToolCall { at, .. } if at.is_none() && ti < tool_ts.len() => {
                *at = Some(tool_ts[ti]);
                ti += 1;
            }
            _ => {}
        }
    }
}

fn read_lines_bounded(path: &Path, max_bytes: u64) -> Result<(Vec<String>, bool), String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let truncated_head = len > max_bytes;
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if truncated_head && !lines.is_empty() {
        lines.remove(0);
    }
    Ok((lines, truncated_head))
}

fn push_full(
    kind: AgentKind,
    v: &Value,
    out: &mut Vec<TranscriptEvent>,
    open_tools: &mut Vec<usize>,
) {
    match kind {
        AgentKind::Cursor | AgentKind::Claude => push_msg(v, out, open_tools),
        AgentKind::Codex => push_codex(v, out, open_tools),
        AgentKind::Grok => push_grok(v, out, open_tools),
    }
}

fn push_msg(v: &Value, out: &mut Vec<TranscriptEvent>, open_tools: &mut Vec<usize>) {
    let role = v
        .get("role")
        .and_then(|r| r.as_str())
        .or_else(|| v.get("type").and_then(|t| t.as_str()))
        .unwrap_or("");
    // Export focus: agent behaviour + user turns. Skip system / meta roles.
    if role == "system" || role == "system_prompt" {
        return;
    }
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| v.get("content"));
    let Some(arr) = content.and_then(|c| c.as_array()) else {
        // plain string content
        if role == "user" || role == "human" {
            if let Some(t) = content.and_then(|c| c.as_str()) {
                let (t, label) = clean_user(t);
                if !t.is_empty() {
                    out.push(TranscriptEvent::UserText {
                        text: trunc(&t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: label,
                    });
                }
            }
        }
        return;
    };
    let is_assistant = role == "assistant";
    let is_user = role == "user" || role == "human";
    for item in arr {
        let t = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match t {
            "text" | "input_text" | "output_text" => {
                if let Some(text) = item.get("text").and_then(|x| x.as_str()) {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if is_assistant {
                        if is_noise_assistant(text) {
                            continue;
                        }
                        out.push(TranscriptEvent::AssistantText {
                            text: trunc(text, MAX_TEXT_CHARS),
                            at: event_time(v),
                            at_label: None,
                        });
                    } else if is_user {
                        let (t, label) = clean_user(text);
                        if !t.is_empty() {
                            out.push(TranscriptEvent::UserText {
                                text: trunc(&t, MAX_TEXT_CHARS),
                                at: event_time(v),
                                at_label: label,
                            });
                        }
                    }
                }
            }
            // Thinking kept but heavily truncated — user cares more about output + tools.
            "thinking" | "reasoning" => {
                if let Some(text) = item
                    .get("thinking")
                    .or_else(|| item.get("text"))
                    .and_then(|x| x.as_str())
                {
                    let text = text.trim();
                    if !text.is_empty() {
                        out.push(TranscriptEvent::Thinking {
                            text: trunc(text, 800),
                            at: event_time(v),
                            at_label: None,
                        });
                    }
                }
            }
            "tool_use" => {
                let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("tool");
                if super::activity::is_todo_tool(name) {
                    // TodoWrite / update_plan：不入行为时间线（同 watch）。
                    continue;
                }
                let args = item.get("input");
                // 与 watch 卡同源：归一化「读取/写入/运行 + 对象」；不单独解析 AskHuman。
                let td = super::activity::classify_tool(name, args);
                out.push(TranscriptEvent::ToolCall {
                    name: name.to_string(),
                    args_summary: format_tool_line(&td),
                    result_summary: None,
                    is_error: false,
                    ask_human: None,
                    at: event_time(v),
                    at_label: None,
                });
                open_tools.push(out.len() - 1);
            }
            "tool_result" => {
                let err = item
                    .get("is_error")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let content = tool_result_text(item);
                close_tool(out, open_tools, content, err);
            }
            _ => {}
        }
    }
}

fn push_codex(v: &Value, out: &mut Vec<TranscriptEvent>, open_tools: &mut Vec<usize>) {
    let ttype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let Some(payload) = v.get("payload") else {
        return;
    };
    let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match (ttype, ptype) {
        ("response_item", "message") => {
            let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if let Some(t) = value_text(payload.get("content")) {
                let t = t.trim();
                if t.is_empty() {
                    return;
                }
                if role == "user" {
                    let (t, label) = clean_user(t);
                    if !t.is_empty() {
                        out.push(TranscriptEvent::UserText {
                            text: trunc(&t, MAX_TEXT_CHARS),
                            at: event_time(v),
                            at_label: label,
                        });
                    }
                } else if role == "assistant" {
                    out.push(TranscriptEvent::AssistantText {
                        text: trunc(t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: None,
                    });
                }
            }
        }
        ("response_item", "reasoning") => {
            if let Some(t) = value_text(payload.get("summary"))
                .or_else(|| value_text(payload.get("content")))
                .or_else(|| {
                    payload
                        .get("text")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                })
            {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(TranscriptEvent::Thinking {
                        text: trunc(t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: None,
                    });
                }
            }
        }
        ("response_item", "function_call") => {
            let name = payload
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("tool");
            if super::activity::is_todo_tool(name) {
                return;
            }
            let args_val = parse_args_value(payload.get("arguments"));
            let td = super::activity::classify_tool(name, args_val.as_ref());
            out.push(TranscriptEvent::ToolCall {
                name: name.to_string(),
                args_summary: format_tool_line(&td),
                result_summary: None,
                is_error: false,
                ask_human: None,
                at: event_time(v),
                at_label: None,
            });
            open_tools.push(out.len() - 1);
        }
        ("response_item", "function_call_output") => {
            let content = payload
                .get("output")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            close_tool(out, open_tools, content, false);
        }
        ("event_msg", "agent_message") => {
            if let Some(t) = payload.get("message").and_then(|m| m.as_str()) {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(TranscriptEvent::AssistantText {
                        text: trunc(t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: None,
                    });
                }
            }
        }
        ("event_msg", "user_message") => {
            if let Some(t) = payload.get("message").and_then(|m| m.as_str()) {
                let (t, label) = clean_user(t.trim());
                if !t.is_empty() {
                    out.push(TranscriptEvent::UserText {
                        text: trunc(&t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: label,
                    });
                }
            }
        }
        _ => {}
    }
}

fn push_grok(v: &Value, out: &mut Vec<TranscriptEvent>, open_tools: &mut Vec<usize>) {
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "user" => {
            if let Some(t) = value_text(v.get("content")) {
                let (t, label) = clean_user(t.trim());
                if !t.is_empty() {
                    out.push(TranscriptEvent::UserText {
                        text: trunc(&t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: label,
                    });
                }
            }
        }
        "assistant" => {
            if let Some(t) = value_text(v.get("content")) {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(TranscriptEvent::AssistantText {
                        text: trunc(t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: None,
                    });
                }
            }
            if let Some(r) = v.get("reasoning").and_then(|x| x.as_str()) {
                let r = r.trim();
                if !r.is_empty() {
                    out.push(TranscriptEvent::Thinking {
                        text: trunc(r, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: None,
                    });
                }
            }
            if let Some(arr) = v.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in arr {
                    let func = tc.get("function");
                    let name = func
                        .and_then(|f| f.get("name"))
                        .or_else(|| tc.get("name"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("tool");
                    if super::activity::is_todo_tool(name) {
                        continue;
                    }
                    let args_val = parse_args_value(
                        func.and_then(|f| f.get("arguments"))
                            .or_else(|| tc.get("arguments")),
                    );
                    let td = super::activity::classify_tool(name, args_val.as_ref());
                    out.push(TranscriptEvent::ToolCall {
                        name: name.to_string(),
                        args_summary: format_tool_line(&td),
                        result_summary: None,
                        is_error: false,
                        ask_human: None,
                        at: event_time(v),
                        at_label: None,
                    });
                    open_tools.push(out.len() - 1);
                }
            }
        }
        "tool_result" => {
            let err = v.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false);
            let content = v
                .get("content")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            close_tool(out, open_tools, content, err);
        }
        "thinking" | "reasoning" => {
            if let Some(t) = value_text(v.get("content")).or_else(|| {
                v.get("text")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
            }) {
                let t = t.trim();
                if !t.is_empty() {
                    out.push(TranscriptEvent::Thinking {
                        text: trunc(t, MAX_TEXT_CHARS),
                        at: event_time(v),
                        at_label: None,
                    });
                }
            }
        }
        _ => {}
    }
}

fn close_tool(
    out: &mut [TranscriptEvent],
    open_tools: &mut Vec<usize>,
    content: String,
    is_error: bool,
) {
    let _ = content;
    if let Some(idx) = open_tools.pop() {
        if let Some(TranscriptEvent::ToolCall { is_error: ie, .. }) = out.get_mut(idx) {
            // 与 watch 一致：不展示 tool result；仅标记失败。
            *ie = is_error;
        }
    }
}

/// Watch 同款：类别词 + 对象。`args_summary` 存 **纯文本** `读取: file.rs`；
/// 渲染层负责 **粗体类别** / *斜体对象*（与 watch 卡 `**类别**: *对象*` 一致）。
pub(super) fn format_tool_line(td: &super::activity::ToolDisplay) -> String {
    use super::activity::ToolLabel;
    let label = match &td.label {
        ToolLabel::Run => "运行",
        ToolLabel::Read => "读取",
        ToolLabel::Write => "写入",
        ToolLabel::Other(n) => n.as_str(),
    };
    match &td.object {
        Some(o) => format!("{label}: {o}"),
        None => label.to_string(),
    }
}

/// Split `读取: file.rs` → (`读取`, `Some(file.rs)`).
pub fn split_tool_line(s: &str) -> (&str, Option<&str>) {
    if let Some((a, b)) = s.split_once(": ") {
        (a, Some(b))
    } else if let Some((a, b)) = s.split_once(':') {
        (a.trim(), Some(b.trim()))
    } else {
        (s, None)
    }
}

fn tool_result_text(item: &Value) -> String {
    if let Some(s) = item.get("content").and_then(|c| c.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = item.get("content").and_then(|c| c.as_array()) {
        let mut parts = Vec::new();
        for it in arr {
            if let Some(t) = it.get("text").and_then(|x| x.as_str()) {
                parts.push(t.to_string());
            }
        }
        return parts.join("\n");
    }
    String::new()
}

fn summarize_args(name: &str, args: Option<&Value>) -> String {
    let Some(a) = args else {
        return String::new();
    };
    // Prefer common fields.
    for key in [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "message",
        "description",
    ] {
        if let Some(s) = a.get(key).and_then(|v| v.as_str()) {
            return format!("{}: {}", key, trunc(s, MAX_ARG_CHARS));
        }
        if let Some(arr) = a.get(key).and_then(|v| v.as_array()) {
            if key == "command" {
                let joined: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
                if !joined.is_empty() {
                    return format!("command: {}", trunc(&joined.join(" "), MAX_ARG_CHARS));
                }
            }
        }
    }
    if name.eq_ignore_ascii_case("ask") {
        if let Some(s) = a.get("message").and_then(|v| v.as_str()) {
            return trunc(s, MAX_ARG_CHARS);
        }
    }
    let s = a.to_string();
    trunc(&s, MAX_ARG_CHARS)
}

pub(super) fn detect_askhuman(
    name: &str,
    args: Option<&Value>,
    result: Option<&str>,
) -> Option<AskHumanBlock> {
    let name_l = name.to_ascii_lowercase();
    let mut message = String::new();
    let mut questions: Vec<String> = Vec::new();
    let mut is_ah = false;

    if let Some(a) = args {
        // CLI 形态（Bash/Shell 命令，string 或 argv 数组）。仅当解析出**命令位**的 AskHuman
        // 提问调用才算——`rg 'AskHuman'`、`pkill -f AskHuman`、`AskHuman daemon status` 等
        // 只是提到或非提问子命令，一律不算（用户实证：脚本里的字面量全被误判成问答卡）。
        let cmd_string = a
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                a.get("command").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            });
        if let Some(cmd) = cmd_string {
            if let Some((m, qs)) = parse_askhuman_cli(&cmd) {
                is_ah = true;
                message = m;
                questions = qs;
            }
        }
        // MCP `ask` 形态：message + questions[]。
        if name_l == "ask" {
            is_ah = true;
            if let Some(m) = a.get("message").and_then(|v| v.as_str()) {
                message = m.to_string();
            }
            if let Some(qs) = a.get("questions").and_then(|v| v.as_array()) {
                for q in qs {
                    if let Some(qt) = q
                        .get("question")
                        .and_then(|x| x.as_str())
                        .or_else(|| q.get("message").and_then(|x| x.as_str()))
                    {
                        questions.push(qt.to_string());
                    }
                }
            }
            if message.is_empty() && questions.is_empty() {
                message = summarize_args(name, args);
            }
        }
    }
    if !is_ah {
        return None;
    }
    let answers = result
        .map(|r| parse_askhuman_answers(r, questions.len().max(1)))
        .unwrap_or_default();
    let qa: Vec<AskQA> = if questions.is_empty() {
        // 无显式 -q / questions：单隐式问题（text 空）承载答案。
        vec![AskQA {
            text: String::new(),
            answer: answers.first().cloned().flatten(),
        }]
    } else {
        questions
            .into_iter()
            .enumerate()
            .map(|(i, text)| AskQA {
                text: trunc(&text, MAX_TEXT_CHARS),
                answer: answers.get(i).cloned().flatten(),
            })
            .collect()
    };
    Some(AskHumanBlock {
        message: trunc(&message, MAX_TEXT_CHARS),
        questions: qa,
    })
}

/// Quote-aware best-effort tokenizer for a shell-ish command line: double/single quotes and
/// backslash escapes inside double quotes; no expansion. Never fails — returns whatever parsed.
fn shellish_tokens(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_double = false;
    let mut in_single = false;
    let mut has_any = false;
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        if in_double {
            match c {
                '"' => in_double = false,
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                _ => cur.push(c),
            }
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                has_any = true;
            }
            '"' => {
                in_double = true;
                has_any = true;
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if has_any || !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                    has_any = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if has_any || !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// AskHuman 后第一个位置参数是这些 → 非提问子命令（daemon 管理 / 待办 / 配置等），不算 ask。
/// 隐藏 hook 子命令（`__` 前缀）另行排除。
const NON_ASK_SUBCOMMANDS: &[&str] = &[
    "daemon", "agents", "dev", "todo", "channel", "config", "doctor", "mcp", "debug", "help",
    "version",
];

/// Parse an AskHuman CLI invocation into `(message, questions)`（spec gui-agent-console C14）。
/// 判定（用户实证修正 2026-07-25：字面量/子命令全被误判）：
/// 1. AskHuman 必须处于**命令位**（整条命令或 `&&`/`;`/`|` 某段的首 token，允许 env 前缀）；
/// 2. 首位置参数是管理子命令 / `__` 隐藏 hook → 不算；
/// 3. 必须有**提问特征**：`-q/--question`、`-m/--message`、`--whats-next`、`--stdin`
///    或非空位置 message；纯旗标调用（--show-last 等）不算。
///
/// 多段命令取第一个满足条件的段。不是 ask 调用返回 None。
fn parse_askhuman_cli(cmd: &str) -> Option<(String, Vec<String>)> {
    let tokens = shellish_tokens(cmd);
    // 按 shell 操作符切段（shellish_tokens 后操作符是独立 token 或粘连 token 的边界近似）。
    let mut segments: Vec<Vec<&str>> = vec![Vec::new()];
    for t in &tokens {
        if matches!(t.as_str(), "&&" | "||" | ";" | "|" | "&") {
            segments.push(Vec::new());
        } else {
            segments.last_mut().unwrap().push(t.as_str());
        }
    }
    segments.iter().find_map(|seg| parse_ask_segment(seg))
}

/// 单段解析：命令位是 AskHuman 且具备提问特征才返回 Some。
fn parse_ask_segment(seg: &[&str]) -> Option<(String, Vec<String>)> {
    // 跳过 env 前缀（VAR=val）与常见包装器。
    let mut idx = 0;
    while idx < seg.len()
        && (seg[idx].contains('=') && !seg[idx].starts_with('-')
            || matches!(seg[idx], "env" | "nohup" | "command"))
    {
        idx += 1;
    }
    let head = seg.get(idx)?;
    let base = head.rsplit('/').next().unwrap_or(head).to_ascii_lowercase();
    if base != "askhuman" && base != "humaninloop" {
        return None;
    }
    let rest = &seg[idx + 1..];
    // 首位置参数是管理子命令 / 隐藏 hook → 不是 ask。
    if let Some(first) = rest.first() {
        let f = first.to_ascii_lowercase();
        if f.starts_with("__") || NON_ASK_SUBCOMMANDS.contains(&f.as_str()) {
            return None;
        }
    }
    let mut message = String::new();
    let mut questions: Vec<String> = Vec::new();
    let mut has_signal = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        match tok {
            "-q" | "--question" => {
                has_signal = true;
                if let Some(v) = rest.get(i + 1) {
                    questions.push(v.to_string());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "-m" | "--message" => {
                has_signal = true;
                if let Some(v) = rest.get(i + 1) {
                    message = v.to_string();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--whats-next" | "--stdin" => {
                has_signal = true;
                i += 1;
            }
            "-o" | "--option" | "-o!" | "--option!" | "-f" | "--file" => {
                i += 2; // 跳过取值
            }
            _ => {
                if !tok.starts_with('-')
                    && !tok.contains("<<")
                    && !tok
                        .chars()
                        .any(|c| matches!(c, '<' | '>' | '|' | '&' | ';'))
                {
                    positional.push(tok.to_string());
                }
                i += 1;
            }
        }
    }
    if message.is_empty() {
        message = positional.into_iter().next().unwrap_or_default();
    }
    if !message.is_empty() {
        has_signal = true;
    }
    if !has_signal {
        return None; // 纯旗标调用（--show-last / --settings 等）。
    }
    Some((message, questions))
}

/// Split a multi-question output（`# Qn` 分组 + `---` 分隔）into per-question answers；
/// 单问题输出整体回填到第一题。`n` 为期望题数（越界分组忽略）。
fn parse_askhuman_answers(content: &str, n: usize) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; n];
    let has_groups = content
        .lines()
        .any(|l| l.trim().starts_with("# Q") && l.trim()[3..].trim().parse::<usize>().is_ok());
    if !has_groups {
        if n > 0 {
            out[0] = parse_askhuman_answer(content);
        }
        return out;
    }
    let mut current: Option<usize> = None;
    let mut buf = String::new();
    let flush = |q: Option<usize>, buf: &mut String, out: &mut Vec<Option<String>>| {
        if let Some(qn) = q {
            if qn >= 1 && qn <= out.len() {
                out[qn - 1] = parse_askhuman_answer(buf);
            }
        }
        buf.clear();
    };
    for line in content.lines() {
        let lt = line.trim();
        if let Some(rest) = lt.strip_prefix("# Q") {
            if let Ok(qn) = rest.trim().parse::<usize>() {
                flush(current.take(), &mut buf, &mut out);
                current = Some(qn);
                continue;
            }
        }
        if lt == "---" {
            continue;
        }
        if current.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    flush(current, &mut buf, &mut out);
    out
}

fn parse_askhuman_answer(content: &str) -> Option<String> {
    // JSON result
    if let Ok(v) = serde_json::from_str::<Value>(content) {
        let mut parts = Vec::new();
        if let Some(s) = v.get("user_input").and_then(|x| x.as_str()) {
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
        if let Some(arr) = v.get("selected_options").and_then(|x| x.as_array()) {
            for o in arr {
                if let Some(s) = o.as_str() {
                    parts.push(s.to_string());
                }
            }
        }
        if !parts.is_empty() {
            return Some(trunc(&parts.join("\n"), MAX_TOOL_RESULT_CHARS));
        }
    }
    // Text markers
    if let Some(i) = content.find("[user_input]") {
        let rest = &content[i + "[user_input]".len()..];
        let end = rest.find('[').unwrap_or(rest.len());
        let s = rest[..end].trim();
        if !s.is_empty() {
            return Some(trunc(s, MAX_TOOL_RESULT_CHARS));
        }
    }
    if let Some(i) = content.find("[selected_options]") {
        let rest = &content[i + "[selected_options]".len()..];
        let end = rest.find('[').unwrap_or(rest.len());
        let s = rest[..end].trim();
        if !s.is_empty() {
            return Some(trunc(s, MAX_TOOL_RESULT_CHARS));
        }
    }
    None
}

/// Clean user text; also return Cursor `<timestamp>` label if present.
/// `pub(super)`：title.rs 的「首条用户消息」回退共用同一清洗（Cursor 把用户输入包在
/// `<timestamp>`/`<user_query>` 里，直接按「`<` 开头＝注入块」过滤会漏掉全部真实输入）。
pub(super) fn clean_user(text: &str) -> (String, Option<String>) {
    let t = text.trim();
    if t.is_empty() {
        return (String::new(), None);
    }
    let (t, cursor_label) = extract_cursor_timestamp(t);
    let t = t.trim();
    // Prefer real user payload when wrapped.
    if let Some(inner) = extract_tag(t, "user_query") {
        let inner = inner.trim();
        if !inner.is_empty() {
            return (strip_system_reminders(inner), cursor_label);
        }
    }
    // Skip pure injection / system-instruction blobs (not agent behaviour).
    if is_injected_or_system_blob(t) {
        return (String::new(), cursor_label);
    }
    (strip_system_reminders(t), cursor_label)
}

fn is_injected_or_system_blob(t: &str) -> bool {
    let head = t.chars().take(200).collect::<String>().to_ascii_lowercase();
    if t.starts_with('<')
        && (t.starts_with("<environment_context>")
            || t.starts_with("<user_info>")
            || t.starts_with("<git_status>")
            || t.starts_with("<INSTRUCTIONS>")
            || t.starts_with("<system>")
            || t.starts_with("<system-reminder>")
            || t.starts_with("<agent_skills>")
            || t.starts_with("<available_skills>")
            || t.starts_with("<mcp_")
            || t.starts_with("<functions>"))
    {
        return true;
    }
    // Common rule-file dumps / CLI system prompts.
    if head.contains("# agents.md")
        || head.contains("# claude.md")
        || head.contains("you are a coding assistant")
        || head.contains("you are claude")
        || head.contains("mandatory interaction protocol")
        || head.contains("askhuman managed skill")
        || head.starts_with("system:")
        || head.starts_with("# system")
    {
        return true;
    }
    // Huge rule dumps without a real user ask.
    if t.len() > 4000
        && (head.contains("follow these instructions")
            || head.contains("project instructions")
            || head.contains("always apply these"))
        && !t.contains("<user_query>")
    {
        return true;
    }
    false
}

fn is_noise_assistant(t: &str) -> bool {
    let head = t.chars().take(120).collect::<String>().to_ascii_lowercase();
    head.starts_with("i'll follow") && head.contains("instruction")
        || head == "ok"
        || head == "understood."
}

/// Cursor embeds wall-clock labels inside user text:
/// `<timestamp>Monday, May 25, 2026, 7:57 AM (UTC+8)</timestamp>`
fn extract_cursor_timestamp(text: &str) -> (String, Option<String>) {
    if let Some(inner) = extract_tag(text, "timestamp") {
        let label = inner.trim().to_string();
        // strip tag from text
        let open = "<timestamp>";
        let close = "</timestamp>";
        let mut s = text.to_string();
        if let Some(start) = s.find(open) {
            if let Some(rel) = s[start..].find(close) {
                let end = start + rel + close.len();
                s.replace_range(start..end, "");
            }
        }
        (s.trim().to_string(), Some(label))
    } else {
        (text.to_string(), None)
    }
}

/// Strip trailing/leading system-reminder blocks while keeping the real ask.
fn strip_system_reminders(t: &str) -> String {
    let mut s = t.to_string();
    // Remove <system-reminder>…</system-reminder> chunks.
    while let Some(start) = s.find("<system-reminder>") {
        if let Some(rel) = s[start..].find("</system-reminder>") {
            let end = start + rel + "</system-reminder>".len();
            s.replace_range(start..end, "");
        } else {
            break;
        }
    }
    s.trim().to_string()
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_string())
}

fn value_text(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = v.as_array() {
        let mut parts = Vec::new();
        for it in arr {
            if let Some(t) = it.get("text").and_then(|x| x.as_str()) {
                parts.push(t.to_string());
            } else if let Some(t) = it
                .get("type")
                .and_then(|t| t.as_str())
                .filter(|t| *t == "output_text" || *t == "input_text" || *t == "text")
                .and_then(|_| it.get("text").and_then(|x| x.as_str()))
            {
                parts.push(t.to_string());
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

fn parse_args_value(v: Option<&Value>) -> Option<Value> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return serde_json::from_str(s)
            .ok()
            .or_else(|| Some(Value::String(s.to_string())));
    }
    Some(v.clone())
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max).collect();
    format!("{t}…")
}

// Re-export path helper visibility: title::transcript_path is pub(super) — same module tree OK.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claude_style_assistant_and_tool() {
        let lines = vec![
            r#"{"type":"user","message":{"content":[{"type":"text","text":"hello"}]}}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#.to_string(),
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"a\nb"}]}}"#.to_string(),
        ];
        let mut events = Vec::new();
        let mut open = Vec::new();
        for l in lines {
            let v: Value = serde_json::from_str(&l).unwrap();
            push_full(AgentKind::Claude, &v, &mut events, &mut open);
        }
        assert!(matches!(&events[0], TranscriptEvent::UserText { text, .. } if text == "hello"));
        assert!(matches!(&events[1], TranscriptEvent::AssistantText { text, .. } if text == "hi"));
        match &events[2] {
            TranscriptEvent::ToolCall {
                name,
                args_summary,
                result_summary,
                ..
            } => {
                assert_eq!(name, "Bash");
                // Watch-style one-liner; results omitted unless error / AskHuman.
                assert!(args_summary.contains("运行") || args_summary.contains("ls"));
                assert_eq!(result_summary.as_deref(), None);
            }
            _ => panic!("expected tool"),
        }
    }

    #[test]
    fn detect_askhuman_cli() {
        let args = serde_json::json!({"command": "AskHuman -m \"pick one\""});
        let ah = detect_askhuman("Bash", Some(&args), None).unwrap();
        assert!(ah.message.contains("pick one"));
        assert_eq!(ah.questions.len(), 1);
        assert!(ah.questions[0].text.is_empty());
    }

    /// CLI 多问题（spec gui-agent-console C14）：message 为位置参数，-q 逐条入列，
    /// -o/-o! 取值被跳过；答案按 `# Qn` 分组回填对应题。
    #[test]
    fn detect_askhuman_cli_multi_question() {
        let args = serde_json::json!({
            "command": "AskHuman \"整体背景说明\" -q \"先修 port 吗？\" -o! \"修\" -o \"不修\" -q \"要加配置项吗？\""
        });
        let result =
            "# Q1\n[selected_options]\n修\n\n---\n\n# Q2\n[user_input]\n不用，保持固定。\n";
        let ah = detect_askhuman("Bash", Some(&args), Some(result)).unwrap();
        assert_eq!(ah.message, "整体背景说明");
        assert_eq!(ah.questions.len(), 2);
        assert_eq!(ah.questions[0].text, "先修 port 吗？");
        assert_eq!(ah.questions[0].answer.as_deref(), Some("修"));
        assert_eq!(ah.questions[1].text, "要加配置项吗？");
        assert_eq!(ah.questions[1].answer.as_deref(), Some("不用，保持固定。"));
    }

    /// MCP `ask` 多问题：message + questions[]；单段答案回填第一题。
    #[test]
    fn detect_askhuman_mcp_questions() {
        let args = serde_json::json!({
            "message": "背景",
            "questions": [{ "question": "Q甲？" }, { "question": "Q乙？" }]
        });
        let ah = detect_askhuman("ask", Some(&args), Some("[user_input]\n答甲\n")).unwrap();
        assert_eq!(ah.message, "背景");
        assert_eq!(ah.questions.len(), 2);
        assert_eq!(ah.questions[0].answer.as_deref(), Some("答甲"));
        assert_eq!(ah.questions[1].answer, None);
    }

    #[test]
    fn parse_answer_markers() {
        let c = "[status] answered\n[user_input]\nyes please\n[files]\n";
        assert_eq!(parse_askhuman_answer(c).as_deref(), Some("yes please"));
    }

    /// heredoc / 操作符不当作 message；--stdin 场景 message 允许为空。
    #[test]
    fn cli_parser_ignores_shell_operators() {
        let (m, qs) =
            parse_askhuman_cli("AskHuman -q \"继续吗？\" --stdin <<'EOF'").expect("is an ask");
        assert_eq!(m, "");
        assert_eq!(qs, vec!["继续吗？"]);
    }

    /// 判定收紧（用户实证 2026-07-25）：字面量提及 / 管理子命令 / 纯旗标不算 ask；
    /// 链式命令取第一个真正的提问段。
    #[test]
    fn cli_parser_rejects_mentions_and_subcommands() {
        // 字面量提及（非命令位）。
        assert!(parse_askhuman_cli("rg -n 'AskHuman' src-tauri/src | head -5").is_none());
        assert!(parse_askhuman_cli("pkill -f '\\.local/bin/AskHuman --gui-host'").is_none());
        assert!(parse_askhuman_cli("ls -la ~/.local/bin/AskHuman").is_none());
        // 管理子命令 / 隐藏 hook / 纯旗标。
        assert!(parse_askhuman_cli("AskHuman daemon status").is_none());
        assert!(parse_askhuman_cli("AskHuman agents monitor --json").is_none());
        assert!(parse_askhuman_cli("AskHuman todo add \"买菜\"").is_none());
        assert!(parse_askhuman_cli("AskHuman __agent-hook cursor activity").is_none());
        assert!(parse_askhuman_cli("AskHuman --show-last").is_none());
        // 链式：管理段被跳过，提问段命中。
        let (m, qs) = parse_askhuman_cli(
            "AskHuman agents monitor && AskHuman \"报告\" -q \"下一步？\" -o \"A\"",
        )
        .expect("second segment is an ask");
        assert_eq!(m, "报告");
        assert_eq!(qs, vec!["下一步？"]);
        // whats-next 是提问。
        assert!(parse_askhuman_cli("AskHuman --whats-next \"总结\" -o \"好\"").is_some());
        // 带路径的命令位。
        assert!(parse_askhuman_cli("/usr/local/bin/AskHuman -m \"选一个\"").is_some());
    }

    /// 事件 JSON：ask 块打 `type:"ask"`，普通工具行打 `type:"tool"` 且 label/object 拆分。
    #[test]
    fn event_json_tags_ask_and_tool() {
        let ask = TranscriptEvent::ToolCall {
            name: "Bash".into(),
            args_summary: "运行: AskHuman".into(),
            result_summary: None,
            is_error: false,
            ask_human: Some(AskHumanBlock {
                message: "m".into(),
                questions: vec![AskQA {
                    text: "q?".into(),
                    answer: Some("a".into()),
                }],
            }),
            at: Some(1),
            at_label: None,
        };
        let v = event_json(&ask);
        assert_eq!(v["type"], "ask");
        assert_eq!(v["questions"][0]["answer"], "a");
        let tool = TranscriptEvent::ToolCall {
            name: "Bash".into(),
            args_summary: "运行: cargo test".into(),
            result_summary: None,
            is_error: false,
            ask_human: None,
            at: None,
            at_label: None,
        };
        let v2 = event_json(&tool);
        assert_eq!(v2["type"], "tool");
        assert_eq!(v2["label"], "运行");
        assert_eq!(v2["object"], "cargo test");
    }

    #[test]
    fn parse_iso8601_z_and_offset() {
        // 2026-06-13T10:09:57Z
        let t = parse_iso8601_secs("2026-06-13T10:09:57.062Z").unwrap();
        assert!(t > 1_700_000_000);
        // with +08:00 should be 8h earlier in UTC epoch than local wall for same digits
        let t2 = parse_iso8601_secs("2026-06-13T18:09:57+08:00").unwrap();
        assert_eq!(t2, t); // 18:09+08 == 10:09Z
    }

    #[test]
    fn event_time_from_claude_style() {
        let v = serde_json::json!({
            "type": "user",
            "timestamp": "2026-06-13T10:09:57.062Z",
            "message": { "content": [{"type":"text","text":"hi"}] }
        });
        assert!(event_time(&v).is_some());
    }

    /// Optional smoke against a local Claude transcript. CI runners have no
    /// `~/.claude/projects`, so skip instead of failing the Linux-only `cargo test` job.
    #[test]
    fn real_claude_file_has_times() {
        let home = dirs::home_dir().expect("home");
        let projects = home.join(".claude/projects");
        let mut found = None;
        if let Ok(walk) = std::fs::read_dir(&projects) {
            for e in walk.flatten() {
                if let Ok(rd) = std::fs::read_dir(e.path()) {
                    for f in rd.flatten() {
                        let path = f.path();
                        if path.extension().and_then(|x| x.to_str()) == Some("jsonl")
                            && path.metadata().map(|m| m.len()).unwrap_or(0) > 5000
                        {
                            found = Some(path);
                            break;
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }
        let Some(path) = found else {
            eprintln!("skip real_claude_file_has_times: no ~/.claude/projects/*.jsonl sample");
            return;
        };
        let doc = load_path(AgentKind::Claude, &path).expect("load");
        let with_time = doc
            .events
            .iter()
            .filter(|e| match e {
                TranscriptEvent::UserText { at, .. }
                | TranscriptEvent::AssistantText { at, .. } => at.is_some(),
                _ => false,
            })
            .count();
        let total_ua = doc
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    TranscriptEvent::UserText { .. } | TranscriptEvent::AssistantText { .. }
                )
            })
            .count();
        eprintln!(
            "file={path:?} events={} user/assistant={total_ua} with_time={with_time}",
            doc.events.len()
        );
        assert!(
            with_time > 0,
            "expected some User/Assistant events to have timestamps from real claude file"
        );
        // also render md snippet
        let md = crate::export::render_transcript_md(&doc, "test");
        eprintln!("md head:\n{}", md.chars().take(800).collect::<String>());
        assert!(
            md.contains("·") || md.contains(":"),
            "expected time in md headings"
        );
    }

    #[test]
    fn real_grok_and_claude_times() {
        let home = dirs::home_dir().unwrap();
        // Claude
        let claude = home.join(".claude/projects");
        let mut claude_path = None;
        if let Ok(walk) = std::fs::read_dir(&claude) {
            for e in walk.flatten() {
                if let Ok(rd) = std::fs::read_dir(e.path()) {
                    for f in rd.flatten() {
                        let p = f.path();
                        if p.extension().and_then(|x| x.to_str()) == Some("jsonl")
                            && p.metadata().map(|m| m.len()).unwrap_or(0) > 10000
                        {
                            claude_path = Some(p);
                            break;
                        }
                    }
                }
                if claude_path.is_some() {
                    break;
                }
            }
        }
        if let Some(p) = claude_path {
            let doc = load_path(AgentKind::Claude, &p).unwrap();
            let md = crate::export::render_transcript_md(&doc, "claude");
            let has = md.lines().any(|l| l.starts_with("### ") && l.contains("·"));
            eprintln!("CLAUDE has_time_in_heading={has}");
            assert!(has);
        }
        // Grok this workspace
        let grok = home.join(".grok/sessions");
        let mut grok_path = None;
        if let Ok(walk) = std::fs::read_dir(&grok) {
            for e in walk.flatten() {
                let p = e
                    .path()
                    .join("019f4c59-48ad-7462-b148-e50634641c3e")
                    .join("chat_history.jsonl");
                if p.is_file() {
                    grok_path = Some(p);
                    break;
                }
                // also any chat_history
                if let Ok(rd) = std::fs::read_dir(e.path()) {
                    for f in rd.flatten() {
                        let ch = f.path().join("chat_history.jsonl");
                        if ch.is_file() && ch.metadata().map(|m| m.len()).unwrap_or(0) > 100000 {
                            grok_path = Some(ch);
                            break;
                        }
                    }
                }
                if grok_path.is_some() {
                    break;
                }
            }
        }
        if let Some(p) = grok_path {
            let doc = load_path(AgentKind::Grok, &p).unwrap();
            let md = crate::export::render_transcript_md(&doc, "grok");
            let has = md.lines().any(|l| l.starts_with("### ") && l.contains("·"));
            eprintln!(
                "GROK file={p:?} events={} has_time_in_heading={has}",
                doc.events.len()
            );
            eprintln!("GROK sample headings:");
            for l in md.lines().filter(|l| l.starts_with("### ")).take(8) {
                eprintln!("  {l}");
            }
        }
    }
}
