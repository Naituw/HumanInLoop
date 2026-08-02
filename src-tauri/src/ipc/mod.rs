//! 进程间通信（IPC）：CLI / GUI Helper ↔ 常驻 Daemon。
//!
//! 传输：NDJSON（一行一个 JSON 消息）over Unix domain socket（mac/Linux）/ Windows named pipe。
//! 本文件定义协议消息类型；编解码见 `codec`，传输（socket 路径/连接/监听）见 `transport`。
//!
//! Phase 0 仅含握手与 daemon 控制（status/stop）；任务提交（submit/show/...）在后续 Phase 引入。

pub mod codec;
pub mod transport;

pub use codec::{read_msg, write_msg};

use crate::daemon::lifecycle::Fingerprint;
use crate::models::{
    ChannelAction, ConfirmFallbackReason, ConfirmResult, ConfirmSpec, FileAttachment,
    InteractionRequest, MessagePrompt, OutputFormat, Question, QuestionAnswer,
};
use serde::{Deserialize, Serialize};

/// IPC 协议版本：不兼容变更时 +1，握手不一致即触发换新。
pub const PROTOCOL_VERSION: u32 = 2;

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// CLI/GUI 连接时的握手信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub protocol_version: u32,
    pub client_version: String,
    pub binary_path: String,
    pub fingerprint: Fingerprint,
    pub pid: u32,
}

/// 握手结果状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HelloStatus {
    /// 正常，可继续。
    Ok,
    /// Daemon 已过时（二进制指纹/协议变化），将自行退出；客户端应等其下线后用新二进制拉起。
    Restarting,
    /// Daemon 正在排空（graceful drain）：在途请求完结后退出；排空期拒绝新提问。
    /// 客户端应等其下线后用新二进制拉起再提交。
    Draining,
}

/// 对 `ClientHello` 的回应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloAck {
    pub protocol_version: u32,
    pub daemon_version: String,
    pub status: HelloStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `daemon status` 返回的运行信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusInfo {
    pub pid: u32,
    pub version: String,
    pub protocol_version: u32,
    pub uptime_secs: u64,
    pub socket: String,
    pub active_requests: usize,
    /// 当前常热的 IM 长连接（"dingtalk" / "feishu" / "telegram" / "slack"），按已建连且存活计入。
    #[serde(default)]
    pub im_connections: Vec<String>,
    /// 是否处于排空状态（旧 Daemon 回包缺字段 → false）。
    #[serde(default)]
    pub draining: bool,
    /// 各渠道最近一次未恢复的故障（R7 渠道故障可见化；旧 Daemon 回包缺字段 → 空）。
    #[serde(default)]
    pub channel_issues: Vec<ChannelIssueInfo>,
}

/// 一条渠道故障摘要（R7）：渠道 id + 错误文案 + 首次出现时间。该渠道下一次成功操作即从
/// daemon 的健康表清除，故出现在快照里即表示「仍未恢复」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelIssueInfo {
    /// 渠道 id："telegram" / "dingding" / "feishu" / "slack"。
    pub channel: String,
    /// 错误文案（源语言英文，与 daemon.log 一致）。
    pub message: String,
    /// 首次出现的 Unix 毫秒时间戳。
    pub at_ms: u64,
}

/// CLI 提交的一次提问任务（A11：`-f` 已在 CLI 解析为绝对路径；硬性上送 source name 与解析好的 lang；
/// `request_id` 由 Daemon 分配，故此处不含 id）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRequest {
    /// 共享 Message：描述文本与展示附件（绝对路径）。
    pub message: MessagePrompt,
    /// 问题列表（CLI 已归一化，恒 ≥1）。
    pub questions: Vec<Question>,
    /// 是否按 Markdown 渲染（全局）。
    pub is_markdown: bool,
    /// Per-request caller source (custom environment name, detected Agent label, or `the Loop`).
    pub source: String,
    /// CLI 解析好的界面语言（"en" / "zh"），使 `auto` 跟随调用方而非 Daemon。
    pub lang: String,
    /// 当前项目 key（CLI 计算：向上找 .git 根、回退 cwd），用于回复历史归类。
    /// 旧 CLI 不带此字段 → 默认空串（归入「未知项目」）。
    #[serde(default)]
    pub project: String,
    /// 严格选择：禁用自由文本 / 回复附件，只能勾选预设项（全局）。
    #[serde(default)]
    pub select_only: bool,
    /// 单选：每题恰好一个选择（默认多选，全局）。
    #[serde(default)]
    pub single: bool,
    /// 结果输出格式（全局）。
    #[serde(default)]
    pub output_format: OutputFormat,
    /// Whether to record this request in ordinary reply history. Stop confirmation disables it.
    /// Missing fields from older clients default to true for compatibility.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub record_history: bool,
    /// 调用方 Agent 家族（"claude"/"codex"/"cursor"）——CLI 经 env 探测后顺带上送（生命周期追踪，spec D21）。
    /// 旧 CLI 不带 → None；daemon 据此刷新对应 session 的「最近活动 + TTL」，仅刷新已追踪 session。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// 调用方 Agent 会话 ID（从 env 取，见 spec D21）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// AskHuman MCP server 进程级随机实例 ID。仅作为拿不到真实 Agent session 时的弱隔离键；
    /// 普通 CLI 与旧 MCP server 不带 → None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_instance_id: Option<String>,
    /// 调用方 Agent 进程 pid（可空）。方案5(b) 起 CLI 不再同步 walk → 恒 None；改由 daemon accept 后
    /// 从 `caller_pid` 异步 walk 得到，再经 `AgentResolved` 后推弹窗（旧字段保留以兼容旧 CLI）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
    /// 调用方（CLI 自身）进程 pid。方案5(b)：daemon 据此**异步**向上 walk 进程树定位 agent 进程，
    /// 把 ps 游走开销移出 CLI 关键路径（CLI 请求存续期保持连接，进程树仍在）。旧 CLI 不带 → 0（daemon 跳过 walk）。
    #[serde(default)]
    pub caller_pid: u32,
    /// 该 ask 是否经 MCP 模式发起（`AskHuman mcp` spawn 的子进程，由 env `ASKHUMAN_FROM_MCP` 置位）。
    /// 只有每次调用取得可信绑定时才会附带 `agent_session_id`；daemon 仍对 MCP 请求只刷新
    /// 已追踪 session、不因一次工具调用新建 lifecycle session。
    /// 旧 CLI 不带 → 默认 false（行为不变）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_mcp: bool,
    /// 性能埋点关联 id（`ASKHUMAN_PERF` 开启时 CLI 生成；空=不埋点）。daemon/helper/前端共用此 id
    /// 把同一次调用的各阶段时间线串起来。旧 CLI 不带 → 空串（埋点关闭）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub perf_id: String,
    /// 性能测试专用：弹窗画完首帧后自动取消（仅 harness 用，避免人工点按）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub perf_autodismiss: bool,
    /// whats-next 提问（spec todo-whats-next D2）：daemon 侧透传到 `AskRequest.whats_next`，
    /// 结果渲染为一段纯文本。旧 CLI 不带 → false（普通提问，行为不变）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whats_next: bool,
}

