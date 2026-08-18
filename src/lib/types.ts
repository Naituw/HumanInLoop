export type OutputFormat = "text" | "json";

export interface AskRequest {
  id: string;
  isMarkdown: boolean;
  message: MessagePrompt;
  questions: Question[];
  /** 严格选择：禁用自由文本 / 回复附件，只能勾选预设项（全局）。 */
  selectOnly: boolean;
  /** 单选：每题恰好一个选择（默认多选，全局）。 */
  single: boolean;
  /** 结果输出格式（全局；仅影响 CLI 输出，弹窗不关心）。 */
  outputFormat: OutputFormat;
  /** whats-next 提问（spec todo-whats-next D2/D7）：待办已是问题选项，折叠待办区只留增删。 */
  whatsNext?: boolean;
}

export type ConfirmFieldKind = "text" | "path" | "timestamp";
export type ConfirmActionRole = "primary" | "default" | "destructive";

export interface ConfirmField {
  id: string;
  label: string;
  value: string;
  kind: ConfirmFieldKind;
}

export interface ConfirmDetail {
  summary: string;
  bodyMd: string;
}

/** 前缀档位元数据（D51）：同 group 的 choice 是同一动作的不同泛化档位。 */
export interface ChoiceVariant {
  group: string;
  level: number;
  levelLabel: string;
  /** Exact token chunk added at this level; old daemons fall back to levelLabel. */
  segmentLabel?: string;
  recommended: boolean;
}

export interface ConfirmChoice {
  id: string;
  label: string;
  description: string;
  role: ConfirmActionRole;
  variant?: ChoiceVariant | null;
}

export interface ConfirmInput {
  id: string;
  visibleWhenActionId: string;
  label: string;
  placeholder: string;
  maxChars: number;
}

export interface ConfirmPresentation {
  type: "singleSelectSubmit";
  input?: ConfirmInput | null;
  submitLabel: string;
  defaultActionId?: string | null;
}

export interface ConfirmRequest {
  id: string;
  title: string;
  context: ConfirmField[];
  detail: ConfirmDetail;
  choices: ConfirmChoice[];
  presentation: ConfirmPresentation;
  dismissActionId: string;
  createdAtMs: number;
  expiresAtMs: number;
}

export type SnapshotStatus =
  | "payload_only"
  | "snapshot_ready"
  | "new_file"
  | "protected_path"
  | "timeout"
  | "too_large"
  | "too_many_files"
  | "non_utf8"
  | "not_regular_file"
  | "unreadable"
  | "source_mismatch"
  | "unsupported";

export type PermissionFileChangeKind =
  | "added"
  | "modified"
  | "deleted"
  | "moved"
  | "proposed";

export type PermissionDiffLineKind = "context" | "add" | "delete" | "meta";

export interface PermissionDiffLine {
  kind: PermissionDiffLineKind;
  oldLine?: number | null;
  newLine?: number | null;
  text: string;
}

export interface PermissionDiffHunk {
  oldStart?: number | null;
  newStart?: number | null;
  header: string;
  lines: PermissionDiffLine[];
}

export interface PermissionDiffFile {
  changeKind: PermissionFileChangeKind;
  oldPath?: string | null;
  newPath: string;
  snapshotStatus: SnapshotStatus;
  hunks: PermissionDiffHunk[];
  additions: number;
  deletions: number;
  omittedHunks: number;
  omittedLines: number;
}

export interface PermissionDiffModel {
  requestId: string;
  snapshotStatus: SnapshotStatus;
  snapshotAtMs?: number | null;
  files: PermissionDiffFile[];
  totalFiles: number;
  additions: number;
  deletions: number;
  omittedFiles: number;
  omittedHunks: number;
  omittedLines: number;
  truncated: boolean;
}

export interface PatchLine {
  kind: PermissionDiffLineKind;
  text: string;
}

export interface PatchHunk {
  header: string;
  lines: PatchLine[];
}

export interface PatchFile {
  kind: "add" | "update" | "delete" | "move";
  oldPath?: string | null;
  newPath: string;
  hunks: PatchHunk[];
}

