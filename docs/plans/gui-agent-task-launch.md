# 从 Mac GUI 创建 Agent 任务 —— 开发计划

> spec: `docs/specs/gui-agent-task-launch.md`

## 概述

新增全局唯一的「新建任务」窗口（GUI Host 承载，label `newtask`，单页表单）：
项目下拉 + 任务来源（直接输入 / 项目待办 + 可选补充）+ Agent 列表（四家全列，就绪判定复用
`agent_launch::readiness`）+ 权限（按 `agentTasks.permissionPrompt`）+「启动任务」。
启动复用 IM 的 `create_record` + `open_terminal` 链路；成功后待办按快照出队、活跃槽
best-effort 切 `popup`、窗口自动关闭。入口：待办窗口行内按钮 + 托盘菜单项，仅
macOS 且 `terminal_available()` 时显示。

## 1. 窗口路由（gui_host + app）

### 1.1 `gui_host/mod.rs`

- `WindowKind` 新增 `NewTask`（serde lowercase → `"newtask"`）。
- `HostMsg::OpenWindow` 新增字段 `#[serde(default, skip_serializing_if = "Option::is_none")]
  todo: Option<String>`（预选待办 id；旧宿主 serde default 兼容）。`project` 槽位在本窗口
  语义下 = 预选项目 key（待办所属 git 根）。
- `host_open` / `host_open_async` / `send_open` 增加 `todo: Option<String>` 参数，既有调用点
  传 `None`。

### 1.2 `app/gui_host.rs`

- `is_hosted_label` 增加 `"newtask"`（窗口计数 / 续命判定）。
- `open_window` 增加 `todo: Option<String>` 参数（既有调用点传 `None`）：
  `WindowKind::NewTask` → `create_new_task_window(app, &cfg, project, todo, pin_above_popup)`，
  label `"newtask"` 参与建后聚焦。
- 宿主 IPC 分发（`OpenWindow` 处理处）把新 `todo` 字段透传给 `open_window`。
- 托盘菜单：操作区 `open_todos` 之后新增条目 `open_new_task`（i18n `tray.newTask`），仅
  `cfg!(target_os = "macos") && agent_launch::terminal_available()` 时加入；点击 →
  `open_window(app, WindowKind::NewTask, false, None, None, None)`。

### 1.3 `app/mod.rs`

- 新增 `create_new_task_window(manager, config, project_override, todo_override, pin)`（cfg unix）：
  - 已有 label `newtask` 窗口 → `set_focus` + emit `newtask-goto`（payload JSON
    `{ project, todo }`，两字段皆可空）；
  - 新建：URL `index.html?view=newtask[&project=...][&todo=...]`（urlencode），标题
    `title.newTask`，尺寸 520×640 / min 440×520，center，主题 / 材质 / 置顶处理与
    `create_todos_window` 相同；
  - 建窗后 `watch_todos_file(win)` 复用：`todos.json` 变化 → `todos-updated` → 前端重载
    所选项目的待办列表。
- Tauri invoke handler 注册新增命令（见 §2）。

### 1.4 `commands.rs` 路由命令

- `route_open_window` 增加 `todo: Option<String>` 参数；fallback 分支
  `WindowKind::NewTask` → `create_new_task_window`。
- 新增 `#[tauri::command] open_new_task(app, project: Option<String>, todo: Option<String>)`：
  经 `route_open_window(app, WindowKind::NewTask, false, project, todo, None)`；非 unix 返回
  `Err("unsupported")`。供待办窗口行内按钮调用。

## 2. 后端命令（commands.rs）

### 2.1 `new_task_init`

返回 `{ theme, lang, popupSubmitKey, permissionPrompt }`：现读
`AppConfig::load_without_secrets()`（同 `todos_init`，避免常驻宿主过期快照）；
`permissionPrompt` 为 `"ask" | "agent-default" | "yolo"` 字符串。

### 2.2 项目候选 `new_task_projects(refresh: bool)`

返回 `Vec<NewTaskProject { path, label, source }>`：

- `refresh=false`：`workspaces::list()` 过滤 hidden / 非 dir（本地快路径，首屏即时）；
- `refresh=true`：`tokio::task::spawn_blocking(workspaces::refresh)`（有界四家冷扫描，
  与 IM `/new` 同源；前端首屏后后台调用）；
- 两种模式都在 workspace 之后合并 `todos::all()` 的项目 key（仅现存目录、去重，
  `source: "todos"`），排序：pinned → last_used 倒序 → 待办独有项目。
- 预选项目不在候选中时由**前端**兜底追加（与 TodosView `applyProjects` 同模式）。

### 2.3 Agent 就绪 — 复用现有 `agent_task_readiness`