/// Hidden PermissionRequest hook → daemon confirmation task. The daemon owns request ids and
/// deadlines, so this wire payload carries only the validated semantic spec and caller context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmTask {
    pub spec: ConfirmSpec,
    /// Structured native edit intent used only by the local permission popup. It is forwarded in
    /// `ShowPayload`, never copied into the daemon-owned `ConfirmRequest` used by IM/history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popup_edit: Option<Box<crate::permission_diff::PermissionEditIntent>>,
    /// Caller source label used by popup/IM headers.
    pub source: String,
    /// Resolved UI language ("en" / "zh").
    pub lang: String,
    /// Workspace path/key shown to the human and used for agent association.
    pub project: String,
    /// Permission hooks are supported for claude/codex only.
    pub agent_kind: String,
    /// Native agent session id from the hook input.
    pub agent_session_id: String,
    /// Hook process pid; daemon may asynchronously resolve the owning agent process.
    #[serde(default)]
    pub caller_pid: u32,
    /// Codex permission memory metadata (spec codex-permission-remember): auto-allow query and
    /// per-action save operations. The daemon owns matching and persistence (D26).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<crate::permission_rules::PermissionMemory>,
}

/// 权限授权管理面板操作（设置进程 → Daemon，spec codex-permission-remember §6.3）。
/// Daemon 是 rule store 的唯一写入方：面板读写都经这里，设置进程不直接碰存储文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PermissionRulesOp {
    /// 对话摘要列表 + 跨会话（D41）授权计数。
    Summaries,
    /// 单个对话的完整规则列表（展开时按需加载）。
    SessionDetail { session_id: String },
    /// 跨会话授权的完整规则列表。
    GlobalDetail,
    /// 重置某对话的全部 AskHuman session rules（不触碰 Codex 原生配置）。
    ResetSession { session_id: String },
    /// 重置全部跨会话授权。
    ResetGlobal,
    /// 只关闭某对话的 YOLO 模式（D53），其余授权保留。
    DisableYolo { session_id: String },
}

/// 管理面板的一条对话分组：store 摘要 + agent registry 的标题/项目名增强（可为空）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSessionGroup {
    pub summary: crate::permission_rules::SessionRuleSummary,
    /// 对话标题（registry 已解析时给出；空 = 前端回退显示缩短的 session id）。
    #[serde(default)]
    pub title: String,
    /// 项目显示名（registry cwd basename；空 = 未知）。
    #[serde(default)]
    pub project_name: String,
}

/// 管理面板的一条规则展示行（D48：原样键文本，不脱敏）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleInfo {
    /// 规则类型标签："fileExact" / "fileProject" / "fileDisk" / "mcpTool" /
    /// "networkHost" / "shellExact" / "shellPrefix"。
    pub kind: String,
    /// 原样键文本（fileDisk 为空串）。
    pub display: String,
    pub created_at_ms: u64,
    pub last_used_at_ms: u64,
    /// 预计清理时间（最后命中 + 30 天滚动窗口，D15）。
    pub expires_at_ms: u64,
}

/// `PermissionRulesOp` 的回帧负载。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PermissionRulesResult {
    Summaries {
        sessions: Vec<PermissionSessionGroup>,
        global_count: usize,
    },
    Rules {
        rules: Vec<PermissionRuleInfo>,
    },
    Reset {
        removed: usize,
    },
}

/// 自动识别 userId/open_id 请求（设置进程 → Daemon，Q6）：用表单当前凭据，
/// 等用户私聊机器人发送识别码后返回其 id。Daemon 若已有同 `app_key` 的活动长连接则**观察现有连接**
/// （零冲突），否则自行临时开一条连接完成识别。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectRequest {
    /// 渠道类型："dingtalk" | "feishu" | "slack"。
    pub kind: String,
    /// 钉钉 client_id / 飞书 app_id / Slack App Token（也是「是否复用现有连接」的匹配键）。
    pub app_key: String,
    /// 钉钉 client_secret / 飞书 app_secret / Slack Bot Token。
    pub app_secret: String,
    /// 飞书自定义 base_url（钉钉/Slack 忽略，可传空）。
    pub base_url: String,
    /// 用户需私聊发送的识别码。
    pub code: String,
    /// 设置进程解析好的界面语言（"en" / "zh"），供 Daemon 本地化超时/断连等提示。
    pub lang: String,
}

/// 一条在途请求的菜单栏摘要（D→宿主，托盘「待答」子菜单用）：`id` 定位请求（点击回 `FocusRequest`），
/// `preview` 为该请求 Message 首个非空行（空则第一题题干）的截断预览。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRequestInfo {
    pub id: String,
    pub preview: String,
}

