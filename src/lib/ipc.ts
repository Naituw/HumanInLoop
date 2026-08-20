import { invoke } from "@tauri-apps/api/core";
import type {
  AgentsInit,
  AppConfig,
  ChannelIssue,
  DingTalkDetectArgs,
  DingTalkTestArgs,
  DingTalkWaitArgs,
  FeishuDetectArgs,
  FeishuTestArgs,
  FeishuWaitArgs,
  AgentId,
  AgentKind,
  AgentTaskReadiness,
  AgentTaskWorkspace,
  AgentMode,
  AgentModeStatus,
  ClaudeHookStatus,
  HistoryEntry,
  HistoryInit,
  HistorySessionTitleRequest,
  HistorySessionTitleResult,
  HookStatus,
  InterjectInit,
  InterjectPending,
  LifecycleStatus,
  NewTaskInit,
  NewTaskProject,
  ForkTaskInit,
  PopupInit,
  PermissionDiffModel,
  PopupSoundSupport,
  PushedAgent,
  PushedUpdateState,
  RuleStatus,
  PermissionRulesOp,
  PermissionRulesResult,
  PopupSubmission,
  ProjectInfo,
  SecretActions,
  SettingsPayload,
  SlackDetectArgs,
  SlackTestArgs,
  SlackWaitArgs,
  TelegramTestArgs,
  ThemeMode,
  DiffFileView,
  DiffStatPage,
  ImageAttachment,
  TodoDoneEntry,
  TodoEntry,
  TodoProjectInfo,
  TodosInit,
  TranscriptPage,
  UpdateInfo,
  WindowEffect,
} from "./types";

export const popupInit = () => invoke<PopupInit>("popup_init");

export const enrichPermissionDiff = (requestId: string) =>
  invoke<PermissionDiffModel>("enrich_permission_diff", { requestId });

/** 上报一个前端性能埋点（`stage` + 前端 epoch ms 时间戳）；埋点关闭时后端为 no-op。 */
export const perfMark = (stage: string, ts: number) =>
  invoke<void>("perf_mark", { stage, ts });

/** 异步解析指定 agent pid 所在终端类型（独立于 popup_init，避免进程链 ps 拖慢弹窗首屏）。 */
export const popupAgentTerminal = (pid: number) =>
  invoke<string | null>("popup_agent_terminal", { pid });

/** 拉取调用方 agent 的异步解析结果初值（方案5/b；之后靠 `agent-resolved` 事件实时更新）。 */
export const popupAgentResolved = () =>
  invoke<PushedAgent>("popup_agent_resolved");

/** Report that popup content is ready; daemon authorizes foreground or background presentation. */
export const popupShowWindow = () => invoke<void>("popup_show_window");

export const submitPopup = (submission: PopupSubmission) =>
  invoke<void>("submit_popup", { submission });

export const submitConfirmAction = (
  choiceIndex: number,
  comment?: string | null
) => invoke<void>("submit_confirm_action", { choiceIndex, comment });

export const confirmPopupReady = () => invoke<void>("confirm_popup_ready");

export const cancelPopup = () => invoke<void>("cancel_popup");

export const openPath = (path: string) => invoke<void>("open_path", { path });

export const previewAttachments = (paths: string[], index: number) =>
  invoke<void>("preview_attachments", { paths, index });

export const refocusPreview = () => invoke<void>("refocus_preview");

export const closePreview = () => invoke<void>("close_preview");

export const readImageDataUrl = (path: string) =>
  invoke<string>("read_image_data_url", { path });

export const fileIconDataUrl = (path: string) =>
  invoke<string>("file_icon_data_url", { path });

export const showAttachmentMenu = (path: string) =>
  invoke<void>("show_attachment_menu", { path });

export const getSettings = () => invoke<SettingsPayload>("get_settings");

