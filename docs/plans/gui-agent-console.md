# Agent 状态窗口改版为双栏「Agent 控制台」—— 开发计划

> spec: `docs/specs/gui-agent-console.md`（C1–C16 + 前瞻预留 R1–R5）
> 视觉/交互基准: `src/prototype/agent-console.html`（原型，mock 数据，浏览器直跑）

## 概述

把 `AgentsView.vue` 从单栏卡片列表重构为双栏控制台：边栏（项目分组会话 + 最近项目 + 「＋」）
+ 详情区（Watch 帧 / 完整会话 / 交互区插槽 / diff 状态条 / 内嵌新建任务表单）。daemon 侧新增
「焦点会话」子订阅（复用 watch 引擎推帧）、快照补 `waitingRequestId`；transcript 与 diff 由
GUI Host 进程直调本地模块（不经 daemon）。

## 1. daemon：快照扩展 + 焦点会话子订阅

### 1.1 快照补 `waitingRequestId`（C7/R2）

- `daemon/request.rs`：新增 `RequestRegistry::in_flight_agent_requests() -> Vec<(String, String)>`
  （session_id → request_id；ask 与 confirm 都算，同 session 多请求取最早登记的）。
- `daemon/unix_impl/subs.rs::agents_snapshot_for_gui`：注入 `waitingRequestId`（同
  `pendingInterject` 的注入模式；IM /status 等其它 snapshot 消费方不注入）。
- 提问创建 / 完结时已有 `broadcast_agents_state` 调用点则徽标自动实时；缺的调用点补上
  （`request.rs` 登记/完结 → 通知 subs，复用现有 watch Notify 时机）。
- 前端 `AgentRecord` 补 `waitingRequestId?: string | null` 与 `activeElapsedSecs?: number`
  （后者 registry snapshot 已有、TS 类型缺声明）。

### 1.2 焦点会话子订阅（C8/R5）

- `ipc/mod.rs`：
  - `ClientMsg` 新增 `AgentsFocus { session_id: Option<String> }`——在**既有 agents 订阅连接**上
    发送（None = 取消焦点）；旧 daemon 不会收到（新前端只随新二进制发布，同二进制无兼容问题）。
  - `ServerMsg` 新增 `AgentDetail { detail: serde_json::Value }`，`detail` 为 tagged 结构
    `{ type: "watchFrame", sessionId, phase, title, project, text, steps, stepsOmitted, todos,
    activeElapsedSecs, at }`（R5：未来可加新 type 变体）。steps/todos 结构与
    `watch::WatchFrame` 字段一一对应（serde 序列化 `ToolStep`/`TodoItem` 需补 `Serialize`）。
- `daemon/unix_impl/subs.rs::handle_agents_sub`：读端从「只探测 EOF」改为消息循环——收到
  `AgentsFocus` 更新该订阅者的焦点 session（存入 `ServerState` 的 `gui_focus` 表：
  订阅者 tx → session_id + 上次签名）。断开时清除表项。
- 帧推送复用 watch 引擎节奏（`daemon/unix_impl/watch.rs`）：
  - `watch_tick` 末尾追加「GUI 焦点」处理：对每个 gui_focus 项按 `watch::build_frame`
    （snapshot 记录 + `in_flight` waiting 标志）算帧与签名，签名变化才推 `AgentDetail`；
  - tick 自适应节奏沿用（焦点会话工作中 2s / 空闲 10s；`has_gui_focus` 并入「有订阅」判定）；
  - AgentEvent / 提问创建 / 答复完结的 Notify 已唤醒同一 tick，无需新触发源；
  - 不入 `WatchState::entries`（不占 IM 名额、不持久化、无跟底/终态语义——窗口关闭即停）。
- 闲退守卫：agents 订阅连接本就计入 `active`，焦点订阅不需要额外保活逻辑。

### 1.3 插话（C3/C9）

- `ipc/mod.rs` 的 `InterjectAppend` 携带文本与可选附件（独立连接即发即走）——控制台输入框直接用；
  文本与附件均为空不发送，附件-only 合法。
- 待送达气泡文本：`ipc/mod.rs` 新增一问一答 `InterjectPeek { session_id }` →
  `ServerMsg::InterjectState`（复用现有结构；dispatch 中独立连接处理，回完即断），返回文本、条目数与
  展平后的附件引用。快照的 `pendingInterject` 驱动显隐，气泡内容经 Peek 按需取（选中会话变化/
  快照变化时）。

### 1.4 `open_agents` 可寻址（C10/R4）

- `gui_host/mod.rs`：`HostMsg::OpenWindow` 复用既有 `project` 槽位模式，新增
  `session: Option<String>`（serde default 兼容）；`app/gui_host.rs::open_window` 透传。
- `app/mod.rs::create_agents_window`：已开窗 → `set_focus` + emit `agents-goto`
  （payload `{ session }`）；新建 → URL `?view=agents&session=...`。
- 托盘 Agent 子菜单点击某 agent：从「开 composer」改为 `open_window(Agents, session)`
  （C10 的入口收敛；托盘「发送消息」子项保留原 composer 行为不变，本期不动）。

