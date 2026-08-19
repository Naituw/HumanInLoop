// Agent 控制台原型的 mock 数据与「活会话」模拟脚本（纯前端，无后端依赖）。
// 数据形状对齐 spec gui-agent-console：registry 快照 + WatchFrame 的合体（原型简化版）。

export type RunState = "working" | "idle" | "ended";
export type Kind = "claude" | "codex" | "cursor" | "grok" | "pi";
export type StepState = "running" | "done" | "failed";
export type TodoState = "pending" | "inProgress" | "completed";

export interface StepM {
  label: string;
  object?: string;
  state: StepState;
}

export interface TodoM {
  content: string;
  state: TodoState;
}

/** 一帧 Watch 内容（模拟脚本按 tick 替换到会话上）。 */
export interface FrameM {
  text?: string;
  steps?: StepM[];
  stepsOmitted?: number;
  todos?: TodoM[];
}

export interface SessionM {
  id: string;
  kind: Kind;
  title: string;
  /** 项目 key（原型直接用路径）。 */
  projectPath: string;
  state: RunState;
  /** 有在途 AskHuman 提问（🙋 覆盖 working/idle）。 */
  waiting: boolean;
  /** 在途提问摘要（waiting 时展示在横幅里）。 */
  waitingQuestion?: string;
  startedAt: number;
  lastActivity: number;
  endedAt?: number;
  activeElapsedSecs: number;
  // Watch 帧内容
  text?: string;
  steps: StepM[];
  stepsOmitted: number;
  todos: TodoM[];
  /** 待送达插话消息（追加语义）。 */
  pendingMsgs: string[];
}

export interface ProjectM {
  path: string;
  name: string;
  todoCount: number;
}

export const KIND_LABEL: Record<Kind, string> = {
  claude: "Claude Code",
  codex: "Codex",
  cursor: "Cursor",
  grok: "Grok",
  pi: "Pi",
};

const now = () => Math.floor(Date.now() / 1000);

// ── 项目 ──

export const PROJECTS: ProjectM[] = [
  { path: "/Users/emily/Developer/HumanInLoop", name: "HumanInLoop", todoCount: 2 },
  { path: "/Users/emily/Developer/WebApp", name: "WebApp", todoCount: 1 },
];

/** 「最近项目」折叠区（workspace 索引，无活跃会话）。 */
export const RECENT_PROJECTS: ProjectM[] = [
  { path: "/Users/emily/Developer/StatusBoard", name: "StatusBoard", todoCount: 0 },
  { path: "/Users/emily/Developer/blog", name: "blog", todoCount: 0 },
];

/** 各项目待办（「＋」新建任务表单的任务来源）。 */
export const PROJECT_TODOS: Record<string, { text: string; auto?: boolean }[]> = {
  "/Users/emily/Developer/HumanInLoop": [
    { text: "同步 Codex shell 判定对拍结果到 PROGRESS" },
    { text: "调研 Cursor 用户级 Skill 迁移", auto: true },
  ],
  "/Users/emily/Developer/WebApp": [{ text: "修复移动端登录跳转循环" }],
  "/Users/emily/Developer/StatusBoard": [],
  "/Users/emily/Developer/blog": [],
};

// ── 会话 ──

