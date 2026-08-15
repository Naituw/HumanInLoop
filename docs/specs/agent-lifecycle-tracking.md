# 需求：Agent 生命周期追踪 + 状态窗口（实验性功能）

> 状态：已实现（macOS/Linux/Windows），当前覆盖 Claude Code / Codex / Cursor / Grok。
> 关联计划：`docs/plans/agent-lifecycle-tracking.md`
> 关联调研：`demo/agent-lifecycle/FINDINGS.md`（三家 hook 事件/env、Cursor 双触发去重、进程存活轮询为唯一不漏的结束信号、身份相关结论、各家标题来源——全部实测）
> 影响面：daemon（新增 agent 注册表 + 存活轮询 + 持久化 + 闲退守卫 + 订阅推送）、IPC（`ipc/mod.rs` 新增消息）、CLI（`cli/mod.rs` 新增 `__agent-hook` 与 `agents` 子命令）、客户端（`client/` ask 顺带上报活动）、新 GUI 窗口（`?view=agents` + `app` 角色 + `commands`）、Hook 集成（新增三家 lifecycle hook 安装/卸载/状态 + Codex 信任哈希 Rust 实现）、配置（`config.rs` 新增 `experimental`）、设置前端（`SettingsView.vue` 实验区 + 新 Tab）、i18n。
> **不改**：stdout 洁净契约、退出码语义（0/1/3）、既有 timeout hook 行为、IM 渠道与弹窗逻辑、graceful-drain 既有判据。daemon 协议仅增量演进、向后兼容。

> **当前实现补充（2026-07）**：设置入口已移到常显的「高级」Tab，不再受实验开关门控；状态窗口命令为
> `AskHuman agents monitor` 并由统一 GUI Host 承载。生命周期状态现被 IM `/status`、watch、插话、托盘和
> daemon 闲退共同消费；Grok 的兼容 Hook 双触发由 `agents/report.rs` 去重。后文保留最初三家方案的决策过程，
> D25–D27 与 §8 是 Codex shared app-server 的现行补充。
>
> **Windows 对齐补充（2026-08）**：Hook 通过安全 named pipe 上报；进程父链、命令行、启动时间、
> session 与可执行路径使用 Toolhelp、QueryFullProcessImageName、NtQueryInformationProcess、
> ProcessIdToSessionId 和 GetProcessTimes 原生读取，不在热路径启动 PowerShell/WMI。Codex 生成
> `commandWindows` 并按 Windows 实际命令计算 trusted hash；Claude/Cursor timeout 产物使用
> PowerShell 5 兼容 `.ps1`。

## 1. 背景

上一阶段在 `demo/agent-lifecycle/` 实测了 Claude Code / Codex / Cursor 三家 CLI 的生命周期信号，结论已写入 `FINDINGS.md`。本需求把这些结论落到产品里，但**单独成一个可独立测试的功能**，**不含** IM 渠道的「激活 / 反激活」逻辑（那是后续的「IM 渠道激活」需求 `docs/plans/im-channel-activation.md`，将构建在本功能之上）。

本功能交付两件事：

1. **设置里一个隐藏的「实验性功能」区**：默认不显示，需先在「通用」Tab 底部打开一个隐蔽开关才出现。展开后是一个「实验」Tab，内含 **Claude Code / Codex / Cursor 三个「生命周期追踪」开关**，开/关即**安装 / 卸载**对应 agent 的**用户级** lifecycle hook。
2. **`AskHuman agents status` 打开一个动态更新的 GUI 窗口**：按 agent 类型分组，展示当前**工作中 / 空闲**以及**最近结束**的 agent，每个含「类型 / 标题 / sessionID / 项目(cwd) / 启动时间 / 最近活动时间 / 状态 / pid」。窗口实时刷新。

## 2. 目标（用法）

```bash
# 打开 agent 状态窗口（动态更新；daemon 不在则自动拉起）
AskHuman agents status
```

- 在「设置 → 通用」底部打开「实验性功能」→ 出现「实验」Tab → 分别为 Claude Code / Codex / Cursor 打开「生命周期追踪」→ 本应用把用户级 lifecycle hook 写入各家配置。
- 之后任意启动 / 使用这些 agent，`agents status` 窗口即按类型分组实时显示其状态。
- 关闭某家开关即移除其 lifecycle hook（不影响既有 timeout hook、不影响其它无关 hook）。

## 3. 术语

- **lifecycle hook**：本功能安装的、上报生命周期事件的 hook（区别于既有的 **timeout hook** `askhuman-timeout.sh`）。
- **agent 记录 / session**：被追踪的一次 agent 会话，**以 `session_id` 为身份**。
- **存活轮询（poller）**：daemon 周期性 `kill -0 <pid>` 判断 agent 进程是否还在。

