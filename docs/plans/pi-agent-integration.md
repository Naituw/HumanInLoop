# Pi Agent 一等集成实施计划

> 状态：实现完成；macOS / Windows 真机验证通过，Linux 等待 `main` CI
>
> 调研基线：2026-08-18
>
> Pi 版本基线：`>= 0.82.0`
>
> 目标平台：macOS、Linux、Windows

## 1. 背景与目标

AskHuman 目前把 Claude Code、Codex、Cursor Agent 与 Grok CLI 纳入了统一的 Agent
模型，但还不能识别、配置、启动或观察 Pi Agent。

本计划将 Pi 作为**一等核心 Agent**加入现有体系，而不是只提供一段手工安装说明。首版需要覆盖：

- 设置页中的安装状态、集成模式和诊断；
- CLI 阻塞式 AskHuman 调用与 24 小时等待；
- Agent 生命周期、活动状态和工作中插话；
- 上下文压缩后的协议恢复；
- Stop 完成确认，并且 Pi 默认开启；
- 会话发现、转录解析、标题、工作区和 Agent Console；
- GUI / IM 新建任务与继续/分叉任务；
- macOS、Linux、Windows 同步支持。

首版明确不声称支持 Pi 不具备的原生能力：MCP 集成、内置权限审批模式，以及对任意第三方
subagent Extension 的自动隔离。

## 2. 官方能力调研结论

### 2.1 产品与安装面