export function initialSessions(): SessionM[] {
  const t = now();
  return [
    {
      id: "s-cursor-refactor",
      kind: "cursor",
      title: "重构 daemon 空闲退出逻辑",
      projectPath: "/Users/emily/Developer/HumanInLoop",
      state: "working",
      waiting: false,
      startedAt: t - 23 * 60,
      lastActivity: t - 3,
      activeElapsedSecs: 21 * 60,
      text: "我先看一下 registry 的现有实现，确认空闲判定的边界条件。",
      steps: [
        { label: "读取", object: "registry.rs", state: "done" },
        { label: "读取", object: "lifecycle.rs", state: "done" },
        { label: "搜索", object: "idle_deadline", state: "running" },
      ],
      stepsOmitted: 0,
      todos: [
        { content: "定位空闲判定重置时机问题", state: "inProgress" },
        { content: "修改 registry 与 lifecycle", state: "pending" },
        { content: "跑单测验证", state: "pending" },
        { content: "更新 spec 文档", state: "pending" },
      ],
      pendingMsgs: [],
    },
    {
      id: "s-claude-audit",
      kind: "claude",
      title: "Codex 权限判定对拍",
      projectPath: "/Users/emily/Developer/HumanInLoop",
      state: "working",
      waiting: true,
      waitingQuestion:
        "对拍发现 exec_policy.rs 有 3 处差异：先改 port 对齐上游，还是先抬已审计版本？",
      startedAt: t - 48 * 60,
      lastActivity: t - 70,
      activeElapsedSecs: 45 * 60,
      text:
        "对拍完成：`bash.rs` 与 `is_safe_command.rs` 与上游一致；`exec_policy.rs` 有 **3 处差异**" +
        "（fallback 判定 2 处、amendment 派生 1 处），差异明细我已经整理好了。",
      steps: [{ label: "运行命令", object: "AskHuman (提问中)", state: "running" }],
      stepsOmitted: 1,
      todos: [
        { content: "对拍 bash.rs 脚本拆分", state: "completed" },
        { content: "对拍 command_safety heuristics", state: "completed" },
        { content: "对拍 exec_policy fallback", state: "inProgress" },
        { content: "抬升已审计版本常量", state: "pending" },
      ],
      pendingMsgs: [],
    },
    {
      id: "s-cursor-watch-tests",
      kind: "cursor",
      title: "补充 watch 跟底重发单测",
      projectPath: "/Users/emily/Developer/HumanInLoop",
      state: "ended",
      waiting: false,
      startedAt: t - 5 * 3600,
      lastActivity: t - 2 * 3600,
      endedAt: t - 2 * 3600,
      activeElapsedSecs: 52 * 60,
      text: "全部 9 个新增用例通过，跟底节流与提问期间抑制的边界都覆盖到了。",
      steps: [{ label: "运行命令", object: "cargo test watch::", state: "done" }],
      stepsOmitted: 0,
      todos: [],
      pendingMsgs: [],
    },
    {
      id: "s-codex-login",
      kind: "codex",
      title: "修复登录跳转循环",
      projectPath: "/Users/emily/Developer/WebApp",
      state: "idle",
      waiting: false,
      startedAt: t - 95 * 60,
      lastActivity: t - 12 * 60,
      activeElapsedSecs: 34 * 60,
      text:
        "修复完成：`redirect_uri` 在 SSO 回跳后没有清掉 state 参数，导致再次进入登录循环。" +
        "已在 `auth/callback.ts` 里收敛，等你验证。",
      steps: [
        { label: "编辑", object: "auth/callback.ts", state: "done" },
        { label: "运行命令", object: "pnpm test auth", state: "done" },
      ],
      stepsOmitted: 0,
      todos: [
        { content: "定位循环跳转原因", state: "completed" },
        { content: "修复 callback 参数清理", state: "completed" },
        { content: "补充回归用例", state: "completed" },
      ],
      pendingMsgs: [],
    },
    {
      id: "s-grok-release",
      kind: "grok",
      title: "梳理发布流程文档",
      projectPath: "/Users/emily/Developer/WebApp",
      state: "working",
      waiting: false,
      startedAt: t - 9 * 60,
      lastActivity: t - 20,
      activeElapsedSecs: 8 * 60,
      text: "我在逐节核对现有发布文档与 CI 工作流的差异，稍后给出需要更新的清单。",
      steps: [
        { label: "读取", object: ".github/workflows/release.yml", state: "done" },
        { label: "读取", object: "docs/release.md", state: "running" },
      ],
      stepsOmitted: 0,
      todos: [],
      pendingMsgs: [],
    },
  ];
}

// ── 「活会话」模拟脚本：按 tick 循环推进 s-cursor-refactor 的帧 ──

export const LIVE_SESSION_ID = "s-cursor-refactor";

