# Agent Stop 结束确认 —— 可行性与设计规格

> 状态：实现、单元测试、安装与用户真机验收均已完成。
>
> 调研基线（2026-07-12）：Claude Code 2.1.205、Codex CLI 0.144.1、
> Cursor CLI 2026.06.26-7079533、Grok CLI 0.2.93。

## 1. 背景与目标

当前 Agent 即使收到强制交互提示词，仍可能不调用人机交互工具，直接自然结束本轮。
人在 IM 侧只能看到经 AskHuman 投递的内容，此时无法向已经停止的 Agent 追加消息。

本功能在 Agent **自然完成一轮**时，通过 Stop Hook 向本地弹窗 / 当前 IM 投递一张确认卡：

- **继续对话**：可附带一段后续指令；Hook 把它作为 continuation prompt 交回 Agent。
- **结束对话**：放行 Stop，Agent 进入空闲。
- 关闭弹窗、取消、基础设施失败或 24 小时无人答复均等同放行结束（fail-open）。

目标是给 IM 用户一个可靠的“最后接管点”，而不是让 Agent 无条件自动循环。

## 2. 四家能力结论

| Agent | 自然完成时 Hook | 原生继续方式 | 错误 / 取消 | 首期结论 |
|---|---|---|---|---|
| Claude Code | `Stop`，输入含 `last_assistant_message`、`stop_hook_active` | 返回 `decision: "block"` + `reason` | API 错误改发被动 `StopFailure`，输出/退出码被忽略；用户中断不发 `Stop` | 支持自然完成 |
| Codex | `Stop`，输入含 `last_assistant_message`、`stop_hook_active` | 返回 `decision: "block"` + `reason`，自动创建 continuation prompt | 无 `StopFailure`；错误/中断在 `Stop` 前返回 | 支持自然完成 |
| Cursor | `stop`，输入含 `status`、`loop_count`，通用字段含 `transcript_path` | 返回 `followup_message`；`loop_limit: null` 可取消次数上限 | Hook 会收到 `aborted/error`，但 `followup_message` 仅在 `completed` 时消费 | 支持自然完成 |
| Grok | `Stop` 是被动事件 | stdout 被忽略；只有 `PreToolUse` 可阻塞 | `StopFailure` 同样被动 | 首期明确不支持 |

因此首期只在 **Claude Code / Codex / Cursor 的自然完成**时发确认卡。
错误和用户取消不发卡、不尝试外部 `resume`。三家错误路径继续沿用现有生命周期管理：

- Claude `StopFailure → turn-end`，立即置空闲；
- Cursor `stop → turn-end`，包括 `aborted/error`；
- Codex 没有即时 Hook，继续使用 30 分钟 working backstop 降空闲及无 PID 时 1 小时 TTL；
- Grok 的既有 `StopFailure → turn-end` 不受本功能影响。

依据：