/// 一条活动 agent 的菜单栏摘要（D→宿主，托盘「Agent 状态」子菜单用，spec agent-interject D7）。
/// 仅含活动会话（工作中在前）；ended 不下发。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayAgentInfo {
    pub session_id: String,
    /// `/status` 同源的稳定编号（条目文案 `[n]` 前缀，可直接用于 IM `/msg <n>`）。
    #[serde(default)]
    pub seq: u64,
    /// 家族小写标识（claude/codex/cursor/grok）。
    pub kind: String,
    /// 会话标题（transcript 解析；空=未解析出）。
    #[serde(default)]
    pub title: String,
    /// 项目显示名（cwd basename；空=未知）。
    #[serde(default)]
    pub project_name: String,
    /// 原始工作目录（插话窗口头部展示透传用）。
    #[serde(default)]
    pub cwd: Option<String>,
    /// working / idle。
    pub state: String,
    /// 有待送达的插话消息。
    #[serde(default)]
    pub pending_interject: bool,
    /// 「聚焦终端」可用（有 pid 且所在终端受支持）。
    #[serde(default)]
    pub focusable: bool,
    #[serde(default)]
    pub pid: Option<u32>,
}

/// Daemon → GUI Helper 的题目下发（show 是 submit 的子集 + Daemon 分配的 request_id + 上下文）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowPayload {
    pub request_id: String,
    /// Complete daemon-owned interaction, discriminated for popup rendering.
    pub interaction: InteractionRequest,
    /// Local-popup-only native edit intent. Snapshot contents are never carried through daemon IPC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popup_edit: Option<Box<crate::permission_diff::PermissionEditIntent>>,
    /// 调用方来源名（弹窗标题「Question from {source}」）。
    pub source: String,
    /// 界面语言（"en" / "zh"）。
    pub lang: String,
    /// 当前项目 key（供历史窗口默认过滤当前项目）。
    #[serde(default)]
    pub project: String,
    /// 发起本次提问的 agent 家族（claude/codex/cursor/grok），探测不到为 None。弹窗据此显示来源 agent badge。
    #[serde(default)]
    pub agent_kind: Option<String>,
    /// Native Agent conversation/session id used by reply-history filtering. Unlike
    /// `agent_console_session_id`, this does not require an active AgentRegistry record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// MCP process fallback used by reply-history filtering when no native session is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_instance_id: Option<String>,
    /// 发起本次提问的 agent 进程 pid（进程树 walk 得到），探测不到为 None。弹窗据此判断 / 执行「聚焦终端」。
    #[serde(default)]
    pub agent_pid: Option<u32>,
    /// Agent 状态窗口可寻址的精确会话。仅当调用方的 `(kind, session_id)` 已命中 daemon
    /// `AgentRegistry` 中的活动记录时下发；未追踪或绑定不可信时为 None，弹窗据此隐藏快捷入口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_console_session_id: Option<String>,
    /// 性能埋点关联 id（方案6 热路径用）：冷 helper 经 env 拿到，热 helper 没有 env，故由 Show 透传，
    /// 领用时写入 perf 运行时上下文，使热进程的 `fe.painted`/`gui.win_show` 与 CLI 的 `cli.start` 同 id 关联。
    #[serde(default)]
    pub perf_id: String,
    /// 性能测试：画完首帧后自动取消弹窗（仅 harness 用）。热 helper 同样经 Show 透传（无 env）。
    #[serde(default)]
    pub perf_autodismiss: bool,
    /// 提问创建时刻（daemon 建请求时的 epoch 毫秒）。弹窗据此显示「几秒/分钟/小时前」的相对时间。
    /// 预热弹窗领用时得到的即为提问真正到达时刻（而非热进程 spawn 时刻）。
    #[serde(default)]
    pub created_at_ms: u64,
}

/// Daemon-authorized first presentation for a popup helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum PopupPresentation {
    /// The current owner may activate the application and take keyboard focus.
    Foreground,
    /// A waiting popup is shown without activation, cascaded behind its predecessor.
    BackgroundCascade {
        cascade_index: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        behind_window_number: Option<i64>,
    },
}