export type PermissionEditOperation =
  | {
      type: "textReplace";
      path: string;
      oldText: string;
      newText: string;
      replaceAll: boolean;
    }
  | { type: "wholeFileWrite"; path: string; content: string }
  | { type: "patchSet"; files: PatchFile[] }
  | { type: "unsupported"; reason: "notebook_edit" | "invalid_payload" };

export interface PermissionEditIntent {
  agentKind: string;
  nativeTool: string;
  workspace: string;
  operation: PermissionEditOperation;
  initialDiff?: PermissionDiffModel | null;
}

export type InteractionRequest =
  | { type: "ask"; request: AskRequest }
  | { type: "confirm"; request: ConfirmRequest };

export interface MessagePrompt {
  text: string;
  files: FileAttachment[];
}

/** 单个预定义选项：文本 + 是否为提问方（AI）的推荐答案。 */
export interface OptionItem {
  text: string;
  recommended: boolean;
  /** whats-next / Stop 卡待办 chip 对应的待办条目 id（spec todo-whats-next D2/D5）。 */
  todoId?: string | null;
  todoText?: string | null;
  todoAttachments?: TodoAttachmentSnapshot[];
}

export type TodoAttachmentStorage = "managed" | "reference";

/** One attachment owned by or referenced from a project todo. */
export interface TodoAttachmentView {
  id: string;
  name: string;
  size: number;
  isImage: boolean;
  sourcePath: string;
  /** Effective path opened or delivered to an Agent. */
  path: string;
  storage: TodoAttachmentStorage;
  available: boolean;
}

export interface TodoAttachmentSnapshot {
  id: string;
  name: string;
  path: string;
  sourcePath: string;
  storage: TodoAttachmentStorage;
}

/** 项目级待办条目（spec todo-whats-next D1）。 */
export interface TodoEntry {
  id: string;
  text: string;
  createdAtMs: number;
  /** Agent family that added the todo through the CLI; absent for human-created and legacy rows. */
  agentKind?: string | null;
  /** 自动执行：whats-next 时不提问直接派发（后端 auto=false 时省略该字段）。 */
  auto?: boolean;
  attachments?: TodoAttachmentView[];
}

/** 已执行的历史待办（仅执行出队进历史）。 */
export interface TodoDoneEntry {
  id: string;
  text: string;
  createdAtMs: number;
  /** Preserved Agent origin from the pending todo. */
  agentKind?: string | null;
  doneAtMs: number;
  attachments?: TodoAttachmentView[];
}

/** 待办窗口项目选择器候选（spec todo-whats-next D9）。 */
export interface TodoProjectInfo {
  /** 项目 key（git 根路径）。 */
  key: string;
  /** 显示名（basename）。 */
  name: string;
  /** 该项目当前待办条数。 */
  count: number;
  /**
   * 选择器分组：
   * - `withTodos`：当前有待办的项目
   * - `recent`：最近工作过的项目（活跃 Agent / workspace；不含已在 withTodos 出现的 key）
   */
  section: "withTodos" | "recent" | string;
}

/** 待办窗口 init 负载。 */
export interface TodosInit {
  theme: ThemeMode;
  lang: string;
  /** 与弹窗一致的提交快捷键（添加待办）。 */
  popupSubmitKey: PopupSubmitKey;
  /** Whether a supported platform terminal is available for creating Agent tasks. */
  newTaskSupported: boolean;
}

/** 新建任务窗口 init 负载（spec gui-agent-task-launch）。 */
export interface NewTaskInit {
  theme: ThemeMode;
  lang: string;
  /** 与弹窗一致的提交快捷键（⌘↵ 启动任务）。 */
  popupSubmitKey: PopupSubmitKey;
  /** `agentTasks.permissionPrompt`（G6）。 */
  permissionPrompt: "ask" | "agent-default" | "yolo" | string;
}

/** 新建任务窗口的项目下拉候选。 */
export interface NewTaskProject {
  /** workspace 路径（canonical cwd）或待办项目 git 根。 */
  path: string;
  /** 显示名（basename）。 */
  label: string;
  /** 置顶 workspace（列表已按置顶排序；展示加 ★）。 */
  pinned: boolean;
  /** `workspace`（最近 workspace 索引）或 `todos`（仅存在于待办存储）。 */
  source: "workspace" | "todos" | string;
}