/** Codex 权限授权管理面板（spec codex-permission-remember §6.3）：全部操作经 daemon 完成。 */
export const permissionRulesPanel = (op: PermissionRulesOp) =>
  invoke<PermissionRulesResult>("permission_rules_panel", { op });

export const saveSettings = (config: AppConfig, secretActions: SecretActions) =>
  invoke<void>("save_settings", { config, secretActions });

export const agentTaskWorkspaces = (refresh = false) =>
  invoke<AgentTaskWorkspace[]>("agent_task_workspaces", { refresh });
export const agentTaskWorkspaceAdd = (path: string) =>
  invoke<AgentTaskWorkspace>("agent_task_workspace_add", { path });
export const agentTaskWorkspacePin = (path: string, pinned: boolean) =>
  invoke<void>("agent_task_workspace_pin", { path, pinned });
export const agentTaskWorkspaceHide = (path: string, hidden: boolean) =>
  invoke<void>("agent_task_workspace_hide", { path, hidden });
export const agentTaskWorkspaceForget = (path: string) =>
  invoke<void>("agent_task_workspace_forget", { path });
export const agentTaskReadiness = (opts?: { kind?: AgentKind; force?: boolean }) =>
  invoke<AgentTaskReadiness[]>(
    "agent_task_readiness",
    opts ? { kind: opts.kind, force: opts.force } : undefined,
  );
export const agentTaskTestTerminal = () =>
  invoke<void>("agent_task_test_terminal");

export const getPrompt = (variant?: "cli" | "mcp") =>
  invoke<string>("get_prompt", { variant });

export const collaborationStyleDefaults = () =>
  invoke<{ aligned: string; autonomous: string }>("collaboration_style_defaults");

/** Rewrite rules/skill for every agent with an enabled integration mode. */
export const collaborationStyleApplyIntegrations = () =>
  invoke<void>("collaboration_style_apply_integrations");

export const openTestPopup = () => invoke<void>("open_test_popup");

export const popupSoundSupport = () =>
  invoke<PopupSoundSupport>("popup_sound_support");

export const playPopupSound = (name: string) =>
  invoke<void>("play_popup_sound", { name });

export const setTheme = (theme: ThemeMode) =>
  invoke<void>("set_theme", { theme });

export const updateTheme = (theme: ThemeMode) =>
  invoke<void>("update_theme", { theme });

export const openSettings = (tab?: string) =>
  invoke<void>("open_settings", { tab: tab ?? null });

/** 打开 Agent Window 并定位到 daemon 为当前弹窗严格匹配的会话。 */
export const openAgentConsole = () => invoke<void>("open_agent_console");

export const popupImTipVisible = () =>
  invoke<boolean>("popup_im_tip_visible");

export const popupImTipDismiss = () =>
  invoke<void>("popup_im_tip_dismiss");

export const openHistory = () => invoke<void>("open_history");

export const historyInit = () => invoke<HistoryInit>("history_init");

export const agentsInit = () => invoke<AgentsInit>("agents_init");

export const agentsStartSubscription = () =>
  invoke<void>("agents_start_subscription");

// ===== Agent 控制台（spec gui-agent-console）=====

/** 控制台焦点会话（C8）：daemon 对焦点会话按签名推 `agent-detail` 帧；null＝取消焦点。 */
export const agentsFocus = (sessionId: string | null) =>
  invoke<void>("agents_focus", { sessionId });

/** 「去回答」（C7）：请求 daemon 聚焦对应请求的弹窗（托盘同款链路）。 */
export const focusRequest = (requestId: string) =>
  invoke<void>("focus_request", { requestId });

/** 控制台输入框发消息（C3 追加语义，同 IM /msg）。 */
export const interjectAppend = (
  sessionId: string,
  text: string,
  filePaths: string[] = [],
  pastedImages: ImageAttachment[] = [],
) => invoke<void>("interject_append", { sessionId, text, filePaths, pastedImages });

/** 待送达气泡内容查询；daemon 未运行时返回空状态。 */
export const interjectPeek = (sessionId: string) =>
  invoke<InterjectPending>("interject_peek", { sessionId });