## 4. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 功能边界 | **仅**做「生命周期追踪 + 状态窗口 + Hook 安装开关」。**不含** IM 渠道 attach / 激活逻辑（留给后续激活需求） |
| D2 | 平台范围 | **macOS + Linux + Windows**；三平台共享 daemon、Hook 协议和状态机，操作系统差异限于 IPC 与进程适配。 |
| D3 | 总架构 | **daemon 为中枢**：内存维护 agent 注册表 + 持久化到 `~/.askhuman/agents.json`；hook 子进程经用户私有 daemon endpoint 上报事件；`agents monitor` 打开一个**长驻订阅** daemon 推送的 GUI 窗口（动态更新），`--text/--json` 返回一次快照。 |
| D4 | Hook 命令实现 | 统一走二进制隐藏子命令 `AskHuman __agent-hook <agent> <event>`：读 stdin JSON 取 `session_id`、运行时 `detectRunningAgent` 去重、向上 walk 进程树找 agent pid、连 daemon 上报、**exit 0 + 空 stdout**。**不**再写独立 shell 脚本 |
| D5 | 事件集 | 安装 **sessionStart + turn-start + turn-end + sessionEnd** 四类（Codex 无 sessionEnd，仅前三）。**进程存活轮询是权威的「已结束」判据**（关窗 / kill-9 时 turn-end/sessionEnd 都不触发，全靠它）；turn 起止仅用于切「工作中 / 空闲」 |
| D6 | 鲁棒性（不依赖 sessionStart） | 任意事件（**尤其 turn-start**）都要做 pid 发现 + **幂等登记**；缺 sessionStart 也能正常追踪 |
| D7 | 身份模型 | **身份 = `session_id`**（不同 session_id ⇒ 不同记录）。**pid 仅用于存活轮询**。同一 pid 出现**新** session_id ⇒ 旧 session 标记「已结束」、新 session 复用该 pid 追踪（**一个 pid 同时至多一个活动 session**）。pid 死亡 ⇒ 该 pid 当前活动 session 标记「已结束」 |
| D8 | 状态三态 | **工作中**（turn 进行中）/ **空闲**（等输入）/ **已结束**。由 turn 事件 + 存活/TTL 推导 |
| D9 | 展示字段 | 类型 / 标题 / sessionID / 项目(cwd) / 启动时间 / 最近活动时间 / 状态 / pid |
| D10 | 标题来源（实测） | **Cursor**：`~/.cursor/chats/*/<sid>/meta.json` 的 `.title`（缺失→回退首条用户消息）；**Codex**：`~/.codex/sessions/.../rollout-*-<sid>.jsonl` **无存储标题**→取首条**真实**用户消息（跳过 `<environment_context>`/`<user_instructions>` 等注入块）；**Claude**：`~/.claude/projects/*/<sid>.jsonl` 最后一条 `summary`，否则首条真实用户消息。全取不到→「(未命名)」。按 `session_id` 全局 glob 定位文件 |
| D11 | 「已结束」保留 | 全局**最多 10 条**（按结束时间 FIFO 淘汰），**不设**时间窗 |
| D12 | TTL 兜底 | **仅当拿不到 / 无法轮询 pid 时**（如 Linux 上 Claude `CLAUDE_CODE_ENV_SCRUB` 的 PID namespace 隔离）启用：**超过 1 小时无任何活动**即判「已结束」。**任意 hook 事件**与**每次 `AskHuman` 提问调用**都重置该 session 的活动时间（一个 session 跑超过 1h 很正常，期间可能多次提问）。pid 可轮询时以轮询为准、**不**应用 TTL |
| D13 | 排序 / 分组 | 顶层**按类型分组**（Claude / Codex / Cursor 区块）；区块内**按状态【工作中 → 空闲 → 已结束】**，同状态内按时间倒序（工作中/空闲按「最近活动」，已结束按「结束时间」） |
| D14 | 显示范围 | **跨项目全部** agent（daemon 为 per-user，能看到所有项目） |
| D15 | UI 入口 | 当前入口为常显的「高级」Tab；macOS、Linux 与 Windows 均显示四家追踪开关。早期 `config.experimental.enabled` 只保留配置兼容。 |
| D16 | per-agent 开关语义 | 开 = 安装用户级 lifecycle hook，关 = 卸载；开关状态以**实际安装状态**为准（同既有 hook 卡的 `*_status`）。与既有 timeout hook **各自独立**（不同标记、可共存）。**隐藏**「实验性功能」开关**不**卸载 hook（仅隐藏 UI；追踪继续） |
| D17 | 写入方式 | 沿用既有 hook 的**格式保留编辑**（`claude_hook.rs` 的 jsonc CST 风格）：**只增删本功能自己的条目**，绝不改动其它 hook 的字节 / JSON 转义。Cursor=`~/.cursor/hooks.json`、Claude=`~/.claude/settings.json`、Codex=`~/.codex/config.toml` 的 `[hooks]` + `[hooks.state]` `trusted_hash`（**Rust 实现信任哈希**，参考 `FINDINGS §6.2` + `demo/agent-lifecycle/harness/codex-trust.cjs`） |
| D18 | daemon 闲退 / 持久化 / 重连 | 闲时退出守卫**只受**【工作中 agent 数】与【状态窗口连接】影响（**空闲 agent 不保活**）；**版本更新 graceful-drain 不受 agent 影响**（仅在途 ASK 请求 gate drain，与今一致）；状态持久化 `agents.json`，daemon 重启 / 换新后重载并 `kill-0` 复核、剔除已死；状态窗口断连**自动重连**（必要时拉起 daemon） |
| D19 | CLI 形态 | `AskHuman agents <sub>`，本期仅 `status`（打开 GUI 窗口）。`agents` 设计为**可扩展子命令组**，预留未来子命令。**不**做纯文本 `list`，CLI **不**做 enable/disable（开关只在设置里） |
| D20 | IPC 增量 | 新增 `ClientMsg::AgentEvent` / `ClientMsg::AgentsSubscribe`、`ServerMsg::AgentsState`（快照推送）；`TaskRequest` 增**可选** agent 身份字段（type/session_id/pid）。serde 默认 + 同二进制两端，向后兼容 |
| D21 | ask 调用＝活动信号 | agent 通过 `AskHuman` 提问时，CLI **顺带 best-effort**（不阻塞作答主链路）上报 agent 身份给 daemon → 刷新该 session「最近活动」+ 重置 TTL。**仅刷新已存在的追踪 session，不新建**（尊重「未装 hook = 不追踪」） |
| D22 | 去重细则 | **仅当从 env 明确识别出「不同的」running agent 时**才跳过（`exit 0` 不上报）；env 无法判定 → 按 intended 处理（不跳过），避免漏报。识别顺序 `CURSOR_*`→cursor、`CODEX_*`→codex、`CLAUDECODE`→claude；**`CLAUDE_PROJECT_DIR` 不可作判据**（Cursor 也设它） |
| D23 | stdout 契约 | `__agent-hook` **永远 `exit 0` + 空 stdout**（sessionStart/turn-start 的 stdout 会被注入模型上下文；Cursor `stop` 空输出 = no-op）。失败全部 fail-open |
| D24 | i18n | 新 UI / 窗口中英双语（zh/en），沿用既有 i18n 体系 |
| D25 | Codex app-server 共享 pid **不作会话 pid** | 新版 Codex TUI 经 UDS 连**长寿共享 app-server 守护**（reparent 到 PID 1、多 TUI 共用），hook/工具/MCP 都跑在 app-server 进程树内 → walk 只会命中 app-server、**永远拿不到 TUI pid**（源码+实测坐实，见 §8）。故 walk 命中的 Codex 祖先若是 app-server（判据见 D27）→ **视为「无可用会话 pid」记 `pid=None`**，走 D7/D12 的**无 pid 路径**（与 Claude 被 PID-scrub 时**同一条既有代码路径**）：不做 D7 轮换、不做存活轮询、由 D12 TTL + 「工作中兜底超时」治理。理由：app-server pid 打破 D7「一个 pid 至多一个活动 session、pid 死＝session 结束」假设——否则并发会话在 `apply_event` 里互相轮换误杀、幽灵会话永不结束、`touch_activity_by_pid` 跨 session 串味；且 app-server 无 tty，terminal 定位本就失败 |
| D26 | interrupt / 关窗 结束判据（同 Claude 兜底） | Codex **无任何** interrupt/abort/SessionEnd hook（源码坐实：`Stop` 仅在回合**自然完成**触发，用户 Esc 打断走 `CodexErr::TurnAborted` 在 Stop 之前直接返回；见 §8）。故 Codex 的「打断 / 关窗 / 报错结束」＝Claude 无 Stop / 被 scrub 时的**同一兜底**：`working_backstop_sweep`（静默超时 工作中→空闲）+ D12 TTL（→已结束），**接受延迟**。正常回合结束仍由已安装的 `Stop`→turn-end hook 即时切「空闲」 |
| D27 | app-server 判据 | walk 命中的 Codex 祖先满足以下即判为「共享 app-server、非 TUI」→ 按 D25 记 `pid=None`：**`codex` 之后跳过前导全局选项，第一个非选项 token 为 `app-server` 子命令**（`codex app-server …`、`node …/codex app-server …`、ChatGPT/Codex Desktop 的 `codex -c features.…=true app-server …` 皆算；`--listen unix://`/`stdio://` 等为子命令后 flag）。**不得**用「参数任意位置出现 `app-server`」——`codex exec … app-server …` 子命令仍是 `exec`。取值型前导选项至少识别 `-c`/`--config`（及合并写法 `--config=`）。**兜底（可选）**：无 tty **且** 父链上溯到 PID 1。嵌入/旧模式 TUI 为纯 `codex`（无该子命令）→ pid 照常可用 |
| D28 | 累计有效工作时长 | `AgentRecord` 持久化 `activeElapsedSecs` + `activeSince`：同一 session 跨 Turn 累计 Working 区间，只扣真正 Idle；AskHuman / Stop confirmation 等待仍算 Working，Stop continuation 不重置。正常 `turn-end` 冻结，backstop/TTL 用最后活动时刻封口、不计兜底宽限。旧记录字段缺失从 0 开始，不用 `startedAt` 近似历史。snapshot 的 `activeElapsedSecs` 注入含当前区间的生效总值，供 Watch 与工作中 Agent 单选卡统一展示；Idle/Ended Watch 终态显示冻结值。 |
| D29 | Resume 继承与孪生并账（2026-07-25，用户实证） | 记录被判结束（SessionEnd / pid 死亡 / TTL / 重启存活复核）后同 session 事件再来 ＝ **resume**：`apply_event` 复活 ended 里的原记录并**继承**累计时长/起点/编号，不再新建零时长记录——此前 IDE 会话（pid 常缺失或误判）被反复拆成多条、累计时长归零重计（跑一上午只显示 30 分钟）。`load()` 时对历史数据做**孪生并账**：活动记录吸收同 (kind, session) 已结束孪生的时长与最早起点，纯已结束孪生合并到最新一条（幂等，仅影响累计展示与列表去重）。 |

