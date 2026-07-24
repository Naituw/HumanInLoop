//! `ask` 工具：把 MCP 入参翻译成 `AskHuman --output json …` argv，spawn 子进程复用既有 ask 流程，
//! 再把子进程的 JSON 结果整理成 MCP `structuredContent` + `TextContent`，并将人类回复中的图片读回为
//! `ImageContent` 一并返回。
//!
//! 关键点：
//! - 子进程用 Tokio `Command` 运行 —— stdin 被置空、stdout/stderr 被捕获，因此**不会**污染本
//!   server 的 STDIO MCP 协议流；`kill_on_drop(true)` + 对 rmcp `CancellationToken` 的 `select!`
//!   保证 MCP 调用被客户端取消时子进程随之终止，进而让 daemon 从 CLI socket EOF 取消在途请求。
//! - 子进程的 JSON 含脚本专用的 `selected_indices`；反序列化进 [`AskResult`]（无该字段）即自动丢弃，
//!   再重新序列化为 `structuredContent`，对 MCP 客户端不暴露该字段。
//!
//! ## 取消语义（为何必须 await token，而不能只靠 drop future）
//!
//! rmcp 收到 `notifications/cancelled` 时**只** `cancel()` 该 request 的 `CancellationToken`，
//! **不会** abort / drop 已 spawn 的 tool handler。因此仅设 `kill_on_drop` 而不 `select!` token
//! 时，`ask` 会继续 await 子进程，弹窗与 IM 卡成为孤儿。token 取消路径必须显式 `kill` 子进程。

use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, Meta, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

const CODEX_TURN_METADATA_KEY: &str = "x-codex-turn-metadata";
const CODEX_THREAD_SOURCE_KEY: &str = "thread_source";
/// Thread origins whose AskHuman calls must be silently rejected. `system` is the label used by
/// ambient/background threads up to ChatGPT.app 26.7xx; `ambient_suggestions` replaced it in
/// 26.721+. Unknown origins are allowed through (fail-open) but leave a "passed" audit line.
/// Shared with the Codex stop-confirmation guard in `crate::agents::stop`.
pub(crate) const CODEX_BLOCKED_THREAD_SOURCES: [&str; 2] = ["system", "ambient_suggestions"];
const CODEX_BLOCKED_THREAD_MESSAGE: &str =
    "AskHuman is disabled for this Codex system-generated background thread.\n\
Do not retry or contact the human; finish the host-requested non-interactive output directly.";

fn codex_thread_source(metadata: &Value) -> Option<&str> {
    metadata.as_object()?.get(CODEX_THREAD_SOURCE_KEY)?.as_str()
}

fn is_blocked_thread_source(source: &str) -> bool {
    CODEX_BLOCKED_THREAD_SOURCES.contains(&source)
}

/// Codex attaches trusted per-turn context under this namespaced `_meta` entry for every MCP call.
/// Older versions may encode the inner value as JSON text, so both representations are accepted.
fn codex_turn_metadata(meta: &Meta) -> Option<Value> {
    let turn_metadata = meta.0.get(CODEX_TURN_METADATA_KEY)?;
    match turn_metadata {
        Value::Object(_) => Some(turn_metadata.clone()),
        Value::String(encoded) => serde_json::from_str::<Value>(encoded).ok(),
        _ => None,
    }
}

fn metadata_id<'a>(metadata: &'a Value, snake_case: &str, camel_case: &str) -> Option<&'a str> {
    metadata
        .get(snake_case)
        .or_else(|| metadata.get(camel_case))
        .and_then(Value::as_str)
}

/// Guard decision + audit label for one call: `(action, reason)`. `None` means the call does not
/// carry the Codex `_meta` namespace at all (non-Codex client; nothing to guard or audit).
fn codex_guard_action_reason(meta: &Meta) -> Option<(&'static str, &'static str)> {
    meta.0.get(CODEX_TURN_METADATA_KEY)?;
    let Some(metadata) = codex_turn_metadata(meta) else {
        return Some(("passed", "codex_turn_metadata_unreadable"));
    };
    Some(match codex_thread_source(&metadata) {
        Some(source) if is_blocked_thread_source(source) => {
            ("suppressed", "codex_blocked_thread_source")
        }
        Some(_) => ("passed", "codex_thread_source_passed"),
        None => ("passed", "codex_thread_source_missing"),
    })
}

/// Runtime guard for Codex-originated calls, evaluated before any side effect (popup, IM push,
/// todo write). Blocked thread sources get an error result plus a "suppressed" audit line; every
/// other Codex call proceeds but leaves a "passed" audit line carrying the observed
/// `thread_source`, so a future host-side label rename shows up in `daemon.log` instead of
/// failing open invisibly.
fn codex_thread_guard(meta: &Meta, tool: &str) -> Option<CallToolResult> {
    let (action, reason) = codex_guard_action_reason(meta)?;
    let metadata = codex_turn_metadata(meta);
    let metadata = metadata.as_ref();
    crate::daemon::lifecycle::log_guard_audit(crate::daemon::lifecycle::GuardAudit {
        component: "mcp_tool",
        action,
        reason,
        tool: Some(tool),
        agent: Some("codex"),
        thread_source: metadata.and_then(|metadata| codex_thread_source(metadata)),
        session_id: metadata.and_then(|metadata| metadata_id(metadata, "session_id", "sessionId")),
        thread_id: metadata.and_then(|metadata| metadata_id(metadata, "thread_id", "threadId")),
        turn_id: metadata.and_then(|metadata| metadata_id(metadata, "turn_id", "turnId")),
    });
    (action == "suppressed")
        .then(|| CallToolResult::error(vec![ContentBlock::text(CODEX_BLOCKED_THREAD_MESSAGE)]))
}

// `ask` 工具的入参（MCP 入参 schema 由 schemars 从本结构派生）。结构体级注释用 `//` 以免泄漏进对外
// schema 的 description；字段级 `///` 才是给 agent 读的描述，须为英文。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AskParams {
    /// Shared context/description shown above all questions, rendered as Markdown
    /// (GitHub-flavored: headings, lists, code blocks, tables, links, etc.). When no
    /// `questions` are given, this text itself becomes the single question.
    #[serde(default)]
    pub message: Option<String>,
    /// One or more questions to ask the human.
    #[serde(default)]
    pub questions: Option<Vec<AskQuestion>>,
    /// Optional file paths (images or documents) to attach to what the human sees.
    #[serde(default)]
    pub files: Option<Vec<String>>,
    #[serde(default, rename = "__askhuman_session_token_v1")]
    #[schemars(skip)]
    session_token: Option<String>,
}

