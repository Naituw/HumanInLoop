//! 生命周期上报器：由三家 Agent 的用户级 hook 调用，向 daemon 上报一条事件即退出（spec D20）。
//!
//! 调用形如 `AskHuman __agent-hook <agent> <event>`：
//! - `<agent>`：claude / codex / cursor（hook 安装时写死，**意图**家族）。
//! - `<event>`：session-start / turn-start / turn-end / session-end。
//!
//! 会话 ID 解析优先级：env 专用变量 → hook 经 stdin 传入的 JSON（`session_id` 等）。
//! pid 通过向上 walk 进程树定位到真实 Agent 进程；cwd 取 stdin / env / 当前目录。
//!
//! 去重（Cursor 双触发，FINDINGS §7.6）：Cursor 会同时按自身 hook 与兼容的 Claude hook 触发；
//! 若 env 探测出的「真实运行家族」与意图家族不一致，则**跳过**本次上报，避免重复登记。

use std::collections::HashMap;
use std::io::{BufReader, Cursor, IsTerminal, Read};

use serde::de::IgnoredAny;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::detect;
use super::AgentKind;
use crate::ipc::{ClientMsg, ToolPhase, ToolReport};

const MAX_HOOK_STDIN_BYTES: usize = 10 * 1024 * 1024;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;

/// 入口：`args` 为 `__agent-hook` 之后的参数（`[<agent>, <event>]`）。失败一律静默退出。
pub fn run(args: &[String]) {
    let Some(intended) = args.first().and_then(|s| AgentKind::parse(s)) else {
        return;
    };
    let Some(event) = args.get(1).and_then(|s| super::LifecycleEvent::parse(s)) else {
        return;
    };

    let env: HashMap<String, String> = std::env::vars().collect();

    // 去重：跳过「兼容加载他家 hook」造成的误触发。
    if should_skip(intended, &env) {
        return;
    }

    let stdin = read_stdin_json();
    let session_id = resolve_session_id(intended, &env, stdin.as_ref());
    // 无会话 ID 无法作为身份键（spec D7），直接放弃（best-effort）。
    if session_id.is_empty() {
        return;
    }
    // 不在 hook 侧 walk 进程树（~280ms），改发 ppid 给 daemon 缓存解析。
    let hint_pid = detect::parent_pid(std::process::id());
    let cwd = resolve_cwd(&env, stdin.as_ref());
    let transcript_path = resolve_transcript_path(&env, stdin.as_ref());
    let launch_id = env
        .get(crate::integrations::agent_launch::LAUNCH_ID_ENV)
        .cloned();
    let prompt_sha256 = matches!(event, super::LifecycleEvent::TurnStart)
        .then(|| initial_prompt(stdin.as_ref()))
        .flatten()
        .map(|prompt| format!("{:x}", Sha256::digest(prompt.as_bytes())));

    // 仅 activity 事件（Pre/PostToolUse）才尝试解析工具信息；其余事件无工具。
    let tool = if matches!(event, super::LifecycleEvent::Activity) {
        extract_tool(stdin.as_ref())
    } else {
        None
    };

    // 插话轮询（spec agent-interject D3/D4）：仅 **PreToolUse**（stdin 判定阶段为 pre）、
    // 且已通过上方去重、且非 Grok（首期排除，无可靠传话通道）时，在上报的同一连接上
    // 读回一帧裁决；PostToolUse 与其余事件保持即发即走。
    let interject_poll = matches!(event, super::LifecycleEvent::Activity)
        && intended != AgentKind::Grok
        && stdin.as_ref().and_then(detect_phase) == Some(ToolPhase::Pre);

    let msg = ClientMsg::AgentEvent {
        agent: intended.as_str().to_string(),
        event: event.as_str().to_string(),
        session_id,
        pid: None,
        hint_pid,
        cwd,
        transcript_path,
        launch_id,
        prompt_sha256,
        ts: 0,
        tool,
        interject_poll,
    };
    if interject_poll {
        if let crate::client::InterjectPollOutcome::Deny { text, attachments } =
            crate::client::report_agent_event_with_poll(msg)
        {
            print_deny_json(intended, &text, &attachments);
        }
    } else {
        crate::client::report_agent_event(msg);
    }
}