export const LIVE_SCRIPT: FrameM[] = [
  {
    steps: [
      { label: "搜索", object: "idle_deadline", state: "done" },
      { label: "读取", object: "runtime/mod.rs", state: "running" },
    ],
  },
  {
    text:
      "已确认问题：宽限期内收到 hook 事件时 `idle_deadline` 被**整体重置**，" +
      "而 spec 要求只有真正回到 Working 才重置。改动集中在 `registry.rs` 的两处。",
    steps: [{ label: "编辑", object: "registry.rs", state: "running" }],
    todos: [
      { content: "定位空闲判定重置时机问题", state: "completed" },
      { content: "修改 registry 与 lifecycle", state: "inProgress" },
      { content: "跑单测验证", state: "pending" },
      { content: "更新 spec 文档", state: "pending" },
    ],
  },
  {
    steps: [
      { label: "编辑", object: "registry.rs", state: "done" },
      { label: "编辑", object: "lifecycle.rs", state: "running" },
    ],
  },
  {
    steps: [
      { label: "编辑", object: "registry.rs", state: "done" },
      { label: "编辑", object: "lifecycle.rs", state: "done" },
      { label: "运行命令", object: "Run unit tests (cargo test)", state: "running" },
    ],
    todos: [
      { content: "定位空闲判定重置时机问题", state: "completed" },
      { content: "修改 registry 与 lifecycle", state: "completed" },
      { content: "跑单测验证", state: "inProgress" },
      { content: "更新 spec 文档", state: "pending" },
    ],
  },
  {
    text:
      "单测通过 **12/12**。现在跑一遍 daemon 相关的集成用例，确认宽限期语义没有回归。",
    steps: [
      { label: "运行命令", object: "cargo test --workspace daemon::", state: "running" },
    ],
    stepsOmitted: 2,
  },
  {
    steps: [
      { label: "运行命令", object: "cargo test --workspace daemon::", state: "done" },
      { label: "编辑", object: "docs/specs/agent-lifecycle-tracking.md", state: "running" },
    ],
    stepsOmitted: 2,
    todos: [
      { content: "定位空闲判定重置时机问题", state: "completed" },
      { content: "修改 registry 与 lifecycle", state: "completed" },
      { content: "跑单测验证", state: "completed" },
      { content: "更新 spec 文档", state: "inProgress" },
    ],
  },
  {
    text: "文档也同步好了。我把改动整理成一次提交，然后给你一个变更摘要。",
    steps: [{ label: "运行命令", object: "git diff --stat", state: "running" }],
    stepsOmitted: 0,
  },
  // 循环回开头前的过渡帧
  {
    text: "我先看一下 registry 的现有实现，确认空闲判定的边界条件。",
    steps: [
      { label: "读取", object: "registry.rs", state: "done" },
      { label: "读取", object: "lifecycle.rs", state: "done" },
      { label: "搜索", object: "idle_deadline", state: "running" },
    ],
    stepsOmitted: 0,
    todos: [
      { content: "定位空闲判定重置时机问题", state: "inProgress" },
      { content: "修改 registry 与 lifecycle", state: "pending" },
      { content: "跑单测验证", state: "pending" },
      { content: "更新 spec 文档", state: "pending" },
    ],
  },
];

// ── 完整会话（transcript）mock：分页加载用的长事件流 ──

export interface TxEvent {
  kind: "user" | "assistant" | "tool" | "ask";
  /** user / assistant 的文字。 */
  text?: string;
  /** tool 的类别词与对象。 */
  label?: string;
  object?: string;
  failed?: boolean;
  /** ask（AskHuman 问答，对应真实解析器的 AskHumanBlock）：共享 message（可为长 Markdown）。 */
  question?: string;
  /** 单问题的回答；无回答（在途/取消）时为空。 */
  answer?: string;
  /** 多问题形态：每个问题独立携带自己的回答（真实实现 = 扩展 AskHumanBlock 为结构化）。 */
  questions?: { q: string; a?: string }[];
  /** Unix 秒。 */
  at: number;
}

// 直接敲进 Agent TUI 的 User Prompt（真实场景很少，只偶尔出现）。
const TX_USER_SAMPLES = [
  "帮我看一下空闲判定的宽限期逻辑，感觉有会话提前被置空闲了。",
  "这段可以简化吗？我觉得分支太多了。",
];

// AskHuman 问答（真实场景中人类输入的主要来源）。
const TX_ASK_SAMPLES: { question: string; answer: string }[] = [
  {
    question: "宽限期内收到 hook 事件是否应该重置计时？spec 里有两种读法。",
    answer: "不重置，只有真正回到 Working 才重置。",
  },
  {
    question: "宽限期要不要顺便改成可配置项？",
    answer: "不用，保持固定 5 分钟。",
  },
  {
    question: "单测覆盖这些边界够吗？还需要补别的场景吗？",
    answer: "够了，再补一个跨重启恢复的就行。",
  },
  {
    question: "文档更新放主 overview 还是单独 spec？",
    answer: "单独 spec，overview 只留一句引用。",
  },
  {
    question: "改动完成，要我顺手把相邻的 watch 收尾逻辑也重构了吗？",
    answer: "不要，保持本次改动最小。",
  },
];

const TX_ASSISTANT_SAMPLES = [
  "我先通读 `registry.rs` 的状态推导部分，确认 `lastActivity` 的写入时机。",
  "找到问题了：宽限期内收到 hook 事件时计时被整体重置，而 spec 要求只有真正回到 Working 才重置。",
  "两处改动完成，现在跑单测验证边界条件。",
  "单测全部通过，我再补一个 daemon 重启后恢复宽限期的集成用例。",
  "文档已同步，状态机图里补上了宽限期的自环转移。",
  "这个分支确实可以合并：`ended` 和记录消失走同一条收尾路径即可。",
];