/** 完整会话分页（C14）：`before` 为事件绝对下标游标（null＝末尾），每页默认 200 条。 */
export const consoleTranscript = (
  kind: string,
  sessionId: string,
  before: number | null,
) => invoke<TranscriptPage>("console_transcript", { kind, sessionId, before, limit: null });

/** 项目未暂存变更统计（C15 第一级）。busy/超时以 Err 返回，调用方跳过本次刷新。 */
export const consoleDiffStat = (project: string) =>
  invoke<DiffStatPage>("console_diff_stat", { project });

/** 单文件 hunk 视图（C15 第二级，展开时才调）。 */
export const consoleDiffFile = (project: string, path: string) =>
  invoke<DiffFileView>("console_diff_file", { project, path });

/** 暂存指定路径（单文件与全部共用），返回实际暂存数。 */
export const consoleStage = (project: string, paths: string[]) =>
  invoke<number>("console_stage", { project, paths });

export const getHistory = (project: string | null, all: boolean) =>
  invoke<HistoryEntry[]>("get_history", { project, all });

export const getHistoryProjects = () =>
  invoke<ProjectInfo[]>("get_history_projects");

export const historyCount = () => invoke<number>("history_count");

export const trimHistory = (limit: number) =>
  invoke<number>("trim_history", { limit });

export const deleteHistoryEntries = (ids: string[]) =>
  invoke<number>("delete_history_entries", { ids });

export const clearAllHistory = () => invoke<number>("clear_all_history");

export const resolveHistorySessionTitles = (
  requests: HistorySessionTitleRequest[]
) =>
  invoke<HistorySessionTitleResult[]>("resolve_history_session_titles", {
    requests,
  });

export const applyWindowEffect = (effect: WindowEffect) =>
  invoke<void>("apply_window_effect", { effect });

export const startSpeech = (locale: string) =>
  invoke<void>("start_speech", { locale });

export const stopSpeech = () => invoke<void>("stop_speech");

export const flushSpeech = () => invoke<void>("flush_speech");

export const speechAvailable = () => invoke<boolean>("speech_available");

export const cursorHookStatus = () => invoke<HookStatus>("cursor_hook_status");

export const cursorHookInstall = () => invoke<string>("cursor_hook_install");

export const cursorHookUpdate = () => invoke<string>("cursor_hook_update");

export const cursorHookUninstall = () => invoke<string>("cursor_hook_uninstall");

export const cursorHookReveal = () => invoke<void>("cursor_hook_reveal");

export const claudeHookStatus = () =>
  invoke<ClaudeHookStatus>("claude_hook_status");

export const claudeHookInstall = () => invoke<string>("claude_hook_install");

export const claudeHookUpdate = () => invoke<string>("claude_hook_update");

export const claudeHookUninstall = () =>
  invoke<string>("claude_hook_uninstall");

export const claudeHookReveal = () => invoke<void>("claude_hook_reveal");

export const agentRuleStatus = (agent: AgentId) =>
  invoke<RuleStatus>("agent_rule_status", { agent });

export const agentRuleInstall = (agent: AgentId) =>
  invoke<string>("agent_rule_install", { agent });

export const agentRuleUpdate = (agent: AgentId) =>
  invoke<string>("agent_rule_update", { agent });

export const agentRuleUninstall = (agent: AgentId) =>
  invoke<string>("agent_rule_uninstall", { agent });

export const agentRuleReveal = (agent: AgentId) =>
  invoke<void>("agent_rule_reveal", { agent });

export const agentRuleOpen = (agent: AgentId) =>
  invoke<void>("agent_rule_open", { agent });

export const agentModeStatus = (agent: AgentId) =>
  invoke<AgentModeStatus>("agent_mode_status", { agent });

export const agentModeSet = (agent: AgentId, mode: AgentMode) =>
  invoke<void>("agent_mode_set", { agent, mode });

