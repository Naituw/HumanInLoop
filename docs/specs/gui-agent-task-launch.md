# 需求：从桌面 GUI 创建 Agent 任务（新建任务窗口）

> 状态：已实现（2026-07-21）。
> 关联计划：`docs/plans/gui-agent-task-launch.md`
> 依赖 / 复用：`docs/specs/im-agent-task-launch.md`（LaunchRecord + 原生终端启动链路、
> Agent readiness 判定、workspace 索引）、`docs/specs/todo-whats-next.md`（项目待办）、
> `docs/specs/menu-bar-tray.md`（GUI Host 统一窗口）。Todo 附件随任务启动的后续扩展见
> `docs/specs/todo-attachments.md`。
> 平台：macOS（Terminal.app）与 Windows（Windows Terminal `wt.exe`）；Linux 仍后置。

## 1. 背景与目标

IM `/new` 已支持从四种 IM 选择 workspace / Agent / 权限并在 macOS 或 Windows 上启动真实交互式 Agent 会话。
本需求把同一能力搬到本地 GUI：

1. 新增一个**通用的「新建 Agent 任务」独立窗口**（GUI Host 承载，全局唯一），单页表单完成
   选项目 → 选任务来源（手动输入 / 项目待办）→ 选 Agent → 选权限 → 启动；
2. 待办窗口每条待办提供「创建任务」按钮：带该项目 + 该待办打开同一窗口（即预选了项目与待办的
   同一面板），不做单独流程；
3. 托盘菜单提供「新建 Agent 任务」入口（无预选打开）；
4. Agent 可用性判定与 IM `/new` **完全一致**；启动复用 IM 的 LaunchRecord + 平台原生终端链路。

## 2. 已确认决策（用户经 AskHuman 定案）

| 编号 | 决策项 | 结论 |
|---|---|---|
| G1 | 功能门控 | **不要求**开启 `agentTasks.enabled` 实验功能；要求 macOS 的 Terminal.app 或 Windows Terminal `wt.exe` 可用。不满足时入口隐藏或给出可恢复的安装提示。 |
| G2 | 流程形态 | 通用流程 + **独立窗口**（便于复用未来更多入口）；**单页表单**，不做分步向导 |
| G3 | 面板统一 | 待办入口与菜单入口共用同一面板；待办入口＝预选了项目与该条待办；菜单入口可自由选待办或直接输入 |
| G4 | Agent 判定 | 与 IM `/new` 相同：login shell 可解析 CLI 二进制 + lifecycle installed/current + AskHuman 集成 CLI/MCP 通道产物可用（`agent_launch::readiness`，一字不改地复用） |
| G5 | Agent 展示 | 五家全部列出：就绪可选；未就绪灰显并标注原因（binary / lifecycle / integration），原因可点：binary→官方安装文档、lifecycle→设置「Agents」tab 对应卡内 capability、integration→同一 Agent 卡，并滚动定位 + 短暂高亮对应行。Pi 额外要求版本 >=0.82.0 |
| G6 | 权限 | 跟随全局 `agentTasks.permissionPrompt`：`ask` 时表单内显示「Agent 默认 / YOLO（危险）」单选（不预选）；`agent-default` / `yolo` 时不显示选择，仅以元数据展示最终模式。Pi 没有内置权限模式，选中后隐藏选择、说明安全边界并固定 `AgentDefault` |
| G7 | 待办语义 | 与 IM D29–D31 一致：待办原文只读展示 + 可选补充输入；最终任务 = 原文 + 空行 + 补充；**Terminal 成功打开后**才按快照 best-effort 出队（`todos::take`），失败保留 |
| G8 | 项目预选 | 待办入口预选该项目但**仍可改**；改选其它项目时清除待办预选，任务来源回到「直接输入」 |
| G9 | 待办数量 | 窗口内全部列出、可滚动（GUI 无 IM 渠道的 10 条限制） |
| G10 | 启动成功后 | 自动关闭窗口（Terminal 已打开，注意力已转移） |
| G11 | 活跃槽 | 启动成功后 best-effort 把 IM 活跃槽切到 `popup`（对应 IM D20 的 GUI 语义：人在电脑旁，新 Agent 的提问默认弹窗）。daemon 在运行经 IPC 切（含反激活回执 / auto-end-watch 语义）；未运行则直接写 `auto-channel.json` |
| G12 | 入口范围 | 本次实现：待办窗口行内按钮 + 托盘菜单项；窗口本身支持无预选打开，后续入口直接复用 |
| G13 | 自动 watch | GUI 入口**不**注册 PendingLaunchWatch（无来源 IM 渠道；用户就在电脑旁）。lifecycle 追踪照常经 hook 生效 |

## 3. 用户流程

### 3.1 从待办行进入

```text
待办窗口某行 hover → 「创建任务」按钮
  → 打开（或聚焦）「新建任务」窗口：
       项目：<该待办所属项目>（可改）
       任务来源：(●) 该条待办（原文只读） + [可选补充输入]
                 ( ) 直接输入新任务
                 ( ) 其它待办 …
       Agent：Claude Code ✓ / Codex ✓ / Cursor ✗(原因) / Grok ✗(原因)
       权限：Agent 默认 / YOLO（仅 permissionPrompt=ask 时显示单选）
       [启动任务]
  → 新 Terminal.app 窗口或独占 Windows Terminal 窗口启动 Agent TUI 并执行任务
  → 该待办出队进执行历史；窗口自动关闭
```