const TX_TOOLS: { label: string; object: string }[] = [
  { label: "读取", object: "registry.rs" },
  { label: "搜索", object: "idle_deadline" },
  { label: "编辑", object: "registry.rs" },
  { label: "读取", object: "runtime/mod.rs" },
  { label: "运行命令", object: "cargo test registry::" },
  { label: "编辑", object: "lifecycle.rs" },
  { label: "读取", object: "docs/specs/agent-lifecycle-tracking.md" },
  { label: "运行命令", object: "cargo build" },
];

/** 生成一条确定性的长会话事件流（旧→新），供分页加载演示。 */
export function makeTranscript(sessionId: string, count: number, endAt: number): TxEvent[] {
  // 简易确定性伪随机（按 sessionId 播种），保证两次打开内容一致。
  let seed = 0;
  for (const ch of sessionId) seed = (seed * 31 + ch.charCodeAt(0)) >>> 0;
  const rnd = () => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 0xffffffff;
  };
  const events: TxEvent[] = [];
  let at = endAt - count * 20;
  // 会话以一条真实 User Prompt 开头（初始任务），之后人类输入主要经 AskHuman 问答。
  events.push({ kind: "user", text: TX_USER_SAMPLES[0], at });
  at += 20;
  while (events.length < count) {
    events.push({
      kind: "assistant",
      text: TX_ASSISTANT_SAMPLES[Math.floor(rnd() * TX_ASSISTANT_SAMPLES.length)],
      at,
    });
    at += 10 + Math.floor(rnd() * 20);
    const tools = 2 + Math.floor(rnd() * 4);
    for (let k = 0; k < tools && events.length < count; k++) {
      const tool = TX_TOOLS[Math.floor(rnd() * TX_TOOLS.length)];
      events.push({
        kind: "tool",
        label: tool.label,
        object: tool.object,
        failed: rnd() < 0.04,
        at,
      });
      at += 5 + Math.floor(rnd() * 15);
    }
    const r = rnd();
    if (r < 0.5 && events.length < count) {
      // 主要形态：AskHuman 问答。
      const qa = TX_ASK_SAMPLES[Math.floor(rnd() * TX_ASK_SAMPLES.length)];
      events.push({ kind: "ask", question: qa.question, answer: qa.answer, at });
      at += 30 + Math.floor(rnd() * 60);
    } else if (r < 0.58 && events.length < count) {
      // 少量：直接敲的 User Prompt。
      events.push({
        kind: "user",
        text: TX_USER_SAMPLES[Math.floor(rnd() * TX_USER_SAMPLES.length)],
        at,
      });
      at += 15 + Math.floor(rnd() * 30);
    }
  }
  return events;
}

/** 多问题 + 长 message 的 AskHuman 示例（追加在 live 会话末尾，演示折叠与 Q1/Q2/Q3 子块）。 */
export function richAskEvent(at: number): TxEvent {
  return {
    kind: "ask",
    question: [
      "宽限期重构完成，单测与集成用例全部通过。合并前有三个点需要你拍板，先给你完整背景：",
      "",
      "**现状**：`idle_deadline` 在宽限期内收到任何 hook 事件都会整体重置计时，导致频繁但零碎的活动（如编辑器自动保存触发的 lifecycle 事件）可以无限续命一个实际已经空闲的会话；watch 卡因此长期停在「空闲宽限」态不收尾。",
      "",
      "**方案 A（本次实现）**：只有状态真正回到 Working 才重置宽限期计时；宽限期内的零碎事件只更新 `lastActivity`，不动 deadline。行为与 spec 原意一致，改动集中在 `registry.rs` 两处 + `lifecycle.rs` 一处。",
      "",
      "**方案 B（备选）**：为宽限期引入独立的 `graceDeadline` 字段并持久化，重启后精确恢复剩余时长。语义最精确，但要动 `agents.json` 的 schema，旧版本降级不兼容。",
      "",
      "**迁移影响**：方案 A 下旧 `agents.json` 无需迁移；方案 B 需要 schema 版本号与降级路径。两个方案的单测我都写了对照用例，A 的实现已在本分支。",
    ].join("\n"),
    questions: [
      {
        q: "宽限期语义按方案 A 收敛吗？",
        a: "A，按 spec 原意收敛，不引入新字段。",
      },
      {
        q: "旧 agents.json 需要做一次性迁移吗？",
        a: "不迁移，缺字段走默认即可。",
      },
      {
        q: "要顺手加宽限期相关的性能埋点吗？",
      },
    ],
    at,
  };
}