/// Compatibility-loader deduplication shared by lifecycle and Stop hooks.
pub(super) fn should_skip(intended: AgentKind, env: &HashMap<String, String>) -> bool {
    let running = detect::detect_running_agent_from(env);
    (running == Some(AgentKind::Grok) && intended != AgentKind::Grok)
        || (intended == AgentKind::Claude && running == Some(AgentKind::Cursor))
}

/// Send a lifecycle event without tool data or interjection polling.
pub(super) fn report_simple_event(
    intended: AgentKind,
    event: super::LifecycleEvent,
    session_id: String,
    cwd: Option<String>,
) {
    if session_id.trim().is_empty() {
        return;
    }
    let hint_pid = detect::parent_pid(std::process::id());
    crate::client::report_agent_event(ClientMsg::AgentEvent {
        agent: intended.as_str().to_string(),
        event: event.as_str().to_string(),
        session_id,
        pid: None,
        hint_pid,
        cwd,
        transcript_path: std::env::var("PI_SESSION_FILE")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        launch_id: std::env::var(crate::integrations::agent_launch::LAUNCH_ID_ENV).ok(),
        prompt_sha256: None,
        ts: 0,
        tool: None,
        interject_poll: false,
    });
}

fn initial_prompt(value: Option<&Value>) -> Option<&str> {
    let value = value?;
    ["prompt", "user_prompt", "userPrompt", "message"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// 输出各家 PreToolUse 的 deny JSON（stdout，随后调用方 exit 0；spec agent-interject D3）。
/// 消息经 `prompts::interject_deny_reason` 包装（`[USER INTERJECTION]` 协议文案）。
fn print_deny_json(kind: AgentKind, message: &str, attachments: &[crate::models::FileAttachment]) {
    let json = deny_json(kind, message, attachments);
    println!("{json}");
}

/// 构造 deny JSON（纯函数，供单测）。Claude / Codex 同构（`hookSpecificOutput`）；
/// Cursor 用 `permission` + `user_message`/`agent_message` **双字段同文**：live 实测 + bundle
/// 静态核对（IDE cursor-agent-exec 与 CLI hooks-exec 的 deny 分支）证实**模型看到的拒绝理由
/// 取自 `user_message`**（`agent_message` 仅透传 protobuf、未见进模型的消费点，与官方文档
/// 「fed back to the agent」不符）；两字段都放完整协议文本，兼容未来 Cursor 按文档语义改用
/// `agent_message`。代价：UI 拦截提示显示整段协议文本（内含用户原话），可接受。
fn deny_json(
    kind: AgentKind,
    message: &str,
    attachments: &[crate::models::FileAttachment],
) -> Value {
    let reason = crate::prompts::interject_deny_reason(message, attachments);
    match kind {
        AgentKind::Cursor => serde_json::json!({
            "permission": "deny",
            "agent_message": reason.clone(),
            "user_message": reason,
        }),
        AgentKind::Pi => serde_json::json!({
            "block": true,
            "reason": reason,
        }),
        // Claude / Codex（Grok 不会走到：上游已排除）。
        _ => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }),
    }
}

/// 从 hook stdin 解析本次工具调用（best-effort）：判 pre/post，pre 需能取到工具名（否则退化为无工具）。
fn extract_tool(stdin: Option<&Value>) -> Option<ToolReport> {
    let v = stdin?;
    match detect_phase(v)? {
        // post：清除不依赖名字，尽力带上（daemon 只按 session 清）。
        ToolPhase::Post => Some(ToolReport {
            name: tool_name(v).unwrap_or_default(),
            object: None,
            phase: ToolPhase::Post,
        }),
        // pre：无工具名无法展示 → 退化为无工具（纯心跳）。
        ToolPhase::Pre => {
            let name = tool_name(v)?;
            let object = super::activity::classify_tool(&name, tool_input(v).as_ref()).object;
            Some(ToolReport {
                name,
                object,
                phase: ToolPhase::Pre,
            })
        }
    }
}