export const agentModeUpdate = (agent: AgentId) =>
  invoke<void>("agent_mode_update", { agent });

export const agentModeUpdateArtifact = (
  agent: AgentId,
  artifact: "rule" | "hook" | "mcp",
) => invoke<void>("agent_mode_update_artifact", { agent, artifact });

export const agentPermissionSet = (agent: AgentId, enabled: boolean) =>
  invoke<void>("agent_permission_set", { agent, enabled });

export const agentStopSet = (agent: AgentId, enabled: boolean) =>
  invoke<void>("agent_stop_set", { agent, enabled });

export const agentAskQuestionSet = (agent: AgentId, enabled: boolean) =>
  invoke<void>("agent_ask_question_set", { agent, enabled });

export const mcpConfigReveal = (agent: AgentId) =>
  invoke<void>("mcp_config_reveal", { agent });

export const mcpConfigOpen = (agent: AgentId) =>
  invoke<void>("mcp_config_open", { agent });

export const mcpCommandPath = () => invoke<string>("mcp_command_path");

export const agentHookReveal = (agent: AgentId) =>
  invoke<void>("agent_hook_reveal", { agent });

export const agentHookOpen = (agent: AgentId) =>
  invoke<void>("agent_hook_open", { agent });

export const agentLifecycleStatus = (agent: AgentKind) =>
  invoke<LifecycleStatus>("agent_lifecycle_status", { agent });

export const agentLifecycleInstall = (agent: AgentKind) =>
  invoke<string>("agent_lifecycle_install", { agent });

export const agentLifecycleUninstall = (agent: AgentKind) =>
  invoke<string>("agent_lifecycle_uninstall", { agent });

/** Focus the exact registered terminal surface. Failures are handled silently by callers. */
export const focusAgentTerminal = (
  pid?: number | null,
  launchId?: string | null
) => invoke<void>("focus_agent_terminal", { pid, launchId });

/** 手动把某 agent 置为「空闲」（纠正漏 hook 卡「工作中」）。即发即走，daemon 改后推回新快照。 */
export const agentForceIdle = (sessionId: string) =>
  invoke<void>("agent_force_idle", { sessionId });

/** 打开某 agent 的插话 composer 窗口（经统一宿主路由，每 session 全局单窗）。 */
export const openInterject = (
  sessionId: string,
  kind: string | null,
  cwd: string | null,
) => invoke<void>("open_interject", { sessionId, kind, cwd });

/** 插话窗口初始化：登记 composer 打开 + 取待送达预填全文。 */
export const interjectInit = (sessionId: string) =>
  invoke<InterjectInit>("interject_init", { sessionId });

/** 提交插话（整体覆盖待送达队列；空文本＝清空），随后后端关连接、关窗口。 */
export const interjectSubmit = (
  sessionId: string,
  text: string,
  filePaths: string[] = [],
  pastedImages: ImageAttachment[] = [],
) => invoke<void>("interject_submit", { sessionId, text, filePaths, pastedImages });

/** 取消插话（队列不动），后端关连接、关窗口。 */
export const interjectCancel = (sessionId: string) =>
  invoke<void>("interject_cancel", { sessionId });

/** 撤回某 session 的全部待送达插话。 */
export const interjectClear = (sessionId: string) =>
  invoke<void>("interject_clear", { sessionId });

export const telegramTest = (args: TelegramTestArgs) =>
  invoke<string>("telegram_test", { args });

export const dingtalkTest = (args: DingTalkTestArgs) =>
  invoke<string>("dingtalk_test", { args });

export const dingtalkDetectPrepare = (args: DingTalkDetectArgs) =>
  invoke<string>("dingtalk_detect_prepare", { args });

export const dingtalkDetectWait = (args: DingTalkWaitArgs) =>
  invoke<string>("dingtalk_detect_wait", { args });