export interface ForkTaskSource {
  sessionId: string;
  seq: number;
  kind: AgentKind;
  title: string;
  cwd: string;
  state: AgentRunState;
  forkedFromSessionId?: string | null;
  /** Runtime-probed native Fork capability for this active source session. */
  forkReady?: boolean;
}

export interface ForkTaskInit {
  theme: ThemeMode;
  lang: string;
  popupSubmitKey: PopupSubmitKey;
  permissionPrompt: "ask" | "agent-default" | "yolo" | string;
  source: ForkTaskSource;
}

export interface Question {
  message: string;
  predefinedOptions: OptionItem[];
}

export interface FileAttachment {
  path: string;
  name: string;
  size: number;
  isImage: boolean;
}

export interface ImageAttachment {
  data: string;
  mediaType: string;
  filename?: string | null;
}

export type ThemeMode = "system" | "light" | "dark";

export type PopupAnimation = "none" | "document" | "alert";

export type WindowEffect = "glass" | "blur" | "solid";

export interface PopupInit {
  /** Current interaction. A prewarmed popup returns null until assigned. */
  interaction: InteractionRequest | null;
  /** Local-popup-only native edit intent for permission confirmations. */
  popupEdit?: PermissionEditIntent | null;
  theme: ThemeMode;
  alwaysOnTop: boolean;
  sourceName: string;
  /** 来源 workspace 完整路径（hover 显示）；空表示未知，前端隐藏该元素。 */
  project: string;
  /** workspace 目录名（标题区展示）。 */
  projectName: string;
  /** 发起本次提问的 agent 家族（claude/codex/cursor/grok）；空表示未识别，不显示 agent badge。 */
  agentKind?: string | null;
  /** 发起本次提问的 agent 进程 pid；「聚焦终端」用。 */
  agentPid?: number | null;
  /** daemon 严格匹配到活动 Agent 记录的会话 ID；有值才显示 Agent Window 快捷入口。 */
  agentConsoleSessionId?: string | null;
  /** 界面语言原始值（auto/en/zh）；弹窗据此 applyLanguage，免再走 get_settings()。 */
  language?: string;
  /** 语音识别语言（BCP-47，如 zh-CN；auto 跟随系统）。 */
  speechLanguage?: string;
  /** 语音输入快捷键（规范串如 cmd+d；空串=关闭）。 */
  speechShortcut?: string;
  /** 提交快捷键：cmdEnter（默认）或 enter。 */
  popupSubmitKey?: PopupSubmitKey;
  /** 实验：多问题弹窗纵向同时显示所有问题（默认关 = 旧版一次一题）。 */
  verticalQuestions?: boolean;
  /** 性能埋点是否开启（helper 收到 ASKHUMAN_PERF_ID）；前端据此决定是否上报 perf 标记。 */
  perf?: boolean;
  /** 性能测试：画完首帧后自动取消弹窗（仅 harness 用）。 */
  perfAutodismiss?: boolean;
  /** Whether this helper started as a hidden prewarmed popup before it adopted the interaction. */
  warm?: boolean;
  /** 提问创建时刻（epoch 毫秒）：弹窗据此显示相对时间（几秒/分钟/小时前），超过一天显示绝对时间。0=未知。 */
  createdAtMs?: number;
}

export interface QuestionAnswer {
  selectedOptions: string[];
  userInput: string;
  images: ImageAttachment[];
  files: string[];
  /** 折叠待办区选中的待办条目 id（spec todo-whats-next D7）：文本已并入 userInput，id 供后端出队。 */
  todoIds?: string[];
  todoSelections?: Array<{
    id: string;
    attachments: TodoAttachmentSnapshot[];
  }>;
}

export interface PopupSubmission {
  answers: QuestionAnswer[];
}

export type ChannelAction = "send" | "cancel";

/** One question's recorded answer in history (paths only, no base64). */
export interface HistoryAnswer {
  selectedOptions: string[];
  userInput?: string | null;
  /** Saved image file paths (best-effort to display). */
  images: string[];
  /** Reply file paths (best-effort to display). */
  files: string[];
}