## 5. 非目标（明确不做）

- 不做 IM 渠道的 attach / detach / 激活门控（后续需求）。
- 不做 Windows RDS 多交互会话 broker；正式 Windows 范围是单个交互式桌面会话。
- 不做 CLI 端的 enable/disable 与纯文本状态输出。
- 不追踪 Pre/PostToolUse 等工具级事件（噪音大、对状态判定无必要）。
- 不在本功能里改既有 timeout hook / 弹窗 / IM 渠道行为。

## 6. 已知风险

- **Codex 信任哈希与 Codex 版本相关**：`trusted_hash` 由 Codex 源码的 hook identity 结构推导；若 Codex 改了该结构，旧哈希失效、hook 会被判 Untrusted 而不执行。需在 `status` 里能识别「未受信任 / 已漂移」并提示重装；算法与出处记于 `FINDINGS §6.2`。
- **Linux 上 Claude 的 PID namespace 隔离**可能让 walk/`kill-0` 失效 → 落到 D12 的 TTL 兜底。
- **空闲 agent 不保活 daemon** 的代价（D18）：agent 空闲超过 daemon 空闲上限后 daemon 退出，下个 turn-start 重新拉起（首事件略有延迟）；窗口开着时无此问题。**用户已确认可接受**。
- **Codex app-server 共享 pid（D25–D27，§8）**：新版默认走共享 app-server → 追踪只能靠 `session_id` 身份 + hook 状态机 + TTL/兜底超时，**不能靠 pid 判存活**。代价：(1) 打断/关窗 无 hook → 结束/降级有延迟（同 Claude，D26）；(2) app-server 崩溃时其名下会话不会即时判死，最多滞留到 TTL（1h）；(3) Codex 会话「聚焦终端」按钮因无 tty 恒隐藏。**判据（D27）依赖 Codex 命令行/进程形态**，若 Codex 改变 app-server 启动形态需同步；子命令位识别（含跳过 `-c` 等前导选项）比「无 tty+PID 1」更专一、更不易误伤真实 TUI。历史回归：ChatGPT Desktop 曾用 `codex -c features.code_mode_host=true app-server …`，旧实现要求 `codex` **紧邻** `app-server` 漏检 → 共享 pid 被写入多会话 → D7 互相轮换误杀。