/// 客户端（CLI / GUI Helper）→ Daemon 的消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMsg {
    /// CLI / 控制连接握手。
    Hello(ClientHello),
    /// `daemon status`。
    Status,
    /// `daemon stop`：默认 graceful（有在途请求时排空后退出）；`force` 立即退出。
    /// 旧 Daemon 解析时忽略多余字段，旧 CLI 不带 `force` → 默认 false，双向兼容。
    Stop {
        #[serde(default)]
        force: bool,
    },
    /// CLI 提交一次提问任务（握手后发送）。
    Submit(TaskRequest),
    /// Hidden PermissionRequest hook submits a structured confirmation.
    /// Boxed: the task (with optional permission memory) dwarfs the other variants.
    SubmitConfirm(Box<ConfirmTask>),
    /// GUI Helper 握手：出示 Daemon 下发的一次性 token。
    GuiHello { token: String },
    /// 预热 GUI Helper 握手（方案6）：由 daemon 以 `--popup --warm` 拉起的进程在建好隐藏窗 + 挂载前端后
    /// 发送，表示「已就绪、入热池待命」。daemon 据此把该连接登记进热池，来请求时直接发 `Show` 领用，
    /// 无需现 spawn 新进程。无 token（领用时才关联具体请求）。
    GuiWarmReady,
    /// Popup content and its hidden native window are ready; daemon decides how it may appear.
    PopupReady {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_number: Option<i64>,
    },
    /// A visible waiting popup gained native focus through an explicit user action.
    PopupFocused { request_id: String },
    /// The native popup window has been destroyed and no longer blocks focus handoff.
    PopupDismissed { request_id: String },
    /// 设置进程请求「自动识别 userId/open_id」（Q6）。握手后发送，阻塞等单个结果。
    Detect(DetectRequest),
    /// GUI Helper 回传用户作答（`action` 区分发送/取消）。
    Answer {
        request_id: String,
        action: ChannelAction,
        #[serde(default)]
        answers: Vec<QuestionAnswer>,
    },
    /// GUI Helper submits a structured confirmation choice. `choice_index` is mapped through the
    /// daemon-owned request ledger; GUI-provided action ids are never accepted.
    ConfirmAnswer {
        request_id: String,
        choice_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
    },
    /// GUI reports readiness only after the confirmation view has completed its first paint.
    ConfirmReady { request_id: String },
    /// Agent 生命周期事件上报（`AskHuman __agent-hook <agent> <event>` → daemon，spec D20）。
    /// 即发即走、不等回包；daemon 据此更新注册表。
    AgentEvent {
        /// 家族 "claude"/"codex"/"cursor"。
        agent: String,
        /// 归一化事件 "session-start"/"turn-start"/"turn-end"/"session-end"。
        event: String,
        /// 会话 ID（身份键）。
        #[serde(default)]
        session_id: String,
        /// Agent 进程 pid（已解析的 agent PID；旧 hook 由 walk 得到，新 hook 发 None）。
        #[serde(default)]
        pid: Option<u32>,
        /// hook 进程的 parent PID（ppid），供 daemon 从其向上 walk 解析 agent PID 并缓存。
        /// 旧 daemon 忽略此字段（`default`）；旧 hook 不带 → None。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint_pid: Option<u32>,
        /// 工作目录（可空）。
        #[serde(default)]
        cwd: Option<String>,
        /// Optional inherited id for an IM-created terminal launch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch_id: Option<String>,
        /// SHA-256 of the initial user prompt. Raw prompt text never crosses lifecycle IPC.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_sha256: Option<String>,
        /// 事件时间（unix 秒，0 表示由 daemon 取当前时间）。
        #[serde(default)]
        ts: u64,
        /// 工具实时信息（仅 activity 事件、且能从 hook stdin 解析出工具时携带）。
        /// 旧 daemon 忽略此字段（`default`）；旧 report 不带 → `None`。用于 `/status <编号>` 实时「当前工具」。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<ToolReport>,
        /// 插话轮询（spec agent-interject D4）：hook 侧仅在 **PreToolUse** 且通过去重的那次上报置 true，
        /// daemon 须立即回一帧 `InterjectDecision`（none/message/hold）。旧 daemon 忽略此字段不回帧 →
        /// hook 侧 300ms 超时放行（fail-open）；旧 report 不带 → false（保持即发即走）。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interject_poll: bool,
    },
    /// Register one long-lived AskHuman MCP server process for Grok's best-effort session binding.
    McpInstanceRegister {
        mcp_instance_id: String,
        project: String,
        server_pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_pid_hint: Option<u32>,
    },
    /// Grok PreToolUse side channel. It carries only a versioned canonical-arguments hash.
    GrokBindingPending {
        agent_session_id: String,
        qualified_tool_name: String,
        arguments_sha256: String,
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_parent_hint: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
        created_at_ms: i64,
    },
    /// Claim one pending Grok binding in the current MCP instance/project partition.
    GrokBindingClaim {
        mcp_instance_id: String,
        project: String,
        tool_name: String,
        arguments_sha256: String,
        server_pid: u32,
    },
    /// 状态窗口订阅 agent 快照（握手后发；之后 daemon 持续推 `AgentsState`，spec D20）。
    AgentsSubscribe,
    /// 控制台焦点会话（在 agents 订阅连接上发送，spec gui-agent-console C8）：daemon 对焦点
    /// 会话复用 watch 引擎按签名推 `AgentDetail` 帧；`None` = 取消焦点。随连接断开自动清理。
    AgentsFocus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// 菜单栏宿主订阅整合状态（**非保活**，spec D10）：连上即收一帧 `TrayState`，之后变化即推。
    /// 该订阅刻意**不计入 daemon 空闲保活**——图标不得把 daemon 续命（续命只由「有窗口」的
    /// 普通连接承担）。daemon 收到后在 `handle_tray_sub` 中抵消其对 `active` 的占用。
    TraySubscribe,
    /// A successful manual update check changed `update.json`. Reload and broadcast the snapshot
    /// without starting or keeping the daemon alive.
    RefreshUpdateState,
    /// 托盘「待答」子菜单点击：请求 daemon 聚焦 / 闪烁对应请求的弹窗（宿主→daemon，即发即走）。
    /// daemon 找到该请求的弹窗连接转发 `FocusPopup`；无弹窗（如弹窗拉起失败）则静默忽略。
    FocusRequest { request_id: String },
    /// 手动把某 agent 置为「空闲」（状态窗口→daemon，即发即走）：用户发现某 agent 因漏 hook
    /// （如 Claude 被打断）卡在「工作中」时，可在状态窗口手动纠正。仅改状态、不结束会话。
    AgentForceIdle { session_id: String },
    /// 插话 composer 窗口连接登记（spec agent-interject D7）：daemon 接管该连接并把该 session 标记
    /// 「composer 打开中」（此后到来的 PreToolUse poll 会挂起等待）；**连接断开＝关闭**（杜绝宿主
    /// 崩溃后的僵尸状态挂起 hook）。同连接上可继续收 `InterjectSubmit` / `InterjectQuery`。非保活。
    InterjectComposer { session_id: String },
    /// 插话提交（整体覆盖该 session 的待送达队列，D2）：空文本＝清空。有等待中的 hook 时立即交付。
    /// 可在 composer 连接上发，也可独立连接即发即走。
    InterjectSubmit {
        session_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<FileAttachment>,
    },
    /// 插话追加：不覆盖已有待送达条目；有等待中的 hook 时立即交付。用于一键快捷插话。
    InterjectAppend {
        session_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<FileAttachment>,
    },
    /// 撤回：清空该 session 的待送达队列（AgentsView 撤回按钮 / IM `/msg-clear`）。即发即走。
    InterjectClear { session_id: String },
    /// 查询该 session 的待送达全文（composer 预填 / IM 回显）。回一帧 `InterjectState`。
    InterjectQuery { session_id: String },
    /// 权限授权管理面板操作（设置进程，§6.3）。回一帧 `PermissionRules`。
    PermissionRules { op: PermissionRulesOp },
    /// GUI 启动新任务后把活跃槽切到 popup（spec gui-agent-task-launch G11）：人在电脑旁，
    /// 新 Agent 的提问默认弹窗。即发即走，无回包；旧 daemon 解析失败断连无副作用。
    ActivatePopupSlot,
}

/// 一次工具调用的实时上报（随 `AgentEvent` 的 activity 事件携带）。跨进程只传**原始工具名**与
/// **已归一化截断的短对象**（文件名 / 命令首段 / 参数前段），绝不传工具输入/结果正文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolReport {
    /// 原始工具名（渲染侧据此复得类别标签）。
    pub name: String,
    /// 简短对象（可空，如询问类工具无对象）。
    #[serde(default)]
    pub object: Option<String>,
    /// 阶段：pre = 开始（置「当前工具」）；post = 结束（清除）。
    pub phase: ToolPhase,
}