/// 判断工具阶段：优先看显式 hook 事件名，否则按「有无结果字段 / 有无工具输入」启发式。
fn detect_phase(v: &Value) -> Option<ToolPhase> {
    for k in ["hook_event_name", "hookEventName"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            let l = s.to_ascii_lowercase();
            if l.contains("pretooluse") {
                return Some(ToolPhase::Pre);
            }
            if l.contains("posttooluse") {
                return Some(ToolPhase::Post);
            }
        }
    }
    // 结果字段（非空）→ post。
    for k in [
        "tool_response",
        "tool_result",
        "tool_output",
        "function_call_output",
        "response",
        "output",
    ] {
        if v.get(k).map(|x| !x.is_null()).unwrap_or(false) {
            return Some(ToolPhase::Post);
        }
    }
    // 有工具名 / 输入 → pre。
    let has_tool = [
        "tool_name",
        "toolName",
        "tool",
        "tool_input",
        "toolInput",
        "tool_calls",
    ]
    .iter()
    .any(|k| v.get(*k).map(|x| !x.is_null()).unwrap_or(false));
    has_tool.then_some(ToolPhase::Pre)
}

/// 取工具名（各家字段兼容）。
pub(super) fn tool_name(v: &Value) -> Option<String> {
    for k in ["tool_name", "toolName", "tool"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// 取工具输入（对象或原始 JSON 字符串，`classify_tool` 内部再 `parse_args`）。
pub(super) fn tool_input(v: &Value) -> Option<Value> {
    for k in ["tool_input", "toolInput", "input", "arguments"] {
        if let Some(x) = v.get(k) {
            if !x.is_null() {
                return Some(x.clone());
            }
        }
    }
    None
}

/// 解析会话 ID：env 专用变量优先，其次 stdin JSON 的若干常见字段。
pub(super) fn resolve_session_id(
    kind: AgentKind,
    env: &HashMap<String, String>,
    stdin: Option<&Value>,
) -> String {
    if let Some(s) = detect::session_id_from_env_map(kind, env) {
        return s;
    }
    if let Some(v) = stdin {
        for key in [
            "session_id",
            "sessionId",
            "conversation_id",
            "conversationId",
            "thread_id",
            "threadId",
        ] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                let s = s.trim();
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    String::new()
}

/// 解析工作目录：stdin JSON `cwd` → env 工程目录 → 当前目录。
pub(crate) fn resolve_cwd(env: &HashMap<String, String>, stdin: Option<&Value>) -> Option<String> {
    if let Some(v) = stdin {
        if let Some(s) = v.get("cwd").and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    for key in [
        "CURSOR_PROJECT_DIR",
        "GROK_WORKSPACE_ROOT",
        "CLAUDE_PROJECT_DIR",
    ] {
        if let Some(s) = env.get(key) {
            if !s.trim().is_empty() {
                return Some(s.clone());
            }
        }
    }
    std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
}

fn resolve_transcript_path(env: &HashMap<String, String>, stdin: Option<&Value>) -> Option<String> {
    if let Some(value) = env
        .get("PI_SESSION_FILE")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_string());
    }
    let value = stdin?;
    [
        "transcript_path",
        "transcriptPath",
        "session_file",
        "sessionFile",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(Value::as_str))
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
}

/// Read JSON delivered to a hook over stdin.
///
/// Inputs up to 10 MiB keep the original full-JSON behavior. Larger inputs are replayed through a
/// streaming summary parser that retains only lifecycle metadata and ignores large tool bodies.
/// Both paths consume stdin through EOF so the hook caller never sees a broken pipe merely because
/// its payload exceeded our in-memory full-JSON limit.
pub(super) fn read_stdin_json() -> Option<Value> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut locked = stdin.lock();
    read_stdin_json_from(&mut locked, MAX_HOOK_STDIN_BYTES)
}

fn read_stdin_json_from<R: Read>(reader: &mut R, max_bytes: usize) -> Option<Value> {
    let mut prefix = Vec::new();
    let prefix_result = reader
        .by_ref()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut prefix);
    if prefix_result.is_err() {
        let _ = std::io::copy(reader, &mut std::io::sink());
        return None;
    }

    if prefix.len() <= max_bytes {
        return parse_stdin_bytes(&prefix, max_bytes);
    }

    // Replay the retained prefix, then continue directly from stdin. `IgnoredAny` makes
    // serde_json validate and skip large response/input values without materializing their base64
    // strings or nested JSON in memory. BufReader avoids byte-at-a-time reads from the pipe.
    let replay = Cursor::new(prefix).chain(reader.by_ref());
    let mut replay = BufReader::with_capacity(STREAM_BUFFER_BYTES, replay);
    let summary = serde_json::from_reader::<_, HookInputSummary>(&mut replay).ok();
    // A malformed document may make serde_json return before EOF. Drain whatever remains so the
    // writer still cannot receive SIGPIPE from our size/error handling path.
    let _ = std::io::copy(&mut replay, &mut std::io::sink());
    summary.map(HookInputSummary::into_value)
}