export const feishuTest = (args: FeishuTestArgs) =>
  invoke<string>("feishu_test", { args });

export const feishuDetectPrepare = (args: FeishuDetectArgs) =>
  invoke<string>("feishu_detect_prepare", { args });

export const feishuDetectWait = (args: FeishuWaitArgs) =>
  invoke<string>("feishu_detect_wait", { args });

export const slackTest = (args: SlackTestArgs) =>
  invoke<string>("slack_test", { args });

export const slackDetectPrepare = (args: SlackDetectArgs) =>
  invoke<string>("slack_detect_prepare", { args });

export const slackDetectWait = (args: SlackWaitArgs) =>
  invoke<string>("slack_detect_wait", { args });

// 取消正在进行的「自动识别」等待（三家共用）。
export const detectCancel = () => invoke<void>("detect_cancel");

// ===== 版本自更新 =====

export const getAppVersion = () => invoke<string>("get_app_version");

export const updateCheck = (manual: boolean) =>
  invoke<UpdateInfo>("update_check", { manual });

export const updateGetNotes = (aggregate: boolean) =>
  invoke<string>("update_get_notes", { aggregate });

export const updateGetVersionNotes = (version: string) =>
  invoke<string>("update_get_version_notes", { version });

export const updateApply = () => invoke<void>("update_apply");

export const updatePrepare = () => invoke<void>("update_prepare");

export const updateDismiss = (version: string) =>
  invoke<void>("update_dismiss", { version });

export const restartSettings = () => invoke<void>("restart_settings");

/** 渠道健康快照（R7）：各渠道最近未恢复的故障；daemon 未运行返回空。 */
export const channelHealth = () =>
  invoke<ChannelIssue[]>("channel_health");

export const popupUpdateState = () =>
  invoke<PushedUpdateState>("popup_update_state");

// ===== 项目级待办队列（spec todo-whats-next D7/D9）：直读直写 todos.json =====

export const todosList = (project: string) =>
  invoke<TodoEntry[]>("todos_list", { project });

export const todosAdd = (
  project: string,
  text: string,
  auto = false,
  filePaths: string[] = [],
  pastedImages: ImageAttachment[] = []
) =>
  invoke<TodoEntry>("todos_add", {
    project,
    text,
    auto,
    filePaths,
    pastedImages,
  });

export const todosUpdate = (
  project: string,
  id: string,
  expectedText: string,
  expectedAttachmentIds: string[],
  text: string,
  keepAttachmentIds: string[],
  addPaths: string[]
) =>
  invoke<TodoEntry>("todos_update", {
    project,
    id,
    expectedText,
    expectedAttachmentIds,
    text,
    keepAttachmentIds,
    addPaths,
  });

export const todosUpdateAttachments = (
  project: string,
  id: string,
  addPaths: string[] = [],
  removeAttachmentIds: string[] = []
) =>
  invoke<TodoEntry>("todos_update_attachments", {
    project,
    id,
    addPaths,
    removeAttachmentIds,
  });

export const todosAttachPastedImages = (
  project: string,
  id: string,
  images: ImageAttachment[]
) => invoke<TodoEntry>("todos_attach_pasted_images", { project, id, images });

export const todoAttachmentThumbnail = (
  project: string,
  todoId: string,
  attachmentId: string
) =>
  invoke<string | null>("todo_attachment_thumbnail", {
    project,
    todoId,
    attachmentId,
  });

/** 切换自动执行标记；返回新状态（条目不存在返回 null）。 */
export const todosSetAuto = (project: string, id: string, auto: boolean) =>
  invoke<boolean | null>("todos_set_auto", { project, id, auto });

/** Update pending todo text (GUI double-click edit). Returns stored text or null. */
export const todosSetText = (project: string, id: string, text: string) =>
  invoke<string | null>("todos_set_text", { project, id, text });

export const todosRemove = (project: string, id: string) =>
  invoke<boolean>("todos_remove", { project, id });