## 2. GUI Host 直调命令（commands.rs，均 cfg unix）

### 2.1 完整会话分页（C14）

- `console_transcript(kind, session_id, before: Option<usize>, limit: usize)`：
  `spawn_blocking(transcript_full::load_events)`，进程内缓存
  `(kind, session, transcript mtime) -> TranscriptDoc`（mtime 变即重解析；解析本身受
  `MAX_READ_BYTES` 上限保护）。返回
  `{ events: [...], total, truncatedHead, partial }`——`before`=事件绝对下标游标，
  `limit` 固定 200；事件序列化为 tagged JSON（user/assistant/tool/meta + askHuman 块）。
- `truncatedHead=true` 且翻到最早页时，前端显示「更早内容超出解析上限」而非「已到会话开头」。

### 2.2 AskHumanBlock 结构化（C14）

- `agents/transcript_full.rs`：`AskHumanBlock` 改为
  `{ message: String, questions: Vec<AskQA { text: String, answer: Option<String> } > }`；
  - MCP `ask`：`message` 参数 + `questions[].question` 逐条入列；
  - CLI：`extract_askhuman_cli_question` 扩展解析全部 `-q`/`--question`（引号感知的轻量
    tokenizer；message 取位置参数 / `--stdin` 场景降级为空）；
  - 答案：`parse_askhuman_answer` 扩展——输出含 `# Qn` 分组时按组回填到对应下标，
    单问题输出维持现状（回填到唯一条目）；`selected_options` 与 `user_input` 合并规则不变；
  - 无 `-q` 的调用 = `questions` 单条（text 空、只有 message + answer），渲染层兜底；
  - IM `/transcript` 渲染器（`export/`）改为从结构化字段拼回现有单串文案（输出不变），
    既有测试断言维持。

### 2.3 项目 diff + stage（C15/C16）

- `gitutil.rs`：新增 `stage_paths(root, paths: &[String]) -> Result<StageResult>`（`git add --`，
  路径来自本进程 `build_diff_model` 输出，不接受自由输入）；`build_diff_model` 拆出轻量
  `diff_stat(root) -> Vec<FileStat { path, kind, adds, dels }>`（`git diff --numstat --status`
  两级懒加载的第一级）。
- `commands.rs` 新增（async + `spawn_blocking`，进程级互斥 `Mutex` + 10s 超时，C16）：
  - `console_diff_stat(project)` → 状态条数据；
  - `console_diff_file(project, path)` → 单文件 hunks（第二级，展开时才调）;
  - `console_stage(project, paths: Vec<String>)` → 单文件与全部共用（全部=传全量路径）。
- 刷新时机在前端（§3.5）；Rust 侧只保证互斥 + 超时 + 失败静默返回上次错误文案。

### 2.4 其它

- `focus_request(request_id)`：发 `ClientMsg::FocusRequest`（「去回答」，托盘同款链路）。
- `interject_append(session_id, text, file_paths, pasted_images)` / `interject_peek(session_id)`：对应 §1.3。
- `agents_focus(session_id: Option<String>)`：经 GUI Host 的 agents 订阅连接发送
  `AgentsFocus`（订阅连接句柄在 `app/mod.rs` 的订阅任务持有，经 channel 传入待发队列）。

## 3. 前端重构（`views/AgentsView.vue` → 双栏控制台）

组件拆分（新目录 `src/views/console/`，AgentsView.vue 变编排层）：

- `Sidebar.vue`：通用 key 分组（R3）：项目组（会话按 工作中→等待→空闲→已结束、时间倒序）
  + 「最近项目」折叠组（`new_task_projects(false)` 的 workspace 候选中滤掉已有会话的项目）；
  组头徽标（工作中数 + 待办数 `todos_count`）+ hover「＋」（macOS + Terminal 门控，
  复用 `todos_init.newTaskSupported` 口径）。
- `DetailHeader.vue`：家族 + 标题 + 项目路径 + 状态行（指示器组件）+ 动作按钮
  （聚焦终端 / 项目待办 / 手动置空闲——现有命令平移）。
- `StatusInd.vue`（C13）：sine_3x3 点阵组件（帧表 + 175ms 全局共享 ticker）、静态 4 点空闲、
  🙋 等待、已结束无；颜色变量入 `styles/tokens.css`（`--ind-working` 深浅两套）。
- `WatchPane.vue`（C2）：消费 `AgentDetail` 帧（`agent-detail` Tauri 事件）：Markdown 文字 +
  足迹时间线 + TODO 面板 + 等待横幅（`waitingRequestId` → 「去回答」`focus_request`）。
- `TranscriptPane.vue`（C14）：`console_transcript` 分页（200/页、向上加载、滚动锚定、
  sticky 计数行）；AskHuman 问答卡（多问题 Q1/Qn 子块、长 message 4 行折叠）。