fn parse_stdin_bytes(bytes: &[u8], max_bytes: usize) -> Option<Value> {
    if bytes.len() > max_bytes {
        return None;
    }
    let trimmed = std::str::from_utf8(bytes).ok()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// Minimal metadata retained when a hook payload is too large for full in-memory parsing.
///
/// Potentially large values use `IgnoredAny`, whose serde_json implementation skips strings and
/// containers in place. The reconstructed `Value` intentionally contains only fields consumed by
/// lifecycle reporting; tool input details and response bodies are not needed to close a
/// PostToolUse activity.
#[derive(Default, Deserialize)]
struct HookInputSummary {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id_camel: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default, rename = "conversationId")]
    conversation_id_camel: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default, rename = "threadId")]
    thread_id_camel: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    hook_event_name: Option<String>,
    #[serde(default, rename = "hookEventName")]
    hook_event_name_camel: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default, rename = "toolName")]
    tool_name_camel: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default, rename = "transcriptPath")]
    transcript_path_camel: Option<String>,

    #[serde(default)]
    tool_input: Option<IgnoredAny>,
    #[serde(default, rename = "toolInput")]
    tool_input_camel: Option<IgnoredAny>,
    #[serde(default)]
    input: Option<IgnoredAny>,
    #[serde(default)]
    arguments: Option<IgnoredAny>,
    #[serde(default)]
    tool_calls: Option<IgnoredAny>,

    #[serde(default)]
    tool_response: Option<IgnoredAny>,
    #[serde(default)]
    tool_result: Option<IgnoredAny>,
    #[serde(default)]
    tool_output: Option<IgnoredAny>,
    #[serde(default)]
    function_call_output: Option<IgnoredAny>,
    #[serde(default)]
    response: Option<IgnoredAny>,
    #[serde(default)]
    output: Option<IgnoredAny>,
}