/** One recorded reply (one per request: the winning terminal result). */
export interface HistoryEntry {
  id: string;
  timestampMs: number;
  project: string;
  source: string;
  /** Caller agent family (claude/codex/cursor/grok); absent on legacy entries. */
  agentKind?: string | null;
  /** Native Agent conversation/session id; absent on legacy or unbound entries. */
  agentSessionId?: string | null;
  /** AskHuman MCP server process id; a fallback partition when no native session is known. */
  mcpInstanceId?: string | null;
  /** Channel that submitted / cancelled: popup / dingding / feishu / telegram. */
  channel: string;
  action: ChannelAction;
  isMarkdown: boolean;
  message: MessagePrompt;
  questions: Question[];
  answers: HistoryAnswer[];
}

/** Aggregated project info for the history window's project picker. */
export interface ProjectInfo {
  key: string;
  name: string;
  count: number;
  lastMs: number;
}

/** Trustworthy session partition used by the history filter. */
export type HistorySessionRef =
  | { type: "agent"; agentKind: string; sessionId: string }
  | { type: "mcp"; project: string; instanceId: string }
  | { type: "unbound" };

/** One aggregated session option derived from the currently loaded history entries. */
export interface HistorySessionGroup {
  token: string;
  ref: HistorySessionRef;
  count: number;
  lastMs: number;
}

/** Batch title lookup sent only for exact native Agent sessions. */
export interface HistorySessionTitleRequest {
  token: string;
  agentKind: string;
  sessionId: string;
}

export interface HistorySessionTitleResult {
  token: string;
  title: string;
}

/** Popup-originated initial/retarget filter for the global history window. */
export type HistoryOpenTarget =
  | { type: "agent"; agentKind: string; sessionId: string }
  | { type: "mcp"; project: string; instanceId: string };

export interface HistoryOpenRequest {
  all: boolean;
  project?: string | null;
  target?: HistoryOpenTarget | null;
}

/** History window init payload. */
export interface HistoryInit {
  theme: ThemeMode;
  /** 界面语言（已解析为 en/zh）；历史窗口据此 applyLanguage。 */
  lang: string;
  project: string;
  projectName: string;
}

/** Agent 控制台（状态窗口）init 负载。 */
export interface AgentsInit {
  theme: ThemeMode;
  lang: string;
  /** 与弹窗一致的提交快捷键（输入框 ⌘↵ 发送）。 */
  popupSubmitKey: PopupSubmitKey;
  /** Whether a supported platform terminal is available for creating Agent tasks. */
  newTaskSupported: boolean;
}

export type AgentKind = "claude" | "codex" | "cursor" | "grok";

/** 生命周期 hook 安装状态（实验区开关据此渲染）。 */
export interface LifecycleStatus {
  installed: boolean;
  outdated: boolean;
  supported: boolean;
}

export type AgentRunState = "working" | "idle" | "ended";

/** 单个被追踪 agent（一条 session）的快照记录。 */
export interface AgentRecord {
  /** 稳定数字编号（当前 daemon 生命周期内单调、不复用）；供 IM `/status <编号>` 寻址。 */
  seq?: number;
  kind: AgentKind;
  sessionId: string;
  pid?: number | null;
  title?: string | null;
  cwd?: string | null;
  /** Direct parent session for a branch created by AskHuman's native Fork flow. */
  forkedFromSessionId?: string | null;
  /** AskHuman-created terminal task UUID; required for exact Windows Terminal focus. */
  launchId?: string | null;
  /** Runtime-probed native Fork capability for this active source session. */
  forkReady?: boolean;
  startedAt: number;
  lastActivity: number;
  state: AgentRunState;
  endedAt?: number | null;
  /** 所在终端类型（apple-terminal/iterm2/windows-terminal/vscode/…）；用于聚焦按钮显隐。 */
  terminal?: string | null;
  /** 实时「当前工具」（hook 上报，仅 snapshot、不落盘）：`{name, object?, at}`。GUI 暂不消费。 */
  currentTool?: { name: string; object?: string | null; at: number } | null;
  /** 有待送达的插话消息（daemon 注入；驱动「待送达」徽标与撤回按钮）。 */
  pendingInterject?: boolean;
  /** 在途 AskHuman 提问的请求 id（daemon 注入，spec gui-agent-console C7/R2）：
   *  🙋 徽标 + 「去回答」精确聚焦对应弹窗。 */
  waitingRequestId?: string | null;
  /** 在途提问的摘要预览（等待横幅展示；可缺省）。 */
  waitingPreview?: string | null;
  /** 累计有效工作时长（秒，registry snapshot 含当前区间的生效总值）。 */
  activeElapsedSecs?: number | null;
}