/** 每个会话的 mock 事件总数（演示分页：live 会话 3 页多）。 */
export const TX_COUNTS: Record<string, number> = {
  "s-cursor-refactor": 465,
  "s-claude-audit": 328,
  "s-cursor-watch-tests": 96,
  "s-codex-login": 154,
  "s-grok-release": 42,
};

// ── 项目 diff 状态 mock（对应真实 gitutil::DiffModel）──

export interface DiffLineM {
  kind: "context" | "add" | "del";
  text: string;
}

export interface DiffFileM {
  path: string;
  kind: "M" | "A" | "D";
  adds: number;
  dels: number;
  hunks: { header: string; lines: DiffLineM[] }[];
  staged: boolean;
}

export const PROJECT_DIFFS: Record<string, DiffFileM[]> = {
  "/Users/emily/Developer/HumanInLoop": [
    {
      path: "src-tauri/src/agents/registry.rs",
      kind: "M",
      adds: 96,
      dels: 31,
      staged: false,
      hunks: [
        {
          header: "@@ -412,9 +412,12 @@ impl AgentRegistry {",
          lines: [
            { kind: "context", text: "    fn on_hook_event(&mut self, ev: &AgentEvent) {" },
            { kind: "del", text: "        self.idle_deadline = None; // any event resets grace" },
            { kind: "add", text: "        // Grace period: only a real Working transition resets the deadline;" },
            { kind: "add", text: "        // stray events inside grace only refresh lastActivity." },
            { kind: "add", text: "        if ev.kind == EventKind::TurnStart {" },
            { kind: "add", text: "            self.idle_deadline = None;" },
            { kind: "add", text: "        }" },
            { kind: "context", text: "        self.last_activity = now_secs();" },
          ],
        },
      ],
    },
    {
      path: "src-tauri/src/daemon/lifecycle.rs",
      kind: "M",
      adds: 24,
      dels: 6,
      staged: false,
      hunks: [
        {
          header: "@@ -88,6 +88,10 @@ fn idle_check(state: &ServerState) {",
          lines: [
            { kind: "context", text: "    for rec in state.agents.iter() {" },
            { kind: "add", text: "        if rec.in_grace_period(now) {" },
            { kind: "add", text: "            continue; // grace keeps the daemon alive" },
            { kind: "add", text: "        }" },
            { kind: "context", text: "        // ..." },
          ],
        },
      ],
    },
    {
      path: "docs/specs/agent-lifecycle-tracking.md",
      kind: "M",
      adds: 18,
      dels: 4,
      staged: false,
      hunks: [
        {
          header: "@@ -52,4 +52,8 @@ ## 状态推导",
          lines: [
            { kind: "context", text: "- 宽限期语义：" },
            { kind: "del", text: "  - 宽限期内任何事件重置计时。" },
            { kind: "add", text: "  - 只有真正回到 Working 才重置宽限期计时；" },
            { kind: "add", text: "  - 零碎事件只刷新 lastActivity，不动 deadline。" },
          ],
        },
      ],
    },
    {
      path: "src-tauri/src/agents/registry_grace_tests.rs",
      kind: "A",
      adds: 44,
      dels: 0,
      staged: false,
      hunks: [
        {
          header: "@@ -0,0 +1,44 @@",
          lines: [
            { kind: "add", text: "#[test]" },
            { kind: "add", text: "fn grace_deadline_survives_stray_events() {" },
            { kind: "add", text: "    // ..." },
            { kind: "add", text: "}" },
          ],
        },
      ],
    },
  ],
  "/Users/emily/Developer/WebApp": [
    {
      path: "src/auth/callback.ts",
      kind: "M",
      adds: 12,
      dels: 5,
      staged: false,
      hunks: [
        {
          header: "@@ -31,7 +31,9 @@ export async function handleCallback(req) {",
          lines: [
            { kind: "context", text: "  const url = new URL(req.url);" },
            { kind: "del", text: "  return redirect(url.searchParams.get(\"redirect_uri\"));" },
            { kind: "add", text: "  const target = new URL(url.searchParams.get(\"redirect_uri\"));" },
            { kind: "add", text: "  target.searchParams.delete(\"state\"); // break the login loop" },
            { kind: "add", text: "  return redirect(target.toString());" },
          ],
        },
      ],
    },
  ],
};

// ── 新建任务表单的 Agent readiness mock ──

export interface ReadinessM {
  kind: Kind;
  ready: boolean;
  reason?: string;
}

export const READINESS: ReadinessM[] = [
  { kind: "claude", ready: true },
  { kind: "codex", ready: true },
  { kind: "cursor", ready: false, reason: "生命周期 Hook 未安装" },
  { kind: "grok", ready: false, reason: "未检测到 CLI 二进制" },
];