- `InteractSlot.vue`（R1，优先级切换）：插话输入（working 且非 grok；`interject_append` +
  待送达气泡 `interject_peek`/撤回 `interjectClear`）/ Grok 提示 / 空闲提示 + 新建任务快捷 /
  已结束提示。
- `DiffBar.vue`（C15/C16）：状态条 + 文件列表 + hunk 懒加载 + 暂存（单文件直接 / 全部行内
  确认）。刷新触发：选中会话或项目变化、展开面板、`AgentDetail` 帧含 编辑/写入 步（2s 防抖）、
  窗口 focus 事件；进行中的请求未返回不重发（前端与 Rust 双保险）。
- `NewTaskPane.vue`（C4）：抽取 NewTaskView 的表单域逻辑为 `views/newtask/useNewTaskForm.ts`
  组合式（项目候选 / 待办来源 / readiness / 权限 / launch），窗口版与内嵌版共用；内嵌版项目
  锁定为「＋」所在项目、启动成功后按 cwd + startedAt 匹配自动选中新会话（±120s 内最新）。
- 窗口版 NewTaskView 行为不变（仅内部改用共享组合式）。
- 交互细节：↑↓ 切会话、⌘N 新任务、⌘↵ 发送；`agents-goto` 事件选中目标会话；顶部过滤
  全部/工作中/空闲（C6，移除三维切换与对应 localStorage 键）；窗口记住尺寸
  （同 popup `rememberSize` 模式，默认 ~980×640 / min 760×520，`create_agents_window` 调整）。

## 4. i18n 与样式

- `src/i18n/zh.ts` / `en.ts`：新增 `console.*` 键组（过滤、最近项目、等待横幅、完整会话、
  分页按钮、问答卡、diff 状态条、暂存确认、交互区提示等）；删除 `agents.view.*` 三维切换键。
- `styles/tokens.css`：`--ind-working`（深浅两套）；diff 红绿沿用既有 `--diff-*` token。
- 原型 `src/prototype/` 保留（对照基准），不参与 `pnpm build` 产物（Vite 多页仅 dev 可达；
  确认 build 只以 `index.html` 为入口）。

## 5. 文档

- `docs/overview.md`：`views/AgentsView.vue` 描述改「Agent 控制台（双栏）」+ console/ 目录；
  「前端 ↔ 后端命令」补新命令；「Agent 生命周期追踪 + 状态窗口」节补一句焦点订阅与入口；
- `docs/overview-im-commands.md` 不改（IM 行为无变化；/transcript 输出不变）。
- 完成后按 PROGRESS 约定删除对应 section。

## 6. 实施顺序（每阶段末 `cargo test` + `pnpm build`）

1. **daemon 数据面**：waitingRequestId（§1.1）+ AgentsFocus/AgentDetail 焦点订阅（§1.2）+
   InterjectPeek（§1.3）；Rust 单测：in_flight_agent_requests、焦点帧签名门控、snapshot 注入。
2. **前端骨架**：双栏布局 + Sidebar + DetailHeader + StatusInd + 过滤/键盘/选中；WatchPane 接
   `AgentDetail`；交互区插槽接插话（append + peek + 撤回）；旧卡片视图代码移除。
3. **完整会话**：AskHumanBlock 结构化（§2.2，先行，含 IM 渲染回归测试）→ console_transcript
   分页命令 + TranscriptPane。
4. **diff 状态条**：gitutil stage_paths/diff_stat + 三命令 + DiffBar（含 C16 时机）；
   Rust 单测：stage_paths 路径白名单、diff_stat 解析。
5. **新建任务内嵌 + 寻址**：useNewTaskForm 抽取 + NewTaskPane + 「＋」/空闲快捷入口 +
   open_agents(session)（§1.4）+ 托盘入口改造 + 启动后自动选中。
6. **i18n / 样式 / 文档收尾**；`./scripts/install.sh` 后真机验收：多 agent 并发下边栏状态、
   等待横幅去回答、插话送达与回执、完整会话长 session 分页、大仓库 diff 首开耗时、
   「＋」启动全链路（经 AskHuman 批准后做真实 Agent 启动）。

## 7. 风险与对策

- **`ToolStep`/`TodoItem` 无 Serialize**：为二者补 derive（IM 渠道渲染不受影响）；签名函数
  复用 `watch::signature`，帧与 IM 完全同源。
- **AskHumanBlock 改动波及 IM /transcript**：渲染层拼回单串保持输出不变 + 既有测试守护；
  CLI `-q` tokenizer 只做引号感知的 best-effort，解析失败降级整串进 message（不 panic）。
- **大仓库 git 慢**（C16，用户强调）：两级懒加载 + 互斥 + 超时 + 无常驻轮询；首开状态条为
  异步占位（「统计中…」），超时显示「变更统计不可用」。
- **transcript 超上限**：`MAX_READ_BYTES` 截头由 `truncatedHead` 透传，前端如实标注。
- **焦点订阅泄漏**：gui_focus 表项随连接断开清除；daemon 重启后前端重连自动重发
  `AgentsFocus`（订阅任务重连循环里补发当前焦点）。