/// 工具调用阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolPhase {
    Pre,
    Post,
}

/// 插话轮询的裁决动作（`InterjectDecision.action`，spec agent-interject D3/D4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterjectAction {
    /// 无消息、无等待 → hook 立即放行（首帧）。
    None,
    /// 交付消息 → hook 输出 deny+消息（首帧或 Hold 后二帧）。
    Message,
    /// composer 打开中 → hook 保持连接等待二帧（首帧）。
    Hold,
    /// 放行（Hold 后二帧：composer 取消/关窗，或消息被并发的另一个 hook 拿走）。
    Release,
}

/// Daemon → 客户端（CLI / GUI Helper）的消息。
// 大变体（如 Ask 请求全文）装箱会波及所有构造/匹配点；消息短命且量小，体积差可接受。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMsg {
    HelloAck(HelloAck),
    Status(StatusInfo),
    Stopping,
    /// 排空期收到新 Submit 时的拒绝回复（回完即断开），回带剩余在途请求数。
    Draining {
        active: usize,
    },
    Error {
        message: String,
    },
    /// 任务已受理，回带 Daemon 分配的 request_id（D→CLI）。
    Accepted {
        request_id: String,
    },
    /// Structured confirmation accepted by the daemon.
    ConfirmAccepted {
        request_id: String,
    },
    /// 流式警告 / 诊断 → CLI 的 stderr（D→CLI）。
    Warn {
        text: String,
    },
    /// 终态：渲染好的结果文本 + 退出码（D→CLI）。CLI 原样打印 stdout 后按码退出。
    Final {
        stdout: String,
        exit_code: i32,
    },
    /// A human decision won the structured confirmation race.
    ConfirmFinal {
        result: ConfirmResult,
    },
    /// No human decision was produced; caller must return to its native approval flow.
    ConfirmFallback {
        reason: ConfirmFallbackReason,
    },
    /// 自动识别成功，回带识别出的 userId/open_id（D→设置进程，Q6）。失败用 `Error`。
    Detected {
        id: String,
    },
    /// 下发题目（D→GUI）。
    Show(ShowPayload),
    /// Authorize the helper's first visible presentation after `PopupReady`.
    PresentPopup {
        request_id: String,
        presentation: PopupPresentation,
    },
    /// 被其它渠道抢答，通知 GUI 收尾关窗（D→GUI）。
    Cancel {
        request_id: String,
        winner: String,
    },
    /// 配置实时变更，下发新的 `general` 配置给活动 GUI Helper 以即时切主题/语言（D→GUI，A12）。
    ConfigChanged {
        general: serde_json::Value,
    },
    /// 版本自更新状态（D→GUI）：`available` 有新版可更新；`pending` 新二进制已落盘、
    /// 待所有在途弹窗答完后由 graceful-drain 换新生效。弹窗据此显示更新入口 / 待生效横条。
    UpdateState {
        available: bool,
        latest_version: String,
        pending: bool,
    },
    /// 调用方 agent 信息异步解析完成（D→GUI，方案5/b）：daemon 从 `caller_pid` walk 出 agent 家族 / pid 后
    /// 后推弹窗，使顶栏 badge「后到补全 / 升级为可聚焦终端」。家族在 env 探到时随 Show 即给（这里可能与之
    /// 一致或为 MCP 兜底新探到的），`pid` 供「聚焦终端」。
    AgentResolved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// Agent 注册表全量快照（D→状态窗口订阅者，spec D20）。变化时推 + 周期心跳推。
    /// `agents` 为记录数组，前端按类型分组、按状态排序渲染。
    AgentsState {
        agents: serde_json::Value,
    },
    /// 控制台焦点会话详情帧（D→agents 订阅者，spec gui-agent-console C8/R5）：tagged JSON
    /// （`type: "watchFrame"` + 会话 id + Watch 帧字段），签名变化才推。未来可扩展新 type。
    AgentDetail {
        detail: serde_json::Value,
    },
    /// 菜单栏宿主整合状态（D→宿主，spec D10）：连上即一帧 + 变化即推。
    /// 字段名 snake_case（与既有结构体变体一致；IPC 两端同二进制）。宿主据 `running` 与
    /// `active_requests` 切图标三态，菜单文字取最近一帧；`pending` 触发宿主二进制换新。
    TrayState {
        /// daemon 是否在运行（本帧来自运行中的 daemon → 恒为 true；断连由宿主自行判定为「停止」）。
        running: bool,
        version: String,
        uptime_secs: u64,
        active_requests: usize,
        /// 当前常热的 IM 长连接名（"dingtalk"/"feishu"/"telegram"/"slack"）。
        im_connections: Vec<String>,
        /// 是否处于排空（graceful drain）。
        draining: bool,
        /// 「工作中」agent 数（生命周期追踪未开启时为 0）。
        agents_working: usize,
        /// 「空闲」agent 数。
        agents_idle: usize,
        /// 有可用更新（远端正式版高于本地且未被忽略）。
        update_available: bool,
        /// 最新正式版版本号（available 时有意义）。
        update_latest: String,
        /// 新二进制已落盘、待 drain 换新生效（宿主据此换新自身）。
        pending: bool,
        /// 在途请求摘要（托盘「待答」子菜单逐条列出，点击可聚焦对应弹窗）。
        /// 旧端回包缺此字段 → 空 Vec（仍显示数量、无子菜单项）。
        #[serde(default)]
        pending_requests: Vec<PendingRequestInfo>,
        /// 活动 agent 摘要（托盘「Agent 状态」子菜单逐条列出，spec agent-interject D7）。
        /// 旧 daemon 缺此字段 → 空 Vec（父项退回普通「打开状态窗口」条目）。
        #[serde(default)]
        agents: Vec<TrayAgentInfo>,
        /// 各渠道最近未恢复的故障（R7；旧 daemon 缺此字段 → 空 Vec，托盘不显示警示）。
        #[serde(default)]
        channel_issues: Vec<ChannelIssueInfo>,
    },
    /// 聚焦并闪烁某请求的弹窗（daemon→该请求的 GUI Helper）。弹窗进程据此 `set_focus` + 通知前端闪烁。
    FocusPopup {
        request_id: String,
    },
    /// 插话轮询裁决（D→hook；`AgentEvent.interject_poll=true` 的回帧，spec agent-interject D4）。
    /// 首帧 none/message/hold；hold 后二帧 message/release。`text` 仅 message 时有意义。
    InterjectDecision {
        action: InterjectAction,
        #[serde(default)]
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<FileAttachment>,
    },
    /// Unique Grok side-channel claim. None means unavailable or ambiguous.
    GrokBindingClaim {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_session_id: Option<String>,
    },
    /// 插话待送达状态（D→composer/查询方；`InterjectQuery` 的回帧）。`text` 为按空行拼接的全文
    /// （composer 预填），`entries` 为条数（IM 回执）。
    InterjectState {
        text: String,
        entries: usize,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<FileAttachment>,
    },
    /// 权限授权管理面板回帧（D→设置进程，§6.3）。
    PermissionRules {
        result: PermissionRulesResult,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_request_history_flag_defaults_true_and_false_is_transmitted() {
        let legacy = r#"{
          "message":{"text":"done","files":[]},
          "questions":[{"message":"continue?","predefinedOptions":[]}],
          "isMarkdown":true,
          "source":"Codex",
          "lang":"en"
        }"#;
        let legacy: TaskRequest = serde_json::from_str(legacy).unwrap();
        assert!(legacy.record_history);
        assert!(legacy.agent_session_id.is_none());
        assert!(legacy.mcp_instance_id.is_none());
        let serialized = serde_json::to_string(&legacy).unwrap();
        assert!(!serialized.contains("recordHistory"));
        assert!(!serialized.contains("agentSessionId"));
        assert!(!serialized.contains("mcpInstanceId"));

        let internal = legacy.clone();
        let mut value = serde_json::to_value(internal).unwrap();
        value["recordHistory"] = serde_json::Value::Bool(false);
        let internal: TaskRequest = serde_json::from_value(value).unwrap();
        assert!(!internal.record_history);
        assert!(serde_json::to_string(&internal)
            .unwrap()
            .contains(r#""recordHistory":false"#));

        let mut value = serde_json::to_value(internal).unwrap();
        value["agentSessionId"] = serde_json::json!("session-1");
        value["mcpInstanceId"] = serde_json::json!("instance-1");
        let bound: TaskRequest = serde_json::from_value(value).unwrap();
        assert_eq!(bound.agent_session_id.as_deref(), Some("session-1"));
        assert_eq!(bound.mcp_instance_id.as_deref(), Some("instance-1"));
        let serialized = serde_json::to_string(&bound).unwrap();
        assert!(serialized.contains(r#""agentSessionId":"session-1""#));
        assert!(serialized.contains(r#""mcpInstanceId":"instance-1""#));
    }

    #[test]
    fn grok_binding_messages_roundtrip_all_partition_fields() {
        let messages = [
            ClientMsg::McpInstanceRegister {
                mcp_instance_id: "instance".into(),
                project: "/p".into(),
                server_pid: 10,
                parent_pid_hint: Some(9),
            },
            ClientMsg::GrokBindingPending {
                agent_session_id: "session".into(),
                qualified_tool_name: "askhuman__ask".into(),
                arguments_sha256: "hash".into(),
                project: "/p".into(),
                hook_parent_hint: Some(11),
                tool_use_id: Some("tool".into()),
                created_at_ms: 12,
            },
            ClientMsg::GrokBindingClaim {
                mcp_instance_id: "instance".into(),
                project: "/p".into(),
                tool_name: "ask".into(),
                arguments_sha256: "hash".into(),
                server_pid: 10,
            },
        ];
        for message in messages {
            let json = serde_json::to_string(&message).unwrap();
            let decoded: ClientMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&decoded),
                std::mem::discriminant(&message),
                "{json}"
            );
            assert!(json.contains("/p"));
        }
        let reply = ServerMsg::GrokBindingClaim {
            agent_session_id: Some("session".into()),
        };
        let json = serde_json::to_string(&reply).unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(&json).unwrap(),
            ServerMsg::GrokBindingClaim {
                agent_session_id: Some(session)
            } if session == "session"
        ));
        let empty = serde_json::to_string(&ServerMsg::GrokBindingClaim {
            agent_session_id: None,
        })
        .unwrap();
        assert!(!empty.contains("agent_session_id"));
    }

    fn confirm_task() -> ConfirmTask {
        ConfirmTask {
            spec: crate::models::ConfirmSpec {
                title: "Approve?".into(),
                context: vec![],
                detail: crate::models::ConfirmDetail {
                    summary: "Run command".into(),
                    body_md: String::new(),
                },
                choices: vec![
                    crate::models::ConfirmChoice {
                        id: "approve_once".into(),
                        label: "Approve once".into(),
                        description: String::new(),
                        role: crate::confirm::ActionRole::Primary,
                        variant: None,
                    },
                    crate::models::ConfirmChoice {
                        id: "deny".into(),
                        label: "Deny".into(),
                        description: String::new(),
                        role: crate::confirm::ActionRole::Destructive,
                        variant: None,
                    },
                ],
                presentation: crate::models::ConfirmPresentation::SingleSelectSubmit {
                    input: None,
                    submit_label: "Submit".into(),
                    default_action_id: None,
                },
                dismiss_action_id: "deny".into(),
            },
            popup_edit: None,
            source: "Claude Code".into(),
            lang: "en".into(),
            project: "/tmp/project".into(),
            agent_kind: "claude".into(),
            agent_session_id: "session-1".into(),
            caller_pid: 42,
            memory: None,
        }
    }

    /// 旧 CLI 发的 `{"type":"stop"}`（无 force 字段）→ force 默认 false。
    #[test]
    fn stop_without_force_defaults_false() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"stop"}"#).unwrap();
        assert!(matches!(msg, ClientMsg::Stop { force: false }));
    }

    #[test]
    fn confirm_messages_roundtrip_without_action_ids_from_gui() {
        let task = ClientMsg::SubmitConfirm(Box::new(confirm_task()));
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains(r#""type":"submitConfirm""#));
        assert!(!json.contains("expiresAtMs"));
        assert!(matches!(
            serde_json::from_str::<ClientMsg>(&json).unwrap(),
            ClientMsg::SubmitConfirm(_)
        ));

        let answer = ClientMsg::ConfirmAnswer {
            request_id: "r1".into(),
            choice_index: 1,
            comment: Some("unsafe".into()),
        };
        let json = serde_json::to_string(&answer).unwrap();
        assert!(json.contains(r#""choice_index":1"#));
        assert!(!json.contains("action_id"));

        let final_msg = ServerMsg::ConfirmFinal {
            result: ConfirmResult {
                action_id: "deny".into(),
                comment: Some("unsafe".into()),
                source_channel_id: "popup".into(),
            },
        };
        let json = serde_json::to_string(&final_msg).unwrap();
        assert!(json.contains(r#""type":"confirmFinal""#));
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(&json).unwrap(),
            ServerMsg::ConfirmFinal { .. }
        ));
    }

    /// 新 CLI 发的带 force 字段可正常解析；序列化形态含 force。
    #[test]
    fn stop_with_force_roundtrip() {
        let json = serde_json::to_string(&ClientMsg::Stop { force: true }).unwrap();
        assert!(json.contains(r#""type":"stop""#));
        assert!(json.contains(r#""force":true"#));
        let msg: ClientMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(msg, ClientMsg::Stop { force: true }));
    }

    /// 内部标签枚举的单元变体应忽略多余字段：旧 Daemon 收到新 CLI 的
    /// `{"type":"status","extra":…}` 类负载不报错（以 Status 验证该 serde 行为）。
    #[test]
    fn unit_variant_ignores_extra_fields() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"status","force":true}"#).unwrap();
        assert!(matches!(msg, ClientMsg::Status));
    }

    /// 旧 Daemon 的 StatusInfo 回包缺 draining 字段 → 默认 false。
    #[test]
    fn status_info_missing_draining_defaults_false() {
        let json = r#"{"pid":1,"version":"0.1.0","protocolVersion":1,"uptimeSecs":2,
            "socket":"/tmp/s","activeRequests":3}"#;
        let info: StatusInfo = serde_json::from_str(json).unwrap();
        assert!(!info.draining);
        assert_eq!(info.active_requests, 3);
        assert!(info.im_connections.is_empty());
    }

    /// 新增枚举值序列化往返。
    #[test]
    fn draining_variants_roundtrip() {
        let s = serde_json::to_string(&HelloStatus::Draining).unwrap();
        assert_eq!(s, r#""draining""#);
        let back: HelloStatus = serde_json::from_str(&s).unwrap();
        assert_eq!(back, HelloStatus::Draining);

        let json = serde_json::to_string(&ServerMsg::Draining { active: 2 }).unwrap();
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ServerMsg::Draining { active: 2 }));
    }

    /// UpdateState 序列化往返（含 camelCase 字段）。
    #[test]
    fn update_state_roundtrip() {
        let msg = ServerMsg::UpdateState {
            available: true,
            latest_version: "0.6.0".to_string(),
            pending: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // 变体名走 enum 级 camelCase；结构体变体的字段名保持 snake_case
        // （与既有 `Final { exit_code }` 一致；IPC 两端同二进制，无需 camelCase）。
        assert!(json.contains(r#""type":"updateState""#));
        assert!(json.contains(r#""latest_version":"0.6.0""#));
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ServerMsg::UpdateState {
                available: true,
                pending: false,
                ..
            }
        ));
    }

    #[test]
    fn popup_presentation_handshake_roundtrip() {
        let ready = ClientMsg::PopupReady {
            request_id: "r1".into(),
            window_number: Some(42),
        };
        let json = serde_json::to_string(&ready).unwrap();
        assert!(json.contains(r#""type":"popupReady""#));
        assert!(json.contains(r#""window_number":42"#));
        assert!(matches!(
            serde_json::from_str::<ClientMsg>(&json).unwrap(),
            ClientMsg::PopupReady {
                window_number: Some(42),
                ..
            }
        ));

        let present = ServerMsg::PresentPopup {
            request_id: "r2".into(),
            presentation: PopupPresentation::BackgroundCascade {
                cascade_index: 2,
                behind_window_number: Some(42),
            },
        };
        let json = serde_json::to_string(&present).unwrap();
        assert!(json.contains(r#""type":"presentPopup""#));
        assert!(json.contains(r#""mode":"backgroundCascade""#));
        assert!(matches!(
            serde_json::from_str::<ServerMsg>(&json).unwrap(),
            ServerMsg::PresentPopup {
                presentation: PopupPresentation::BackgroundCascade {
                    cascade_index: 2,
                    behind_window_number: Some(42),
                },
                ..
            }
        ));
    }

    /// TraySubscribe 是单元变体：旧端收到带多余字段的负载不报错（兼容性）。
    #[test]
    fn tray_subscribe_unit_variant() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"traySubscribe"}"#).unwrap();
        assert!(matches!(msg, ClientMsg::TraySubscribe));
    }

    #[test]
    fn refresh_update_state_roundtrip() {
        let json = serde_json::to_string(&ClientMsg::RefreshUpdateState).unwrap();
        assert_eq!(json, r#"{"type":"refreshUpdateState"}"#);
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ClientMsg::RefreshUpdateState));
    }

    /// TrayState 序列化往返（变体名 camelCase、字段 snake_case）。
    #[test]
    fn tray_state_roundtrip() {
        let msg = ServerMsg::TrayState {
            running: true,
            version: "0.7.0".to_string(),
            uptime_secs: 42,
            active_requests: 1,
            im_connections: vec!["feishu".to_string()],
            draining: false,
            agents_working: 2,
            agents_idle: 3,
            update_available: true,
            update_latest: "0.8.0".to_string(),
            pending: false,
            pending_requests: vec![PendingRequestInfo {
                id: "r1".to_string(),
                preview: "deploy?".to_string(),
            }],
            agents: vec![TrayAgentInfo {
                session_id: "s1".to_string(),
                seq: 3,
                kind: "claude".to_string(),
                title: "修复登录".to_string(),
                project_name: "proj".to_string(),
                cwd: Some("/w/proj".to_string()),
                state: "working".to_string(),
                pending_interject: true,
                focusable: true,
                pid: Some(7),
            }],
            channel_issues: vec![ChannelIssueInfo {
                channel: "slack".to_string(),
                message: "invalid_auth".to_string(),
                at_ms: 1_700_000_000_000,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"trayState""#));
        assert!(json.contains(r#""active_requests":1"#));
        assert!(json.contains(r#""agents_working":2"#));
        assert!(json.contains(r#""preview":"deploy?""#));
        assert!(json.contains(r#""pendingInterject":true"#));
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        match back {
            ServerMsg::TrayState {
                running,
                active_requests,
                agents_idle,
                update_available,
                agents,
                channel_issues,
                ..
            } => {
                assert!(running);
                assert_eq!(active_requests, 1);
                assert_eq!(agents_idle, 3);
                assert!(update_available);
                assert_eq!(agents.len(), 1);
                assert!(agents[0].focusable);
                assert_eq!(channel_issues.len(), 1);
                assert_eq!(channel_issues[0].channel, "slack");
            }
            other => panic!("unexpected: {:?}", other),
        }
        // 旧 daemon 回包缺 agents / channel_issues 字段 → 空 Vec（宿主退回普通条目 / 不显示警示）。
        let old = r#"{"type":"trayState","running":true,"version":"0.6","uptime_secs":1,
            "active_requests":0,"im_connections":[],"draining":false,"agents_working":0,
            "agents_idle":0,"update_available":false,"update_latest":"","pending":false}"#;
        match serde_json::from_str::<ServerMsg>(old).unwrap() {
            ServerMsg::TrayState {
                agents,
                channel_issues,
                ..
            } => {
                assert!(agents.is_empty());
                assert!(channel_issues.is_empty());
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    /// 旧端忽略未知变体：当前二进制收到未来新增的 `{"type":"somethingNew"}` 应报错而非 panic；
    /// 实际兼容策略为「读取方遇未知变体时跳过该消息」（各订阅循环用 `Ok(Some(_)) => {}` 兜底）。
    #[test]
    fn unknown_server_variant_is_error_not_panic() {
        let r: Result<ServerMsg, _> = serde_json::from_str(r#"{"type":"somethingNew","x":1}"#);
        assert!(r.is_err());
    }

    /// 旧 report 发的 AgentEvent（无 interject_poll 字段）→ 默认 false（即发即走语义不变）；
    /// false 时序列化省略该字段（旧 daemon 收到的负载与从前逐字一致）。
    #[test]
    fn agent_event_interject_poll_defaults_false_and_omitted() {
        let json = r#"{"type":"agentEvent","agent":"claude","event":"activity","session_id":"s1"}"#;
        let msg: ClientMsg = serde_json::from_str(json).unwrap();
        match msg {
            ClientMsg::AgentEvent { interject_poll, .. } => assert!(!interject_poll),
            other => panic!("unexpected: {:?}", other),
        }
        let out = serde_json::to_string(&ClientMsg::AgentEvent {
            agent: "claude".into(),
            event: "activity".into(),
            session_id: "s1".into(),
            pid: None,
            hint_pid: None,
            cwd: None,
            launch_id: None,
            prompt_sha256: None,
            ts: 0,
            tool: None,
            interject_poll: false,
        })
        .unwrap();
        assert!(!out.contains("interject_poll"));
        // true 时带字段。
        let out = serde_json::to_string(&ClientMsg::AgentEvent {
            agent: "claude".into(),
            event: "activity".into(),
            session_id: "s1".into(),
            pid: None,
            hint_pid: None,
            cwd: None,
            launch_id: None,
            prompt_sha256: None,
            ts: 0,
            tool: None,
            interject_poll: true,
        })
        .unwrap();
        assert!(out.contains(r#""interject_poll":true"#));
    }

    /// 插话裁决帧序列化往返（action 小写；text 缺省为空串）。
    #[test]
    fn interject_decision_roundtrip() {
        let json = serde_json::to_string(&ServerMsg::InterjectDecision {
            action: InterjectAction::Hold,
            text: String::new(),
            attachments: Vec::new(),
        })
        .unwrap();
        assert!(json.contains(r#""action":"hold""#));
        // hook 侧解析（text 缺省）。
        let back: ServerMsg =
            serde_json::from_str(r#"{"type":"interjectDecision","action":"message","text":"停"}"#)
                .unwrap();
        match back {
            ServerMsg::InterjectDecision { action, text, .. } => {
                assert_eq!(action, InterjectAction::Message);
                assert_eq!(text, "停");
            }
            other => panic!("unexpected: {:?}", other),
        }
        let none: ServerMsg =
            serde_json::from_str(r#"{"type":"interjectDecision","action":"none"}"#).unwrap();
        assert!(matches!(
            none,
            ServerMsg::InterjectDecision {
                action: InterjectAction::None,
                ..
            }
        ));
    }

    /// 插话客户端消息序列化往返（composer 登记 / 提交 / 撤回 / 查询）。
    #[test]
    fn interject_client_msgs_roundtrip() {
        let msgs = [
            ClientMsg::InterjectComposer {
                session_id: "s1".into(),
            },
            ClientMsg::InterjectSubmit {
                session_id: "s1".into(),
                text: "调整方向".into(),
                attachments: Vec::new(),
            },
            ClientMsg::InterjectAppend {
                session_id: "s1".into(),
                text: "马上提问".into(),
                attachments: Vec::new(),
            },
            ClientMsg::InterjectClear {
                session_id: "s1".into(),
            },
            ClientMsg::InterjectQuery {
                session_id: "s1".into(),
            },
        ];
        for m in msgs {
            let json = serde_json::to_string(&m).unwrap();
            let back: ClientMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&back),
                std::mem::discriminant(&m),
                "roundtrip variant mismatch: {json}"
            );
        }
    }
}