- [Claude Code Hooks reference](https://code.claude.com/docs/en/hooks)
- [Codex Hooks](https://learn.chatgpt.com/docs/hooks)
- [Cursor Hooks](https://cursor.com/docs/hooks)
- 本机 Grok `~/.grok/docs/user-guide/10-hooks.md`
- 仓库既有实测与源码分析：`demo/agent-lifecycle/FINDINGS.md`、
  `docs/specs/agent-lifecycle-tracking.md`

## 3. 已确认的产品决策

### 3.1 开关与支持范围

- Claude Code / Codex / Cursor 各自一个“结束时确认”开关，默认关闭。
- preference 在产品语义上独立于“生命周期追踪”和“权限审批”，并跨 integration mode 切换保留；
  但 active confirmation 只在 CLI/MCP mode 安装，None 下暂停并隐藏，重新集成时按原 preference 恢复。
- Grok 显示不支持或不展示开关，不安装 Stop 确认 Hook。
- 仅自然完成触发；错误、API failure、用户主动取消不发卡。
- Codex 的内部 thread 不触发结束确认。新版 Hook 输入优先按
  `thread_source ∈ {"system", "ambient_suggestions"}` 精确识别（拦截集合与 MCP guard 共用，见
  `mcp.md`；后者为 ChatGPT.app 26.721+ 对 ambient 线程的新标签）；在尚未提供该字段的旧版 Codex 中，
  `transcript_path` 字段明确为 `null` 表示未创建 rollout 的 ephemeral thread，同样静默放行。
  字段缺失不按 ephemeral 处理，Claude / Cursor 的行为不受影响。命中后向
  `~/.askhuman/daemon.log` 写一条结构化审计，`reason` 分别为 `codex_blocked_thread_source`（附
  `threadSource` 观测原值）或 `codex_transcript_path_null`。

### 3.2 确认卡

- 标题表达“Agent 准备结束本轮对话”。
- 展示 Agent、项目/工作区、结束时间和最后一段助手回复。
- 最后一段回复按 Unicode 字符计数：
  - `≤ 2,000` 字符：完整展示；
  - `> 2,000` 字符：展示开头 2,000 字符并追加截断标记；
  - 不生成完整内容附件；
  - Cursor transcript 读取失败时显示降级占位，不影响裁决。
- 选择项：
  - `continue` / “继续对话”：Primary，可填写最多 1,000 字符的可选后续指令；
  - `end` / “结束对话”：Destructive，同时作为 dismiss action。
- 用户自由输入后续指令时，各 IM 应自动选中“继续对话”，不得沿用 Permission 的“预选拒绝”。
- 终态展示沿用普通 Ask 的既有回答/取消样式和所选项状态，不新增“允许 / 拒绝”或 Stop 专属终态协议。
- 卡片与交互直接复用普通 Ask 单选流程，不复用 Permission Confirm；Stop 确认不写入普通回复历史。
- 自由文字与选项的结果映射：
  - 选择“继续”时保留可选文字并继续；
  - 只输入文字、未选择选项时视为继续，文字即后续指令；
  - 选择“结束”时结束并丢弃同时提交的文字；
  - 取消 / 关窗视为结束。

### 3.3 继续提示词

按各 Agent 原生 continuation 语义分流（不要一刀切包裹）：

| 场景 | Claude（`reason` = 拒绝停止原因） | Codex（`reason` 会变成新的 user prompt） | Cursor（`followup_message` = 下一条用户消息） |
|---|---|---|---|
| **有后续指令** | 结构化包裹：`[USER CONTINUATION]` + `<user_message>` 装原话 + 要求据此继续 | **裸传**用户原文 | **裸传**用户原文 |
| **无后续指令** | 三家共用 meta：立即用 Instructions 中规定的提问工具联系人类，禁止普通输出代替提问 | 同左 | 同左 |

- 不写死 MCP server、工具或命令名称；文案放在 `prompts.rs` 作为单一来源。
- 单元测试锁定：Claude 有指令必包裹；Cursor/Codex 有指令等于原文；无指令三家 meta 相同且不出现产品/工具名。

### 3.4 重复确认、超时与投放

- 每次用户选择继续后，下一次自然 Stop 仍再次发确认卡。
- 默认 CLI / MCP 提示词在既有“结束前必须询问、必须得到可以结束且没有更多任务的确认”规则后，
  要求 Agent **仅在用户明确批准结束本轮之后** 输出 `[user_confirmed_end_turn]`（提示词仍要求作为最终输出的独立尾行；
  未获该批准时严禁输出该标记）。该规则与 Stop 开关无关，避免切换开关时联动更新提示词。
- Stop Hook 在自然 Stop 的原始最后回复中，只要**任意位置包含**该标记子串即静默放行，并剥离所有出现；
  容忍 markdown 包裹、同行粘连、标记后继续写正文。完全不含标记时安全退化为再次发 Stop 卡。
  标记只用于去重，不能替代通过提问工具发送任何需要用户看到的报告、总结或文件。
- Cursor Hook 配置 `loop_limit: null`，取消默认 5 次上限。
- Hook timeout 与 Hook 客户端等待 deadline 都是 24 小时；超时后主动断开请求连接，daemon 沿既有
  CLI EOF 路径取消普通 Ask。
- daemon 不可达、draining、无可用渠道、渠道全失败、CLI/daemon 连接断开、请求失效或 24 小时到期：
  全部 fail-open，输出 no-op 让 Agent 正常停止。
- 投放规则直接复用普通 Ask：
  - `autoActivation` 关闭：全部已启用渠道；
  - `autoActivation` 开启：当前有效活跃槽 ∪ 正在 watch 该 session 的渠道；Popup 不可用且候选为空时，全发所有可用 IM 作可达性兜底；
  - popup 按现有启用状态与 display 可用性参与抢答；
  - 首个合法答复胜出，其它端异步定格。

## 4. 数据来源与协议输出

### 4.1 Hook 输入

- Claude / Codex：直接使用 Stop stdin 的 `session_id`、`cwd`、`last_assistant_message`；
  不解析 transcript 获取最后回复。
- Cursor：使用 `conversation_id` / `session_id`、`workspace_roots`、`status`、`loop_count`、
  `transcript_path`。仅 `status == "completed"` 进入确认；最后回复从 transcript best-effort 解析。
- Cursor transcript 路径必须经过类型、长度、规范化和预期目录约束，不能把 Hook 输入当作任意文件读取入口。
- 复用 `agents::detect` 的真实家族判定：Cursor/Grok 兼容加载 Claude Hook 时，非目标家族必须 no-op，
  防止重复卡片或把 Grok 错当成 Claude/Cursor。

### 4.2 Hook 输出

用户选择继续：

```jsonc
// Claude Code / Codex
{ "decision": "block", "reason": "<continuation prompt>" }

// Cursor
{ "followup_message": "<continuation prompt>" }
```

用户选择结束或任意 fail-open 路径：

```json
{}
```

stdout 必须只有一个合法 JSON 对象；普通诊断日志只写 stderr，system / ephemeral 抑制审计写
`~/.askhuman/daemon.log`。审计只含 agent、reason 与可用的 session/thread/turn id，不含最后回复或
transcript 路径。Hook 始终以 Agent 可接受的成功码退出。

## 5. 架构设计

### 5.1 单一 AskHuman Stop handler

虽然“结束时确认”和“生命周期追踪”是两个独立开关，但同一家 Agent 的磁盘配置里，
AskHuman **只能拥有一个 Stop handler**。原因是三家都会并发启动同一事件的多条匹配 Hook；若一条先上报
`turn-end`、另一条再等待确认，注册表会提前变空闲并产生竞态。

共享 reconcile 的四种状态：

| 生命周期追踪 | 结束时确认 | AskHuman Stop handler 行为 |
|---|---|---|
| 关 | 关 | 不安装 |
| 开 | 关 | 直接上报 `turn-end` |
| 关 | 开 | 等待确认；不写生命周期注册表 |
| 开 | 开 | 等待确认；结束/fail-open 后上报 `turn-end`，继续时保持 working |

integration mode 在上述矩阵之外再门控 `结束时确认`：None 把 confirm 视为关，但不改持久化
preference；CLI/MCP 才按 preference 取值。故 None + lifecycle 开时共享 handler 必须降为
`track`-only，None + lifecycle 关时完整删除；从 None 返回 CLI/MCP 时自动恢复 `confirm`。

启停任一开关都必须在现有 `IntegrationMutationLock` 内重算目标状态：

- 开启结束确认时接管 AskHuman 自己原有的 lifecycle Stop 条目；
- 关闭结束确认但生命周期仍开时恢复纯上报条目；
- 两者都关才删除 AskHuman Stop 条目；
- 只编辑带自身 marker 的 handler，保留同事件用户/其它插件 Hook；
- 生命周期 `status/outdated` 在 Stop 由确认 handler 代理时仍应判为完整安装；
- Codex 每次 handler 身份变化都要同步重算/迁移 `[hooks.state]` trust hash。

建议新增隐藏入口 `AskHuman __stop-hook <agent>`，但由共享安装器决定它是否带 lifecycle tracking 语义；
不要同时保留原 `__agent-hook <agent> turn-end`。

### 5.2 复用普通 Ask 单选流程

Stop 卡片直接构造一条内部普通 Ask 请求：

- 共享 Message：Agent / 项目元信息 + 最后一段助手回复；
- 单个 Question：“Agent 准备结束本轮对话，接下来怎么做？”；
- 两个预定义选项按稳定顺序放置：index 0=`继续对话`，index 1=`结束对话`；
- `single=true`、`select_only=false`，故 popup / 四 IM 均复用现有“单选 + 自由文字 + 提交”；
- `output_format=json`，Hook 端只按内部 JSON 的 `selected_indices` 和 `user_input` 映射语义，
  不比较本地化后的选项文字。

复用范围包括 popup、飞书、Telegram、Slack、钉钉的渲染/回调、普通 Ask 抢答协调器、活跃槽、
watch 并集、渠道扰动、取消及现有卡片终态，不新增 Stop 专属卡片协议，也不改 Permission Confirm。

需要的薄适配只有：

- 从现有 `client::run_ask_async` 抽出可返回 Final stdout/exit code、而非打印后直接退出的 capture 入口；
- Stop Hook 用 JSON 输出调用普通 Ask，解析结果后再生成 Claude/Codex/Cursor 的 Hook stdout；
- 为内部请求增加默认兼容为 `true` 的 `record_history`（或等价）标志，Stop 确认置 `false`，
  协调器仍完成抢答/收尾但跳过 `history::record`；普通 CLI/MCP Ask 行为不变；
- capture 入口施加 24 小时客户端 deadline；超时 drop 连接触发现有 daemon EOF 取消，随后 Hook fail-open。

结果映射必须是纯函数并完整测试：

| Ask 结果 | Stop 语义 |
|---|---|
| 选 index 0，文字可空 | 继续；文字非空时作为后续指令 |
| 未选项，文字非空 | 继续；文字作为后续指令 |
| 选 index 1（无论是否带文字） | 结束；丢弃文字 |
| Send 但选项/文字均空 | 结束（防御性 fail-open） |
| Cancel / timeout / 非零退出 / 畸形 JSON / 断连 | 结束（fail-open） |

### 5.3 生命周期状态时序

- Stop 确认开始：若生命周期追踪已开，记录继续保持 `working`；等待确认属于在途请求，现有活跃刷新
  机制应防止 working backstop 误降空闲。
- 选择继续：不发 `turn-end`；Agent 的 continuation 保持/重新进入 working。
- 选择结束、超时或 fail-open：恰好上报一次 `turn-end`，清 current tool / turn steps 并置 `idle`。
- 不允许出现“先 idle、后 working”的可见闪烁。

## 6. 单元测试与验证要求

用户明确要求完整单元测试。实现至少覆盖以下矩阵。

### 6.1 Hook 解析与输出

- Claude / Codex 自然 Stop：continue/end/fallback 的精确 JSON shape。
- Cursor：只有 `completed` 进入确认；`aborted/error` no-op；`loop_limit: null` 安装形态。
- Claude `StopFailure`、非 Stop 事件、缺 session、畸形/超大 stdin 全部 fail-open。
- Claude Hook 在 Cursor、Claude/Cursor Hook 在 Grok 下的兼容加载去重。
- `last_assistant_message` 的空值、2,000 边界、2,001 截断、多字节 Unicode。
- `[user_confirmed_end_turn]`：原文任意位置包含该子串即命中；命中后剥离所有出现；近形串不命中；
  Claude / Codex 直接字段与 Cursor transcript（含多 text block、长回复）均在截断前判定。
- Cursor transcript 正常、缺失、半写入、格式漂移、越界路径和超大文件的有界降级。
- continuation prompt：Claude 有指令 XML 包装；Cursor/Codex 有指令裸传；无指令三家共用 meta；不出现 AskHuman/MCP server/具体工具名。

### 6.2 安装器与配置保留

- 生命周期 × 结束确认四种组合的 install/uninstall/update 全矩阵及任意切换顺序。
- None/CLI/MCP × lifecycle × Stop preference 全矩阵：None 清除 `confirm` 但保留 preference，
  tracking 开时仅保留 `track`，重新集成时恢复 `confirm`。
- 同一 Agent 任一状态下 AskHuman 自己最多一条 Stop handler。
- 保留同事件其它用户 handler、注释、JSONC/TOML 格式与无关字段。
- Claude Nested、Cursor Flat、Codex hooks.json + trust hash 的 status/outdated/reconcile。
- 二进制路径变化、旧 marker、半安装、trust 缺失的迁移。
- mutation lock 下重复执行幂等。

### 6.3 普通 Ask 复用回归

- Stop 请求固定为单题、单选、非严格模式；选项 index 稳定且本地化文案不参与协议判断。
- 上表五类 Ask 结果到 continue/end 的纯函数映射全覆盖。
- popup/飞书/Slack/Telegram/钉钉均能“选项 + 自由文字”提交；纯文字视为 continue；end 丢弃文字。
- 首答胜出、取消、draining、CLI EOF、24h capture timeout、渠道全失败均符合 fail-open。
- Stop 请求 `record_history=false`，普通 CLI/MCP Ask 仍记录历史，现有历史单测不回归。
- autoActivation 关/开、活跃槽、watch 并集的候选渠道矩阵。
- Permission Confirm 的协议、渲染和测试不做功能修改；运行既有测试作为旁路回归。

### 6.4 生命周期状态机

- track off/on 下 continue/end/fallback 的状态变化。
- track on + stop confirm on 时不提前触发 TurnEnd、不出现 idle 闪烁、最终只应用一次 TurnEnd。
- 24 小时等待期间在途请求保护 working backstop；请求完结后保护解除。
- Claude StopFailure、Cursor aborted/error、Codex 无 Hook 兜底的既有行为回归。

### 6.5 实装验证（实现阶段）

功能逻辑完成后必须按项目约定：

1. 运行 Rust/前端相关测试；
2. `./scripts/install.sh` 安装新二进制；
3. 使用新安装的 AskHuman 继续后续确认；
4. 经用户授权后分别真机验证 Claude Code、Codex、Cursor 的自然 Stop：
   end、空指令 continue、有指令 continue、连续多轮、24h 以缩短测试 deadline 模拟；
5. 验证四 IM 与 popup 的至少一轮普通 Ask 单选卡，尤其“纯文字=继续、结束丢弃文字”的映射；
6. 验证 Grok 兼容加载用户级 Claude/Cursor Hook 时不会出现 Stop 卡。

实际启动 Agent 会消耗用户 token，必须另经 AskHuman 明确许可并由用户操作 Agent。

## 7. 非目标与风险

- 不通过 Hook 内另起 `claude --resume` / `codex resume` / `cursor-agent --resume` 恢复错误会话；
  这不等价于继续当前 TUI，并有终端归属、并发 transcript、权限继承风险。
- 不支持 Grok Stop 自动续跑，除非 Grok 后续提供可阻塞 Stop 或可消费的 follow-up 输出。
- 不保证 Agent 错误、用户中断、进程崩溃时出现接管卡。
- Agent 仍可在续跑后再次不按 Instructions 提问；本设计通过每次自然 Stop 都再次询问来保留人工接管点。
- 多个第三方 Stop Hook 仍可能彼此影响；AskHuman 只能保证自身 handler 单一，设置/doctor 应提示同事件
  其它 handler 的共存风险。