## 8. Codex app-server 架构补充（2026-07 源码 + 实测坐实）

> 背景：早期 Codex TUI 每个会话独占进程，可用 pid 追踪生命周期。新版 TUI 改为经 **Unix domain socket** 连一个**长寿共享 app-server 守护**来跑 agent，打破了原有 pid 假设。以下结论来自阅读 `/Users/wutian/Developer/codex` 源码 + 对真实运行会话的实测取证。

### 8.1 拿不到 TUI pid（不可靠，已定论）

hook→app-server→TUI 这条链上，TUI pid 在我们能触及的任何位置都不存在：

1. **hook stdin 无 TUI pid**：`hooks/src/events/pre_tool_use.rs`（`PreToolUseCommandInput`）/ `stop.rs`（`StopCommandInput`）字段只有 `session_id`/`turn_id`/`transcript_path`/`cwd`/`model`/`permission_mode`/`tool_*` 等；env 也只有 hooks.json 里配置的**静态** env（`hooks/src/engine/command_runner.rs` 仅 `.envs(handler.env)`，运行时不注入 pid）。
2. **hook 跑在 app-server 进程内**：`command_runner.rs` 直接 `Command::spawn`，父进程＝app-server；TUI 不是其祖先。
3. **TUI 经 UDS 连共享守护**：`app-server-client/remote.rs::connect_unix_socket_endpoint` 用 `UnixStream::connect(控制 socket)`；`app-server-transport/unix_socket.rs` accept 后**直接升级 websocket、不读对端凭证**（无 SO_PEERCRED/LOCAL_PEERPID）。守护有 `app-server.pid` 单例锁、可 reparent 到 PID 1。
4. **握手不带 pid**：`app-server-protocol/src/protocol/v1.rs::ClientInfo { name, title, version }` —— app-server 自己都不知道 TUI 的 pid。
5. **落盘元数据无 pid**：thread-store / rollout metadata / session_index 只有 `originator`/`cli_version`/`updated_at` 等。