// ===== Agent 控制台（spec gui-agent-console）=====

/** 焦点会话详情帧的工具步（daemon `frame_detail_json`）。 */
export interface DetailStep {
  /** 结构化类别：run/read/write/other（本地化由前端完成）。 */
  kind: "run" | "read" | "write" | "other" | string;
  /** kind=other 时的原始工具名。 */
  name?: string | null;
  object?: string | null;
  state: "running" | "done" | "failed" | string;
}

/** 焦点会话详情帧的 TODO 条目。 */
export interface DetailTodo {
  content: string;
  state: "pending" | "inProgress" | "completed" | string;
}

/** 完整会话事件（console_transcript 输出，tagged；spec gui-agent-console C14）。 */
export type TranscriptEventJson =
  | { type: "user"; text: string; at?: number | null; atLabel?: string | null }
  | { type: "assistant"; text: string; at?: number | null; atLabel?: string | null }
  | { type: "thinking"; text: string; at?: number | null; atLabel?: string | null }
  | {
      type: "tool";
      label: string;
      object?: string | null;
      isError: boolean;
      resultSummary?: string | null;
      at?: number | null;
      atLabel?: string | null;
    }
  | {
      type: "ask";
      kind?: "ask" | "whatsNext";
      message: string;
      questions: { text: string; answer?: string | null }[];
      at?: number | null;
      atLabel?: string | null;
    }
  | { type: "meta"; text: string };

/** 完整会话一页（`[start, start+events.length)` 窗口）。 */
export interface TranscriptPage {
  events: TranscriptEventJson[];
  start: number;
  total: number;
  truncatedHead: boolean;
  partial: boolean;
}

/** 项目未暂存变更统计的一行（console_diff_stat；spec gui-agent-console C15）。 */
export interface DiffFileStat {
  path: string;
  /** M 修改 / D 删除 / A 新增 / B 二进制。 */
  kind: "M" | "D" | "A" | "B" | string;
  adds: number;
  dels: number;
}

export interface DiffStatPage {
  /** git 根（stage 调用沿用）。 */
  root: string;
  files: DiffFileStat[];
}

/** 单文件 hunk 视图（console_diff_file）。 */
export interface DiffFileView {
  path: string;
  kind: string;
  skipped: boolean;
  skipReason?: string | null;
  lines: { kind: "add" | "del" | "context" | "header" | string; text: string }[];
}

/** 焦点会话详情帧（tagged，daemon 签名变化才推；spec gui-agent-console C8/R5）。 */
export interface AgentDetailFrame {
  type: "watchFrame" | string;
  sessionId: string;
  seq: number;
  kindLabel: string;
  phase: "working" | "idle" | "waiting" | "ended" | string;
  title?: string | null;
  project?: string | null;
  text?: string | null;
  steps: DetailStep[];
  stepsOmitted: number;
  todos: DetailTodo[];
  activeElapsedSecs?: number | null;
  at?: number | null;
}

/** 插话 composer 窗口 init 负载。 */
export interface InterjectInit {
  theme: ThemeMode;
  lang: string;
  /** 待送达全文（预填编辑；空 = 无待送达）。 */
  text: string;
  /** 待送达条数。 */
  entries: number;
  /** Flattened pending attachment references. */
  attachments: InterjectAttachment[];
}

export interface InterjectAttachment extends FileAttachment {
  available: boolean;
}

export interface InterjectPending {
  text: string;
  entries: number;
  attachments: InterjectAttachment[];
}

export type UiLanguage = "auto" | "en" | "zh";

/** Popup/Confirm submit key mode (mirrors Rust `PopupSubmitKey`). */
export type PopupSubmitKey = "cmdEnter" | "enter";