// Input for `todo_add` (project-scoped queue; same store as CLI `todo add`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodoAddParams {
    /// Concise task text to append to the current project's todo queue.
    /// Prefer a single executable sentence, ideally under 100 characters.
    pub text: String,
    /// When true, mark the todo for auto-run on the next `whats_next` (⚡).
    #[serde(default)]
    pub auto: Option<bool>,
}

// Output for `todo_add`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodoAddResult {
    /// 1-based index in the project's pending list after the add.
    pub index: usize,
    /// Stable id of the new entry (for debugging / future tools).
    pub id: String,
    /// Trimmed task text that was stored.
    pub text: String,
    /// Absolute project key (git root path) the todo was attached to.
    pub project: String,
    /// Whether the entry is marked auto-run.
    pub auto: bool,
}

// Input for `whats_next` (spec todo-whats-next D2).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WhatsNextParams {
    /// Optional completion report shown to the human above the fixed "What should we do
    /// next?" question, rendered as Markdown.
    #[serde(default)]
    pub message: Option<String>,
    /// Optional concrete next-task suggestions. NEVER include an end, stop, or no-more-work option;
    /// AskHuman adds the ending choice. Omit this field when you have no task to suggest. Options
    /// appear before project todos, and `recommended` renders the same emphasis as in `ask`.
    #[serde(default)]
    pub options: Option<Vec<AskOption>>,
    /// Optional file paths (e.g. a report or summary document) to attach to what the human sees.
    #[serde(default)]
    pub files: Option<Vec<String>>,
    #[serde(default, rename = "__askhuman_session_token_v1")]
    #[schemars(skip)]
    session_token: Option<String>,
}

// Publicly zero-argument input. Managed hooks may add the private field after model generation;
// it is deliberately absent from tools/list.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ShowLastParams {
    #[serde(default, rename = "__askhuman_session_token_v1")]
    #[schemars(skip)]
    session_token: Option<String>,
}

// 单个问题。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub struct AskQuestion {
    /// The question text.
    pub question: String,
    /// Optional predefined options the human may pick from.
    #[serde(default)]
    pub options: Option<Vec<AskOption>>,
}

// 单个选项。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub struct AskOption {
    /// The option label.
    pub text: String,
    /// Mark this option as recommended (rendered with emphasis).
    #[serde(default)]
    pub recommended: bool,
}

// `ask` 工具的出参（同时用于声明 output schema 与承载 `structuredContent`）。
//
// 字段名刻意与 `cli::output::render_json` 的 snake_case 输出保持一致，从而能直接反序列化子进程的
// JSON；对外刻意精简：
//   - **不含** `selected_indices`（脚本专用，反序列化时被 serde 自动忽略）；
//   - **不含** `channel`（MCP 客户端无需；子进程 JSON 里的 `channel` 作为未知字段被忽略）；
//   - `action` 仅在**取消**时出现（正常作答省略，见 `ask()` 里的归一化）。
// 结构体级注释用 `//`，避免泄漏进对外 schema。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskResult {
    /// Only present (value "cancel") when the human dismissed the request without answering;
    /// omitted on a normal answer (in which case `answers` carries the reply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Guidance present only when the human cancelled: they dismissed the request without
    /// answering, so you MUST ask again and keep asking until they give an explicit reply.
    /// Never treat a cancel as approval or as permission to proceed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// One entry per answered question (questions left blank are omitted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<AskAnswer>,
}

// 单题作答。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskAnswer {
    /// Zero-based index of the question this answer refers to.
    pub question_index: usize,
    /// Labels of the options the human selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_options: Vec<String>,
    /// Free text the human typed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input: Option<String>,
    /// Absolute paths of files the human attached (images and/or documents).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// MCP server：暴露 `ask`、`whats_next`、`show_last`、`todo_add`。
#[derive(Clone)]
pub struct AskServer {
    tool_router: ToolRouter<Self>,
    mcp_instance_id: String,
    project: String,
}