不新增命令；窗口直接调用（内部 `spawn_blocking(all_readiness)`，login shell 探测 ≤2s）。

### 2.4 待办列表 — 复用现有 `todos_list(project)`

前端把所选 workspace 路径经 §2.5 的 launch 及待办读取都按 git 根 project key 处理；
读取 key 由新增命令 `project_key_of(dir: String) -> String`（包一层
`project::detect_from`）换算，避免前端自行实现 git 根解析。

### 2.5 启动 `new_task_launch`

入参 `{ workspace, kind, permission, task, todoProject?, todoId? }`；
`permission` 仅接受 `"agent-default" | "yolo"`（`ask` 已在前端消解）。async 命令，核心在
`spawn_blocking` 中执行：

1. `AgentKind::parse(kind)`；task 校验：trim 非空、无 NUL、≤3000 字符（create_record 内亦有
   兜底校验）。task 由前端拼装：选待办时 = 待办原文快照 + `"\n\n"` + 补充（补充为空则仅原文）；
2. `agent_launch::create_record(LaunchSource { channel: "gui", target: "" }, workspace, kind,
   permission, task)` —— 内部完成 workspace canonicalize、readiness 复检、一次性 0600 record
   落盘（与 IM 完全同链路）；
3. `agent_launch::open_terminal(&record)`；
4. 成功后：
   - `todoProject`/`todoId` 均非空 → `todos::take(project, &[id])` best-effort 出队
     （写执行历史，失败不报错）；
   - 活跃槽切 popup（§3），tokio spawn 即发即走；
5. 任一步失败 → `Err(格式化错误)` 返回前端展示；record 留给 5 分钟 TTL 过期清理，
   不需要回滚。

命令**不**注册 PendingLaunchWatch（spec G13）。

## 3. 活跃槽切 popup（best-effort，spec G11）

- `ipc/mod.rs`：`ClientMsg` 新增无字段变体 `ActivatePopupSlot`（即发即走，无回包；旧 daemon
  解析失败断连无副作用）。
- `daemon/runtime/mod.rs` 连接分发：收到后 `set_active_channel(state, "popup").await`
  （自然获得旧 IM 反激活回执与 auto-end-watch 语义），不回包。
- `client/mod.rs`：新增 `pub async fn activate_popup_slot()`：`connect_split()` 成功则写
  `ActivatePopupSlot`（模式同 `notify_update_state_changed`，不做 Hello）；连接失败（daemon
  未运行）→ 直接 `autochannel::save_active(Some("popup"))`（daemon 启动时 `load_active`
  读回）。

## 4. 前端「新建任务」窗口（NewTaskView.vue）

### 4.1 路由与初始化

- `App.vue`：`view === "newtask"` → 异步组件 `NewTaskView`。
- 挂载：`new_task_init` 应用主题 / 语言 / 提交快捷键 / permissionPrompt；URL `?project=` /
  `?todo=` 读预选；监听 `settings-updated`（主题/语言/快捷键实时同步）、`todos-updated`
  （重载所选项目待办；若预选待办已被删，回落「直接输入」）、`newtask-goto`
  （已开窗二次打开：整体重置表单到新预选）。
- 数据加载分两拍（同 TodosView 模式）：`new_task_projects(false)` + `todos_list` 首屏即时；
  后台并行 `new_task_projects(true)`（冷扫描合并）与 `agent_task_readiness()`（Agent 区显示
  spinner 占位，结果返回后填充）。

### 4.2 表单区块（自上而下）

1. **项目**：下拉（含预选兜底追加）。切换项目 → 重新 `project_key_of` + `todos_list`，
   并清除待办预选（spec G8）。
2. **任务来源**：单选列表——首项「直接输入新任务」+ 该项目全部待办（⚡ 标记自动待办；全列出、
   区域内滚动，spec G9）。选待办 → 该行展开显示原文（只读、快照存入组件状态）；下方输入框
   label 变「补充说明（可选）」；选「直接输入」→ 输入框 label「任务描述」且必填。
3. **Agent**：四家全列（顺序 Claude Code / Codex / Cursor / Grok）。就绪：可选（radio 卡片，
   secondary 显示 `integration_mode · executable`）；未就绪：灰显不可选，行内列
   binary / lifecycle / integration 三项 ✗ 链接（就绪项打 ✓ 不可点）：
   - binary ✗ → `openPath(官方安装文档 URL)`（复用 `useAgentTasks` 的 `AGENT_INSTALL_DOCS`
     映射，抽为共享常量导出）；
   - lifecycle ✗ → `open_settings(tab = "advanced#lifecycle-<kind>")`；
   - integration ✗ → `open_settings(tab = "integration#integration-<kind>")`。