/** Global collaboration style for agent prompts (mirrors Rust `CollaborationStyle`). */
export type CollaborationStyle = "aligned" | "autonomous" | "custom";

export interface GeneralConfig {
  theme: ThemeMode;
  /** 界面语言：auto（跟随系统）/ en / zh。回退英文。 */
  language: UiLanguage;
  alwaysOnTop: boolean;
  appearAnimation: PopupAnimation;
  windowEffect: WindowEffect;
  /** 语音识别语言（BCP-47，如 "zh-CN"）；"auto" 跟随系统首选语言。 */
  speechLanguage: string;
  /** 语音输入快捷键（弹窗内）。规范串如 "cmd+d"；空串表示关闭。 */
  speechShortcut: string;
  /**
   * Popup/Confirm submit shortcut:
   * - `cmdEnter`: ⌘/Ctrl+Enter submits (default); bare Enter newlines
   * - `enter`: bare Enter submits; any modifier+Enter newlines
   */
  popupSubmitKey: PopupSubmitKey;
  /** 协作风格：对齐 / 自主 / 自定义。 */
  collaborationStyle: CollaborationStyle;
  /** 自定义协作风格正文；空则回退对齐默认。 */
  collaborationStyleCustomText: string;
  /** 回复历史保留条数上限。默认 200；0 = 停止新增记录（但保留旧记录）。 */
  historyLimit: number;
  /** 待办执行历史保留条数（每项目）。默认 100；0 = 停止新增记录（保留旧历史）。 */
  todoHistoryLimit: number;
  /** Built-in popup sound. Empty disables it; macOS stores a name, other desktops use a toggle. */
  popupSound: string;
  /** Menu bar / system-tray status icon mode (off/active/always). */
  menuBarIcon: MenuBarIconMode;
  /** Popup pre-warm (faster popups by keeping one mounted, hidden helper ready). Default true. */
  popupPrewarm: boolean;
  /** Daemon lifecycle: activity（按需起+空闲退出）/ keepalive（常驻+开机自启）。 */
  daemonLifecycle: DaemonLifecycleMode;
}

/** Menu bar / tray status icon mode (mirrors Rust `MenuBarIconMode`). */
export type MenuBarIconMode = "off" | "active" | "always";

/** Daemon lifecycle mode (mirrors Rust `DaemonLifecycleMode`). */
export type DaemonLifecycleMode = "activity" | "keepalive";

/** Popup sound support: kind="named" with names, "toggle", or "none". */
export interface PopupSoundSupport {
  kind: "named" | "toggle" | "none";
  names: string[];
}

export interface PopupChannelConfig {
  enabled: boolean;
  width: number;
  height: number;
  rememberSize: boolean;
}

export interface TelegramChannelConfig {
  enabled: boolean;
  botToken: string;
  chatId: string;
  apiBaseUrl: string;
}

export interface DingTalkChannelConfig {
  enabled: boolean;
  clientId: string;
  clientSecret: string;
  userId: string;
  cardTemplateId: string;
  confirmCardTemplateId: string;
  permissionConfirmCardTemplateId: string;
  inlineSmallText: boolean;
  convertTextToDocx: boolean;
}

export interface FeishuChannelConfig {
  enabled: boolean;
  appId: string;
  appSecret: string;
  openId: string;
  baseUrl: string;
}

export interface SlackChannelConfig {
  enabled: boolean;
  botToken: string;
  appToken: string;
  userId: string;
}

export interface ChannelsConfig {
  popup: PopupChannelConfig;
  telegram: TelegramChannelConfig;
  dingding: DingTalkChannelConfig;
  feishu: FeishuChannelConfig;
  slack: SlackChannelConfig;
  /** 「IM 渠道按需发送」开关（默认关；显式配置的用户设置保持原值）。 */
  autoActivation: boolean;
  /** 「自动结束 watch」——「按需发送」子开关（默认开，仅 autoActivation 开时生效）。 */
  autoEndWatch: boolean;
}

/** 实验性功能开关（默认隐藏；开启后显示「实验」Tab）。 */
export interface ExperimentalConfig {
  enabled: boolean;
  /** 多问题弹窗纵向同时显示所有问题（默认关 = 旧版一次一题）。 */
  verticalQuestions: boolean;
}