#[tool_router(router = tool_router)]
impl AskServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            mcp_instance_id: uuid::Uuid::new_v4().to_string(),
            project: crate::project::detect(),
        }
    }

    pub async fn register_instance(&self) {
        register_mcp_instance(
            self.mcp_instance_id.clone(),
            self.project.clone(),
            std::process::id(),
        )
        .await;
    }

    /// Ask the human a question (or several) and block until they reply.
    #[tool(
        name = "ask",
        description = "Ask the human operator one or more questions and wait (possibly for a long \
time) until they reply. Use this whenever you need a decision, clarification, review, approval, or \
any input that only the human can provide. Provide `message` for free-form questions, or \
`questions` for structured choices. Each `questions` item requires `question`; each nested \
`options` item requires `text` and may set `recommended` to true. Example: \
`{\"questions\":[{\"question\":\"Continue?\",\"options\":[{\"text\":\"Yes\",\"recommended\":true}]}]}`. \
The reply is returned as \
structured content; any images the human attaches are returned as image content.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<AskResult>(),
        // Truthful hints that also let Codex-style clients skip per-call approval
        // (their default treats missing destructive/open_world hints as true):
        // asking the operator destroys nothing and reaches no external world.
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn ask(
        &self,
        Parameters(params): Parameters<AskParams>,
        context: RequestContext<RoleServer>,
        // rmcp extracts the per-request token; cancelled when the client sends
        // `notifications/cancelled` (timeout / user stop / host abort). Not human dismiss.
        cancel: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        if let Some(blocked) = codex_thread_guard(&context.meta, "ask") {
            return Ok(blocked);
        }
        let has_questions = params
            .questions
            .as_ref()
            .map(|q| !q.is_empty())
            .unwrap_or(false);
        let has_message = params
            .message
            .as_ref()
            .map(|m| !m.trim().is_empty())
            .unwrap_or(false);
        if !has_questions && !has_message {
            return Err(McpError::invalid_params(
                "ask requires `message` or at least one entry in `questions`",
                None,
            ));
        }

        let public_arguments = ask_arguments_value(&params);
        let binding = self
            .resolve_binding(
                &context.meta,
                params.session_token.as_deref(),
                "ask",
                &public_arguments,
            )
            .await;
        let argv = build_argv(&params);
        let exe = std::env::current_exe().map_err(|e| {
            McpError::internal_error(format!("cannot locate AskHuman executable: {e}"), None)
        })?;

        // stdin 置空、stdout/stderr 捕获，确保子进程不碰 MCP 的 STDIO 协议流。
        // `ASKHUMAN_FROM_MCP=1`：告知子进程这是 MCP 发起，daemon 据此「只刷新、不新建」会话（防幽灵）。
        let mut command = tokio::process::Command::new(exe);
        command.args(&argv);
        self.configure_child(&mut command, binding.as_ref());
        let output = match capture_output(command, cancel).await {
            Ok(o) => o,
            Err(CaptureError::Cancelled) => {
                // Caller abort — not a human cancel. Do not invent answers or action:"cancel".
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "AskHuman request was cancelled by the MCP client (not by the human).",
                )]));
            }
            Err(CaptureError::Io(e)) => {
                return Err(McpError::internal_error(
                    format!("failed to spawn AskHuman: {e}"),
                    None,
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let code = output.status.code().unwrap_or(3);

        // 子进程对 answer/cancel 都会输出合法 JSON；解析失败一般意味着系统级错误（如连不上 daemon），
        // 以 is_error 结果回报，把 stderr 透传给模型，不让其误以为人类作答。
        let value: Value = match serde_json::from_str(stdout.trim()) {
            Ok(v) => v,
            Err(_) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let msg = if stderr.trim().is_empty() {
                    format!("AskHuman produced no result (exit code {code})")
                } else {
                    format!("AskHuman failed (exit code {code}): {}", stderr.trim())
                };
                return Ok(CallToolResult::error(vec![ContentBlock::text(msg)]));
            }
        };

        // 反序列化进 AskResult 会自动丢弃脚本专用的 `selected_indices` 与 `channel`（未知字段），
        // 再序列化即为对外的 structuredContent。
        let mut result: AskResult = serde_json::from_value(value).map_err(|e| {
            McpError::internal_error(format!("unexpected AskHuman output: {e}"), None)
        })?;
        // 正常作答不暴露 `action`（由 `answers` 表达）；仅取消时保留 `action:"cancel"` 作为信号。
        if result.action.as_deref() == Some("answer") {
            result.action = None;
        }
        let structured = serde_json::to_value(&result).map_err(|e| {
            McpError::internal_error(format!("failed to serialize ask result: {e}"), None)
        })?;

        // `structured()` 会把 structuredContent 同步序列化为 content[0] 的 JSON 文本，
        // 兼容尚不读 structuredContent 的客户端。
        let mut tool_result = CallToolResult::structured(structured);
        // 把人类附带的图片直接读回为 ImageContent（非图片文件仅以路径出现在 structuredContent 中）。
        for (path, mime) in image_files(&result) {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    tool_result
                        .content
                        .push(ContentBlock::image(b64, mime.to_string()));
                }
                // 读不到就跳过；路径仍在 structuredContent.answers[].files 中可供模型参考。
                Err(_) => continue,
            }
        }

        Ok(tool_result)
    }

    /// End-of-task handoff for requesting a separate next task.
    #[tool(
        name = "whats_next",
        description = "End-of-task handoff: ask the human for a separate next task. You MUST call \
this only after the current task is fully complete and before ending; use `ask` for any question, \
decision, or next step within the current task. Optionally pass `message` with a completion report \
and `files` with report documents. The human replies with the next task (start it immediately), or \
approves ending the turn — only then may you end it.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn whats_next(
        &self,
        Parameters(params): Parameters<WhatsNextParams>,
        context: RequestContext<RoleServer>,
        cancel: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        if let Some(blocked) = codex_thread_guard(&context.meta, "whats_next") {
            return Ok(blocked);
        }
        let public_arguments = whats_next_arguments_value(&params);
        let binding = self
            .resolve_binding(
                &context.meta,
                params.session_token.as_deref(),
                "whats_next",
                &public_arguments,
            )
            .await;
        let argv = build_whats_next_argv(&params);
        let exe = std::env::current_exe().map_err(|e| {
            McpError::internal_error(format!("cannot locate AskHuman executable: {e}"), None)
        })?;
        let mut command = tokio::process::Command::new(exe);
        command.args(&argv);
        self.configure_child(&mut command, binding.as_ref());
        let output = match capture_output(command, cancel).await {
            Ok(o) => o,
            Err(CaptureError::Cancelled) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "AskHuman request was cancelled by the MCP client (not by the human).",
                )]));
            }
            Err(CaptureError::Io(e)) => {
                return Err(McpError::internal_error(
                    format!("failed to spawn AskHuman: {e}"),
                    None,
                ));
            }
        };
        // 结果就是一段纯文本（spec D3）：任务内容 / 固定结束句 / `[status]` 取消引导。
        let stdout = String::from_utf8_lossy(&output.stdout);
        let text = stdout.trim();
        if !output.status.success() || text.is_empty() {
            let code = output.status.code().unwrap_or(3);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = if stderr.trim().is_empty() {
                format!("AskHuman produced no result (exit code {code})")
            } else {
                format!("AskHuman failed (exit code {code}): {}", stderr.trim())
            };
            return Ok(CallToolResult::error(vec![ContentBlock::text(msg)]));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(
            text.to_string(),
        )]))
    }

    /// Recover the complete latest AskHuman exchange for the current Agent session.
    #[tool(
        name = "show_last",
        description = "Retrieve the full latest completed AskHuman question and human answer for \
the current Agent session. Call this immediately after context summarization/compaction, or whenever \
you are unsure of the exact prior AskHuman exchange. Takes no public arguments.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn show_last(
        &self,
        Parameters(params): Parameters<ShowLastParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(blocked) = codex_thread_guard(&context.meta, "show_last") {
            return Ok(blocked);
        }
        let binding = self
            .resolve_binding(
                &context.meta,
                params.session_token.as_deref(),
                "show_last",
                &serde_json::json!({}),
            )
            .await;
        let scope = show_last_scope(binding, &self.mcp_instance_id, &self.project);
        match crate::show_last::recover(&scope) {
            Ok(output) => Ok(CallToolResult::success(vec![ContentBlock::text(output)])),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    /// Append a project todo (same queue as CLI `AskHuman todo add`).
    /// Writes from the MCP server process directly into `todos.json`.
    #[tool(
        name = "todo_add",
        description = "Add a project todo for the human to pick up later (whats-next chips / todos \
window / IM). Use only when the human asked to record a deferred task or accepted a concrete \
suggestion for later — never for your own work plan. Attaches to the project of the MCP server's \
cwd (git root). Returns the 1-based index and stored text on success.",
        output_schema = rmcp::handler::server::tool::schema_for_type::<TodoAddResult>(),
        // Appending a todo is additive (not destructive) and touches no external world.
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn todo_add(
        &self,
        Parameters(params): Parameters<TodoAddParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(blocked) = codex_thread_guard(&context.meta, "todo_add") {
            return Ok(blocked);
        }
        let text = params.text.trim();
        if text.is_empty() {
            return Err(McpError::invalid_params(
                "todo_add requires non-empty `text`",
                None,
            ));
        }
        let project = crate::project::detect();
        if project.is_empty() {
            return Err(McpError::internal_error(
                "cannot determine project (cwd unavailable)",
                None,
            ));
        }
        let auto = params.auto.unwrap_or(false);
        let agent = crate::agents::detect::detect_invoking_agent();
        let added = match agent {
            Some(agent) => crate::todos::add_from_agent(&project, text, auto, agent),
            None if auto => crate::todos::add_auto(&project, text),
            None => crate::todos::add(&project, text),
        };
        let entry = match added {
            Ok(entry) => entry,
            Err(crate::todos::AddError::EmptyInput) => {
                return Err(McpError::invalid_params(
                    "todo_add requires non-empty `text`",
                    None,
                ));
            }
            Err(crate::todos::AddError::Persist) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Failed to save todo: could not write ~/.askhuman/state/todos.json \
(check permissions).",
                )]));
            }
        };
        let Some(index) = crate::todos::index_of(&project, &entry.id) else {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "Failed to save todo: entry missing after write (not persisted).",
            )]));
        };
        let result = TodoAddResult {
            index,
            id: entry.id,
            text: entry.text,
            project,
            auto: entry.auto,
        };
        let structured = serde_json::to_value(&result).map_err(|e| {
            McpError::internal_error(format!("failed to serialize todo_add result: {e}"), None)
        })?;
        // `structured()` mirrors structuredContent into content[0] as JSON text.
        Ok(CallToolResult::structured(structured))
    }

    fn configure_child(
        &self,
        command: &mut tokio::process::Command,
        binding: Option<&crate::context_binding::AgentBinding>,
    ) {
        // Native session env inherited when this long-lived MCP process started may belong to an
        // older conversation. Only explicit per-call binding is authoritative.
        for kind in [
            crate::agents::AgentKind::Claude,
            crate::agents::AgentKind::Codex,
            crate::agents::AgentKind::Cursor,
            crate::agents::AgentKind::Grok,
        ] {
            command.env_remove(crate::agents::detect::session_id_env_var(kind));
        }
        command
            .env(crate::cli::FROM_MCP_ENV, "1")
            .env(
                crate::cli::MCP_INSTANCE_ID_ENV,
                self.mcp_instance_id.as_str(),
            )
            .env_remove(crate::cli::MCP_AGENT_KIND_ENV)
            .env_remove(crate::cli::MCP_AGENT_SESSION_ID_ENV);
        if let Some(binding) = binding {
            command
                .env(crate::cli::MCP_AGENT_KIND_ENV, &binding.agent_kind)
                .env(crate::cli::MCP_AGENT_SESSION_ID_ENV, &binding.session_id);
        }
    }

    async fn resolve_binding(
        &self,
        meta: &Meta,
        token: Option<&str>,
        tool_name: &str,
        public_arguments: &Value,
    ) -> Option<crate::context_binding::AgentBinding> {
        if let Some(binding) = resolve_direct_binding(meta, token, tool_name) {
            return Some(binding);
        }
        let session_id = claim_grok_binding(
            self.mcp_instance_id.clone(),
            self.project.clone(),
            tool_name.to_string(),
            crate::context_binding::canonical_tool_arguments_sha256(tool_name, public_arguments)?,
            std::process::id(),
        )
        .await?;
        Some(crate::context_binding::AgentBinding {
            agent_kind: "grok".into(),
            session_id,
        })
    }
}