4. **权限**：`permissionPrompt === "ask"` → 单选「Agent 默认（不附加权限覆盖参数）/
   YOLO（危险徽标，自动批准操作并绕过沙箱限制）」，**不预选**；另两态 → 只读元数据行
   「权限模式：Agent 默认 / YOLO」。
5. **启动**：主按钮「启动任务」+ ⌘↵ 徽标（`popupSubmitKey` 语义同待办窗口）。禁用条件、
   busy 态与行内错误展示见 spec §3.3；成功 → `getCurrentWindow().close()`。

### 4.3 设置窗口锚点跳转

`open_settings` 的 `tab` 参数扩展为 `tab[#elementId]`：

- `SettingsView.vue`：初始 URL `?tab=` 与 `settings-goto-tab` 事件统一解析 `#` 后缀——先切
  tab，`nextTick` 后 `getElementById(elementId)` scrollIntoView + 复用
  `settingsTargetHighlight` 的短暂高亮（把 `useAgentTasks` 中 scroll+highlight 逻辑提为
  SettingsView 级共享函数，`openReadinessIssue` 改调它）；
- 后端 `create_settings_window` 对 `initial_tab` 不感知 `#`（原样进 URL / 事件），无需改动；
  `TABS.includes` 校验改为对 `#` 前段判断。
- 设置「高级」「Agents」tab 中四家对应行已有 `lifecycle-<kind>` / `integration-<kind>`
  元素 id（`openReadinessIssue` 现有目标），直接复用；若个别缺失则补 id。

### 4.4 i18n（`src/i18n/zh.ts` / `en.ts`）

新增 `newTask.*` 键组：窗口标题、区块标题（项目 / 任务 / Agent / 权限）、「直接输入新任务」、
「补充说明（可选）」、占位符、就绪三项名与 ✓/✗、权限两项及描述、启动按钮、busy、
超长 / 必填校验、错误前缀等。Rust `i18n.rs` 新增 `title.newTask`、`tray.newTask`。

## 5. 待办窗口入口（TodosView.vue + todos_init）

- `todos_init` 返回新增 `newTaskSupported: bool`（`cfg!(target_os="macos") &&
  agent_launch::terminal_available()`）。
- 待办行 `td-meta` 按钮排（编辑按钮之后）新增「创建任务」图标按钮（▶ 风格 SVG，hover 淡入，
  样式同 `.td-copy`；title/aria `todosWin.newTask`）：点击
  `openNewTask(selected, e.id)`（新 ipc 封装 → `open_new_task` 命令）。
  `newTaskSupported=false` 时不渲染。执行历史行不加。

## 6. lib/ipc.ts 与类型

- `ipc.ts` 新增：`newTaskInit`、`newTaskProjects(refresh)`、`newTaskLaunch(payload)`、
  `projectKeyOf(dir)`、`openNewTask(project?, todo?)`。
- `types.ts` 新增：`NewTaskInit`、`NewTaskProject`、`NewTaskLaunchPayload`；`TodosInit` 加
  `newTaskSupported`。复用现有 `AgentTaskReadiness`、`TodoEntry`。

## 7. 文档

- `docs/overview.md`：目录结构加 `views/NewTaskView.vue`；「前端 ↔ 后端命令」补新命令；
  「项目待办 + whats-next」或「IM 命令」节各加一句指向本 spec 的入口说明（GUI 亦可创建任务）。
- `docs/overview-im-commands.md` 不改（IM 行为无变化）。

## 8. 实施顺序

1. **后端窗口骨架**：WindowKind::NewTask + HostMsg.todo + host_open/open_window/
   route_open_window 透传 + create_new_task_window + open_new_task + App.vue 路由 +
   空 NewTaskView 可开窗（托盘项与待办按钮先接上）。
2. **数据命令**：new_task_init / new_task_projects / project_key_of + todos_init.newTaskSupported。
3. **表单 UI**：NewTaskView 完整表单（项目 / 任务来源 / Agent / 权限 / 校验），含
   readiness 灰显与跳转（§4.3 设置锚点扩展）。
4. **启动链路**：new_task_launch + ActivatePopupSlot（ipc/daemon/client）+ 成功关窗 /
   待办出队。
5. **i18n 与样式收尾**；`cargo test`（新增 Rust 单测：new_task_projects 排序去重、
   task 校验边界）+ `pnpm build` 类型检查。
6. `./scripts/install.sh` 安装验证：托盘入口、待办入口、预选行为、ask/固定权限三态、
   启动成功（需经 AskHuman 批准后才做真实 Agent 启动验收，沿用 IM D27）、失败路径、
   活跃槽切换（daemon 在跑 / 未跑两种）。