### 3.2 从托盘菜单进入

同一窗口，无预选：项目默认取候选列表首项，任务来源默认「直接输入新任务」；选中的项目有待办时
待办作为单选项列出（含 ⚡ 自动标记），选中某条待办后输入框变为可选补充。

### 3.3 校验与失败

- 「启动任务」在以下条件全部满足前禁用：已选项目、已选**就绪** Agent、权限已定
  （ask 模式下已选）、任务有效（直接输入非空；选待办时补充可空）、组合任务 ≤3000 字符；
- 启动按钮点击后进入 busy 态；失败（Terminal 拒绝 / workspace 失效 / Agent 不再就绪等）在
  窗口内显示错误并保留表单，待办**不**出队；
- 与 IM 相同，task 不进入 shell：仍走一次性 LaunchRecord（0600、5 分钟 TTL、原子 claim）+
  `AskHuman __agent-launch <uuid>`。

## 4. 项目候选与待办来源

- 项目下拉候选 = 最近 workspace 索引（`agents/workspaces.rs`，过滤 hidden 与不存在路径）
  ∪ 有待办的项目（`todos.json` 的 git 根 key）∪ 预选项目（不在前两者时兜底追加）；
  按路径字符串去重。排序：置顶 workspace → 其余 workspace 按 last_used 倒序 → 仅存在于
  待办存储的项目。
- 窗口打开先用本地索引即时填充，后台执行一次有界五家冷扫描合并（与 IM `/new` 的
  `workspaces::refresh()` 同源），完成后无感刷新下拉。
- 待办单选项 = 所选项目（即下拉所选路径的 git 根 project key）的全部待办，含 ⚡ 自动待办；
  待办文本快照在选中时固定，启动按快照执行（与 IM D31 一致，并发删除不阻止启动）。
- 注意：下拉所选是 workspace 路径（canonical cwd，可能是 git 子目录 / worktree，D13 语义
  不变），待办按其 git 根 project key 读取（与 IM `start_task_input` 的
  `project::detect_from` 口径一致）。

## 5. 非目标

- 不新增第二套 readiness / 启动实现；`agent_launch.rs` 的判定与链路原样复用；
- 不做 GUI 侧的自动 watch、任务队列、进程管理（同 IM 非目标）；
- 不支持 Linux（入口隐藏）；Windows Server/RDS 多会话 broker 后置；
- 不在本窗口内管理 workspace（pin/hide/添加仍在设置「高级 → 从 IM 创建 Agent 任务」面板）；
- 不改变 IM `/new` 的任何行为。

## 6. 验收标准

1. macOS 且 Terminal.app 存在，或 Windows 且 `wt.exe` 可解析时，待办行 hover 出现「创建任务」按钮、
   托盘菜单出现「新建 Agent 任务」；Linux 隐藏。Windows Terminal 缺失时不拼接 shell 字符串，
   只给出明确安装提示。
2. 未开启 `agentTasks.enabled` 时功能完整可用。
3. 待办行进入：项目与该待办已预选；改选其它项目后待办预选清除。
4. Agent 列表五家全列；就绪状态与同机 IM `/new` 的可选集一致；未就绪原因可点且跳转定位正确。
5. permissionPrompt 三态表现正确：ask 显示单选且不预选；另两态直接以元数据显示。
6. 启动成功：新 Terminal 窗口内 Agent TUI 运行、cwd 正确、YOLO flag 映射与 IM 相同；
   所选待办进入执行历史；窗口关闭；活跃槽变为 popup（daemon 在跑时含旧渠道反激活回执）。
7. 启动失败：错误显示在窗口内、待办保留、LaunchRecord 按 TTL 过期，不产生半启动状态。
8. 组合任务（待办原文 + 空行 + 补充）超 3000 字符时禁止提交。
9. 窗口全局唯一：重复入口聚焦既有窗口并更新预选。
10. 自动化测试不启动真实 Agent（沿用 IM D27）。
11. Windows 启动的任务把 launch UUID 关联到 AgentRegistry；状态窗口、Popup badge 与托盘均只按该
    已登记 identity 精确聚焦原窗口。目标窗口已关闭、HWND 被复用或 owner identity 变化时返回失败，
    不创建替代窗口；其他 tab 当前处于活动状态不是失败，聚焦会定向切回固定的 Agent tab 0。

## 7. 反馈记录

- **2026-07-21**：初版定案（G1–G13）：不要求实验功能开关；通用流程做成独立单页表单窗口；
  待办入口＝同一面板预选项目与待办；四家 Agent 全列灰显可跳转；启动成功自动关窗并把活跃槽
  切到 popup；顺带添加托盘入口。
- **2026-08-19**：Lifecycle readiness 修复入口改到「Agents」Tab 对应 Agent 卡内的 lifecycle 行；
  自动集成首次启用默认开启 lifecycle，用户显式关闭后仍会保持 readiness 不通过。