impl HookInputSummary {
    fn into_value(self) -> Value {
        let mut map = serde_json::Map::new();

        let session_id = self
            .session_id
            .or(self.session_id_camel)
            .or(self.conversation_id)
            .or(self.conversation_id_camel)
            .or(self.thread_id)
            .or(self.thread_id_camel);
        insert_summary_string(&mut map, "session_id", session_id);
        insert_summary_string(&mut map, "cwd", self.cwd);
        insert_summary_string(
            &mut map,
            "hook_event_name",
            self.hook_event_name.or(self.hook_event_name_camel),
        );
        insert_summary_string(&mut map, "source", self.source);
        insert_summary_string(
            &mut map,
            "tool_name",
            self.tool_name.or(self.tool_name_camel).or(self.tool),
        );
        insert_summary_string(&mut map, "status", self.status);
        insert_summary_string(
            &mut map,
            "transcript_path",
            self.transcript_path.or(self.transcript_path_camel),
        );

        if [
            self.tool_input,
            self.tool_input_camel,
            self.input,
            self.arguments,
            self.tool_calls,
        ]
        .into_iter()
        .any(|field| field.is_some())
        {
            map.insert("tool_input".to_string(), Value::Object(Default::default()));
            map.insert("toolInputTruncated".to_string(), Value::Bool(true));
        }
        if [
            self.tool_response,
            self.tool_result,
            self.tool_output,
            self.function_call_output,
            self.response,
            self.output,
        ]
        .into_iter()
        .any(|field| field.is_some())
        {
            map.insert("tool_response".to_string(), Value::Bool(true));
        }

        Value::Object(map)
    }
}