/** 权限确认相关全局设置（spec codex-permission-remember）。 */
export interface PermissionsConfig {
  /** Codex shell 宽松模式全局开关（D52）：非危险且可解析的 shell 命令自动放行。 */
  codexRelaxedShell: boolean;
}

export type AgentTaskPermission = "ask" | "agent-default" | "yolo";

export interface AgentTasksConfig {
  enabled: boolean;
  permissionPrompt: AgentTaskPermission;
}

export interface AgentTaskWorkspace {
  path: string;
  label: string;
  lastUsedAt: number;
  agents: AgentKind[];
  pinned: boolean;
  hidden: boolean;
}

export interface AgentTaskReadiness {
  kind: AgentKind;
  label: string;
  command: string;
  executable: string | null;
  binaryReady: boolean;
  lifecycleReady: boolean;
  integrationReady: boolean;
  integrationMode: string;
  ready: boolean;
  diagnostics: string[];
}

export interface AppConfig {
  general: GeneralConfig;
  channels: ChannelsConfig;
  agentTasks: AgentTasksConfig;
  permissions: PermissionsConfig;
  experimental: ExperimentalConfig;
}

/** Whether each channel secret is currently stored (drives the "Saved" placeholder). */
export interface SecretsPresent {
  dingdingSecret: boolean;
  feishuSecret: boolean;
  telegramToken: boolean;
  slackBotToken: boolean;
  slackAppToken: boolean;
}

/** Settings payload: config with secrets blanked + per-secret presence flags. */
export interface SettingsPayload {
  config: AppConfig;
  secretsPresent: SecretsPresent;
}

/** 一条渠道故障摘要（R7，镜像 Rust `ipc::ChannelIssueInfo`）：出现即表示该渠道仍未恢复。 */
export interface ChannelIssue {
  /** 渠道 id："telegram" / "dingding" / "feishu" / "slack"。 */
  channel: string;
  /** 错误文案（源语言英文，与 daemon.log 一致）。 */
  message: string;
  /** 首次出现的 Unix 毫秒时间戳。 */
  atMs: number;
}

// ===== Codex 权限授权管理面板（spec codex-permission-remember §6.3，镜像 Rust ipc 类型）=====

/** 一个对话的授权摘要（镜像 `permission_rules::SessionRuleSummary`）。 */
export interface PermissionSessionSummary {
  sessionId: string;
  ruleCount: number;
  fileExactCount: number;
  projectRoots: string[];
  fullDisk: boolean;
  shellCount: number;
  networkCount: number;
  mcpCount: number;
  /** 该会话 YOLO 模式开启中（D53）。 */
  yolo: boolean;
  lastUsedAtMs: number;
}

/** 面板分组：store 摘要 + registry 标题/项目名增强（可为空串）。 */
export interface PermissionSessionGroup {
  summary: PermissionSessionSummary;
  title: string;
  projectName: string;
}

export type PermissionRuleKind =
  | "fileExact"
  | "fileProject"
  | "fileDisk"
  | "mcpTool"
  | "networkHost"
  | "shellExact"
  | "shellPrefix"
  | "shellRelaxed"
  | "yolo";

/** 一条规则展示行（D48：原样键文本）。 */
export interface PermissionRuleInfo {
  kind: PermissionRuleKind;
  display: string;
  createdAtMs: number;
  lastUsedAtMs: number;
  expiresAtMs: number;
}

export type PermissionRulesOp =
  | { op: "summaries" }
  | { op: "sessionDetail"; sessionId: string }
  | { op: "globalDetail" }
  | { op: "resetSession"; sessionId: string }
  | { op: "resetGlobal" }
  | { op: "disableYolo"; sessionId: string };

export type PermissionRulesResult =
  | { kind: "summaries"; sessions: PermissionSessionGroup[]; globalCount: number }
  | { kind: "rules"; rules: PermissionRuleInfo[] }
  | { kind: "reset"; removed: number };

/** Per-secret edit intent sent on save. Secrets never round-trip through the config object. */
export type SecretAction =
  | { kind: "unchanged" }
  | { kind: "set"; value: string }
  | { kind: "clear" };