fn show_last_scope(
    binding: Option<crate::context_binding::AgentBinding>,
    mcp_instance_id: &str,
    project: &str,
) -> crate::show_last::Scope {
    match binding {
        Some(binding) => crate::show_last::Scope::AgentSession {
            agent_kind: binding.agent_kind,
            session_id: binding.session_id,
        },
        None => crate::show_last::Scope::McpInstance {
            mcp_instance_id: mcp_instance_id.to_string(),
            project: project.to_string(),
        },
    }
}

fn resolve_direct_binding(
    meta: &Meta,
    token: Option<&str>,
    tool_name: &str,
) -> Option<crate::context_binding::AgentBinding> {
    // Codex overwrites this field with the current thread id on every tools/call.
    if let Some(session_id) = meta
        .0
        .get("threadId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
    {
        return Some(crate::context_binding::AgentBinding {
            agent_kind: "codex".into(),
            session_id: session_id.to_string(),
        });
    }
    token.and_then(|token| crate::context_binding::consume_token(token, tool_name))
}

pub(crate) fn ask_arguments_value(params: &AskParams) -> Value {
    let mut object = serde_json::Map::new();
    if let Some(value) = &params.message {
        object.insert("message".into(), Value::String(value.clone()));
    }
    if let Some(value) = &params.questions {
        object.insert(
            "questions".into(),
            serde_json::to_value(value).expect("AskQuestion is serializable"),
        );
    }
    if let Some(value) = &params.files {
        object.insert(
            "files".into(),
            serde_json::to_value(value).expect("file paths are serializable"),
        );
    }
    Value::Object(object)
}

pub(crate) fn whats_next_arguments_value(params: &WhatsNextParams) -> Value {
    let mut object = serde_json::Map::new();
    if let Some(value) = &params.message {
        object.insert("message".into(), Value::String(value.clone()));
    }
    if let Some(value) = &params.options {
        object.insert(
            "options".into(),
            serde_json::to_value(value).expect("AskOption is serializable"),
        );
    }
    if let Some(value) = &params.files {
        object.insert(
            "files".into(),
            serde_json::to_value(value).expect("file paths are serializable"),
        );
    }
    Value::Object(object)
}

#[cfg(unix)]
async fn register_mcp_instance(mcp_instance_id: String, project: String, server_pid: u32) {
    let parent_pid_hint = Some(unsafe { libc::getppid() } as u32);
    crate::client::register_mcp_instance(mcp_instance_id, project, server_pid, parent_pid_hint)
        .await;
}

#[cfg(not(unix))]
async fn register_mcp_instance(_mcp_instance_id: String, _project: String, _server_pid: u32) {}

#[cfg(unix)]
async fn claim_grok_binding(
    mcp_instance_id: String,
    project: String,
    tool_name: String,
    arguments_sha256: String,
    server_pid: u32,
) -> Option<String> {
    crate::client::claim_grok_binding(
        mcp_instance_id,
        project,
        tool_name,
        arguments_sha256,
        server_pid,
    )
    .await
}

#[cfg(not(unix))]
async fn claim_grok_binding(
    _mcp_instance_id: String,
    _project: String,
    _tool_name: String,
    _arguments_sha256: String,
    _server_pid: u32,
) -> Option<String> {
    None
}

/// Errors from [`capture_output`].
#[derive(Debug)]
enum CaptureError {
    /// MCP client cancelled the in-flight `tools/call` (`notifications/cancelled`).
    Cancelled,
    Io(std::io::Error),
}

/// 捕获子进程输出，并把 **rmcp request CancellationToken** 与 **future drop** 都传播为子进程终止。
///
/// - Token 取消（`notifications/cancelled`）：abort 持有 `Child` 的 task → `kill_on_drop` 杀进程 →
///   CLI socket EOF → daemon `wait_cli_eof` 取消 popup / IM。rmcp **不会** drop handler，故必须
///   在此 `select!` token（不能假设 future 被 drop）。
/// - Future drop（测试 abort / 极端宿主杀任务）：同一 `kill_on_drop(true)` 兜底。
///
/// 子进程放进独立 task，是因为 `wait_with_output` 会 move `Child`，无法在 `select!` 另一臂再 `kill`。
async fn capture_output(
    mut command: tokio::process::Command,
    cancel: CancellationToken,
) -> Result<Output, CaptureError> {
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(CaptureError::Io)?;
    // AbortOnDropHandle: if this future is dropped without resolving (handler abort), the
    // child task is aborted → Child drop → kill_on_drop. Plain JoinHandle would detach.
    // Wrapped in Option so the cancel arm can take/drop it without fighting select! moves.
    let mut output_task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
        child.wait_with_output().await
    })));

    tokio::select! {
        // Prefer cancel: if the client already abandoned the call, tear down even if the child
        // is about to exit with an orphaned answer nobody will read.
        biased;
        _ = cancel.cancelled() => {
            // Drop aborts the wait task → Child::drop → kill_on_drop → CLI socket EOF.
            drop(output_task.take());
            Err(CaptureError::Cancelled)
        }
        result = output_task.as_mut().unwrap() => {
            // Task finished (or aborted externally); disarm so Drop does not abort again.
            let _ = output_task.take();
            match result {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(e)) => Err(CaptureError::Io(e)),
                Err(e) if e.is_cancelled() => Err(CaptureError::Cancelled),
                Err(e) => Err(CaptureError::Io(std::io::Error::other(e.to_string()))),
            }
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AskServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
            "AskHuman bridges the agent and a human operator. Call the `ask` tool whenever you \
need the human to decide, clarify, review, or approve something; it blocks until they reply. \
Call the `whats_next` tool after completing the current task and before ending your turn to ask \
the human what to do next. Call `show_last` after context summarization when exact prior AskHuman \
details may be missing. Call the `todo_add` tool when the human asks to record a deferred \
project todo.",
        );
        // `from_build_env()` 的名字/版本来自 rmcp crate 自身，改成本应用的品牌名与版本。
        let mut implementation = Implementation::from_build_env();
        implementation.name = "AskHuman".to_string();
        implementation.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = implementation;
        info
    }
}