fn insert_summary_string(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        map.insert(key.to_string(), Value::String(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hook_stdin_parser_rejects_empty_malformed_invalid_utf8_and_oversize() {
        assert!(parse_stdin_bytes(b"", 64).is_none());
        assert!(parse_stdin_bytes(b"not json", 64).is_none());
        assert!(parse_stdin_bytes(&[0xff], 64).is_none());
        assert!(parse_stdin_bytes(&[b' '; 65], 64).is_none());
        assert_eq!(
            parse_stdin_bytes(br#" {"session_id":"s1"} "#, 64).unwrap()["session_id"],
            "s1"
        );
    }

    #[test]
    fn oversized_post_tool_input_is_drained_and_summarized() {
        let payload = serde_json::to_vec(&json!({
            "session_id": "session-large",
            "cwd": "/tmp/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "view_image",
            "tool_input": {"path": "/tmp/large.png"},
            "tool_response": format!("data:image/png;base64,{}", "A".repeat(4_096)),
            "tool_use_id": "call-1"
        }))
        .unwrap();
        let mut reader = Cursor::new(payload.as_slice());

        let parsed = read_stdin_json_from(&mut reader, 128).unwrap();

        assert_eq!(reader.position(), payload.len() as u64);
        assert_eq!(parsed["session_id"], "session-large");
        assert_eq!(parsed["cwd"], "/tmp/project");
        assert_eq!(parsed["hook_event_name"], "PostToolUse");
        let tool = extract_tool(Some(&parsed)).unwrap();
        assert_eq!(tool.name, "view_image");
        assert_eq!(tool.phase, ToolPhase::Post);
        assert_eq!(tool.object, None);
    }

    #[test]
    fn small_hook_input_preserves_full_tool_details() {
        let payload = serde_json::to_vec(&json!({
            "session_id": "session-small",
            "hook_event_name": "PreToolUse",
            "tool_name": "Shell",
            "tool_input": {"command": "cargo test"}
        }))
        .unwrap();
        let mut reader = Cursor::new(payload.as_slice());

        let parsed = read_stdin_json_from(&mut reader, 1_024).unwrap();
        let tool = extract_tool(Some(&parsed)).unwrap();

        assert_eq!(tool.phase, ToolPhase::Pre);
        assert_eq!(tool.object.as_deref(), Some("cargo test"));
    }

    #[test]
    fn oversized_summary_preserves_heuristic_phase_markers() {
        for (field, expected) in [
            ("tool_input", ToolPhase::Pre),
            ("tool_response", ToolPhase::Post),
        ] {
            let payload = serde_json::to_vec(&json!({
                "session_id": "session-phase",
                "tool_name": "custom_tool",
                (field): "A".repeat(4_096)
            }))
            .unwrap();
            let mut reader = Cursor::new(payload.as_slice());

            let parsed = read_stdin_json_from(&mut reader, 128).unwrap();
            assert_eq!(extract_tool(Some(&parsed)).unwrap().phase, expected);
        }
    }

    #[test]
    fn malformed_oversized_input_is_still_drained() {
        let payload = format!("{{not-json:{}", "x".repeat(4_096));
        let mut reader = Cursor::new(payload.as_bytes());

        assert!(read_stdin_json_from(&mut reader, 64).is_none());
        assert_eq!(reader.position(), payload.len() as u64);
    }

    #[test]
    fn full_json_limit_is_ten_mib() {
        assert_eq!(MAX_HOOK_STDIN_BYTES, 10 * 1024 * 1024);
    }

    #[test]
    fn phase_pre_from_tool_input() {
        let v = json!({"tool_name":"Shell","tool_input":{"command":"cargo test"}});
        assert_eq!(detect_phase(&v), Some(ToolPhase::Pre));
        let t = extract_tool(Some(&v)).unwrap();
        assert_eq!(t.phase, ToolPhase::Pre);
        assert_eq!(t.name, "Shell");
        assert_eq!(t.object.as_deref(), Some("cargo test"));
    }

    #[test]
    fn phase_post_from_response_field() {
        let v = json!({"tool_name":"Read","tool_input":{"file_path":"/a/b.rs"},"tool_response":{"ok":true}});
        assert_eq!(detect_phase(&v), Some(ToolPhase::Post));
        assert_eq!(extract_tool(Some(&v)).unwrap().phase, ToolPhase::Post);
    }

    #[test]
    fn explicit_hook_event_name_wins() {
        // 显式事件名优先于「有 tool_input 像 pre」的启发式。
        let v = json!({"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{}});
        assert_eq!(detect_phase(&v), Some(ToolPhase::Post));
    }

    #[test]
    fn no_tool_fields_is_none() {
        let v = json!({"session_id":"s","cwd":"/x"});
        assert_eq!(detect_phase(&v), None);
        assert!(extract_tool(Some(&v)).is_none());
    }

    #[test]
    fn pre_without_name_degrades() {
        let v = json!({"tool_input":{"command":"ls"}});
        assert_eq!(detect_phase(&v), Some(ToolPhase::Pre));
        assert!(extract_tool(Some(&v)).is_none());
    }

    #[test]
    fn deny_json_claude_codex_shape() {
        for kind in [AgentKind::Claude, AgentKind::Codex] {
            let v = deny_json(kind, "改用方案 B", &[]);
            let out = &v["hookSpecificOutput"];
            assert_eq!(out["hookEventName"], "PreToolUse");
            assert_eq!(out["permissionDecision"], "deny");
            let reason = out["permissionDecisionReason"].as_str().unwrap();
            assert!(reason.starts_with("[USER INTERJECTION]"));
            assert!(reason.contains("<user_message>\n改用方案 B\n</user_message>"));
            assert!(v.get("permission").is_none(), "不应混入 Cursor 字段");
        }
    }

    #[test]
    fn deny_json_cursor_shape() {
        let v = deny_json(AgentKind::Cursor, "停一下", &[]);
        assert_eq!(v["permission"], "deny");
        // live 实测：Cursor 喂回模型的拒绝理由取自 user_message（agent_message 未见消费）——
        // 两字段须同为完整协议文本，缺一即丢话。
        let user_msg = v["user_message"].as_str().unwrap();
        assert!(user_msg.starts_with("[USER INTERJECTION]"));
        assert!(user_msg.contains("停一下"));
        assert_eq!(v["agent_message"], v["user_message"]);
        assert!(
            v.get("hookSpecificOutput").is_none(),
            "不应混入 Claude 字段"
        );
    }

    #[test]
    fn deny_json_pi_extension_shape() {
        let value = deny_json(AgentKind::Pi, "change direction", &[]);
        assert_eq!(value["block"], true);
        let reason = value["reason"].as_str().unwrap();
        assert!(reason.starts_with("[USER INTERJECTION]"));
        assert!(reason.contains("change direction"));
        assert!(value.get("hookSpecificOutput").is_none());
    }
}