调研时 Pi 的官方仓库是 [`earendil-works/pi`](https://github.com/earendil-works/pi)，
CLI 命令为 `pi`，npm 包为 `@earendil-works/pi-coding-agent`。调研时最新官方 release
为 `v0.84.2`；本集成不绑定最新版本，只设最低兼容线 `0.82.0`。

Pi 会自动加载：

- 全局规则：`~/.pi/agent/AGENTS.md`；
- 全局 Extension：`~/.pi/agent/extensions/*.ts` 或子目录中的 `index.ts`；
- 默认会话：`~/.pi/agent/sessions/` 下的 JSONL；
- 自定义 `sessionDir`，因此不能只靠默认目录定位所有转录。

### 2.2 与 AskHuman 相关的核心能力

Pi 官方 Extension API 足以承载本集成：

| 需求 | Pi 能力 | 结论 |
| --- | --- | --- |
| 修改 Bash 工具超时 | `tool_call` 事件允许修改 `event.input` | 可把实际 AskHuman 命令的 `timeout` 精确设为 `86400` 秒 |
| 生命周期 | `session_start`、`agent_start/end/settled`、`turn_start/end` 等事件 | 可映射现有生命周期协议 |
| 工作中插话 | `tool_call` 可阻断工具并返回原因 | 可在工具边界注入待处理的用户消息 |
| Stop 后继续 | `pi.sendUserMessage(...)` | 可在用户选择继续时触发下一轮 |
| 会话绑定 | Bash 原生环境变量 `PI_SESSION_ID`、`PI_SESSION_FILE` 等 | 从 Pi `0.82.0` 起可稳定绑定请求与转录 |
| 上下文压缩 | `session_compact` 及后续 Agent 生命周期事件 | 可在下一轮恢复 AskHuman 协议 |

Pi 的 Bash 内置超时以秒为单位且可省略。shell 脚本只能约束自己启动的子进程，不能修改
外层 Bash 工具的等待上限，所以长等待必须由 Pi Extension 修改 Bash 工具输入，不能依赖提示词。

### 2.3 Pi 不提供的核心能力

Pi 官方 README 明确强调保持核心精简，以下能力不内置：

- MCP；
- 权限确认弹窗或统一的 YOLO/审批模式；
- subagent 框架。

这些能力可以由第三方 Extension 或 Package 添加，但 AskHuman 不能把第三方实现当作稳定的 Pi
核心契约。因此首版不显示 MCP 选项、不伪造权限参数，也不承诺拦截任意第三方 subagent。

### 2.4 会话格式

Pi 会话是版本 3 JSONL：

- 首行为 `type: "session"` 的 header，含 `id`、`cwd` 等字段；
- 对话项的外层为 `type: "message"`，实际消息位于 `message`；
- assistant 消息的 `stopReason` 包含 `stop`、`length`、`toolUse`、`error`、`aborted`；
- `session_info` 可提供用户命名；
- compaction、branch summary、model change 等条目需要被解析器容错跳过或归一化。

这与现有 Agent 转录结构不同，必须增加 Pi 专用解析器，不能套用 Claude/Codex 的字段假设。

## 3. 已确认的产品决策

| 决策项 | 结论 |
| --- | --- |
| 产品定位 | Pi 是一等核心集成 |
| 首个交互通道 | `CLI + 超时 Extension` |
| 设置页模式 | 只显示 `CLI` 与 `未集成`，不显示 MCP |
| 首版能力边界 | 核心能力 + Stop |
| 新建任务权限选择 | 隐藏权限选择，并解释 Pi 没有内置权限模式 |
| Stop 默认值 | Pi 默认开启 |
| 平台 | macOS、Linux、Windows 同步支持 |
| 最低版本 | 要求 Pi `>= 0.82.0` |
| 文档交付 | 调研结论和后续决策写入本计划文档 |

版本 `0.82.0` 的选择不是因为更早版本不能修改 timeout 或不能发送用户消息，而是因为该版本
开始原生向 Bash 注入 `PI_SESSION_ID`、`PI_SESSION_FILE`、`PI_PROVIDER`、`PI_MODEL` 和
`PI_REASONING_LEVEL`。这使 AskHuman CLI 请求能稳定绑定正确会话，并能支持自定义会话目录，
避免脆弱地重写 shell 命令或猜测转录文件。

## 4. 对用户可见的产品契约

### 4.1 集成模式

Pi 只有两种可选模式：

- **CLI**：安装规则和运行时 Extension，AskHuman 通过 CLI 阻塞等待；
- **未集成**：移除 AskHuman 管理的 Pi 规则；Extension 是否保留取决于生命周期开关。

Rust 后端和 CLI 都必须拒绝 `pi + mcp`，不能只靠前端隐藏选项。

### 4.2 设置页

Pi 卡片与其他核心 Agent 一起展示，至少包含：

- Pi 是否安装、版本和最低版本诊断；
- `CLI / 未集成` 模式；
- 生命周期开关；
- Stop 开关，首次出现时默认开启；
- 当前规则与 Extension 是否最新；
- 重新安装/更新、卸载和诊断入口。

Pi 不显示 MCP 配置，也不显示会让用户误以为可控制 Pi 内置审批策略的权限开关。

### 4.3 模式与产物真值表

Stop 是 CLI 交互的一部分；用户切换到“未集成”时保留偏好，但不运行 Stop。生命周期是独立能力，
所以未集成模式下仍可按生命周期开关保留轻量 Extension。

| 模式 | 生命周期 | Stop 偏好 | 规则 | Extension 模块 |
| --- | --- | --- | --- | --- |
| 未集成 | 关 | 任意 | 无 | 无 |
| 未集成 | 开 | 任意 | 无 | 生命周期 + 插话 |
| CLI | 关 | 关 | 有 | timeout + 上下文恢复 |
| CLI | 关 | 开 | 有 | timeout + 上下文恢复 + Stop |
| CLI | 开 | 关 | 有 | timeout + 上下文恢复 + 生命周期 + 插话 |
| CLI | 开 | 开 | 有 | 全部模块 |

### 4.4 新建和分叉任务

Pi 不展示权限模式选择器。GUI 与 IM 在 Pi 被选中时显示固定说明：

> Pi 不提供内置权限模式；AskHuman 不附加权限覆盖参数。请通过容器、沙箱或自定义
> Extension 管理安全边界。

后端必须把 Pi 的启动权限固定为 Agent 默认值，并拒绝把其他 Agent 的 YOLO/审批选项透传给 Pi。

## 5. 总体技术设计

### 5.1 扩展统一 Agent 模型

新增并贯通：

- Rust `AgentKind::Pi`；
- Rust `AgentTarget::Pi`；
- 前端 `AgentId` 的 `"pi"`；
- CLI 解析、序列化、IPC、设置持久化和 i18n；
- 所有对 Agent 枚举进行穷举匹配的代码。

不得把 Pi 伪装成 Claude 或 generic agent。显式类型能让模式限制、解析规则、启动参数和诊断在编译期
保持完整。

### 5.2 AskHuman 管理的 Pi 产物

#### 规则

在 `~/.pi/agent/AGENTS.md` 中维护带稳定 begin/end 标记的 AskHuman 区块，内容使用现有 CLI
交互协议及主 Agent 限制。安装器只替换自己的区块，保留用户其他内容。

#### Extension

AskHuman 独占一个明确命名的目录，例如：

```text
~/.pi/agent/extensions/askhuman/index.ts
```

只管理这个目录，绝不扫描或改写其他 Extension。`index.ts` 由仓库内版本化模板生成，并包含独立的
能力开关：

- `cliTimeout`；
- `contextRecovery`；
- `lifecycle`；
- `interject`；
- `stopConfirmation`。

采用一个合并 Extension，而不是多个互相独立的文件，以便统一排序生命周期与 Stop、避免重复上报
和竞态。每次设置变化都根据目标状态完整重算并原子替换该文件；不再需要任何模块时才删除
AskHuman 自有目录。

所有产物修改继续复用 `IntegrationMutationLock`，保证设置页、CLI 和自更新流程不会互相覆盖。

### 5.3 从“Hook”抽象为运行时适配器

现有集成状态大量使用 `hook` 命名，而 Pi 的对应物是 Extension。实现时应增加通用的
`runtime artifact` 概念，并显式标明种类：

```text
none | hook | extension
```

兼容迁移期间保留现有 Hook 字段和序列化行为，新增 Pi 使用 Extension 状态。前端对 Pi 显示
“Extension”，不把它标成 Hook；CLI/doctor 同样输出准确的产物名称。

### 5.4 Extension 与 AskHuman 的桥接

Extension 使用 Node `child_process.spawn(executable, args, options)` 直接传参数，不经过 shell。
这样 macOS/Linux/Windows 共用相同协议，并避免路径、空格和引号转义差异。

桥接分为两类：

- 生命周期等通知：短命令、尽量 fire-and-forget；
- 插话轮询和 Stop：等待结构化输出，最长 24 小时，并支持清理子进程。

子进程环境必须传递或补充：

- `PI_SESSION_ID`；
- `PI_SESSION_FILE`；
- `PI_PROVIDER` / `PI_MODEL` / `PI_REASONING_LEVEL`（用于状态展示时再消费）；
- 当前工作目录；
- AskHuman 生成的 Agent/集成标识。

不得通过字符串拼接执行 AskHuman 命令。可执行文件路径、参数和 Extension 模板哈希进入安装状态，
路径或模板变化时设置页应显示“需更新”。

## 6. 各能力实现

### 6.1 CLI 阻塞与 24 小时 timeout

在 Pi `tool_call` 事件中：

1. 只处理工具名为内置 Bash 的调用；
2. 使用保守的命令 token 识别实际 AskHuman CLI 调用；
3. 命中后把可变的 `event.input.timeout` 设为**恰好 `86400`**；
4. 其他 Bash 命令保持原值，不做全局放宽。

匹配器必须覆盖三平台的 AskHuman 可执行文件形式和受支持的显式路径，同时避免仅凭字符串包含
`AskHuman` 就命中注释、echo 或文件内容。实现应把 matcher 独立成可测试的纯函数。

提示词仍解释长等待语义，但正确性不依赖模型主动提供 timeout。

### 6.2 生命周期与工作中插话

Pi 事件映射到现有统一协议：

| Pi 事件 | AskHuman 事件 |
| --- | --- |
| `session_start` | session/agent 注册与转录路径更新 |
| `agent_start` | agent start / working |
| `turn_start` | turn start |
| `tool_call` / `tool_result` | working 心跳与插话边界 |
| `turn_end` | 暂存 turn 完成状态 |
| `agent_end` | 保存本轮最终消息和 `stopReason` |
| `agent_settled` | 无后续重试/压缩/follow-up 时的稳定完成点 |
| `session_shutdown` | session end / idle 清理 |

插话沿用现有“工具边界轮询”语义：Extension 在 `tool_call` 前询问 daemon 是否有待注入消息；有时
返回 `{ block: true, reason: wrappedUserMessage }`，让 Pi 把它作为工具失败原因带回模型。没有消息时
继续原工具调用。轮询失败必须 fail-open，不能阻断正常工作。

生命周期与插话保持现有产品绑定关系；Pi 不另造一套用户开关。

### 6.3 Stop 完成确认

Pi Stop 的稳定触发点是 `agent_settled`，但判定依据来自最近一次 `agent_end`：

1. 保存当前 session、settled generation、最终 assistant 文本和 `stopReason`；
2. 仅当 `stopReason == "stop"` 且最终文本不含 `[user_confirmed_end_turn]` 时询问；
3. `length`、`toolUse`、`error`、`aborted` 不询问；
4. 通过现有隐藏 Stop adapter 等待最多 24 小时；
5. 用户选择“继续”时，仅在 session 和 generation 仍匹配、Pi 仍 idle 的前提下调用
   `pi.sendUserMessage(prompt)`；
6. 用户结束、取消、超时或 adapter 出错时保持 idle，并 fail-open 完成本轮；
7. 等待期间出现新一轮、切换 session 或 shutdown 时，旧回复变为 stale，永不注入。

Stop 开启时，不能在 `agent_end` 或第一次 `agent_settled` 就向上层重复报告稳定 turn end：

- 选择继续：保持同一逻辑任务继续工作；
- 选择结束或 fail-open：再报告稳定结束；
- marker 已存在：直接报告结束，不再询问。

Pi 偏好字段采用可迁移的可选值：旧配置中缺失 `pi` 时解析为 `true`，用户显式关闭后持久化
`false`。这实现“Pi 默认开启”，同时不改变其他 Agent 默认关闭的既有行为。

必须用真实 Pi 验证能否从 `agent_settled` 回调内安全调用 `sendUserMessage`。如果 Pi 的事件派发要求
回调先返回，则使用带 generation 校验的下一微任务/定时调度；不得改用不稳定的轮询猜测。

### 6.4 上下文压缩恢复

Extension 在 `session_compact` 后为该 session 标记 pending recovery，在下一次 Agent 运行开始前，
通过 Pi 官方允许的上下文注入能力追加现有 CLI 恢复提示，提醒模型：

- 继续遵守 AskHuman mandatory interaction protocol；
- 不要把问题直接输出给用户；
- 不确定上次 AskHuman 问答时先调用 `show_last`。

恢复提示只注入一次，不额外开启用户可见回合，也不污染会话标题候选。具体使用
`before_agent_start` 的系统提示扩展还是隐藏 custom message，以真实 Pi E2E 中上下文可见性与转录表现
为准；这是实现验证门槛，不改变产品语义。

### 6.5 检测、注册与转录路径

#### 进程检测

按强到弱顺序识别 Pi：

1. `PI_CODING_AGENT=true`；
2. `PI_SESSION_ID` / `PI_SESSION_FILE`；
3. argv basename 精确为 `pi` 或 Windows `pi.exe`；
4. 官方 coding-agent CLI 脚本或包路径。

禁止只做进程命令行 substring 匹配，以免把普通文件名、参数或其他程序误判为 Pi。

Extension 直接启动 AskHuman 时，应把 Pi pid/session 信息显式传给 daemon；进程树检测只作为校验和
降级路径。

#### 转录路径

Extension 从 `ctx.sessionManager.getSessionFile()` 上报当前转录文件，以支持自定义 `sessionDir`。
daemon 不得无条件信任任意路径：读取有界 header，至少验证：

- 第一条为 Pi v3 session header；
- header session id 与上报 id 一致；
- `cwd` 与注册工作区一致或通过已有路径归一化规则；
- 文件仍满足普通文件、大小和读取权限约束。

默认目录扫描 `~/.pi/agent/sessions/` 仅作为无 Extension、旧记录迁移和离线发现的 fallback。

### 6.6 转录、标题、工作区与 Console

增加 Pi v3 JSONL 解析器，至少归一化：

- user / assistant 文本；
- thinking；
- tool call 与 tool result；
- `session_info.name`；
- compaction/branch summary；
- 时间戳、provider、model、usage 和 stop reason（字段存在时）；
- 未知未来条目：跳过并记录低噪声诊断，不使整个会话失败。

CLI 方式执行的 AskHuman 在 Pi 中表现为 Bash tool call，不是原生 AskHuman tool。Console 可以对**精确
匹配的受管 AskHuman 命令和结构化输出**做最佳努力的问答呈现；无法确认时按普通 Bash 工具显示，
避免把任意 shell 输出误标成人类回答。

标题优先级：

1. 最新非空 `session_info.name`；
2. 第一条真实用户消息；
3. 现有工作区/兜底标题规则。

用于上下文恢复的隐藏消息、Extension 内部事件和 AskHuman 控制消息不得成为标题。

工作区扫描从 Pi session header 的 `cwd` 建立索引；watch、最近会话、Agent Console 过滤和 IM 列表
都必须把 Pi 纳入统一枚举。

### 6.7 新建与分叉任务

新建任务首选官方 CLI 形式：

```text
pi <prompt>
```

分叉任务首选：

```text
pi --fork <session-id-or-path> <prompt>
```

参数顺序、路径与 id 两种形式、启动后真实 session id 的变化必须由 E2E 锁定，不能只根据帮助文档
假设。fork readiness 通过 `pi --help` 探测 `--fork`，并结合版本检查给出可操作诊断。

启动层不添加权限覆盖 flag。其他 Agent 的 `LaunchPermission` 对 Pi 必须在边界处归一化为
`AgentDefault`；如果外部调用显式传入不支持的模式，返回清晰错误而不是静默拼出未知参数。

### 6.8 Subagent 边界

Pi 核心没有 subagent API，因此首版：

- 不安装 Pi 专用 `SubagentStart` guard；
- 不宣称能识别或禁止第三方 subagent Extension；
- 若一个进程已被上游明确标记为 subagent，仍沿用全局规则：不得使用 AskHuman；
- 文档和设置页明确第三方 Extension 的安全边界由用户负责。

后续只有在 Pi 建立稳定的官方 subagent 生命周期契约后，才把它纳入自动 guard。

## 7. 预计代码影响面

以下是实施时必须逐项审计的主要区域，不代表允许机械批量替换：

### 7.1 Rust 后端

- `src-tauri/src/agents/mod.rs`、`detect.rs`、`registry.rs`、`activity.rs`；
- `src-tauri/src/agents/report.rs`、`stop.rs`、`context_recovery.rs`；
- `src-tauri/src/agents/title.rs`、`transcript_full.rs`、`workspaces.rs`；
- `src-tauri/src/integrations/agent_rules.rs`、`agent_mode.rs`；
- `src-tauri/src/integrations/agent_lifecycle.rs`、`agent_stop.rs`；
- `src-tauri/src/integrations/agent_launch.rs`、`agent_permission.rs`；
- `src-tauri/src/integrations/agent_context_recovery.rs`；
- `src-tauri/src/integrations/agent_subagent_guard.rs`、`mcp_config.rs`；
- commands、daemon、CLI doctor / agents 命令、IPC models 和 prompt 选择。

### 7.2 前端

- Agent 类型、排序、label、图标与 i18n；
- Settings 的 IntegrationTab / useIntegration；
- AgentsView、NewTask、ForkTask；
- Pi 的模式能力与权限能力条件渲染；
- lifecycle、Stop、diagnostic 和 runtime artifact 状态展示。

### 7.3 新增建议模块

为避免 Pi 逻辑散落，优先增加边界清楚的模块：

- `integrations/pi_extension.rs`：模板、目标配置、安装/卸载/哈希；
- `agents/pi_transcript.rs`：v3 JSONL 解析和 fixture；
- `agents/pi.rs` 或等价模块：路径、版本、进程识别、启动能力；
- `src-tauri/resources/integrations/pi/`：受版本控制的 Extension 模板。

最终文件名可服从现有模块组织，但职责边界应保留。

## 8. 实施阶段

### 阶段 0：真实 Pi 契约验证夹具

- 在隔离环境安装 Pi `0.82.x` 和实现时最新稳定版；
- 保存 `--version`、`--help`、Extension 事件序列和 session v3 fixture；
- 验证 `tool_call` 修改 timeout 生效；
- 验证 `PI_SESSION_*` 环境变量；
- 验证 `agent_settled -> sendUserMessage` 的合法调用时机；
- 验证 `session_compact` 后的单次协议恢复；
- 验证 `--fork` 的参数和新 session 行为。

当前开发机未发现 `pi` 命令，因此以上不能在调研阶段冒充已通过，必须作为编码前后的 E2E 门槛。

### 阶段 1：身份、模式和 CLI 基础集成

- 增加 Pi 枚举、序列化、前端类型、i18n 和图标；
- 实现 `>=0.82.0` 版本检查；
- 实现 AGENTS managed block；
- 实现受管 Extension 的原子安装和 hash 状态；
- 完成 CLI timeout 与上下文恢复；
- 完成设置页 `CLI / 未集成`、CLI agents 命令和 doctor；
- 确保 MCP 模式在 UI 和后端都不可用。

### 阶段 2：生命周期、插话与 Stop

- 接通 Extension 事件到统一生命周期；
- 加入 session id/file 的安全上报；
- 接通工具边界插话；
- 实现 Stop generation 状态机、默认开启迁移和 marker 去重；
- 完成 continue/end/stale/fail-open E2E。

### 阶段 3：会话与 Console

- 实现 v3 转录解析器；
- 加入默认目录 fallback 与自定义 session 路径；
- 接通标题、工作区、watch、历史与 Agent Console；
- 完成 CLI AskHuman Bash 调用的保守识别。

### 阶段 4：新建、继续和分叉任务

- 接通 GUI / IM Agent 选择；
- Pi 路径隐藏权限选择并显示说明；
- 实现启动、继续/分叉和 readiness；
- 验证三平台终端/进程启动与 session 归属。

### 阶段 5：全量回归与文档收口

- 跑单元、集成和真实 Pi E2E；
- 按项目要求执行 `./scripts/install.sh` 并用新安装的 AskHuman 做交互验证；
- 检查 `docs/overview.md` 及它引用的 Agent、生命周期、Stop、Console 说明是否失真；
- 只更新因实现而失真的对应 overview/spec，不把实施细节堆入主 overview；
- 实现完成后从 `docs/PROGRESS.md` 删除对应未完成项（若实施期间新增）。

## 9. 测试与验收矩阵

### 9.1 单元测试

- Pi 版本：`0.81.x` 拒绝、`0.82.0` 接受、新版本接受、非语义输出报诊断；
- 模式：Pi 只接受 none/cli，mcp 在所有入口拒绝；
- installer：首次安装、幂等、升级、卸载、原子失败恢复；
- 规则：只替换 AskHuman block，保留 BOM/换行和用户内容；
- Extension：六种真值表组合生成正确、模板 hash 变化提示更新；
- timeout matcher：真实调用命中，echo/注释/路径片段/其他命令不命中；
- lifecycle：事件顺序、重复事件、缺失事件、session 切换；
- Stop：stop/length/error/aborted、marker、continue/end/cancel/timeout/stale；
- 转录：v3 全类型、截断尾行、未知条目、compaction、自定义路径；
- 安全：伪造 session id/path/cwd 被拒绝；
- 标题：session name、首条真实用户消息、隐藏控制消息过滤；
- 启动：Pi 不接收权限 flag，fork 参数稳定。

### 9.2 集成测试

- 设置页、CLI 和后端状态一致；
- `CLI -> 未集成 -> CLI` 后偏好和产物正确；
- lifecycle 独立开关不会误装规则；
- Stop 在未集成模式暂停，返回 CLI 后按原偏好恢复；
- daemon 重启、Extension 子进程失败、网络/IM 失败均 fail-open；
- 多 Pi session 并行时不串联问题、回答、Stop prompt 或转录；
- AskHuman 路径变化/升级后提示 Extension 更新。

### 9.3 真实 E2E

最低覆盖：

- Pi `0.82.x` 与实现时最新稳定版；
- macOS、Linux、Windows；
- 默认 sessionDir 与自定义 sessionDir；
- 自然完成后 End；
- 自然完成后 Continue；
- 等待 Stop 时用户先启动新一轮，旧回复丢弃；
- error/aborted/length 不弹 Stop；
- CLI `ask` 等待超过 Pi 默认 Bash timeout；
- 工作中插话阻断当前工具并回到模型；
- compaction 后仍通过 AskHuman 提问；
- 新建任务与 `--fork`；
- `--no-extensions` 启动时给出能力不可用的诊断预期。

### 9.4 完成标准

只有同时满足以下条件才算首版完成：

1. 设置、CLI、GUI/IM 启动、Console 和 watch 全部识别 Pi；
2. Pi CLI 模式无需用户手写 timeout，AskHuman 可等待 24 小时；
3. Stop 默认开启，continue/end/stale 行为通过真实 E2E；
4. Pi MCP 与权限模式不会以错误能力暴露；
5. 自定义 sessionDir 不会导致转录串会话或任意文件读取；
6. 三平台构建和平台专项测试通过；
7. 安装脚本验证完成，相关文档与实现一致。

### 9.5 实施与验证记录（2026-08-19）

实现已经按上述边界落地：Pi 作为显式第五种 Agent 贯通 Rust、IPC、前端、设置、CLI、
生命周期、Stop、插话、v3 转录、标题/工作区、Console 与 GUI/IM 新建及分叉任务。受管运行时为
单个 `~/.pi/agent/extensions/askhuman/index.ts`，Pi 仍只暴露 None/CLI，不提供 MCP 或权限模式。

本地 macOS 回归结果：

- Rust：`1149 passed; 0 failed; 2 ignored`；
- 前端：26 个 Vitest 文件、167 个测试，以及 5 个 Node script 测试全部通过；
- `npm run build`、`cargo clippy --all-targets -- -D warnings`、`git diff --check` 全部通过；
- `./scripts/install.sh` 成功安装并以新二进制继续验证；
- 官方 Pi `0.84.2` 使用本地 faux provider 完成真实 Extension/RPC E2E，不消耗外部模型额度：
  CLI Bash 调用被写入 `timeout: 86400`，自定义 sessionDir、生命周期和 Console 数据正确；Stop
  选择 Continue 后同一会话收到 `[USER CONTINUATION]` 并继续；`session_compact` 后下一轮只注入一次
  recovery prompt；工作中插话在工具边界以原生 block reason 返回模型。

Windows 11 真机回归结果：

- Rust：`1139 passed; 0 failed; 2 ignored`；
- 前端：26 个 Vitest 文件、167 个测试，以及 5 个 Node script 测试全部通过；
- `pnpm build`、Windows Clippy、`scripts/install-windows.cmd`、`git diff --check` 全部通过；
- 官方 Pi `0.84.2` 与兼容下界 `0.82.0` 均完成真实进程 E2E：全局协议被加载，AskHuman Bash
  调用观测到 `timeout: 86400`，工具成功返回，v3 自定义会话路径与生命周期 ended 记录正确；
- 验证使用独立克隆、独立 AskHuman home 与临时 Pi npm 前缀；完成后已卸载受管产物、移除测试 PATH
  条目与临时会话/配置，原 `C:\dev\AskHuman` 保持不变。

Linux 平台由现有 `.github/workflows/build.yml` 的 Ubuntu 22.04 job 执行 frontend build/test、
`cargo fmt --check`、Clippy、Rust tests 与 release build；本次改动推入 `main` 后以该 job 作为 Linux
原生平台 gate。CI 通过后，本节状态更新为最终完成。

## 10. 风险与缓解

### 10.1 Pi API 和发行身份变化快

Pi 在近期发生过仓库/包身份调整，Extension API 也持续演进。缓解措施：固定最低版本、只依赖官方公开
API、保存两个版本的契约 fixture，并把 feature smoke test 纳入集成验证。安装检测以可执行文件与能力为
主，不把 npm 包名当作唯一身份。

### 10.2 `agent_settled` 的重入时序

如果在 handler 内直接 `sendUserMessage` 会触发重入或被 Pi 拒绝，Stop 继续可能丢失。缓解措施是先做
阶段 0 E2E，并用 session + generation + idle 三重校验后调度；不在未验证时序上堆补偿轮询。

### 10.3 长等待与过期回答

Stop 最长等待 24 小时，期间 session 可能变化。每个请求绑定 session id、settled generation 和唯一
request id；任何新 run/session/shutdown 都使旧结果失效。取消子进程失败不能阻断 Pi 退出。

### 10.4 用户自定义 Extension 冲突

多个 Extension 可能修改同一 Bash timeout、阻断工具或在 settled 后发消息。AskHuman 只管理自有目录，
采用保守事件行为并记录可诊断状态；不改写第三方文件。若用户用 `--no-extensions` 启动 Pi，CLI
timeout、生命周期、插话、恢复和 Stop 都不可用，这是明确降级而不是可静默修复的情况。

### 10.5 转录路径安全

`PI_SESSION_FILE` 和 Extension 上报路径来自 Agent 进程，不能等同于可信任路径。通过 bounded header、
session id、cwd、文件类型与已有路径策略共同校验；失败时退回默认扫描或只展示无转录状态。

### 10.6 CLI 工具识别误判

Pi 没有原生 AskHuman tool，Bash 字符串可能包含相同文本。timeout 与 Console 特殊呈现都使用严格
token 识别；不确定时只当普通 Bash，不为追求美化扩大匹配范围。

## 11. 首版明确不做

- 为 Pi 实现或捆绑通用 MCP Extension；
- 替 Pi 发明权限审批/YOLO 语义；
- 管理、删除或重排用户的第三方 Pi Extension；
- 保证任意第三方 subagent Package 不调用 AskHuman；
- 支持 Pi `<0.82.0` 的降级兼容；
- 把 `--no-extensions` 启动的 Pi 假装成完整集成状态；
- 在首版引入 Pi 专属的 AskHuman 交互协议分叉。

## 12. 官方参考资料

- [Pi coding agent README](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/README.md)
- [Extensions](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md)
- [Sessions](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/sessions.md)
- [Session format](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md)
- [Environment variables](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/environment-variables.md)
- [Built-in Bash tool source](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/tools/bash.ts)
- [Pi v0.82.0 release](https://github.com/earendil-works/pi/releases/tag/v0.82.0)
- [Pi releases](https://github.com/earendil-works/pi/releases)