/// 把 [`AskParams`] 翻译成 `AskHuman` 的 argv（不含程序名），末尾固定追加 `--output json`。
///
/// 纯函数，便于单测。注意：`message` 必须作为**首个**位置参数（CLI 只接受一个位置参数，且需在所有
/// `-q` 之前）。
fn build_argv(params: &AskParams) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();

    if let Some(message) = params.message.as_ref() {
        if !message.trim().is_empty() {
            argv.push(message.clone());
        }
    }

    if let Some(questions) = params.questions.as_ref() {
        for q in questions {
            argv.push("-q".to_string());
            argv.push(q.question.clone());
            if let Some(options) = q.options.as_ref() {
                for opt in options {
                    argv.push(if opt.recommended { "-o!" } else { "-o" }.to_string());
                    argv.push(opt.text.clone());
                }
            }
        }
    }

    if let Some(files) = params.files.as_ref() {
        for f in files {
            argv.push("-f".to_string());
            argv.push(f.clone());
        }
    }

    argv.push("--output".to_string());
    argv.push("json".to_string());
    argv
}

/// 把 [`WhatsNextParams`] 翻译成 `AskHuman --whats-next` 的 argv（纯函数，便于单测）。
/// 输出保持文本模式（结果本身就是一段纯文本，spec D3），不追加 `--output json`。
fn build_whats_next_argv(params: &WhatsNextParams) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if let Some(message) = params.message.as_ref() {
        if !message.trim().is_empty() {
            argv.push(message.clone());
        }
    }
    argv.push("--whats-next".to_string());
    if let Some(options) = params.options.as_ref() {
        for option in options {
            argv.push(if option.recommended { "-o!" } else { "-o" }.to_string());
            argv.push(option.text.clone());
        }
    }
    if let Some(files) = params.files.as_ref() {
        for f in files {
            argv.push("-f".to_string());
            argv.push(f.clone());
        }
    }
    argv
}

/// 从结果中挑出「可作为 MCP 图片直接返回」的文件，返回 (路径, MIME) 列表。
fn image_files(result: &AskResult) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    for ans in &result.answers {
        for f in &ans.files {
            let path = PathBuf::from(f);
            if let Some(mime) = image_mime(&path) {
                out.push((path, mime));
            }
        }
    }
    out
}