/** GUI 勾选完成：出队并写入执行历史（与 whats-next take 同路径）。 */
export const todosComplete = (project: string, id: string) =>
  invoke<boolean>("todos_complete", { project, id });

export const todosClear = (project: string) =>
  invoke<number>("todos_clear", { project });

/** 拖拽排序（GUI 待办窗口）：按给定 id 顺序重排。 */
export const todosReorder = (project: string, ids: string[]) =>
  invoke<boolean>("todos_reorder", { project, ids });

/** 清空本项目的执行历史。 */
export const todosHistoryClear = (project: string) =>
  invoke<number>("todos_history_clear", { project });

/** 执行历史（最新在前）。 */
export const todosHistory = (project: string) =>
  invoke<TodoDoneEntry[]>("todos_history", { project });

/** 从历史一键恢复回待办队列末尾。 */
export const todosRestore = (project: string, id: string) =>
  invoke<boolean>("todos_restore", { project, id });

/** 待办窗口初始化：主题 + 语言。 */
export const todosInit = () => invoke<TodosInit>("todos_init");

/** 待办窗口项目选择器（本地快路径：有待办 ∪ 最近 workspace，不连 daemon）。 */
export const todosProjects = () =>
  invoke<TodoProjectInfo[]>("todos_projects");

/** 在本地列表上合并活跃 agent 项目；前端首屏后后台调用。 */
export const todosProjectsEnriched = () =>
  invoke<TodoProjectInfo[]>("todos_projects_enriched");

/** 打开（或聚焦）项目待办窗口（经统一宿主路由，全局单窗）；`dir` 为预选项目定位目录。 */
export const openTodos = (dir: string | null) =>
  invoke<void>("open_todos", { dir });

// ===== 「新建 Agent 任务」窗口（spec gui-agent-task-launch）=====

/** 打开（或聚焦）新建任务窗口；可带预选项目 key 与待办 id（待办行入口）。 */
export const openNewTask = (project?: string | null, todo?: string | null) =>
  invoke<void>("open_new_task", { project: project ?? null, todo: todo ?? null });

/** 新建任务窗口初始化：主题 + 语言 + 提交快捷键 + 权限选择方式。 */
export const newTaskInit = () => invoke<NewTaskInit>("new_task_init");

/** 项目候选（本地快路径：workspace 索引 + 待办项目）。 */
export const newTaskProjects = () =>
  invoke<NewTaskProject[]>("new_task_projects");

/** 项目候选（含五家有界冷扫描合并）；首屏后后台调用。 */
export const newTaskProjectsRefreshed = () =>
  invoke<NewTaskProject[]>("new_task_projects_refreshed");

/** 目录 → 项目 key（git 根，回退自身）；按所选 workspace 读取所属项目待办。 */
export const projectKeyOf = (dir: string) =>
  invoke<string>("project_key_of", { dir });

/** Start a task through the private LaunchRecord and platform-terminal bridge. */
export const newTaskLaunch = (payload: {
  workspace: string;
  kind: string;
  permission: "agent-default" | "yolo";
  task: string;
  todoProject?: string | null;
  todoId?: string | null;
  todoAttachments?: import("./types").TodoAttachmentSnapshot[];
}) =>
  invoke<void>("new_task_launch", {
    workspace: payload.workspace,
    kind: payload.kind,
    permission: payload.permission,
    task: payload.task,
    todoProject: payload.todoProject ?? null,
    todoId: payload.todoId ?? null,
    todoAttachments: payload.todoAttachments ?? [],
  });

// ===== Native Agent session Fork =====

export const openForkTask = (session: string) =>
  invoke<void>("open_fork_task", { session });

export const forkTaskInit = (session: string) =>
  invoke<ForkTaskInit>("fork_task_init", { session });

export const forkTaskLaunch = (payload: {
  session: string;
  permission: "agent-default" | "yolo";
  task: string;
}) => invoke<void>("fork_task_launch", payload);