export interface SecretActions {
  dingdingSecret: SecretAction;
  feishuSecret: SecretAction;
  telegramToken: SecretAction;
  slackBotToken: SecretAction;
  slackAppToken: SecretAction;
}

export interface HookStatus {
  installed: boolean;
  outdated: boolean;
  hooksJsonExists: boolean;
  supported: boolean;
}

export interface ClaudeHookStatus {
  installed: boolean;
  outdated: boolean;
  settingsExists: boolean;
  supported: boolean;
}

export type AgentId = "cursor" | "claude" | "codex" | "grok";

export interface UpdateInfo {
  available: boolean;
  currentVersion: string;
  latestVersion: string;
  releaseNotes: string;
  sourceUrl: string;
  isNpm: boolean;
  applyMode: UpdateApplyMode;
  manualCommand: string;
}

export type UpdateApplyMode = "automatic" | "manualDirect" | "manualNpm";

export interface PushedUpdateState {
  available: boolean;
  latestVersion: string;
  pending: boolean;
  applyMode: UpdateApplyMode;
}

/** 调用方 agent 的异步解析结果（方案5/b）：daemon walk 出家族 + pid 后经 `agent-resolved` 后推弹窗。 */
export interface PushedAgent {
  kind?: string | null;
  pid?: number | null;
  launchId?: string | null;
}

export interface RuleStatus {
  installed: boolean;
  outdated: boolean;
  path: string;
  supported: boolean;
}

/** Agent 集成模式（三态互斥）。 */
export type AgentMode = "none" | "cli" | "mcp";

/** 某家 Agent 的模式聚合状态（驱动设置页三态分段控件 + 产物清单）。 */
export interface AgentModeStatus {
  mode: AgentMode;
  needsUpdate: boolean;
  ruleNeedsUpdate: boolean;
  hookNeedsUpdate: boolean;
  mcpNeedsUpdate: boolean;
  rulePath: string;
  ruleInstalled: boolean;
  timeoutHookSupported: boolean;
  timeoutHookInstalled: boolean;
  timeoutHookNeedsUpdate: boolean;
  recoveryHookInstalled: boolean;
  permission: PermissionStatus;
  permissionNeedsUpdate: boolean;
  stop: StopStatus;
  askQuestion: AskQuestionStatus;
  mcpConfigPath: string;
  mcpConfigInstalled: boolean;
}

/** 接管 Claude 内置 AskUserQuestion 的开关状态（仅 Claude Code 支持）。 */
export interface AskQuestionStatus {
  supported: boolean;
  enabled: boolean;
  installed: boolean;
  outdated: boolean;
}

export interface StopStatus {
  supported: boolean;
  enabled: boolean;
  installed: boolean;
  outdated: boolean;
  otherHandlersDetected: boolean;
}

export interface PermissionStatus {
  supported: boolean;
  unsupportedReason: string | null;
  enabled: boolean;
  configured: boolean;
  outdated: boolean;
  needsUpdate: boolean;
  knownBlockedReason: string | null;
  otherHandlersDetected: boolean;
}

export interface TelegramTestArgs {
  botToken: string;
  chatId: string;
  apiBaseUrl: string;
}

export interface DingTalkTestArgs {
  clientId: string;
  clientSecret: string;
  userId: string;
}

export interface DingTalkDetectArgs {
  clientId: string;
  clientSecret: string;
}

export interface DingTalkWaitArgs {
  clientId: string;
  clientSecret: string;
  code: string;
}

export interface FeishuTestArgs {
  appId: string;
  appSecret: string;
  openId: string;
  baseUrl: string;
}

export interface FeishuDetectArgs {
  appId: string;
  appSecret: string;
  baseUrl: string;
}

export interface FeishuWaitArgs {
  appId: string;
  appSecret: string;
  baseUrl: string;
  code: string;
}

export interface SlackTestArgs {
  botToken: string;
  appToken: string;
  userId: string;
}

export interface SlackDetectArgs {
  botToken: string;
  appToken: string;
}

export interface SlackWaitArgs {
  botToken: string;
  appToken: string;
  code: string;
}