/// 按扩展名判断图片 MIME；非图片返回 `None`。
fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServiceExt;
    use serde_json::json;
    use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

    use std::time::Duration;

    fn contains_ref(value: &Value) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key("$ref") || object.values().any(contains_ref)
            }
            Value::Array(items) => items.iter().any(contains_ref),
            _ => false,
        }
    }

    fn params(json: Value) -> AskParams {
        serde_json::from_value(json).unwrap()
    }

    fn meta(json: Value) -> Meta {
        Meta(json.as_object().unwrap().clone())
    }

    fn codex_meta(turn_metadata: Value) -> Meta {
        meta(json!({ CODEX_TURN_METADATA_KEY: turn_metadata }))
    }

    async fn send_json(writer: &mut (impl AsyncWrite + Unpin), value: Value) {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    async fn read_response(reader: &mut (impl AsyncBufRead + Unpin), id: i64) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut line = String::new();
                let bytes = reader.read_line(&mut line).await.unwrap();
                assert!(bytes > 0, "MCP stream closed before response {id}");
                let value: Value = serde_json::from_str(&line).unwrap();
                if value.get("id") == Some(&json!(id)) {
                    break value;
                }
            }
        })
        .await
        .expect("MCP response timeout")
    }

    fn guard_decision(meta: &Meta) -> Option<(&'static str, &'static str)> {
        codex_guard_action_reason(meta)
    }

    #[test]
    fn codex_blocked_thread_detection_is_namespaced_exact_and_compatible() {
        for blocked in [
            codex_meta(json!({ "thread_source": "system" })),
            codex_meta(json!(r#"{"thread_source":"system"}"#)),
            codex_meta(json!({ "thread_source": "ambient_suggestions" })),
            codex_meta(json!(r#"{"thread_source":"ambient_suggestions"}"#)),
        ] {
            assert_eq!(
                guard_decision(&blocked),
                Some(("suppressed", "codex_blocked_thread_source")),
                "{blocked:?}"
            );
        }

        for passed in [
            codex_meta(json!({ "thread_source": "user" })),
            codex_meta(json!({ "thread_source": "automation" })),
            codex_meta(json!({ "thread_source": "conversational_onboarding" })),
            codex_meta(json!({ "thread_source": "pull_request_fix_automation" })),
            codex_meta(json!({ "thread_source": "System" })),
        ] {
            assert_eq!(
                guard_decision(&passed),
                Some(("passed", "codex_thread_source_passed")),
                "{passed:?}"
            );
        }

        assert_eq!(
            guard_decision(&codex_meta(json!({}))),
            Some(("passed", "codex_thread_source_missing"))
        );
        assert_eq!(
            guard_decision(&codex_meta(json!("not json"))),
            Some(("passed", "codex_turn_metadata_unreadable"))
        );

        // No Codex namespace at all: not guarded, not audited.
        assert_eq!(
            guard_decision(&meta(json!({ "thread_source": "system" }))),
            None
        );
        assert_eq!(guard_decision(&Meta::default()), None);
    }

    #[tokio::test]
    async fn blocked_thread_metadata_blocks_all_tools_through_rmcp_routing() -> anyhow::Result<()> {
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            AskServer::new()
                .serve(server_transport)
                .await?
                .waiting()
                .await?;
            anyhow::Ok(())
        });
        let (read, mut write) = tokio::io::split(client_transport);
        let mut reader = BufReader::new(read);

        send_json(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "askhuman-test", "version": "0" }
                }
            }),
        )
        .await;
        let initialized = read_response(&mut reader, 1).await;
        assert!(initialized.get("result").is_some());
        send_json(
            &mut write,
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;

        for (offset, (name, thread_source, arguments)) in [
            ("ask", "system", json!({ "message": "must not spawn" })),
            ("whats_next", "ambient_suggestions", json!({})),
            ("show_last", "system", json!({})),
            // Whitespace is valid at schema decoding time but would fail handler validation.
            // Receiving the blocked-thread result proves the guard ran before any todo write.
            ("todo_add", "ambient_suggestions", json!({ "text": " " })),
        ]
        .into_iter()
        .enumerate()
        {
            let id = offset as i64 + 2;
            send_json(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": {
                        "_meta": {
                            CODEX_TURN_METADATA_KEY: { "thread_source": thread_source }
                        },
                        "name": name,
                        "arguments": arguments
                    }
                }),
            )
            .await;
            let response = read_response(&mut reader, id).await;
            assert_eq!(
                response.pointer("/result/isError"),
                Some(&json!(true)),
                "{name}"
            );
            assert_eq!(
                response.pointer("/result/content/0/text"),
                Some(&json!(CODEX_BLOCKED_THREAD_MESSAGE)),
                "{name}"
            );
        }

        for (id, meta) in [
            (
                5,
                Some(json!({
                    CODEX_TURN_METADATA_KEY: { "thread_source": "user" }
                })),
            ),
            (6, None),
        ] {
            let mut params = json!({ "name": "ask", "arguments": {} });
            if let Some(meta) = meta {
                params
                    .as_object_mut()
                    .expect("tools/call params")
                    .insert("_meta".into(), meta);
            }
            send_json(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": params
                }),
            )
            .await;
            let response = read_response(&mut reader, id).await;
            assert_eq!(response.pointer("/error/code"), Some(&json!(-32602)));
            assert_ne!(
                response.pointer("/error/message"),
                Some(&json!(CODEX_BLOCKED_THREAD_MESSAGE))
            );
        }

        drop(write);
        drop(reader);
        server_task.await??;
        Ok(())
    }

    #[cfg(unix)]
    async fn wait_for_pid_file(pid_file: &std::path::Path) -> i32 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(pid_file) {
                    if let Ok(pid) = text.trim().parse::<i32>() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child should publish its pid")
    }

    #[cfg(unix)]
    async fn wait_until_dead(pid: i32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                // kill(pid, 0) == -1 with ESRCH means the child no longer exists.
                let alive = unsafe { libc::kill(pid, 0) } == 0;
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child process must exit")
    }

    /// rmcp 真实取消路径：cancel token（对应 `notifications/cancelled`）→ 杀子进程。
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_token_kills_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 60", pid_file.display());
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &script]);

        let cancel = CancellationToken::new();
        let task = tokio::spawn(capture_output(command, cancel.clone()));
        let pid = wait_for_pid_file(&pid_file).await;

        cancel.cancel();
        let result = task.await.expect("capture_output task");
        assert!(
            matches!(result, Err(CaptureError::Cancelled)),
            "token cancel must yield CaptureError::Cancelled, got {result:?}"
        );

        wait_until_dead(pid).await;
    }

    /// 兜底：abort/drop future 时 kill_on_drop 仍杀子进程（宿主强杀任务等）。
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_output_future_kills_child() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 60", pid_file.display());
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &script]);

        let task = tokio::spawn(capture_output(command, CancellationToken::new()));
        let pid = wait_for_pid_file(&pid_file).await;

        task.abort();
        let _ = task.await;

        wait_until_dead(pid).await;
    }

    #[test]
    fn argv_message_only_becomes_question() {
        let p = params(json!({ "message": "Continue?" }));
        assert_eq!(build_argv(&p), vec!["Continue?", "--output", "json"]);
    }

    #[test]
    fn argv_full_with_recommended_and_files() {
        let p = params(json!({
            "message": "Pick an env",
            "questions": [{
                "question": "Which environment?",
                "options": [
                    { "text": "production", "recommended": true },
                    { "text": "staging" }
                ]
            }],
            "files": ["/tmp/a.png"]
        }));
        assert_eq!(
            build_argv(&p),
            vec![
                "Pick an env",
                "-q",
                "Which environment?",
                "-o!",
                "production",
                "-o",
                "staging",
                "-f",
                "/tmp/a.png",
                "--output",
                "json",
            ]
        );
    }

    #[test]
    fn whats_next_argv_message_stays_message() {
        // Message 是完成报告，不追加 --output json（结果为纯文本，spec D3）。
        let p: WhatsNextParams =
            serde_json::from_value(json!({ "message": "All tests pass." })).unwrap();
        assert_eq!(
            build_whats_next_argv(&p),
            vec!["All tests pass.", "--whats-next"]
        );
    }

    #[test]
    fn whats_next_argv_includes_suggested_options() {
        let p: WhatsNextParams = serde_json::from_value(json!({
            "message": "All tests pass.",
            "options": [
                { "text": "Write docs" },
                { "text": "Add tests", "recommended": true }
            ]
        }))
        .unwrap();
        assert_eq!(
            build_whats_next_argv(&p),
            vec![
                "All tests pass.",
                "--whats-next",
                "-o",
                "Write docs",
                "-o!",
                "Add tests",
            ]
        );
    }

    #[test]
    fn whats_next_argv_bare_and_with_files() {
        let p: WhatsNextParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(build_whats_next_argv(&p), vec!["--whats-next"]);
        let p: WhatsNextParams =
            serde_json::from_value(json!({ "files": ["/tmp/report.md"] })).unwrap();
        assert_eq!(
            build_whats_next_argv(&p),
            vec!["--whats-next", "-f", "/tmp/report.md"]
        );
    }

    #[test]
    fn whats_next_tool_is_registered() {
        let server = AskServer::new();
        assert!(server.tool_router.get("whats_next").is_some());
    }

    #[test]
    fn show_last_is_registered_with_zero_public_fields() {
        let server = AskServer::new();
        let tool = server.tool_router.get("show_last").unwrap();
        let schema = Value::Object((*tool.input_schema).clone());
        assert!(schema
            .pointer("/properties/__askhuman_session_token_v1")
            .is_none());
        assert!(tool
            .description
            .as_deref()
            .unwrap_or("")
            .contains("context summarization/compaction"));
    }

    #[test]
    fn server_instances_and_show_last_scopes_are_strictly_partitioned() {
        let first = AskServer::new();
        let second = AskServer::new();
        assert!(uuid::Uuid::parse_str(&first.mcp_instance_id).is_ok());
        assert!(uuid::Uuid::parse_str(&second.mcp_instance_id).is_ok());
        assert_ne!(first.mcp_instance_id, second.mcp_instance_id);

        assert!(matches!(
            show_last_scope(None, &first.mcp_instance_id, "/p"),
            crate::show_last::Scope::McpInstance { mcp_instance_id, project }
                if mcp_instance_id == first.mcp_instance_id && project == "/p"
        ));
        assert!(matches!(
            show_last_scope(
                Some(crate::context_binding::AgentBinding {
                    agent_kind: "cursor".into(),
                    session_id: "conversation".into(),
                }),
                &first.mcp_instance_id,
                "/p",
            ),
            crate::show_last::Scope::AgentSession { agent_kind, session_id }
                if agent_kind == "cursor" && session_id == "conversation"
        ));
    }

    #[test]
    fn public_argument_hash_omits_hidden_token_and_preserves_absence() {
        let server = AskServer::new();
        for tool_name in ["ask", "whats_next", "show_last"] {
            let tool = server.tool_router.get(tool_name).unwrap();
            let schema = Value::Object((*tool.input_schema).clone());
            assert!(schema
                .pointer("/properties/__askhuman_session_token_v1")
                .is_none());
        }
        let params: AskParams = serde_json::from_value(json!({
            "message": "hello",
            "__askhuman_session_token_v1": "secret"
        }))
        .unwrap();
        assert_eq!(ask_arguments_value(&params), json!({"message": "hello"}));
        let params: WhatsNextParams = serde_json::from_value(json!({
            "message": "done",
            "__askhuman_session_token_v1": "secret"
        }))
        .unwrap();
        assert_eq!(
            whats_next_arguments_value(&params),
            json!({"message": "done"})
        );
        assert_eq!(params.session_token.as_deref(), Some("secret"));
        let params: ShowLastParams = serde_json::from_value(json!({
            "__askhuman_session_token_v1": "secret"
        }))
        .unwrap();
        assert_eq!(params.session_token.as_deref(), Some("secret"));
    }

    #[test]
    fn codex_binding_accepts_only_uuid_thread_ids() {
        let valid = "5c2bfe55-f587-4f0f-9671-f02d5259f2e1";
        assert_eq!(
            resolve_direct_binding(&meta(json!({"threadId": valid})), None, "show_last"),
            Some(crate::context_binding::AgentBinding {
                agent_kind: "codex".into(),
                session_id: valid.into(),
            })
        );
        assert!(resolve_direct_binding(
            &meta(json!({"threadId": "not-a-codex-thread"})),
            None,
            "show_last"
        )
        .is_none());
        assert_eq!(
            resolve_direct_binding(
                &meta(json!({"threadId": format!("  {valid}  ")})),
                Some("invalid-token"),
                "ask"
            ),
            Some(crate::context_binding::AgentBinding {
                agent_kind: "codex".into(),
                session_id: valid.into(),
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_environment_removes_stale_native_sessions_and_sets_only_binding() {
        async fn configured_env(
            server: &AskServer,
            binding: Option<&crate::context_binding::AgentBinding>,
        ) -> String {
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", "env"]);
            for kind in [
                crate::agents::AgentKind::Claude,
                crate::agents::AgentKind::Codex,
                crate::agents::AgentKind::Cursor,
                crate::agents::AgentKind::Grok,
            ] {
                command.env(crate::agents::detect::session_id_env_var(kind), "stale");
            }
            command
                .env(crate::cli::MCP_AGENT_KIND_ENV, "stale-kind")
                .env(crate::cli::MCP_AGENT_SESSION_ID_ENV, "stale-session");
            server.configure_child(&mut command, binding);
            String::from_utf8(command.output().await.unwrap().stdout).unwrap()
        }

        let server = AskServer::new();
        let binding = crate::context_binding::AgentBinding {
            agent_kind: "claude".into(),
            session_id: "current-session".into(),
        };
        let bound = configured_env(&server, Some(&binding)).await;
        assert!(bound.contains(&format!("{}=1\n", crate::cli::FROM_MCP_ENV)));
        assert!(bound.contains(&format!(
            "{}={}\n",
            crate::cli::MCP_INSTANCE_ID_ENV,
            server.mcp_instance_id
        )));
        assert!(bound.contains(&format!("{}=claude\n", crate::cli::MCP_AGENT_KIND_ENV)));
        assert!(bound.contains(&format!(
            "{}=current-session\n",
            crate::cli::MCP_AGENT_SESSION_ID_ENV
        )));
        for kind in [
            crate::agents::AgentKind::Claude,
            crate::agents::AgentKind::Codex,
            crate::agents::AgentKind::Cursor,
            crate::agents::AgentKind::Grok,
        ] {
            assert!(!bound.contains(&format!(
                "{}=stale",
                crate::agents::detect::session_id_env_var(kind)
            )));
        }

        let unbound = configured_env(&server, None).await;
        assert!(!unbound.contains(&format!("{}=", crate::cli::MCP_AGENT_KIND_ENV)));
        assert!(!unbound.contains(&format!("{}=", crate::cli::MCP_AGENT_SESSION_ID_ENV)));
    }

    #[test]
    fn todo_add_tool_is_registered() {
        let server = AskServer::new();
        assert!(server.tool_router.get("todo_add").is_some());
        let tool = server.tool_router.get("todo_add").unwrap();
        let schema = Value::Object((*tool.input_schema).clone());
        assert_eq!(
            schema.pointer("/properties/text/type"),
            Some(&json!("string"))
        );
        // `Option<bool>` → JSON Schema `["boolean","null"]` (or boolean depending on rmcp/schemars).
        let auto_type = schema.pointer("/properties/auto/type").cloned();
        assert!(
            auto_type == Some(json!("boolean")) || auto_type == Some(json!(["boolean", "null"])),
            "unexpected auto type: {auto_type:?}"
        );
        assert!(tool
            .description
            .as_deref()
            .unwrap_or("")
            .contains("never for your own work plan"));
    }

    #[test]
    fn whats_next_schema_exposes_inline_suggested_options() {
        let server = AskServer::new();
        let tool = server.tool_router.get("whats_next").unwrap();
        let description = tool.description.as_deref().unwrap();
        assert!(description.contains("End-of-task handoff"));
        assert!(description.contains("use `ask` for any question"));
        let schema = Value::Object((*tool.input_schema).clone());
        let options_description = schema
            .pointer("/properties/options/description")
            .and_then(Value::as_str)
            .unwrap();
        assert!(options_description.contains("NEVER include an end, stop, or no-more-work option"));
        assert!(options_description.contains("AskHuman adds the ending choice"));
        assert_eq!(
            schema.pointer("/properties/options/items/type"),
            Some(&json!("object"))
        );
        assert_eq!(
            schema.pointer("/properties/options/items/properties/text/type"),
            Some(&json!("string"))
        );
        assert_eq!(
            schema.pointer("/properties/options/items/properties/recommended/type"),
            Some(&json!("boolean"))
        );
    }

    #[test]
    fn ask_tool_schema_inlines_question_and_option_fields() {
        let server = AskServer::new();
        let tool = server.tool_router.get("ask").unwrap();
        let schema = Value::Object((*tool.input_schema).clone());

        assert!(
            !contains_ref(&schema),
            "ask input schema must not expose $ref"
        );
        assert!(schema.get("$defs").is_none());
        assert_eq!(
            schema.pointer("/properties/questions/items/type"),
            Some(&json!("object"))
        );
        assert_eq!(
            schema.pointer("/properties/questions/items/properties/question/type"),
            Some(&json!("string"))
        );
        assert_eq!(
            schema.pointer("/properties/questions/items/required"),
            Some(&json!(["question"]))
        );
        assert_eq!(
            schema.pointer("/properties/questions/items/properties/options/items/type"),
            Some(&json!("object"))
        );
        assert_eq!(
            schema.pointer(
                "/properties/questions/items/properties/options/items/properties/text/type"
            ),
            Some(&json!("string"))
        );
        assert_eq!(
            schema.pointer("/properties/questions/items/properties/options/items/required"),
            Some(&json!(["text"]))
        );
    }

    /// 模拟 `ask()` 对子进程 JSON 的归一化：反序列化 + 正常作答清空 `action`。
    fn normalize(child: Value) -> Value {
        let mut result: AskResult = serde_json::from_value(child).unwrap();
        if result.action.as_deref() == Some("answer") {
            result.action = None;
        }
        serde_json::to_value(&result).unwrap()
    }

    #[test]
    fn result_answer_drops_channel_action_and_selected_indices() {
        // 模拟 render_json 的输出形态（含脚本专用 selected_indices + channel）。
        let out = normalize(json!({
            "action": "answer",
            "channel": "popup",
            "answers": [{
                "question_index": 0,
                "selected_options": ["production"],
                "selected_indices": [1],
                "user_input": "go",
                "files": ["/tmp/a.png"]
            }]
        }));
        assert_eq!(out["answers"][0]["question_index"], 0);
        assert_eq!(out["answers"][0]["selected_options"][0], "production");
        assert_eq!(out["answers"][0]["user_input"], "go");
        // 对外精简：正常作答不带 action，且从不带 channel；selected_indices 永远剔除。
        assert!(out.get("action").is_none());
        assert!(out.get("channel").is_none());
        assert!(out["answers"][0].get("selected_indices").is_none());
    }

    #[test]
    fn result_cancel_keeps_action_drops_channel() {
        let out = normalize(json!({ "action": "cancel", "channel": "popup" }));
        assert_eq!(out["action"], "cancel");
        assert!(out.get("channel").is_none());
        assert!(out.get("answers").is_none());
    }

    #[test]
    fn result_cancel_passes_through_status() {
        // 子进程 render_json 取消时带 status 引导；薄壳应原样透传到 structuredContent。
        let out = normalize(json!({
            "action": "cancel",
            "channel": "popup",
            "status": "The human cancelled. You must ask again."
        }));
        assert_eq!(out["status"], "The human cancelled. You must ask again.");
    }

    #[test]
    fn result_answer_omits_status() {
        let out = normalize(json!({ "action": "answer", "channel": "popup" }));
        assert!(out.get("status").is_none());
    }

    #[test]
    fn image_files_filters_by_extension() {
        let result = AskResult {
            action: None,
            status: None,
            answers: vec![AskAnswer {
                question_index: 0,
                selected_options: vec![],
                user_input: None,
                files: vec![
                    "/tmp/a.PNG".into(),
                    "/tmp/notes.md".into(),
                    "/tmp/b.jpeg".into(),
                ],
            }],
        };
        let imgs = image_files(&result);
        assert_eq!(imgs.len(), 2);
        assert_eq!(imgs[0].1, "image/png");
        assert_eq!(imgs[1].1, "image/jpeg");
    }
}