**实测取证**（真实会话 `019f2b13-…`）：
- 两个 app-server 并存：共享 UDS 守护（`node codex app-server --listen unix://`，已 reparent 到 **PID 1**）+ Codex.app 自带 stdio 版。
- 该会话 rollout `…-019f2b13….jsonl` 由**共享 app-server（无 tty）以写持有**；**TUI（ttys005）没打开任何 rollout**，env 里也没有 session_id。
- TUI↔app-server 仅可经 socket 端点配对（TUI fd 的对端 == app-server 的 `app-server-control.sock`），能判「哪个 TUI 连了哪个 app-server」，但**看不出该连接归属哪个 session/thread**（多路复用、不外露）。

⇒ `session_id → TUI pid` 不可得；walk 只会命中共享 app-server。这是 D25/D27 的依据。

### 8.2 hook 判「turn 结束」的边界

- **正常回合结束：有信号，且已在用。** `core/src/session/turn.rs` 在 `if !needs_follow_up`（回合自然完成）内调 `run_turn_stop_hooks` → 触发 `Stop` hook（`hook_runtime.rs`），载荷含 `session_id`/`turn_id`/`last_assistant_message`。我们的 `agent_lifecycle.rs` 已安装 Codex `Stop`→`turn-end`，正常结束即时切「空闲」。
- **用户 interrupt / 报错结束：Codex 不触发任何 hook。** `session/turn.rs` 里 `Err(CodexErr::TurnAborted) => return Err(...)` 在到达 Stop hook **之前**直接返回；`context/turn_aborted.rs` 只把 `<turn_aborted>` 注入**下一回合**上下文。Codex 全量 hook 事件（`hooks/src/events/`）＝SessionStart / UserPromptSubmit / Pre·PostToolUse / PermissionRequest / Pre·PostCompact / Stop·SubagentStop —— **无** TurnAbort/Interrupt/SessionEnd/Notification。
- ⇒ 打断/关窗只能靠非 hook 兜底（D26）：现有 `working_backstop_sweep`（工作中→空闲）+ D12 TTL（→已结束）。这与 Claude「Esc 打断不发 Stop」是同一类限制，故**处置方案与 Claude 完全一致**。

## 7. 反馈意见

（评审 / 实测中的修改意见追加到此处，标注日期。）

- **2026-07-04**：并入 Codex app-server 共享 pid 结论（新增 D25/D26/D27 + §8 + 风险项）。定案：app-server pid 记 `None` 走无 pid 路径（同 Claude）；interrupt/关窗兜底同 Claude；判据以命令行 `app-server` 令牌为主、`无 tty + PID 1` 为可选兜底。据此改 `agents/detect.rs` + `agents/registry.rs`（详见计划文档同日补丁节）。
